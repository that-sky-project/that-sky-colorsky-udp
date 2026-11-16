//! Per-QWD-socket command idempotency and collision cache.
//!
//! The manager may retry an acknowledgement after a transport hiccup.  A
//! completed command is replayed from this cache so a retry cannot repeat a
//! side effect such as sending `MoveGame`.  The cache is cleared at each
//! socket epoch by the QWD transport owner and is bounded to keep command ids
//! from becoming unbounded state.

use std::collections::{HashMap, VecDeque};

use serde_json::Value;

const MAX_ENTRIES: usize = 512;

#[derive(Debug)]
struct Entry {
    fingerprint: String,
    response: Value,
}

#[derive(Debug, Clone, PartialEq)]
pub(super) enum Lookup {
    Miss,
    Replay(Value),
    Collision,
}

#[derive(Debug)]
pub(super) struct CommandCache {
    entries: HashMap<i64, Entry>,
    order: VecDeque<i64>,
    epoch: u64,
}

impl CommandCache {
    pub(super) fn new() -> Self {
        Self {
            entries: HashMap::new(),
            order: VecDeque::new(),
            epoch: 0,
        }
    }

    /// Start a fresh socket epoch. Command ids are scoped to a connection.
    pub(super) fn clear(&mut self) {
        self.epoch = self.epoch.wrapping_add(1);
        self.entries.clear();
        self.order.clear();
    }

    #[cfg(test)]
    pub(super) fn epoch(&self) -> u64 {
        self.epoch
    }

    /// Look up an id and distinguish a retry from reuse with a new command.
    pub(super) fn get(&mut self, id: i64, fingerprint: &str) -> Lookup {
        let Some(entry) = self.entries.get(&id) else {
            return Lookup::Miss;
        };

        if entry.fingerprint != fingerprint {
            return Lookup::Collision;
        }

        let response = entry.response.clone();
        self.order.retain(|cached_id| *cached_id != id);
        self.order.push_back(id);
        Lookup::Replay(response)
    }

    /// Store the complete acknowledgement for a command.
    pub(super) fn finish(&mut self, id: i64, fingerprint: impl Into<String>, response: Value) {
        self.entries.insert(
            id,
            Entry {
                fingerprint: fingerprint.into(),
                response,
            },
        );
        self.order.retain(|cached_id| *cached_id != id);
        self.order.push_back(id);

        while self.order.len() > MAX_ENTRIES {
            let Some(oldest) = self.order.pop_front() else {
                break;
            };
            self.entries.remove(&oldest);
        }
    }

    #[cfg(test)]
    pub(super) fn len(&self) -> usize {
        self.entries.len()
    }
}

/// Produce a stable-enough fingerprint for a parsed command envelope.
///
/// The manager retries the same JSON envelope, so serializing the parsed value
/// preserves the fields that affect command execution while ignoring transport
/// framing.  A changed payload with the same command id is rejected.
pub(super) fn fingerprint(value: &Value) -> String {
    serde_json::to_string(value).unwrap_or_else(|_| value.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn replays_same_id_and_rejects_collision() {
        let mut cache = CommandCache::new();
        let value = json!({"id": 1, "cmd": "room.sync"});
        let fp = fingerprint(&value);
        assert_eq!(cache.get(1, &fp), Lookup::Miss);
        cache.finish(1, fp.clone(), json!({"id": 1, "code": 0}));
        assert_eq!(
            cache.get(1, &fp),
            Lookup::Replay(json!({"id": 1, "code": 0}))
        );
        assert_eq!(cache.get(1, "different"), Lookup::Collision);
    }

    #[test]
    fn clear_starts_new_epoch() {
        let mut cache = CommandCache::new();
        let first = cache.epoch();
        cache.finish(1, "a", json!(null));
        cache.clear();
        assert!(cache.epoch() > first);
        assert_eq!(cache.get(1, "a"), Lookup::Miss);
    }

    #[test]
    fn cache_is_bounded() {
        let mut cache = CommandCache::new();
        for id in 1..=513 {
            cache.finish(id, id.to_string(), json!(id));
        }
        assert_eq!(cache.len(), 512);
        assert_eq!(cache.get(1, "1"), Lookup::Miss);
        assert_eq!(cache.get(513, "513"), Lookup::Replay(json!(513)));
    }
}
