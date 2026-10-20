//! Wire shapes shared by QWD node messages.

use serde::Deserialize;
use serde_json::{Value, json};

pub const PROTOCOL_VERSION: u32 = 1;
pub const DEFAULT_TTL_MS: u64 = 10_000;

pub const OK: i64 = 0;
pub const BAD_REQUEST: i64 = 4000;
pub const NOT_FOUND: i64 = 4004;
pub const CONFLICT: i64 = 4009;
pub const NO_CAPACITY: i64 = 4029;
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

    /// Accept both `playerIds: []` and `playerId: ""`.
    pub fn player_ids(&self) -> Vec<String> {
        let mut ids = Vec::new();

        if let Some(values) = self.data.get("playerIds").and_then(Value::as_array) {
            for value in values {
                if let Some(id) = value.as_str()
                    && !ids.iter().any(|existing| existing == id)
                {
                    ids.push(id.to_string());
                }
            }
        }

        if let Some(id) = self.string_field("playerId")
            && !ids.iter().any(|existing| existing == id)
        {
            ids.push(id.to_string());
        }

        ids
    }

    pub fn ttl(&self) -> std::time::Duration {
        let ttl = self.u64_field("ttlMs").unwrap_or(DEFAULT_TTL_MS);
        std::time::Duration::from_millis(ttl.clamp(1_000, 60_000))
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
        assert_eq!(plural.player_ids(), vec!["a", "b"]);

        let singular: ManagerCommand = serde_json::from_str(
            r#"{"v":1,"id":8,"cmd":"player.redirect","data":{"playerId":"a"}}"#,
        )
        .unwrap();
        assert_eq!(singular.player_ids(), vec!["a"]);
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
        assert_eq!(command.ttl(), std::time::Duration::from_secs(1));
    }
}
