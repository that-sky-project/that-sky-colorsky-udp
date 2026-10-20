#![allow(dead_code)]
#![allow(unused)]

use crate::protocol::game_msg::GameMsg;

impl crate::server::state::ServerState {
    /// Handle the client's final level snapshot after a revoke request.
    ///
    /// The snapshot body is informational here; the server only records the
    /// header so the revoke flow can be observed without touching the
    /// authoritative level cache.
    pub(super) fn handle_level_revoke_ack(&mut self, peer_key: usize, msg: GameMsg) -> Option<()> {
        let peer = self.peers.get(&peer_key)?;

        let (&sequence, rest) = msg.payload.split_first()?;
        let &base = rest.first()?;
        let &checksum = rest.get(1)?;
        let payload_bytes = rest.len().saturating_sub(2);

        tracing::debug!(
            player_id = peer.player_id,
            level_id = peer.level_id,
            sequence,
            base,
            checksum,
            payload_bytes,
            "received level data revoke ack"
        );

        Some(())
    }
}
