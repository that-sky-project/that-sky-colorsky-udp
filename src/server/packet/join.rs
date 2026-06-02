use crate::protocol;
use crate::protocol::packet::Packet;
use crate::utils::fnv1a_player_id;

impl crate::server::state::ServerState {
    /// Handles incoming game messages from a peer.
    /// Should send the EnterGame to everyone else.
    pub(super) fn handle_join_game(&mut self, peer_key: usize, payload: &[u8]) -> Option<()> {
        let peer = self.peers.get_mut(&peer_key)?;

        let uuid_bytes = payload.get(..16)?;

        let tgc_uuid = protocol::types::TgcUuid::from(&uuid_bytes)?;

        let level_bytes = payload.get(payload.len().saturating_sub(4)..)?;

        let level_id = u32::from_le_bytes(<[u8; 4]>::try_from(level_bytes).ok()?);

        // Log the join game packet
        tracing::info!(
            "[ENET:RX] JoinGame tgc_uuid={:?} level_id=0x{:x}",
            tgc_uuid,
            level_id
        );

        let player_id = fnv1a_player_id(&tgc_uuid.raw());
        peer.player_id = player_id;
        peer.uuid = tgc_uuid;
        peer.level_id = level_id;

        let count = self.peers.len();

        let mut payload = Vec::with_capacity(1 + 37 * count);

        payload.push(count as u8);
        for (_, peer) in self.peers.iter() {
            payload.push(peer.player_id);
            payload.extend_from_slice(&peer.uuid.raw());
            payload.extend_from_slice(&[0u8; 16]);
            payload.extend_from_slice(&peer.level_id.to_le_bytes());
        }

        let enter_game_packet =
            Packet::build_packet(protocol::packet::PacketId::EnterGame, &payload);

        self.broadcast_all(0, &enter_game_packet, true);
        Some(())
    }
}
