//! Client-requested disconnect (packet id 1).
//!
//! The payload is a single reason byte. XYSKY closes the peer when it sees this
//! packet; we do the same but keep the peer state untouched so the regular
//! `on_disconnect` path still publishes `player.leave` to QWD (a move
//! transaction only settles after both the join and the leave arrive).

use crate::server::peer::PeerState;

impl crate::server::state::ServerState {
    pub(super) fn handle_client_disconnect(
        &mut self,
        peer_key: usize,
        payload: &[u8],
    ) -> Option<()> {
        let reason = payload.first().copied().unwrap_or_default();
        let peer = self.peers.get_mut(&peer_key)?;

        tracing::info!(
            "[ENET:CLIENT_DISCONNECT] player={} uuid={:?} level={:#x} reason={} state={:?}",
            peer.player_id,
            peer.uuid,
            peer.level_id,
            reason,
            peer.state
        );

        // The client already stopped sending; finish the ENet handshake so the
        // slot is released instead of waiting for the timeout.
        if peer.state != PeerState::Closed {
            peer.disconnect_later(0);
        }
        Some(())
    }
}
