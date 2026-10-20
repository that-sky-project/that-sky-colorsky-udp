#![allow(dead_code)]
#![allow(unused)]

use crate::protocol::packet::move_result::MoveResult;

impl crate::server::state::ServerState {
    /// Handle a client `MoveResult`.
    ///
    /// colorsky has no cross-room move service, so the result is parsed and
    /// reported instead of changing session state.
    pub(super) fn handle_move_result(&mut self, peer_key: usize, payload: &[u8]) -> Option<()> {
        let peer = self.peers.get(&peer_key)?;
        let result = MoveResult::from_bytes(payload)?;

        tracing::info!(
            player_id = peer.player_id,
            level_id = peer.level_id,
            result = result.result,
            trailing_bytes = result.payload.len(),
            "received MoveResult"
        );

        Some(())
    }
}
