use crate::protocol;
use crate::protocol::packet::Packet;

impl crate::server::state::ServerState {
    pub(super) fn handle_level_update(&mut self, peer_key: usize, payload: &[u8]) -> Option<()> {
        let peer = self.peers.get_mut(&peer_key)?;

        // The client owns this counter. Trust the packet instead of assuming
        // it increments by one; a mismatch makes the client drop every
        // snapshot frame we send for the new level generation.
        let level_change_count = *payload.first()?;
        let level_bytes = payload.get(14..18)?;

        let new_level = u32::from_le_bytes(<[u8; 4]>::try_from(level_bytes).ok()?);

        let old_level = peer.level_id;
        peer.lv_seq = level_change_count;

        if old_level != new_level {
            let player_id = peer.player_id;
            peer.level_id = new_level;

            tracing::info!(
                "[ENET:LEVEL_UPDATE] player {} switched to level 0x{:x}",
                peer.player_id,
                new_level
            );

            let mut payload = Vec::with_capacity(5);
            payload.push(player_id);
            payload.extend(level_bytes);

            let player_change_level =
                Packet::build_packet(protocol::packet::PacketId::PlayerChangedLevel, &payload);

            // Notify players in both the old and new levels.
            self.broadcast_same_level_without(0, &player_change_level, true, old_level, player_id);
            self.broadcast_same_level_without(0, &player_change_level, true, new_level, player_id);

            // if the authority change level
            // remove it from old level and elect a replacement
            if player_id == self.level_authority.get(&old_level).copied().unwrap_or(0) {
                self.level_authority.remove(&old_level);
                self.rebalance_level(old_level);
            }
        }

        self.send_elect_to(peer_key);

        Some(())
    }
}
