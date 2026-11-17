//! Outgoing telemetry and room lifecycle events.

use std::collections::HashMap;
use std::time::{Duration, Instant};

use serde_json::{Map, Value, json};

use crate::control::{PlayerInfo, ServerStatus};

use super::facts::player_status;
use super::protocol::event;
use super::reservations::CapacityFacts;
use super::util::now_ms;
use super::{QwdClient, Socket};

/// A full telemetry snapshot is re-sent at least this often even when nothing
/// changed. Besides re-syncing a manager whose delta base drifted, this bounds
/// how long a dropped delta can leave the manager's room version frozen (a
/// frozen version makes every later move.prepare look like a STALE_PLAN).
const TELEMETRY_FULL_RESYNC_FRAMES: u64 = 30;
const TELEMETRY_FULL_RESYNC_MS: u64 = 10_000;
/// An unchanged player is re-sent at least this often. Without it a room whose
/// players stand still publishes nothing between full resyncs: the manager ages
/// the node out of its healthy set and freezes `stateAt`, while an empty room
/// (which always sends a full frame) stays visible.
const TELEMETRY_PLAYER_KEEPALIVE_MS: u64 = 2_000;

impl QwdClient {
    /// Occupancy facts shared by `node.ready`, `room.created` and telemetry.
    /// Returns `None` when the control plane is unreachable; callers fall back
    /// to publishing without facts rather than dropping the frame.
    fn occupancy(&self) -> Option<CapacityFacts> {
        let now = Instant::now();
        let status = self.status().ok()?;
        Some(
            self.reservations
                .facts(&status, self.config.capacity, self.telemetry_seq, now),
        )
    }

    pub(super) fn send_node_ready(&mut self, socket: &mut Socket) -> Result<(), String> {
        let players = self
            .status()
            .map(|status| status.player_list.len())
            .unwrap_or(0);
        let load = if self.config.capacity == 0 {
            0.0
        } else {
            (players as f64 / self.config.capacity as f64).clamp(0.0, 1.0)
        };
        let mut data = json!({
            "capacity": self.config.capacity,
            "load": load,
            "version": self.telemetry_seq,
            "timestamp": now_ms()
        });
        if self.caps.occupancy_facts
            && let Some(facts) = self.occupancy()
        {
            data["physicalPlayers"] = json!(facts.physical_players);
            data["activePlayers"] = json!(facts.active_players);
            data["reservedSlots"] = json!(facts.reserved_slots);
        }
        self.send_json(socket, &event("node.ready", data))
    }

    pub(super) fn send_room_created(&mut self, socket: &mut Socket) -> Result<(), String> {
        let players = self
            .status()
            .map(|status| status.player_list.len())
            .unwrap_or(0);
        let mut data = json!({
            "roomId": self.room_id,
            "udpHost": self.config.public_host,
            "port": self.config.public_port,
            "capacity": self.config.capacity,
            "players": players.min(8),
            "version": self.telemetry_seq,
            "timestamp": now_ms()
        });
        if self.caps.occupancy_facts
            && let Some(facts) = self.occupancy()
        {
            data["physicalPlayers"] = json!(facts.physical_players);
            data["activePlayers"] = json!(facts.active_players);
            data["reservedSlots"] = json!(facts.reserved_slots);
        }
        self.send_json(socket, &event("room.created", data))
    }

    pub(super) fn send_telemetry(&mut self, socket: &mut Socket) -> Result<(), String> {
        let status = self.status()?;
        self.publish_telemetry(&status, |client, value| client.send_json(socket, value))
    }

    /// Build against the committed baseline, send, then atomically advance all
    /// wire-visible bookkeeping. A failed send leaves the next attempt intact.
    pub(super) fn publish_telemetry(
        &mut self,
        status: &ServerStatus,
        send: impl FnOnce(&Self, &Value) -> Result<(), String>,
    ) -> Result<(), String> {
        let capacity = self.config.capacity;
        let load = if capacity == 0 {
            0.0
        } else {
            (status.player_list.len() as f64 / capacity as f64).clamp(0.0, 1.0)
        };
        let facts = self.reservations.facts(
            status,
            capacity,
            self.telemetry_seq.wrapping_add(1),
            Instant::now(),
        );

        let current: HashMap<String, Value> = status
            .player_list
            .iter()
            .filter(|player| !player.moving)
            .take(8)
            .map(|player| {
                (
                    player.uuid.clone(),
                    player_telemetry(player, self.caps.player_status_fields),
                )
            })
            .collect();

        // Only advance the sequence when a frame is actually published. The
        // manager derives the room version (and therefore `expectedVersion` in
        // move.prepare) from the last seq it received; advancing through idle
        // ticks makes every later move look stale.
        let seq = self.telemetry_seq.wrapping_add(1);
        let now = Instant::now();

        let can_delta = !self.force_full
            && self.caps.delta_telemetry
            && self
                .telemetry_roster
                .as_ref()
                .is_some_and(|roster| !roster.is_empty())
            && seq.wrapping_sub(self.last_full_telemetry_seq) < TELEMETRY_FULL_RESYNC_FRAMES
            && now.duration_since(self.last_full_telemetry_at)
                < Duration::from_millis(TELEMETRY_FULL_RESYNC_MS);
        let mut next_sent_at = self.last_telemetry_sent_at.clone();

        let mut data = Map::new();
        data.insert("roomId".to_string(), json!(self.room_id));
        data.insert("seq".to_string(), json!(seq));
        data.insert("capacity".to_string(), json!(capacity));
        data.insert("load".to_string(), json!(load));
        data.insert("timestamp".to_string(), json!(now_ms()));
        if self.caps.occupancy_facts {
            data.insert("physicalPlayers".to_string(), json!(facts.physical_players));
            data.insert("activePlayers".to_string(), json!(facts.active_players));
            data.insert("reservedSlots".to_string(), json!(facts.reserved_slots));
            data.insert("version".to_string(), json!(facts.version));
        }

        if can_delta {
            let previous = self
                .telemetry_roster
                .as_ref()
                .expect("roster checked above");
            let sent = &self.last_telemetry_sent_at;
            let mut changed = Vec::new();
            for (player_id, value) in &current {
                let changed_now = previous
                    .get(player_id)
                    .is_none_or(|old| !same_telemetry(old, value));
                // Keepalive: an idle player is re-sent even though nothing
                // observable changed, so the manager keeps a fresh `stateAt`
                // and does not age this node out of its healthy set.
                let stale = sent.get(player_id).is_none_or(|at| {
                    now.duration_since(*at) >= Duration::from_millis(TELEMETRY_PLAYER_KEEPALIVE_MS)
                });
                if changed_now || stale {
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
                // Nothing observable changed and every player was on the wire
                // recently, so no frame is published and the sequence must not
                // advance: the manager's cached room version has to stay equal
                // to the last seq it actually received.
                return Ok(());
            }

            data.insert("players".to_string(), Value::Array(changed.clone()));
            data.insert("kind".to_string(), json!("delta"));
            data.insert("baseSeq".to_string(), json!(self.last_telemetry_seq_sent));
            data.insert("removedPlayers".to_string(), json!(removed));

            for player in &changed {
                if let Some(player_id) = player.get("playerId").and_then(Value::as_str) {
                    next_sent_at.insert(player_id.to_string(), now);
                }
            }
            for player_id in &removed {
                next_sent_at.remove(player_id);
            }
        } else {
            let players: Vec<Value> = current.values().cloned().collect();
            data.insert("players".to_string(), Value::Array(players));
            next_sent_at = current
                .keys()
                .map(|player_id| (player_id.clone(), now))
                .collect();
        }

        let value = event("room.telemetry", Value::Object(data));
        send(self, &value)?;

        self.telemetry_seq = seq;
        self.telemetry_roster = Some(current);
        self.last_telemetry_seq_sent = seq;
        self.last_telemetry_sent_at = next_sent_at;
        if !can_delta {
            self.last_full_telemetry_seq = seq;
            self.last_full_telemetry_at = now;
        }
        self.force_full = false;
        Ok(())
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
}

/// Compare only the fields XYSKY uses to decide whether a player changed.
/// Timestamps move on every packet and must not defeat delta compression; the
/// move status and host flag are compared so a transition to/from MOVING and a
/// host handover are always published.
fn same_telemetry(a: &Value, b: &Value) -> bool {
    a.get("sessionId") == b.get("sessionId")
        && a.get("levelId") == b.get("levelId")
        && a.get("position") == b.get("position")
        && a.get("velocity") == b.get("velocity")
        && a.get("affinity") == b.get("affinity")
        && a.get("status") == b.get("status")
        && a.get("lvSeq") == b.get("lvSeq")
        && a.get("isHost") == b.get("isHost")
        && a.get("levelHash") == b.get("levelHash")
}

/// Shape one player's QWD telemetry entry. Optional fields mirror XYSKY, which
/// omits them when the corresponding data is unavailable.
fn player_telemetry(player: &PlayerInfo, _status_fields: bool) -> Value {
    let mut value = json!({
        "playerId": player.uuid,
        "levelId": player.level_id,
    });
    // Session identity, level generation and the host flag are mandatory
    // safety metadata on every entry, even when an older manager rejects
    // optional player status fields.
    value["sessionId"] = json!(player.session_id);
    value["lvSeq"] = json!(player.lv_seq);
    value["isHost"] = json!(player.is_host);
    if let Some(level_hash) = player.level_hash {
        value["levelHash"] = json!(level_hash);
    }
    value["status"] = json!(player_status(player.moving));
    if player.state_at > 0 {
        value["stateAt"] = json!(player.state_at);
    }
    if player.last_move_at > 0 {
        value["lastMoveAt"] = json!(player.last_move_at);
    }
    if let Some(position) = player.position {
        // serde_json encodes NaN/Infinity as null, which QWD's strict tuple
        // schema rejects. Keep the field absent instead of sending a frame the
        // manager will drop.
        if position.iter().all(|coordinate| coordinate.is_finite()) {
            value["position"] = json!(position);
        }
    }
    if let Some(velocity) = player.velocity
        && velocity.is_finite()
    {
        value["velocity"] = json!(velocity);
    }
    let mut affinity = Map::new();
    for (player_id, weight) in &player.affinity {
        if weight.is_finite() {
            affinity.insert(player_id.clone(), json!(weight));
        }
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
            lv_seq: 0,
            moving: false,
            session_id: "session-1".to_string(),
            is_host: false,
            level_hash: None,
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

        let base = player_telemetry(&player(7), true);
        assert!(same_telemetry(&base, &player_telemetry(&newer, true)));
        assert!(!same_telemetry(&base, &player_telemetry(&player(8), true)));
    }

    #[test]
    fn telemetry_entry_matches_qwd_shape() {
        let value = player_telemetry(&player(7), true);
        assert_eq!(value["levelId"], json!(7));
        assert_eq!(value["position"], json!([1.0, 2.0, 3.0]));
        assert_eq!(value["velocity"], json!(4.0));
        assert_eq!(value["affinity"]["player-2"], json!(0.5));
    }

    #[test]
    fn telemetry_entry_always_carries_session_id_and_host_flag() {
        // sessionId, lvSeq and isHost are mandatory on every entry, even with
        // the optional status fields disabled for an older manager.
        for status_fields in [true, false] {
            let value = player_telemetry(&player(7), status_fields);
            assert_eq!(value["sessionId"], json!("session-1"));
            assert_eq!(value["lvSeq"], json!(0));
            assert_eq!(value["isHost"], json!(false));
            assert_eq!(value["status"], json!("ACTIVE"));
        }
    }

    #[test]
    fn host_flag_change_is_a_telemetry_change() {
        let base = player_telemetry(&player(7), true);
        let mut promoted = player(7);
        promoted.is_host = true;
        assert!(!same_telemetry(&base, &player_telemetry(&promoted, true)));
    }
}
