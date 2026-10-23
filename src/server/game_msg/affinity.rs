#![allow(dead_code)]
#![allow(unused)]

use crate::protocol::game_msg::{GameMsg, affinity::Affinity};

impl crate::server::state::ServerState {
    /// Handle a client `Affinity` update.
    ///
    /// This message is client-to-server only in the current client, so it is
    /// consumed and never relayed to other players.
    pub(super) fn handle_affinity(&mut self, peer_key: usize, msg: GameMsg) -> Option<()> {
        let peer = self.peers.get_mut(&peer_key)?;
        let affinity = Affinity::from_bytes(&msg.payload)?;

        tracing::debug!(
            player_id = peer.player_id,
            count = affinity.count,
            complete = affinity.is_complete(),
            trailing_bytes = affinity.trailing_bytes,
            entries = ?affinity.entries,
            "consumed client affinity update"
        );

        // Exposed to adapters (QWD telemetry) as a UUID-keyed map.
        peer.affinity = affinity.entries;

        Some(())
    }
}
