//! Outgoing QWD events and server-event bridging.

use std::collections::{HashMap, HashSet};
use std::sync::mpsc::TryRecvError;
use std::time::{Duration, Instant};

use serde_json::{Map, Value, json};

use crate::control::{PlayerInfo, ServerEvent};

use super::protocol::event;
use super::util::now_ms;
use super::{PendingQwdMove, QwdClient, Socket};

/// A full telemetry snapshot is re-sent at least this often even when nothing
/// changed. It re-syncs a manager whose delta base drifted and gives the
/// planner an idle liveness signal.
const TELEMETRY_FULL_RESYNC_FRAMES: u64 = 30;
const TELEMETRY_FULL_RESYNC_MS: u64 = 30_000;

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
                "version": self.telemetry_seq,
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
        let value = event(
            "player.join",
            json!({
                "roomId": self.room_id,
                "player": {
                    "playerId": player.uuid,
                    "level": player.level_id
                }
            }),
        );
        self.send_json(socket, &value)
    }

    pub(super) fn send_telemetry(&mut self, socket: &mut Socket) -> Result<(), String> {
        let status = self.status()?;
        let capacity = self.config.capacity;
        let load = if capacity == 0 {
            0.0
        } else {
            (status.player_list.len() as f64 / capacity as f64).clamp(0.0, 1.0)
        };

        let current: HashMap<String, Value> = status
            .player_list
            .iter()
            .take(8)
            .map(|player| (player.uuid.clone(), player_telemetry(player)))
            .collect();

        // Only advance the sequence when a frame is actually published. The
        // manager derives the room version (and therefore `expectedVersion` in
        // move.prepare) from the last seq it received; advancing through idle
        // ticks makes every later move look stale.
        let seq = self.telemetry_seq.wrapping_add(1);
        let now = Instant::now();

        let can_delta = self
            .telemetry_roster
            .as_ref()
            .is_some_and(|roster| !roster.is_empty())
            && seq.wrapping_sub(self.last_full_telemetry_seq) < TELEMETRY_FULL_RESYNC_FRAMES
            && now.duration_since(self.last_full_telemetry_at)
                < Duration::from_millis(TELEMETRY_FULL_RESYNC_MS);

        let mut data = Map::new();
        data.insert("roomId".to_string(), json!(self.room_id));
        data.insert("seq".to_string(), json!(seq));
        data.insert("capacity".to_string(), json!(capacity));
        data.insert("load".to_string(), json!(load));
        data.insert("timestamp".to_string(), json!(now_ms()));

        if can_delta {
            let previous = self
                .telemetry_roster
                .as_ref()
                .expect("roster checked above");
            let mut changed = Vec::new();
            for (player_id, value) in &current {
                let changed_now = previous
                    .get(player_id)
                    .is_none_or(|old| !same_telemetry(old, value));
                if changed_now {
                    changed.push(value.clone());
                }
            }
            let mut removed = Vec::new();
            for player_id in previous.keys() {
                if !current.contains_key(player_id) {
                    removed.push(player_id.clone());
                }
            }

            if changed.is_empty() && removed.is_empty() {
                // Nothing observable changed, so no frame is published and the
                // sequence must not advance: the manager's cached room version
                // has to stay equal to the last seq it actually received.
                self.telemetry_roster = Some(current);
                return Ok(());
            }

            data.insert("players".to_string(), Value::Array(changed));
            data.insert("kind".to_string(), json!("delta"));
            data.insert("baseSeq".to_string(), json!(self.last_telemetry_seq_sent));
            data.insert("removedPlayers".to_string(), json!(removed));
        } else {
            let players: Vec<Value> = current.values().cloned().collect();
            data.insert("players".to_string(), Value::Array(players));
            self.last_full_telemetry_seq = seq;
            self.last_full_telemetry_at = now;
        }

        self.telemetry_seq = seq;
        self.telemetry_roster = Some(current);
        self.last_telemetry_seq_sent = seq;

        let value = event("room.telemetry", Value::Object(data));
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

/// Compare only the fields XYSKY uses to decide whether a player changed.
/// Timestamps move on every packet and must not defeat delta compression.
fn same_telemetry(a: &Value, b: &Value) -> bool {
    a.get("levelId") == b.get("levelId")
        && a.get("position") == b.get("position")
        && a.get("velocity") == b.get("velocity")
        && a.get("affinity") == b.get("affinity")
}

/// Shape one player's QWD telemetry entry. Optional fields mirror XYSKY, which
/// omits them when the corresponding data is unavailable.
fn player_telemetry(player: &PlayerInfo) -> Value {
    let mut value = json!({
        "playerId": player.uuid,
        "levelId": player.level_id,
    });
    if player.state_at > 0 {
        value["stateAt"] = json!(player.state_at);
    }
    if player.last_move_at > 0 {
        value["lastMoveAt"] = json!(player.last_move_at);
    }
    if let Some(position) = player.position {
        value["position"] = json!(position);
    }
    if let Some(velocity) = player.velocity {
        value["velocity"] = json!(velocity);
    }
    let mut affinity = Map::new();
    for (player_id, weight) in &player.affinity {
        affinity.insert(player_id.clone(), json!(weight));
    }
    value["affinity"] = Value::Object(affinity);
    value
}

#[cfg(test)]
mod tests {
    use super::{player_telemetry, same_telemetry};
    use crate::control::PlayerInfo;
    use serde_json::json;

    fn player(level_id: u32) -> PlayerInfo {
        PlayerInfo {
            player_id: 1,
            uuid: "player-1".to_string(),
            level_id,
            position: Some([1.0, 2.0, 3.0]),
            velocity: Some(4.0),
            state_at: 100,
            last_move_at: 200,
            affinity: vec![("player-2".to_string(), 0.5)],
        }
    }

    #[test]
    fn telemetry_comparison_ignores_timestamps() {
        let mut newer = player(7);
        newer.state_at = 300;
        newer.last_move_at = 400;

        let base = player_telemetry(&player(7));
        assert!(same_telemetry(&base, &player_telemetry(&newer)));
        assert!(!same_telemetry(&base, &player_telemetry(&player(8))));
    }

    #[test]
    fn telemetry_entry_matches_qwd_shape() {
        let value = player_telemetry(&player(7));
        assert_eq!(value["levelId"], json!(7));
        assert_eq!(value["position"], json!([1.0, 2.0, 3.0]));
        assert_eq!(value["velocity"], json!(4.0));
        assert_eq!(value["affinity"]["player-2"], json!(0.5));
    }
}
