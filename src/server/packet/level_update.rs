use crate::protocol;
use crate::protocol::packet::Packet;

/// u8 serial ordering permits wrap, but never accepts a same-generation level change.
fn accepts_generation(current: u8, incoming: u8, old_level: u32, new_level: u32) -> bool {
    let advance = incoming.wrapping_sub(current);
    advance < 128 && (advance != 0 || old_level == new_level)
}

impl crate::server::state::ServerState {
    pub(super) fn handle_level_update(&mut self, peer_key: usize, payload: &[u8]) -> Option<()> {
        let peer = self.peers.get_mut(&peer_key)?;
        let level_change_count = *payload.first()?;
        let level_bytes = payload.get(14..18)?;
        let new_level = u32::from_le_bytes(<[u8; 4]>::try_from(level_bytes).ok()?);
        let old_level = peer.level_id;
        if !accepts_generation(peer.lv_seq, level_change_count, old_level, new_level) {
            tracing::debug!(
                player_id = peer.player_id,
                current = peer.lv_seq,
                incoming = level_change_count,
                old_level,
                new_level,
                "ignored stale or conflicting LevelUpdate"
            );
            return Some(());
        }
        peer.lv_seq = level_change_count;
        if old_level != new_level {
            let player_id = peer.player_id;
            peer.level_id = new_level;
            tracing::info!(
                "[ENET:LEVEL_UPDATE] player {} switched to level 0x{:x}",
                player_id,
                new_level
            );
            let mut notification = Vec::with_capacity(5);
            notification.push(player_id);
            notification.extend(level_bytes);
            let packet = Packet::build_packet(
                protocol::packet::PacketId::PlayerChangedLevel,
                &notification,
            );
            self.broadcast_same_level_without(0, &packet, true, old_level, player_id);
            self.broadcast_same_level_without(0, &packet, true, new_level, player_id);
            if player_id == self.level_authority.get(&old_level).copied().unwrap_or(0) {
                self.level_authority.remove(&old_level);
                self.rebalance_level(old_level);
            }
        }
        self.send_elect_to(peer_key);
        Some(())
    }
}

#[cfg(test)]
mod tests {
    use super::accepts_generation;
    #[test]
    fn serial_counter_accepts_wrap_and_rejects_regression_or_conflict() {
        assert!(accepts_generation(255, 0, 1, 2));
        assert!(accepts_generation(3, 3, 1, 1));
        assert!(!accepts_generation(3, 2, 1, 1));
        assert!(!accepts_generation(3, 3, 1, 2));
        assert!(!accepts_generation(0, 255, 1, 2));
        assert!(!accepts_generation(0, 128, 1, 2));
    }
}
