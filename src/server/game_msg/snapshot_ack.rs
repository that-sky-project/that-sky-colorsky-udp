#![allow(dead_code)]
#![allow(unused)]

use crate::{
    protocol::{
        self,
        game_msg::{GameMsg, GameMsgId},
    },
    utils::hex_preview,
};

impl crate::server::state::ServerState {
    /// Handles incoming game messages from a peer.
    pub(super) fn handle_snapshot_ack(&mut self, peer_key: usize, msg: GameMsg) -> Option<()> {
        let peer = self.peers.get_mut(&peer_key)?;

        let pending = peer.snap_writer.pending_ack_seq();

        let &player_stat_ack = msg.payload.first()?;
        let &level_data_ack = msg.payload.get(1)?;

        if pending == Some(player_stat_ack) {
            tracing::debug!(
                "[ENET:ACK] player={} ack={} pending={:?} matched=true payload={}",
                peer.player_id,
                player_stat_ack,
                pending,
                hex_preview(&msg.payload, msg.payload.len())
            );
            peer.snap_writer.ack(player_stat_ack);
        } else {
            tracing::debug!(
                "[ENET:ACK] player={} ack={} pending={:?} matched=false payload={}",
                peer.player_id,
                player_stat_ack,
                pending,
                hex_preview(&msg.payload, msg.payload.len())
            );
        }
        Some(())
    }
}
