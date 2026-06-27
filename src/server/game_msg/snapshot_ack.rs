#![allow(dead_code)]
#![allow(unused)]

use crate::{
    protocol::{
        self,
        game_msg::{GameMsg, GameMsgId},
        packet::Packet,
    },
    utils::hex_preview,
};

impl crate::server::state::ServerState {
    /// Handles snapshot ack
    pub(super) fn handle_snapshot_ack(&mut self, peer_key: usize, msg: GameMsg) -> Option<()> {
        let peer = self.peers.get_mut(&peer_key)?;

        if let Some(player_state_pending) = peer.player_delta.snap_writer.pending_ack_seq() {
            peer.player_delta.snap_writer.ack(*msg.payload.first()?);
        }

        if let Some(level_data_pending) = peer.level_delta.snap_writer.pending_ack_seq() {
            peer.level_delta.snap_writer.ack(*msg.payload.get(1)?);
        }

        Some(())
    }

    // This function will called by server tick
    pub(crate) fn snapshot_ack(&mut self) {
        let acks: Vec<(usize, u8, [u8; 2])> = self
            .peers
            .iter()
            .map(|(peer_key, peer)| {
                (
                    *peer_key,
                    peer.lv_seq,
                    [
                        peer.player_delta.snapshot_ack.unwrap_or(1),
                        peer.level_delta.snapshot_ack.unwrap_or(1),
                    ],
                )
            })
            .collect();
        for (peer_key, lv_seq, ack) in acks {
            let msg = GameMsg::new(GameMsgId::SnapshotAck, lv_seq, 0, &ack);
            self.send_msg_to(msg, peer_key);
        }
    }
}
