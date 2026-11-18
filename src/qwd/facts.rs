//! Validated node occupancy facts and complete manager-supplied source guards.
use super::reservations::CapacityFacts;
use crate::control::PlayerInfo;
use serde_json::{Map, Value, json};
use std::collections::HashSet;

impl CapacityFacts {
    pub fn to_json(self) -> Value {
        json!({"physicalPlayers":self.physical_players,"activePlayers":self.active_players,
            "reservedSlots":self.reserved_slots,"capacity":self.capacity,"version":self.version})
    }
}
pub fn with_facts(data: Option<Value>, facts: CapacityFacts) -> Option<Value> {
    let mut value = data
        .filter(Value::is_object)
        .unwrap_or_else(|| Value::Object(Map::new()));
    let object = value.as_object_mut().expect("normalized object");
    if let Value::Object(fields) = facts.to_json() {
        for (key, field) in fields {
            object.entry(key).or_insert(field);
        }
    }
    Some(value)
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ExpectedParticipant {
    pub player_id: String,
    pub session_id: String,
    pub level_id: u32,
    pub lv_seq: u8,
}

/// If guards are present, malformed or incomplete entries never disable checks.
pub fn expected_participants(value: &Value) -> Result<Vec<ExpectedParticipant>, String> {
    let entries = value
        .as_array()
        .ok_or("expectedParticipants must be an array")?;
    if entries.is_empty() || entries.len() > 7 {
        return Err("expectedParticipants must contain 1..=7 entries".into());
    }
    let mut seen = HashSet::new();
    entries
        .iter()
        .map(|entry| {
            let player_id = entry
                .get("playerId")
                .and_then(Value::as_str)
                .filter(|id| !id.is_empty() && id.len() <= 128)
                .ok_or("invalid participant playerId")?;
            if !seen.insert(player_id) {
                return Err("duplicate participant guard".into());
            }
            let session_id = entry
                .get("sessionId")
                .and_then(Value::as_str)
                .filter(|id| !id.is_empty() && id.len() <= 128)
                .ok_or("invalid participant sessionId")?;
            let level_id = entry
                .get("levelId")
                .and_then(Value::as_u64)
                .and_then(|id| u32::try_from(id).ok())
                .ok_or("invalid participant levelId")?;
            let lv_seq = entry
                .get("lvSeq")
                .and_then(Value::as_u64)
                .and_then(|seq| u8::try_from(seq).ok())
                .ok_or("invalid participant lvSeq")?;
            Ok(ExpectedParticipant {
                player_id: player_id.into(),
                session_id: session_id.into(),
                level_id,
                lv_seq,
            })
        })
        .collect()
}

pub fn participant_matches(expected: &ExpectedParticipant, player: &PlayerInfo) -> bool {
    expected.player_id == player.uuid
        && player.session_id == expected.session_id
        && expected.level_id == player.level_id
        && expected.lv_seq == player.lv_seq
        && !player.moving
}
pub fn player_status(moving: bool) -> &'static str {
    if moving { "MOVING" } else { "ACTIVE" }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn rejects_bad_or_incomplete_guard_instead_of_filtering_it() {
        for value in [
            json!(null),
            json!("bad"),
            json!([]),
            json!([{"playerId":"p"}]),
            json!([{"playerId":"p","sessionId":"s","levelId":1,"lvSeq":256}]),
            json!([{"playerId":"p","sessionId":7,"levelId":1,"lvSeq":1}]),
        ] {
            assert!(expected_participants(&value).is_err());
        }
        let good = json!({"playerId":"p","sessionId":"s","levelId":1,"lvSeq":255});
        assert!(expected_participants(&json!([good.clone(), good.clone()])).is_err());
        assert_eq!(
            expected_participants(&json!([good])).unwrap()[0].lv_seq,
            255
        );
    }
    #[test]
    fn guards_require_the_same_connection_even_with_reused_client_token() {
        let expected = expected_participants(
            &json!([{"playerId":"p","sessionId":"new","levelId":7,"lvSeq":1}]),
        )
        .unwrap()
        .remove(0);
        let mut player = PlayerInfo {
            player_id: 1,
            uuid: "p".into(),
            session_id: "new".into(),
            is_host: false,
            level_hash: None,
            level_id: 7,
            lv_seq: 1,
            moving: false,
            position: None,
            velocity: None,
            state_at: 0,
            last_move_at: 0,
            affinity: vec![],
        };
        assert!(participant_matches(&expected, &player));
        // A replacement session for the same client is a different connection
        // and must not satisfy the guard.
        player.session_id = "old".into();
        assert!(!participant_matches(&expected, &player));
        player.session_id = String::new();
        assert!(!participant_matches(&expected, &player));
    }
    #[test]
    fn facts_merge_without_losing_ack_identity() {
        let facts = CapacityFacts {
            physical_players: 2,
            active_players: 1,
            reserved_slots: 1,
            capacity: 8,
            version: 7,
        };
        let value = with_facts(Some(json!({"accepted":true,"expiresAt":123})), facts).unwrap();
        assert_eq!(value["expiresAt"], 123);
        assert_eq!(value["physicalPlayers"], 2);
        assert_eq!(value["activePlayers"], 1);
    }
}
