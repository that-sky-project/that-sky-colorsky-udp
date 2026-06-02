#![allow(dead_code)]
#![allow(unused)]

use crate::protocol::{
    self,
    game_msg::{GameMsg, GameMsgId},
    packet::Packet,
};

impl crate::server::state::ServerState {
    /// Handles player state
    /// [TODO]
    /// - use a net level data snapshot reader/writer
    /// - resolve net level data
    pub(super) fn handle_level_elect(&mut self, peer_key: usize, msg: GameMsg) -> Option<()> {
        let peer = self.peers.get_mut(&peer_key)?;

        let save_seq = msg.payload[0];
        let base_seq = msg.payload[1];
        let checksum = msg.payload[2];
        let raw_level_data = peer.snap_reader.apply_delta(&msg.payload)?;

        // Immediately reply to the sender with SnapshotAck (ack sequence number = saveSeq).
        if save_seq != 0 {
            let ack = Packet::build_packet(
                protocol::packet::PacketId::GameMsg,
                // the ack struct is [player_state_ack: u8] [level_data_ack: u8]
                // [TODO]
                // - merge level_data_ack and player_state_ack
                // may I need construct a general trait
                &[GameMsgId::SnapshotAck.to_u8(), peer.lv_seq, 0, 1, save_seq],
            );
            peer.send(0, &ack, true);
        }

        Some(())
    }
}
