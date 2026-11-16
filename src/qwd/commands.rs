//! QWD command handling.

use std::time::Instant;

use serde_json::{Value, json};

use crate::control::ControlCommand;

use super::facts::with_facts;
use super::protocol::{
    BAD_REQUEST, CONFLICT, INTERNAL, ManagerCommand, NO_CAPACITY, NOT_FOUND, OK, ack, event,
};
use super::reservations::{ReserveMoveError, matches_token};
use super::util::{control_error, generate_room_id, now_ms};
use super::{AckParts, QwdClient, Socket};

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
            self.handle_manager_rejection(text);
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
            "room.sync" => self.handle_room_sync(&command),
            "move.prepare" | "move.renew" => self.handle_move_prepare(&command),
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

        // Room-scoped success acks carry capacity facts so the manager can
        // correct its cached snapshot from the node's own numbers.
        let data = if code == OK
            && matches!(
                command.cmd.as_str(),
                "room.reserve" | "room.sync" | "move.prepare" | "move.renew" | "move.cancel"
            ) {
            self.facts_data(data)
        } else {
            data
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

    /// Log a manager-side error reply and learn the manager's capabilities
    /// from its validation output.
    fn handle_manager_rejection(&mut self, text: &str) {
        let value: Value = serde_json::from_str(text).unwrap_or(Value::Null);
        let code = value.get("code").and_then(Value::as_i64);
        let message = value
            .pointer("/data/message")
            .and_then(Value::as_str)
            .unwrap_or("");
        let disabled = self.caps.observe_rejection(message);
        if !disabled.is_empty() {
            tracing::warn!(
                features = ?disabled,
                code = ?code,
                "[QWD] manager is an older build; disabling unsupported events"
            );
        }
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
    }

    /// Ack payload for the room-scoped commands, enriched with capacity facts.
    fn facts_data(&mut self, data: Option<Value>) -> Option<Value> {
        let status = self.status().ok()?;
        let now = Instant::now();
        let facts = self
            .reservations
            .facts(&status, self.config.capacity, self.telemetry_seq, now);
        with_facts(data, facts)
    }

    fn handle_room_id_conflict(&mut self, socket: &mut Socket) -> Result<(), String> {
        self.room_id = generate_room_id();
        self.reservations.clear();
        self.internal_to_qwd.clear();
        self.qwd_moves.clear();
        // New room id, so its telemetry sequence starts over.
        self.telemetry_seq = 0;
        self.telemetry_roster = None;
        self.last_telemetry_sent_at.clear();
        self.last_telemetry_seq_sent = 0;
        self.last_full_telemetry_seq = 0;
        self.last_full_telemetry_at = Instant::now();
        self.last_manager_error = None;

        tracing::warn!(room_id = %self.room_id, "[QWD] room id conflict; regenerated room id");
        // The new room id is unknown to the manager, so `room.created` (with
        // its authoritative player count) plus the full telemetry frame that
        // follows is all it needs. Replaying `player.join` here would bump the
        // room's occupancy once per player.
        self.send_room_created(socket)
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
        let presented_token = command.string_field("reservationToken");

        let status = self.status()?;
        let now = Instant::now();

        // An allocation for a player that is already here is a no-op; renewing
        // an existing hold must come from its owner.
        if let Some(existing) = self.reservations.player_token_of(player_id, now) {
            if !matches_token(Some(existing), presented_token) {
                return Ok((
                    CONFLICT,
                    Some("reservation owner conflicts".to_string()),
                    None,
                ));
            }
            let token = self
                .reservations
                .reserve_player(player_id, command.ttl(), now);
            return Ok((
                OK,
                None,
                Some(json!({
                    "accepted": true,
                    "reservationId": player_id,
                    "reservationToken": token,
                    "idempotent": true,
                    "slots": 1
                })),
            ));
        }
        if status
            .player_list
            .iter()
            .any(|player| player.uuid == player_id)
        {
            return Ok((
                CONFLICT,
                Some("player already physically present".to_string()),
                None,
            ));
        }
        if status.player_list.len() + self.reservations.outstanding_slots(now) + 1
            > self.config.capacity
        {
            return Ok((
                NO_CAPACITY,
                Some("room capacity reserved".to_string()),
                None,
            ));
        }

        let token = self
            .reservations
            .reserve_player(player_id, command.ttl(), now);
        let expires_at = now_ms() + command.ttl().as_millis() as u64;

        tracing::debug!(room_id = %room_id, player_id = %player_id, "[QWD] room.reserve");

        Ok((
            OK,
            None,
            Some(json!({
                "accepted": true,
                "reservationId": player_id,
                "reservationToken": token,
                "expiresAt": expires_at,
                "slots": 1
            })),
        ))
    }

    /// `room.sync`: the manager's snapshot drifted and wants a full frame.
    fn handle_room_sync(&mut self, command: &ManagerCommand) -> Result<AckParts, String> {
        if let Some(room_id) = command.room_id()
            && room_id != self.room_id
        {
            return Ok((NOT_FOUND, Some("room not found".to_string()), None));
        }
        self.force_full = true;
        tracing::debug!("[QWD] room.sync; forcing full telemetry");
        Ok((OK, None, None))
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
        let presented_token = command.string_field("reservationToken");

        let status = self.status()?;
        let now = Instant::now();

        if let Some(expected) = command.u64_field("expectedVersion")
            && expected > self.last_telemetry_seq_sent
        {
            // The manager's snapshot is ahead of our last published seq (for
            // example right after our reconnect). It re-syncs with `room.sync`
            // and its own participant guards, so accept the move.
            tracing::debug!(
                move_id = %move_id,
                expected_version = expected,
                current_version = self.last_telemetry_seq_sent,
                "[QWD] move.prepare ahead of our telemetry; accepting"
            );
        }

        if player_ids.iter().any(|player_id| {
            status
                .player_list
                .iter()
                .any(|player| &player.uuid == player_id)
        }) {
            return Ok((
                CONFLICT,
                Some("participant already present".to_string()),
                None,
            ));
        }

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

        let token = match self.reservations.reserve_move(
            move_id,
            &player_ids,
            command.ttl(),
            now,
            presented_token,
        ) {
            Ok(token) => token,
            Err(ReserveMoveError::ForeignToken) => {
                return Ok((
                    CONFLICT,
                    Some("reservation owner conflicts".to_string()),
                    None,
                ));
            }
            Err(ReserveMoveError::Conflict) => {
                return Ok((
                    CONFLICT,
                    Some("move reservation conflicts".to_string()),
                    None,
                ));
            }
        };
        let expires_at = now_ms() + command.ttl().as_millis() as u64;

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
                "reservationToken": token,
                "expiresAt": expires_at,
                "slots": player_ids.len()
            })),
        ))
    }

    fn handle_move_cancel(&mut self, command: &ManagerCommand) -> Result<AckParts, String> {
        let Some(move_id) = command.move_id() else {
            return Ok((BAD_REQUEST, Some("moveId is required".to_string()), None));
        };
        let presented = command.string_field("reservationToken");
        if !matches_token(self.reservations.token_of(move_id), presented) {
            return Ok((
                CONFLICT,
                Some("reservation owner conflicts".to_string()),
                None,
            ));
        }
        self.reservations.release_move(move_id);
        tracing::debug!(move_id = %move_id, "[QWD] move.cancel");
        let mut data = json!({
            "accepted": true,
            "moveId": move_id,
        });
        if let Some(token) = presented {
            data["reservationToken"] = Value::String(token.to_string());
        }
        Ok((OK, None, Some(data)))
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
