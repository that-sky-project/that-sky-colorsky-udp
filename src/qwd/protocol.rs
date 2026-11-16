//! Wire shapes shared by QWD node messages.

use serde::Deserialize;
use serde_json::{Value, json};

pub const PROTOCOL_VERSION: u32 = 1;
pub const DEFAULT_TTL_MS: u64 = 10_000;
/// XYSKY nodes accept leases up to two minutes; the manager renews a hold it
/// still needs instead of letting it lapse, so never expire earlier than that.
pub const MAX_TTL_MS: u64 = 120_000;

pub const OK: i64 = 0;
pub const BAD_REQUEST: i64 = 4000;
pub const NOT_FOUND: i64 = 4004;
pub const CONFLICT: i64 = 4009;
pub const NO_CAPACITY: i64 = 4029;
pub const STALE_PLAN: i64 = 4090;
pub const NODE_UNAVAILABLE: i64 = 5001;
pub const INTERNAL: i64 = 5999;

/// Command sent by QWD to a node.
#[derive(Debug, Clone, Deserialize)]
pub struct ManagerCommand {
    #[serde(default = "default_version")]
    pub v: u32,
    pub id: i64,
    pub cmd: String,
    #[serde(default)]
    pub data: Value,
}

impl ManagerCommand {
    pub fn string_field(&self, key: &str) -> Option<&str> {
        self.data.get(key).and_then(Value::as_str)
    }

    pub fn u64_field(&self, key: &str) -> Option<u64> {
        self.data.get(key).and_then(Value::as_u64)
    }

    pub fn room_id(&self) -> Option<&str> {
        self.string_field("roomId")
            .or_else(|| self.string_field("toRoomId"))
    }

    pub fn move_id(&self) -> Option<&str> {
        self.string_field("moveId")
    }

    /// Parse the singular/plural player id forms without silently repairing the
    /// manager's request. Duplicate, empty, non-string, and mixed forms are
    /// protocol errors because silently dropping one participant would weaken
    /// the move's all-or-one guard.
    pub fn player_ids(&self) -> Result<Vec<String>, String> {
        let mut ids = Vec::new();
        let mut saw_field = false;

        if let Some(value) = self.data.get("playerIds") {
            saw_field = true;
            let Some(values) = value.as_array() else {
                return Err("playerIds must be an array of non-empty strings".to_string());
            };
            for value in values {
                let Some(id) = value.as_str() else {
                    return Err("playerIds must contain only strings".to_string());
                };
                if id.is_empty() {
                    return Err("playerIds must not contain empty ids".to_string());
                }
                if ids.iter().any(|existing| existing == id) {
                    return Err("playerIds must not contain duplicates".to_string());
                }
                ids.push(id.to_string());
            }
        }

        if let Some(value) = self.data.get("playerId") {
            saw_field = true;
            let Some(id) = value.as_str() else {
                return Err("playerId must be a non-empty string".to_string());
            };
            if id.is_empty() {
                return Err("playerId must be a non-empty string".to_string());
            }
            if ids.iter().any(|existing| existing == id) {
                return Err("playerIds must not contain duplicates".to_string());
            }
            ids.push(id.to_string());
        }

        if !saw_field {
            return Ok(Vec::new());
        }
        Ok(ids)
    }

    /// Validate a manager supplied TTL. Missing TTL keeps the wire default;
    /// present values must be integer milliseconds in the supported range.
    pub fn ttl(&self) -> Result<std::time::Duration, String> {
        let Some(value) = self.data.get("ttlMs") else {
            return Ok(std::time::Duration::from_millis(DEFAULT_TTL_MS));
        };
        let Some(ttl) = value.as_u64() else {
            return Err("ttlMs must be an integer number of milliseconds".to_string());
        };
        if !(1_000..=MAX_TTL_MS).contains(&ttl) {
            return Err(format!("ttlMs must be between 1000 and {MAX_TTL_MS}"));
        }
        Ok(std::time::Duration::from_millis(ttl))
    }
}

/// Outgoing node event envelope.
pub fn event(name: &str, data: Value) -> Value {
    json!({
        "v": PROTOCOL_VERSION,
        "event": name,
        "data": data
    })
}

/// Outgoing command acknowledgement.
pub fn ack(id: i64, code: i64, message: Option<&str>, data: Option<Value>) -> Value {
    let mut value = json!({
        "v": PROTOCOL_VERSION,
        "id": id,
        "code": code
    });

    if let Some(message) = message {
        value["message"] = Value::String(message.to_string());
    }
    if let Some(data) = data {
        value["data"] = data;
    }
    value
}

fn default_version() -> u32 {
    PROTOCOL_VERSION
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_plural_and_singular_player_ids() {
        let plural: ManagerCommand = serde_json::from_str(
            r#"{"v":1,"id":7,"cmd":"move.commit","data":{"playerIds":["a","b","a"]}}"#,
        )
        .unwrap();
        assert_eq!(
            plural.player_ids().unwrap_err(),
            "playerIds must not contain duplicates"
        );

        let singular: ManagerCommand = serde_json::from_str(
            r#"{"v":1,"id":8,"cmd":"player.redirect","data":{"playerId":"a"}}"#,
        )
        .unwrap();
        assert_eq!(singular.player_ids().unwrap(), vec!["a"]);
    }

    #[test]
    fn ack_omits_absent_optional_fields() {
        let value = ack(1, OK, None, None);
        assert_eq!(value["id"], 1);
        assert_eq!(value["code"], 0);
        assert!(value.get("message").is_none());
        assert!(value.get("data").is_none());
    }

    #[test]
    fn ttl_is_clamped() {
        let command: ManagerCommand =
            serde_json::from_str(r#"{"id":1,"cmd":"room.reserve","data":{"ttlMs":1}}"#).unwrap();
        assert_eq!(command.ttl().unwrap_err(), "ttlMs must be between 1000 and 120000");
    }
}
