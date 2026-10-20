#![allow(dead_code)]
#![allow(unused)]

use crate::protocol::game_msg::{GameMsg, nominee::NetLevelElectionNominee};

impl crate::server::state::ServerState {
    /// Handle a client level-authority nomination.
    pub(super) fn handle_election_nominee(&mut self, peer_key: usize, msg: GameMsg) -> Option<()> {
        let peer = self.peers.get(&peer_key)?;
        let nominee = NetLevelElectionNominee::from_bytes(&msg.payload)?;

        if nominee.level_id != peer.level_id {
            tracing::debug!(
                player_id = peer.player_id,
                player_level_id = peer.level_id,
                nominee_level_id = nominee.level_id,
                "ignored election nominee for non-current level"
            );
            return None;
        }

        tracing::debug!(
            player_id = peer.player_id,
            level_id = nominee.level_id,
            reason = nominee.reason,
            trailing_bytes = nominee.trailing_bytes,
            "received level election nominee"
        );

        Some(())
    }
}
