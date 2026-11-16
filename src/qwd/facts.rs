//! Node self-reported facts and manager-supplied guards.
//!
//! The manager treats a node's own numbers as authoritative for capacity
//! accounting: every command ack (and `room.sync`) may carry a capacity facts
//! object which is validated and applied to the manager's cached room snapshot.
//! Move commands may carry guards that let the node reject a plan built from
//! stale state (`expectedParticipants`, `reservationToken`).

use serde_json::{Map, Value, json};

use crate::control::PlayerInfo;

use super::reservations::CapacityFacts;

impl CapacityFacts {
    /// Build facts from a status snapshot. `version` is the telemetry sequence;
    /// the manager ignores facts older than its cached snapshot, so it must
    /// keep increasing.
    pub fn to_json(self) -> Value {
        json!({
            "physicalPlayers": self.physical_players,
            "activePlayers": self.active_players,
            "reservedSlots": self.reserved_slots,
            "capacity": self.capacity,
            "version": self.version,
        })
    }
}

/// Append capacity facts to an ack payload without clobbering existing keys.
pub fn with_facts(data: Option<Value>, facts: CapacityFacts) -> Option<Value> {
    let mut value = data.unwrap_or_else(|| Value::Object(Map::new()));
    if !value.is_object() {
        value = Value::Object(Map::new());
    }
    let object = value.as_object_mut().expect("just normalized");
    if let Value::Object(fields) = facts.to_json() {
        for (key, field) in fields {
            object.entry(key).or_insert(field);
        }
    }
    Some(value)
}

/// One entry of the manager's `expectedParticipants` guard on `move.commit`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ExpectedParticipant {
    pub player_id: String,
    pub session_id: Option<String>,
    pub level_id: Option<u32>,
    pub lv_seq: Option<u8>,
}

/// Parse the manager's optional participant guard. Absent or malformed guards
/// disable the check instead of failing the move: the manager also validates
/// against its own telemetry, so a malformed guard must not dead-lock a group.
pub fn expected_participants(value: &Value) -> Vec<ExpectedParticipant> {
    let Some(entries) = value.as_array() else {
        return Vec::new();
    };
    entries
        .iter()
        .filter_map(|entry| {
            let player_id = entry.get("playerId")?.as_str()?.to_string();
            Some(ExpectedParticipant {
                player_id,
                session_id: entry
                    .get("sessionId")
                    .and_then(Value::as_str)
                    .map(str::to_string),
                level_id: entry
                    .get("levelId")
                    .and_then(Value::as_u64)
                    .map(|value| value as u32),
                lv_seq: entry
                    .get("lvSeq")
                    .and_then(Value::as_u64)
                    .map(|value| value as u8),
            })
        })
        .collect()
}

/// Whether a player still matches the manager's expectation. `sessionId` is
/// only enforced when both sides know it (colorsky does not report one).
pub fn participant_matches(expected: &ExpectedParticipant, player: &PlayerInfo) -> bool {
    if let (Some(expected_session), Some(actual)) = (
        expected.session_id.as_deref(),
        player.session_id.map(|session| session.to_string()),
    ) && expected_session != actual
    {
        return false;
    }
    if let Some(level_id) = expected.level_id
        && level_id != player.level_id
    {
        return false;
    }
    if let (Some(lv_seq), Some(actual)) = (expected.lv_seq, Some(player.lv_seq))
        && lv_seq != actual
    {
        return false;
    }
    true
}

/// Telemetry/status string for a player connection state.
pub fn player_status(moving: bool) -> &'static str {
    if moving { "MOVING" } else { "ACTIVE" }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn facts_json_has_all_fields() {
        let value = CapacityFacts {
            physical_players: 2,
            active_players: 2,
            reserved_slots: 1,
            capacity: 8,
            version: 42,
        }
        .to_json();
        assert_eq!(value["physicalPlayers"], 2);
        assert_eq!(value["reservedSlots"], 1);
        assert_eq!(value["version"], 42);
    }

    #[test]
    fn with_facts_preserves_existing_keys() {
        let facts = CapacityFacts {
            physical_players: 1,
            active_players: 1,
            reserved_slots: 0,
            capacity: 8,
            version: 7,
        };
        let value = with_facts(Some(json!({ "accepted": true, "capacity": 8 })), facts).unwrap();
        assert_eq!(value["accepted"], true);
        assert_eq!(value["capacity"], 8);
        assert_eq!(value["version"], 7);
        assert_eq!(value["physicalPlayers"], 1);
    }

    #[test]
    fn with_facts_handles_non_object_payload() {
        let facts = CapacityFacts {
            physical_players: 0,
            active_players: 0,
            reserved_slots: 0,
            capacity: 8,
            version: 1,
        };
        let value = with_facts(Some(json!("ok")), facts).unwrap();
        assert_eq!(value["version"], 1);
    }

    #[test]
    fn expected_participants_parses_optional_fields() {
        let guard = expected_participants(&json!([
            { "playerId": "p1", "levelId": 7, "lvSeq": 3, "sessionId": "s1" },
            { "playerId": "p2" },
            { "levelId": 9 }
        ]));
        assert_eq!(guard.len(), 2);
        assert_eq!(guard[0].level_id, Some(7));
        assert_eq!(guard[0].lv_seq, Some(3));
        assert_eq!(guard[0].session_id.as_deref(), Some("s1"));
        assert_eq!(guard[1].level_id, None);
    }

    #[test]
    fn participant_guard_rejects_session_change() {
        let mut expected = ExpectedParticipant {
            player_id: "p1".to_string(),
            session_id: Some("1000".to_string()),
            level_id: Some(7),
            lv_seq: Some(3),
        };
        let mut player = crate::control::PlayerInfo {
            player_id: 1,
            uuid: "p1".to_string(),
            level_id: 7,
            lv_seq: 3,
            moving: false,
            session_id: Some(1000),
            position: None,
            velocity: None,
            state_at: 0,
            last_move_at: 0,
            affinity: Vec::new(),
        };
        assert!(participant_matches(&expected, &player));
        // The client reconnected (new connection token): the plan is stale.
        player.session_id = Some(2000);
        assert!(!participant_matches(&expected, &player));
        // Without a session on either side the check is skipped.
        expected.session_id = None;
        player.session_id = None;
        assert!(participant_matches(&expected, &player));
    }

    #[test]
    fn expected_participants_ignores_malformed_guard() {
        assert!(expected_participants(&json!("x")).is_empty());
        assert!(expected_participants(&json!(null)).is_empty());
    }
}
