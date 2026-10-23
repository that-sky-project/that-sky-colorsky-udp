//! Outgoing QWD events and server-event bridging.

use std::collections::HashSet;
use std::sync::mpsc::TryRecvError;

use serde_json::{Value, json};

use crate::control::{PlayerInfo, ServerEvent};

use super::protocol::event;
use super::util::now_ms;
use super::{PendingQwdMove, QwdClient, Socket};

impl QwdClient {
    pub(super) fn send_node_ready(&self, socket: &mut Socket) -> Result<(), String> {
        let players = self
            .status()
            .map(|status| status.player_list.len())
            .unwrap_or(0);
        let load = if self.config.capacity == 0 {
            0.0
        } else {
            players as f64 / self.config.capacity as f64
        };

        let value = event(
            "node.ready",
            json!({
                "capacity": self.config.capacity,
                "load": load.clamp(0.0, 1.0),
                "timestamp": now_ms()
            }),
        );
        self.send_json(socket, &value)
    }

    pub(super) fn send_room_created(&mut self, socket: &mut Socket) -> Result<(), String> {
        let players = self
            .status()
            .map(|status| status.player_list.len())
            .unwrap_or(0);

        let value = event(
            "room.created",
            json!({
                "roomId": self.room_id,
                "udpHost": self.config.public_host,
                "port": self.config.public_port,
                "capacity": self.config.capacity,
                "players": players.min(8),
                "version": self.room_version,
                "timestamp": now_ms()
            }),
        );
        self.send_json(socket, &value)
    }

    pub(super) fn replay_players(&mut self, socket: &mut Socket) -> Result<(), String> {
        let status = self.status()?;
        for player in status.player_list {
            self.send_player_join(socket, &player)?;
        }
        Ok(())
    }

    fn send_player_join(&mut self, socket: &mut Socket, player: &PlayerInfo) -> Result<(), String> {
        self.room_version = self.room_version.wrapping_add(1);
        let value = event(
            "player.join",
            json!({
                "roomId": self.room_id,
                "player": {
                    "playerId": player.uuid,
                    "level": player.level_id
                },
                "version": self.room_version
            }),
        );
        self.send_json(socket, &value)
    }

    pub(super) fn send_telemetry(&mut self, socket: &mut Socket) -> Result<(), String> {
        let status = self.status()?;
        let players: Vec<Value> = status
            .player_list
            .iter()
            .take(8)
            .map(|player| {
                json!({
                    "playerId": player.uuid,
                    "levelId": player.level_id,
                    "stateAt": now_ms()
                })
            })
            .collect();

        self.telemetry_seq = self.telemetry_seq.wrapping_add(1);
        let value = event(
            "room.telemetry",
            json!({
                "roomId": self.room_id,
                "seq": self.telemetry_seq,
                "players": players,
                "timestamp": now_ms()
            }),
        );
        self.send_json(socket, &value)
    }

    pub(super) fn send_node_draining(&self, socket: &mut Socket) -> Result<(), String> {
        let value = event(
            "node.draining",
            json!({
                "capacity": self.config.capacity,
                "load": 1.0,
                "timestamp": now_ms()
            }),
        );
        self.send_json(socket, &value)
    }

    /// Tell the manager that every player of a move group has landed. This is
    /// stronger than inferring completion from join/leave pairs, so the move
    /// transaction can settle even if a leave event was lost.
    pub(super) fn send_move_completed(
        &self,
        socket: &mut Socket,
        move_id: &str,
        player_ids: &[String],
    ) -> Result<(), String> {
        let value = event(
            "move.completed",
            json!({
                "roomId": self.room_id,
                "moveId": move_id,
                "playerIds": player_ids,
                "timestamp": now_ms()
            }),
        );
        self.send_json(socket, &value)
    }

    /// Tell the manager that a target reservation expired before its group
    /// arrived, so it can fail the transaction immediately.
    pub(super) fn send_reservation_expired(
        &self,
        socket: &mut Socket,
        move_id: &str,
        player_ids: &[String],
    ) -> Result<(), String> {
        let value = event(
            "reservation.expired",
            json!({
                "roomId": self.room_id,
                "moveId": move_id,
                "playerIds": player_ids,
                "timestamp": now_ms()
            }),
        );
        self.send_json(socket, &value)
    }

    pub(super) fn send_move_result(
        &self,
        socket: &mut Socket,
        move_id: &str,
        accepted: bool,
        moved: &[String],
        failed: &[String],
        reason: Option<&str>,
    ) -> Result<(), String> {
        let mut data = json!({
            "roomId": self.room_id,
            "moveId": move_id,
            "accepted": accepted,
            "movedPlayerIds": moved,
            "failedPlayerIds": failed,
            "timestamp": now_ms()
        });
        if let Some(reason) = reason {
            data["reason"] = Value::String(reason.to_string());
        }
        self.send_json(socket, &event("move.result", data))
    }

    pub(super) fn drain_events(&mut self, socket: &mut Socket) -> Result<(), String> {
        loop {
            match self.events.try_recv() {
                Ok(server_event) => self.handle_server_event(socket, server_event)?,
                Err(TryRecvError::Empty | TryRecvError::Disconnected) => return Ok(()),
            }
        }
    }

    fn handle_server_event(
        &mut self,
        socket: &mut Socket,
        server_event: ServerEvent,
    ) -> Result<(), String> {
        match server_event {
            ServerEvent::PlayerJoined { uuid, level_id, .. } => {
                let player_id = format!("{uuid:?}");
                let completed = self.reservations.confirm_player(&player_id);
                let value = event(
                    "player.join",
                    json!({
                        "roomId": self.room_id,
                        "player": {
                            "playerId": player_id,
                            "level": level_id
                        }
                    }),
                );
                self.send_json(socket, &value)?;
                for (move_id, player_ids) in completed {
                    self.send_move_completed(socket, &move_id, &player_ids)?;
                }
                Ok(())
            }
            ServerEvent::PlayerLeft { uuid, .. } => {
                let player_id = format!("{uuid:?}");
                let _ = self.reservations.confirm_player(&player_id);
                let value = event(
                    "player.leave",
                    json!({
                        "roomId": self.room_id,
                        "player": {
                            "playerId": player_id
                        }
                    }),
                );
                self.send_json(socket, &value)
            }
            ServerEvent::MoveFinished {
                move_id,
                player_uuid,
                accepted,
                result,
                ..
            } => self.finish_qwd_move(
                socket,
                move_id,
                &format!("{player_uuid:?}"),
                accepted,
                Some(format!("client_move_result_{result}")),
            ),
            ServerEvent::MoveTimedOut {
                move_id,
                player_uuid,
                ..
            } => self.finish_qwd_move(
                socket,
                move_id,
                &format!("{player_uuid:?}"),
                false,
                Some("move_timeout".to_string()),
            ),
            ServerEvent::MoveStarted { .. } | ServerEvent::AuthorityElected { .. } => Ok(()),
        }
    }

    fn finish_qwd_move(
        &mut self,
        socket: &mut Socket,
        internal_move_id: u64,
        player_id: &str,
        accepted: bool,
        reason: Option<String>,
    ) -> Result<(), String> {
        let Some(qwd_move_id) = self.internal_to_qwd.remove(&internal_move_id) else {
            return Ok(());
        };
        let Some(state) = self.qwd_moves.get_mut(&qwd_move_id) else {
            return Ok(());
        };

        state.pending.remove(&internal_move_id);
        if accepted {
            if !state.moved.iter().any(|id| id == player_id) {
                state.moved.push(player_id.to_string());
            }
        } else {
            if !state.failed.iter().any(|id| id == player_id) {
                state.failed.push(player_id.to_string());
            }
            if state.reason.is_none() {
                state.reason = reason;
            }
        }

        if !state.pending.is_empty() {
            return Ok(());
        }

        let state = self
            .qwd_moves
            .remove(&qwd_move_id)
            .unwrap_or(PendingQwdMove {
                pending: HashSet::new(),
                moved: Vec::new(),
                failed: Vec::new(),
                reason: None,
            });

        self.send_move_result(
            socket,
            &qwd_move_id,
            state.failed.is_empty(),
            &state.moved,
            &state.failed,
            state.reason.as_deref(),
        )
    }
}
