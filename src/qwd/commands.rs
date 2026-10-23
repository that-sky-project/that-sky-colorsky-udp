//! QWD command handling.

use std::collections::{HashMap, HashSet};
use std::net::IpAddr;
use std::time::Instant;

use serde_json::{Value, json};

use crate::control::{ControlCommand, ControlResponse, RoomEndpoint};
use crate::protocol::net_address::NetAddress;

use super::protocol::{
    BAD_REQUEST, CONFLICT, INTERNAL, ManagerCommand, NO_CAPACITY, NODE_UNAVAILABLE, NOT_FOUND, OK,
    STALE_PLAN, ack, event,
};
use super::util::{control_error, generate_room_id, now_ms};
use super::{AckParts, PendingQwdMove, QwdClient, Socket};

impl QwdClient {
    pub(super) fn handle_command(&mut self, socket: &mut Socket, text: &str) -> Result<(), String> {
        let value: Value = match serde_json::from_str(text) {
            Ok(value) => value,
            Err(error) => {
                tracing::warn!(error = %error, "[QWD] invalid command JSON");
                return Ok(());
            }
        };

        // The manager answers an invalid node event with `{ code, data: {
        // message } }` and no command fields. Treat it as an error rather than
        // letting serde report "missing field id" every second.
        if value.get("cmd").is_none() {
            let code = value.get("code").and_then(Value::as_i64);
            let message = value
                .pointer("/data/message")
                .and_then(Value::as_str)
                .unwrap_or("");
            let signature = format!("{code:?}:{message}");
            if self.last_manager_error.as_deref() != Some(signature.as_str()) {
                tracing::warn!(
                    code = ?code,
                    message,
                    raw = %text,
                    "[QWD] manager rejected a message"
                );
                self.last_manager_error = Some(signature);
            } else {
                tracing::debug!(code = ?code, message, "[QWD] manager error (repeated)");
            }
            return Ok(());
        }

        let command: ManagerCommand = match serde_json::from_value(value) {
            Ok(command) => command,
            Err(error) => {
                tracing::warn!(error = %error, "[QWD] invalid command JSON");
                return Ok(());
            }
        };

        if command.cmd == "room.id_conflict" {
            return self.handle_room_id_conflict(socket);
        }

        if command.id <= 0 {
            tracing::warn!(id = command.id, cmd = %command.cmd, "[QWD] invalid command id");
            return Ok(());
        }

        if command.v == 0 {
            let value = ack(
                command.id,
                BAD_REQUEST,
                Some("unsupported protocol version"),
                None,
            );
            return self.send_json(socket, &value);
        }

        tracing::debug!(id = command.id, cmd = %command.cmd, "[QWD] command");

        let result = match command.cmd.as_str() {
            "room.reserve" => self.handle_room_reserve(&command),
            "move.prepare" => self.handle_move_prepare(&command),
            "move.cancel" => self.handle_move_cancel(&command),
            "move.commit" | "player.redirect" => self.handle_move_commit(socket, &command),
            "room.destroy" => self.handle_room_destroy(socket, &command),
            "node.drain" => self.handle_node_drain(socket, &command),
            _ => Ok((
                BAD_REQUEST,
                Some(format!("unsupported command: {}", command.cmd)),
                None,
            )),
        };

        let (code, message, data) = match result {
            Ok(result) => result,
            Err(message) => (INTERNAL, Some(message), None),
        };

        let value = ack(command.id, code, message.as_deref(), data);
        tracing::debug!(
            id = command.id,
            cmd = %command.cmd,
            code,
            "[QWD] command ack"
        );
        self.send_json(socket, &value)
    }

    fn handle_room_id_conflict(&mut self, socket: &mut Socket) -> Result<(), String> {
        self.room_id = generate_room_id();
        self.reservations.clear();
        self.internal_to_qwd.clear();
        self.qwd_moves.clear();
        // New room id, so its telemetry sequence starts over.
        self.telemetry_seq = 0;
        self.telemetry_roster = None;
        self.last_telemetry_seq_sent = 0;
        self.last_full_telemetry_seq = 0;
        self.last_full_telemetry_at = Instant::now();
        self.last_manager_error = None;

        tracing::warn!(room_id = %self.room_id, "[QWD] room id conflict; regenerated room id");
        self.send_room_created(socket)?;
        self.replay_players(socket)
    }

    fn handle_room_reserve(&mut self, command: &ManagerCommand) -> Result<AckParts, String> {
        if self.draining {
            return Ok((NO_CAPACITY, Some("node is draining".to_string()), None));
        }

        let Some(room_id) = command.room_id() else {
            return Ok((BAD_REQUEST, Some("roomId is required".to_string()), None));
        };
        if room_id != self.room_id {
            return Ok((NOT_FOUND, Some("room not found".to_string()), None));
        }
        let Some(player_id) = command.string_field("playerId") else {
            return Ok((BAD_REQUEST, Some("playerId is required".to_string()), None));
        };

        let status = self.status()?;
        if status
            .player_list
            .iter()
            .any(|player| player.uuid == player_id)
        {
            return Ok((
                OK,
                None,
                Some(json!({
                    "accepted": true,
                    "reservationId": format!("existing-{player_id}"),
                    "slots": 0
                })),
            ));
        }

        let now = Instant::now();
        let reserved = self.reservations.count(now);
        if status.player_list.len() + reserved + 1 > self.config.capacity {
            return Ok((
                NO_CAPACITY,
                Some("room capacity reserved".to_string()),
                None,
            ));
        }

        let reservation_id = self
            .reservations
            .reserve_player(player_id, command.ttl(), now);

        tracing::debug!(
            room_id = %room_id,
            player_id = %player_id,
            reservation_id = %reservation_id,
            "[QWD] room.reserve"
        );

        Ok((
            OK,
            None,
            Some(json!({
                "accepted": true,
                "reservationId": reservation_id,
                "slots": 1
            })),
        ))
    }

    fn handle_move_prepare(&mut self, command: &ManagerCommand) -> Result<AckParts, String> {
        if self.draining {
            return Ok((NO_CAPACITY, Some("node is draining".to_string()), None));
        }

        let Some(room_id) = command.room_id() else {
            return Ok((BAD_REQUEST, Some("roomId is required".to_string()), None));
        };
        if room_id != self.room_id {
            return Ok((NOT_FOUND, Some("room not found".to_string()), None));
        }
        let Some(move_id) = command.move_id() else {
            return Ok((BAD_REQUEST, Some("moveId is required".to_string()), None));
        };
        let player_ids = command.player_ids();
        if player_ids.is_empty() || player_ids.len() > 7 {
            return Ok((
                BAD_REQUEST,
                Some("playerIds must contain 1..=7 entries".to_string()),
                None,
            ));
        }

        let status = self.status()?;
        if player_ids.iter().any(|player_id| {
            status
                .player_list
                .iter()
                .any(|player| &player.uuid == player_id)
        }) {
            return Ok((CONFLICT, Some("player already in room".to_string()), None));
        }

        if let Some(expected) = command.u64_field("expectedVersion") {
            // The manager's room version is the seq of the last telemetry
            // frame it actually received, which may lag telemetry_seq when a
            // delta was skipped because nothing changed.
            let current = self.last_telemetry_seq_sent;
            if expected < current && current - expected > 3 {
                tracing::info!(
                    move_id = %move_id,
                    expected_version = expected,
                    current_version = current,
                    "[QWD] move.prepare rejected: stale room snapshot"
                );
                return Ok((
                    STALE_PLAN,
                    Some("room state advanced past the plan".to_string()),
                    Some(json!({
                        "expectedVersion": expected,
                        "currentVersion": current,
                        "players": status.player_list.len()
                    })),
                ));
            }
        }

        let now = Instant::now();
        let reserved = self.reservations.reserved_players(now);
        let new_players = player_ids
            .iter()
            .filter(|player_id| !reserved.contains(*player_id))
            .count();
        if status.player_list.len() + reserved.len() + new_players > self.config.capacity {
            return Ok((
                NO_CAPACITY,
                Some("room capacity reserved".to_string()),
                None,
            ));
        }

        if self
            .reservations
            .reserve_move(move_id, &player_ids, command.ttl(), now)
            .is_err()
        {
            return Ok((
                CONFLICT,
                Some("move reservation conflicts".to_string()),
                None,
            ));
        }

        tracing::debug!(
            move_id = %move_id,
            room_id = %room_id,
            players = ?player_ids,
            "[QWD] move.prepare"
        );

        Ok((
            OK,
            None,
            Some(json!({
                "accepted": true,
                "moveId": move_id,
                "slots": player_ids.len()
            })),
        ))
    }

    fn handle_move_cancel(&mut self, command: &ManagerCommand) -> Result<AckParts, String> {
        let Some(move_id) = command.move_id() else {
            return Ok((BAD_REQUEST, Some("moveId is required".to_string()), None));
        };
        self.reservations.release_move(move_id);
        tracing::debug!(move_id = %move_id, "[QWD] move.cancel");
        Ok((OK, None, Some(json!({ "accepted": true }))))
    }

    fn handle_move_commit(
        &mut self,
        socket: &mut Socket,
        command: &ManagerCommand,
    ) -> Result<AckParts, String> {
        let move_id = command
            .move_id()
            .map(str::to_string)
            .unwrap_or_else(|| format!("redirect-{}-{}", now_ms(), command.id));

        let player_ids = command.player_ids();
        if player_ids.is_empty() || player_ids.len() > 7 {
            return Ok((
                BAD_REQUEST,
                Some("playerIds must contain 1..=7 entries".to_string()),
                None,
            ));
        }

        if let Some(from_room_id) = command.string_field("fromRoomId")
            && from_room_id != self.room_id
        {
            return Ok((NOT_FOUND, Some("source room not found".to_string()), None));
        }

        let Some(to_room_id) = command.room_id() else {
            return Ok((BAD_REQUEST, Some("toRoomId is required".to_string()), None));
        };

        tracing::debug!(
            move_id = %move_id,
            players = ?player_ids,
            to_room_id = %to_room_id,
            "[QWD] move.commit"
        );
        let Some(udp_host) = command.string_field("udpHost") else {
            return Ok((BAD_REQUEST, Some("udpHost is required".to_string()), None));
        };
        let Some(udp_port) = command.u64_field("udpPort") else {
            return Ok((BAD_REQUEST, Some("udpPort is required".to_string()), None));
        };
        if udp_port == 0 || udp_port > u16::MAX as u64 {
            return Ok((BAD_REQUEST, Some("invalid udpPort".to_string()), None));
        }
        let host: IpAddr = match udp_host.parse() {
            Ok(host) => host,
            Err(_) => {
                return Ok((BAD_REQUEST, Some("invalid udpHost".to_string()), None));
            }
        };

        let status = self.status()?;
        let mut net_to_uuid = HashMap::new();
        let mut uuid_to_net = HashMap::new();
        for player in &status.player_list {
            net_to_uuid.insert(player.player_id, player.uuid.clone());
            uuid_to_net.insert(player.uuid.clone(), player.player_id);
        }

        let mut net_ids = Vec::new();
        let mut missing = Vec::new();
        for player_id in &player_ids {
            match uuid_to_net.get(player_id) {
                Some(net_id) => net_ids.push(*net_id),
                None => missing.push(player_id.clone()),
            }
        }

        if !missing.is_empty() {
            self.send_move_result(
                socket,
                &move_id,
                false,
                &[],
                &missing,
                Some("player_not_found"),
            )?;
            return Ok((
                NOT_FOUND,
                Some("one or more players are not in this room".to_string()),
                Some(json!({ "failedPlayerIds": missing })),
            ));
        }

        let room = RoomEndpoint {
            id: to_room_id.to_string(),
            address: NetAddress::new(host, udp_port as u16),
            uuid: None,
            capacity: 8,
            players: 0,
        };

        if let Err(error) = self.control.upsert_room(room) {
            let message = control_error(error);
            self.send_move_result(socket, &move_id, false, &[], &player_ids, Some(&message))?;
            return Ok((INTERNAL, Some(message), None));
        }

        match self.control.move_players(net_ids, to_room_id) {
            Ok(ControlResponse::Move(outcome)) => {
                if outcome.moved.is_empty() {
                    self.send_move_result(
                        socket,
                        &move_id,
                        false,
                        &[],
                        &player_ids,
                        Some("move_not_started"),
                    )?;
                    return Ok((
                        CONFLICT,
                        Some("move was not started".to_string()),
                        Some(json!({ "accepted": false, "failedPlayerIds": player_ids })),
                    ));
                }

                let mut pending = HashSet::new();
                for internal_id in &outcome.move_ids {
                    self.internal_to_qwd.insert(*internal_id, move_id.clone());
                    pending.insert(*internal_id);
                }
                self.qwd_moves.insert(
                    move_id.clone(),
                    PendingQwdMove {
                        pending,
                        moved: Vec::new(),
                        failed: Vec::new(),
                        reason: None,
                    },
                );

                let moved: Vec<String> = outcome
                    .moved
                    .iter()
                    .filter_map(|net_id| net_to_uuid.get(net_id).cloned())
                    .collect();
                let failed: Vec<String> = outcome
                    .failed
                    .iter()
                    .filter_map(|net_id| net_to_uuid.get(net_id).cloned())
                    .collect();

                Ok((
                    OK,
                    None,
                    Some(json!({
                        "accepted": true,
                        "movedPlayerIds": moved,
                        "failedPlayerIds": failed
                    })),
                ))
            }
            Ok(ControlResponse::Error { message }) => {
                self.send_move_result(socket, &move_id, false, &[], &player_ids, Some(&message))?;
                Ok((CONFLICT, Some(message), Some(json!({ "accepted": false }))))
            }
            Ok(_) => Ok((INTERNAL, Some("unexpected move response".to_string()), None)),
            Err(error) => {
                let message = control_error(error);
                self.send_move_result(socket, &move_id, false, &[], &player_ids, Some(&message))?;
                Ok((NODE_UNAVAILABLE, Some(message), None))
            }
        }
    }

    fn handle_room_destroy(
        &mut self,
        socket: &mut Socket,
        command: &ManagerCommand,
    ) -> Result<AckParts, String> {
        let Some(room_id) = command.room_id() else {
            return Ok((BAD_REQUEST, Some("roomId is required".to_string()), None));
        };
        if room_id != self.room_id {
            return Ok((NOT_FOUND, Some("room not found".to_string()), None));
        }

        self.draining = true;
        if let Err(error) = self.control.set_draining(true) {
            tracing::warn!(error = %control_error(error), "[QWD] failed to set draining");
        }

        let status = self.status()?;
        for player in status.player_list {
            let _ = self.control.submit(ControlCommand::KickPlayer {
                player_id: player.player_id,
                reason: "room destroyed".to_string(),
            });
        }

        let value = event(
            "room.destroyed",
            json!({
                "roomId": self.room_id,
                "timestamp": now_ms()
            }),
        );
        self.send_json(socket, &value)?;
        Ok((OK, None, Some(json!({ "accepted": true }))))
    }

    fn handle_node_drain(
        &mut self,
        socket: &mut Socket,
        _command: &ManagerCommand,
    ) -> Result<AckParts, String> {
        self.draining = true;
        if let Err(error) = self.control.set_draining(true) {
            tracing::warn!(error = %control_error(error), "[QWD] failed to set draining");
        }
        self.send_node_draining(socket)?;
        Ok((OK, None, Some(json!({ "accepted": true }))))
    }
}
