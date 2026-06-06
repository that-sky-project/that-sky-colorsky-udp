use crate::protocol;
use crate::protocol::packet::Packet;

impl crate::server::state::ServerState {
    pub(super) fn handle_level_update(&mut self, peer_key: usize, payload: &[u8]) -> Option<()> {
        let peer = self.peers.get_mut(&peer_key)?;

        // each level updata packet will increase it
        peer.lv_seq += 1;

        let level_bytes = payload.get(14..18)?;

        let new_level = u32::from_le_bytes(<[u8; 4]>::try_from(level_bytes).ok()?);

        let old_level = peer.level_id;

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
            // remove it from old level
            if player_id == self.level_authority.get(&old_level).copied().unwrap_or(0) {
                self.level_authority.remove(&old_level);
            }
        }

        self.send_elect_to(peer_key);

        Some(())
    }
}
