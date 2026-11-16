//! `move.commit` / `player.redirect`: the manager asking this node to hand a
//! group of players to another room.

use std::collections::{HashMap, HashSet};
use std::net::IpAddr;

use serde_json::json;

use crate::control::{ControlResponse, RoomEndpoint};
use crate::protocol::net_address::NetAddress;
use crate::protocol::types::TgcUuid;

use super::facts::{expected_participants, participant_matches};
use super::protocol::{
    BAD_REQUEST, CONFLICT, INTERNAL, ManagerCommand, NODE_UNAVAILABLE, NOT_FOUND, OK, STALE_PLAN,
};
use super::settle::MoveReport;
use super::util::{control_error, destination_uuid, now_ms};
use super::{AckParts, PendingQwdMove, QwdClient, Socket};

impl QwdClient {
    pub(super) fn handle_move_commit(
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

        // Participant guards: reject a plan built from stale state instead of
        // moving a player whose session, level or generation already changed.
        let status = self.status()?;
        let reservation_token = command
            .string_field("reservationToken")
            .map(str::to_string)
            .or_else(|| self.reservations.token_of(&move_id).map(str::to_string));
        if let Some(guard) = command.data.get("expectedParticipants") {
            for expected in expected_participants(guard) {
                let Some(player) = status
                    .player_list
                    .iter()
                    .find(|player| player.uuid == expected.player_id)
                else {
                    continue;
                };
                if !participant_matches(&expected, player) {
                    self.send_move_result(
                        socket,
                        MoveReport {
                            move_id: &move_id,
                            accepted: false,
                            moved: &[],
                            failed: std::slice::from_ref(&expected.player_id),
                            reason: Some("source_participant_changed"),
                            token: reservation_token.as_deref(),
                        },
                    )?;
                    return Ok((
                        STALE_PLAN,
                        Some("source participant changed".to_string()),
                        Some(json!({ "playerId": expected.player_id })),
                    ));
                }
            }
        }

        let Some(to_room_id) = command
            .string_field("toRoomUuid")
            .or_else(|| command.room_id())
        else {
            return Ok((BAD_REQUEST, Some("toRoomId is required".to_string()), None));
        };

        // The client needs the destination room UUID in MoveGame to complete
        // the transfer; a zero UUID makes it drop the connection instead of
        // joining the target node.
        //
        // The manager may send `toRoomUuid` explicitly (newer builds do) or
        // only the room id. Nodes now use 8-digit decimal room ids, which are
        // not `TgcUuid` text, so derive the same deterministic uuid every node
        // uses for that id instead of rejecting the move.
        let destination = command
            .string_field("toRoomUuid")
            .and_then(TgcUuid::parse)
            .or_else(|| destination_uuid(to_room_id));
        let Some(destination) = destination else {
            return Ok((
                BAD_REQUEST,
                Some("invalid destination room uuid".to_string()),
                Some(json!({ "toRoomId": to_room_id })),
            ));
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
        let Ok(host) = udp_host.parse::<IpAddr>() else {
            return Ok((BAD_REQUEST, Some("invalid udpHost".to_string()), None));
        };

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
                MoveReport {
                    move_id: &move_id,
                    accepted: false,
                    moved: &[],
                    failed: &missing,
                    reason: Some("player_not_found"),
                    token: reservation_token.as_deref(),
                },
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
            uuid: Some(destination),
            capacity: 8,
            players: 0,
        };

        if let Err(error) = self.control.upsert_room(room) {
            let message = control_error(error);
            self.send_move_result(
                socket,
                MoveReport {
                    move_id: &move_id,
                    accepted: false,
                    moved: &[],
                    failed: &player_ids,
                    reason: Some(&message),
                    token: reservation_token.as_deref(),
                },
            )?;
            return Ok((INTERNAL, Some(message), None));
        }

        match self.control.move_players(net_ids, to_room_id) {
            Ok(ControlResponse::Move(outcome)) => {
                if outcome.moved.is_empty() {
                    self.send_move_result(
                        socket,
                        MoveReport {
                            move_id: &move_id,
                            accepted: false,
                            moved: &[],
                            failed: &player_ids,
                            reason: Some("move_not_started"),
                            token: reservation_token.as_deref(),
                        },
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
                        token: reservation_token,
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
                self.send_move_result(
                    socket,
                    MoveReport {
                        move_id: &move_id,
                        accepted: false,
                        moved: &[],
                        failed: &player_ids,
                        reason: Some(&message),
                        token: reservation_token.as_deref(),
                    },
                )?;
                Ok((CONFLICT, Some(message), Some(json!({ "accepted": false }))))
            }
            Ok(_) => Ok((INTERNAL, Some("unexpected move response".to_string()), None)),
            Err(error) => {
                let message = control_error(error);
                self.send_move_result(
                    socket,
                    MoveReport {
                        move_id: &move_id,
                        accepted: false,
                        moved: &[],
                        failed: &player_ids,
                        reason: Some(&message),
                        token: reservation_token.as_deref(),
                    },
                )?;
                Ok((NODE_UNAVAILABLE, Some(message), None))
            }
        }
    }
}
