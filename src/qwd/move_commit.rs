//! `move.commit` / `player.redirect`: the manager asking this node to hand a
//! group of players to another room.

use std::collections::{HashMap, HashSet};
use std::net::IpAddr;

use serde_json::json;

use crate::control::{ControlResponse, PlayerInfo, RoomEndpoint};
use crate::protocol::net_address::NetAddress;
use crate::protocol::types::TgcUuid;

use super::facts::{expected_participants, participant_matches};
use super::protocol::{
    BAD_REQUEST, CONFLICT, INTERNAL, ManagerCommand, NODE_UNAVAILABLE, NOT_FOUND, OK, STALE_PLAN,
};
use super::settle::MoveReport;
use super::util::{control_error, destination_uuid, now_ms};
use super::{AckParts, PendingQwdMove, QwdClient, Socket};

/// Affinity weight at or above which a frozen bond must not be torn apart by
/// a move commit (a client value of 230/255 or more).
const BOND_THRESHOLD: f32 = 0.9;

/// Enforce bond completeness for a move commit: for every moving player, each
/// resolved affinity edge at or above [`BOND_THRESHOLD`] must point at another
/// player in the moving group or at a peer that is no longer connected in this
/// room. Bonds are never waived by moving or churn states, so a bonded peer
/// that is still connected outside the group rejects the whole commit.
/// Returns the rejection message naming both players for the first violation.
fn bond_conflict(player_list: &[PlayerInfo], moving_group: &[String]) -> Option<String> {
    let moving: HashSet<&str> = moving_group.iter().map(String::as_str).collect();
    for player in player_list {
        let player_moves = moving.contains(player.uuid.as_str());
        for (peer_uuid, weight) in &player.affinity {
            if *weight < BOND_THRESHOLD {
                continue;
            }
            let Some(peer) = player_list.iter().find(|peer| peer.uuid == *peer_uuid) else {
                continue;
            };
            let peer_moves = moving.contains(peer.uuid.as_str());
            if player_moves != peer_moves {
                let (moving_player, outside_peer) = if player_moves {
                    (&player.uuid, &peer.uuid)
                } else {
                    (&peer.uuid, &player.uuid)
                };
                return Some(format!(
                    "player {moving_player} cannot move: bonded peer {outside_peer} (affinity {weight:.3}) is still connected in this room and not in the moving group"
                ));
            }
        }
    }
    None
}

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

        let player_ids = match command.player_ids() {
            Ok(ids) => ids,
            Err(message) => return Ok((BAD_REQUEST, Some(message), None)),
        };
        let deadline_ms = match command.data.get("expiresAt") {
            None => None,
            Some(value) => match value.as_u64() {
                Some(deadline) if deadline > now_ms() => Some(deadline),
                _ => {
                    return Ok((
                        STALE_PLAN,
                        Some("invalid or expired move delivery deadline".into()),
                        None,
                    ));
                }
            },
        };
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
        let guards = match command.data.get("expectedParticipants") {
            None => Vec::new(),
            Some(value) => match expected_participants(value) {
                Ok(guards) => guards,
                Err(message) => return Ok((BAD_REQUEST, Some(message), None)),
            },
        };
        if !guards.is_empty()
            && (guards.len() != player_ids.len()
                || guards
                    .iter()
                    .any(|guard| !player_ids.contains(&guard.player_id)))
        {
            return Ok((
                BAD_REQUEST,
                Some("participant guards must exactly match playerIds".into()),
                None,
            ));
        }
        for guard in &guards {
            if !status
                .player_list
                .iter()
                .any(|player| participant_matches(guard, player))
            {
                return Ok((STALE_PLAN, Some("source participant changed".into()), None));
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

        // Bond completeness: a strong frozen bond must move with its group or
        // already be gone. This runs before the room is registered and before
        // any MoveGame can be enqueued, so a rejected commit sends nothing to
        // the clients.
        if let Some(message) = bond_conflict(&status.player_list, &player_ids) {
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
            return Ok((CONFLICT, Some(message), Some(json!({ "accepted": false }))));
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

        let execution_guards = guards
            .iter()
            .map(|guard| crate::control::ExpectedPlayerGuard {
                player_id: uuid_to_net[&guard.player_id],
                uuid: guard.player_id.clone(),
                session_id: guard.session_id.clone(),
                level_id: guard.level_id,
                lv_seq: guard.lv_seq,
            })
            .collect();
        match self.control.move_players_guarded(
            net_ids,
            to_room_id.to_string(),
            execution_guards,
            deadline_ms,
        ) {
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
                        failed: outcome
                            .failed
                            .iter()
                            .filter_map(|id| net_to_uuid.get(id).cloned())
                            .collect(),
                        reason: if outcome.failed.is_empty() {
                            None
                        } else {
                            Some("partial_source_rejection".into())
                        },
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

#[cfg(test)]
mod tests {
    use super::bond_conflict;
    use crate::control::PlayerInfo;

    fn player(uuid: &str, affinity: Vec<(&str, f32)>) -> PlayerInfo {
        PlayerInfo {
            player_id: 0,
            uuid: uuid.to_string(),
            level_id: 7,
            lv_seq: 0,
            moving: false,
            session_id: format!("{uuid}-session"),
            is_host: false,
            level_hash: None,
            position: None,
            velocity: None,
            state_at: 0,
            last_move_at: 0,
            affinity: affinity
                .into_iter()
                .map(|(peer, weight)| (peer.to_string(), weight))
                .collect(),
        }
    }

    fn group(ids: &[&str]) -> Vec<String> {
        ids.iter().map(|id| id.to_string()).collect()
    }

    #[test]
    fn connected_bonded_peer_outside_the_group_is_rejected_with_both_ids() {
        let roster = [
            player("mover", vec![("bonded", 1.0)]),
            player("bonded", vec![]),
            player("bystander", vec![]),
        ];
        let message = bond_conflict(&roster, &group(&["mover"])).expect("bond conflict");
        assert!(message.contains("mover"), "{message}");
        assert!(message.contains("bonded"), "{message}");
    }

    #[test]
    fn threshold_is_value_230_of_255() {
        // 230/255 >= 0.9 bonds and must not be left behind.
        let strong = [
            player("mover", vec![("bonded", 230.0 / 255.0)]),
            player("bonded", vec![]),
        ];
        assert!(bond_conflict(&strong, &group(&["mover"])).is_some());

        // 229/255 < 0.9 is not a bond.
        let weak = [
            player("mover", vec![("bonded", 229.0 / 255.0)]),
            player("bonded", vec![]),
        ];
        assert!(bond_conflict(&weak, &group(&["mover"])).is_none());
    }

    #[test]
    fn bonded_peer_inside_the_group_or_already_gone_allows_the_move() {
        // The whole bonded pair moves together.
        let together = [
            player("mover", vec![("peer", 1.0)]),
            player("peer", vec![("mover", 1.0)]),
        ];
        assert!(bond_conflict(&together, &group(&["mover", "peer"])).is_none());

        // The bonded peer disconnected before the commit: solo move passes.
        let solo = [player("mover", vec![("gone", 1.0)])];
        assert!(bond_conflict(&solo, &group(&["mover"])).is_none());
    }

    #[test]
    fn bonds_of_players_outside_the_group_do_not_block_other_moves() {
        let roster = [
            player("mover", vec![]),
            player("stayer", vec![("gone", 1.0)]),
        ];
        assert!(bond_conflict(&roster, &group(&["mover"])).is_none());
    }
}
