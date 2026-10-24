//! Compatibility downgrades for older QWD manager builds.
//!
//! The manager validates every node event with a strict schema and answers an
//! unknown event or field with `{ code: 5999, data: { message } }`, where
//! `message` is a JSON array of Zod issues. Newer managers understand
//! `move.completed`, `reservation.expired` and differential telemetry frames;
//! older builds reject the whole message instead. Rather than dropping those
//! events forever (which leaves move transactions open and makes the planner
//! work from stale telemetry), we turn the unsupported features off.

use serde_json::Value;

/// Events and payload shapes the connected manager understands.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct ManagerCaps {
    /// Manager accepts the `move.completed` event.
    pub move_completed: bool,
    /// Manager accepts the `reservation.expired` event.
    pub reservation_expired: bool,
    /// Manager accepts `kind`/`baseSeq`/`removedPlayers` telemetry frames.
    pub delta_telemetry: bool,
}

impl Default for ManagerCaps {
    fn default() -> Self {
        // Assume a current manager; a rejection downgrades to what it supports.
        Self {
            move_completed: true,
            reservation_expired: true,
            delta_telemetry: true,
        }
    }
}

impl ManagerCaps {
    /// Read a manager rejection and disable every feature it complains about.
    ///
    /// Returns the features that were switched off so the caller can log the
    /// downgrade once instead of repeating the raw rejection every second.
    pub(crate) fn observe_rejection(&mut self, message: &str) -> Vec<&'static str> {
        let Ok(issues) = serde_json::from_str::<Value>(message) else {
            return Vec::new();
        };
        let Some(issues) = issues.as_array() else {
            return Vec::new();
        };

        let mut disabled = Vec::new();
        for issue in issues {
            match issue.get("code").and_then(Value::as_str) {
                Some("invalid_enum_value") => match issue
                    .get("received")
                    .and_then(Value::as_str)
                    .unwrap_or_default()
                {
                    "move.completed" if self.move_completed => {
                        self.move_completed = false;
                        disabled.push("move.completed");
                    }
                    "reservation.expired" if self.reservation_expired => {
                        self.reservation_expired = false;
                        disabled.push("reservation.expired");
                    }
                    _ => {}
                },
                Some("unrecognized_keys") => {
                    let delta_keys =
                        issue
                            .get("keys")
                            .and_then(Value::as_array)
                            .is_some_and(|keys| {
                                keys.iter()
                                    .filter_map(Value::as_str)
                                    .any(|key| matches!(key, "kind" | "baseSeq" | "removedPlayers"))
                            });
                    if delta_keys && self.delta_telemetry {
                        self.delta_telemetry = false;
                        disabled.push("delta telemetry");
                    }
                }
                _ => {}
            }
        }
        disabled
    }
}

#[cfg(test)]
mod tests {
    use super::ManagerCaps;

    /// Rejection captured from a deployed manager that knows level isolation
    /// but predates `move.completed` and differential telemetry.
    const TELEMETRY_REJECTION: &str = r#"[
      {
        "code": "unrecognized_keys",
        "keys": ["baseSeq", "kind", "removedPlayers"],
        "path": ["data"],
        "message": "Unrecognized key(s) in object: 'baseSeq', 'kind', 'removedPlayers'"
      }
    ]"#;

    const MOVE_REJECTION: &str = r#"[
      {
        "received": "move.completed",
        "code": "invalid_enum_value",
        "options": ["player.join", "player.leave", "room.telemetry", "move.result"],
        "path": ["event"],
        "message": "Invalid enum value"
      },
      {
        "code": "unrecognized_keys",
        "keys": ["playerIds"],
        "path": ["data"],
        "message": "Unrecognized key(s) in object: 'playerIds'"
      }
    ]"#;

    #[test]
    fn telemetry_rejection_disables_delta_frames_only() {
        let mut caps = ManagerCaps::default();
        assert_eq!(
            caps.observe_rejection(TELEMETRY_REJECTION),
            vec!["delta telemetry"]
        );
        assert!(!caps.delta_telemetry);
        assert!(caps.move_completed);
        assert!(caps.reservation_expired);
    }

    #[test]
    fn move_rejection_disables_move_completed() {
        let mut caps = ManagerCaps::default();
        assert_eq!(
            caps.observe_rejection(MOVE_REJECTION),
            vec!["move.completed"]
        );
        assert!(!caps.move_completed);
        assert!(caps.delta_telemetry);
    }

    #[test]
    fn repeated_rejection_is_idempotent() {
        let mut caps = ManagerCaps::default();
        caps.observe_rejection(MOVE_REJECTION);
        assert!(caps.observe_rejection(MOVE_REJECTION).is_empty());
    }

    #[test]
    fn non_json_message_is_ignored() {
        let mut caps = ManagerCaps::default();
        assert!(caps.observe_rejection("boom").is_empty());
        assert_eq!(caps, ManagerCaps::default());
    }
}
