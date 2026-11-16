//! Bounded replay of business events whose transport send failed.
use serde_json::Value;
use std::collections::VecDeque;
use std::time::{Duration, Instant};

pub(super) struct EventOutbox {
    pending: VecDeque<(Instant, Value)>,
}
impl EventOutbox {
    pub fn new() -> Self {
        Self {
            pending: VecDeque::new(),
        }
    }
    pub fn enqueue(&mut self, value: Value, now: Instant) -> Result<(), String> {
        self.prune(now);
        if self.pending.len() >= 512 {
            return Err("QWD event outbox capacity exceeded".into());
        }
        self.pending.push_back((now, value));
        Ok(())
    }
    pub fn front(&mut self, now: Instant) -> Option<Value> {
        self.prune(now);
        self.pending.front().map(|(_, value)| value.clone())
    }
    pub fn sent(&mut self) {
        self.pending.pop_front();
    }
    fn prune(&mut self, now: Instant) {
        while self
            .pending
            .front()
            .is_some_and(|(at, _)| now.duration_since(*at) >= Duration::from_secs(60))
        {
            self.pending.pop_front();
            tracing::warn!(
                "QWD unsent business event exceeded replay lifetime; full telemetry remains authoritative"
            );
        }
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn replay_preserves_order_until_confirmed_send_and_has_expiry() {
        let mut outbox = EventOutbox::new();
        let now = Instant::now();
        outbox.enqueue(serde_json::json!({"id":1}), now).unwrap();
        outbox.enqueue(serde_json::json!({"id":2}), now).unwrap();
        assert_eq!(outbox.front(now).unwrap()["id"], 1);
        assert_eq!(outbox.front(now).unwrap()["id"], 1);
        outbox.sent();
        assert_eq!(outbox.front(now).unwrap()["id"], 2);
        assert!(outbox.front(now + Duration::from_secs(61)).is_none());
    }
}
