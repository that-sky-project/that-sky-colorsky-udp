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
    /// Handles player state
    pub(super) fn handle_player_state(&mut self, peer_key: usize, msg: GameMsg) -> Option<()> {
        let peer = self.peers.get_mut(&peer_key)?;

        let save_seq = msg.payload[0];
        let base_seq = msg.payload[1];
        let checksum = msg.payload[2];
        let raw_state = peer.snap_reader.apply_delta(&msg.payload)?;

        // Immediately reply to the sender with SnapshotAck (ack sequence number = saveSeq).
        if save_seq != 0 {
            let ack = Packet::build_packet(
                protocol::packet::PacketId::GameMsg,
                &[GameMsgId::SnapshotAck.to_u8(), peer.lv_seq, 0, save_seq, 1],
            );
            peer.send(0, &ack, true);
        }

        peer.player_state_raw.clear();
        peer.player_state_raw.extend(raw_state);

        self.sync_frame()
    }

    fn sync_frame(&mut self) -> Option<()> {
        // [TODO]
        // - refact those code

        let all_entry: Vec<(u8, Vec<u8>)> = self
            .peers
            .values()
            .filter(|pr| !(pr.player_state_raw.is_empty()))
            .map(|pr| (pr.player_id, pr.player_state_raw.clone()))
            .collect();

        for peer in self.peers.values_mut() {
            let entry: Vec<u8> = all_entry
                .iter()
                .filter(|(player, _)| *player != peer.player_id)
                .map(|(player_id, player_state)| {
                    [
                        vec![*player_id],
                        (player_state.len() as u32).to_le_bytes().to_vec(),
                        player_state.to_vec(),
                    ]
                    .concat()
                })
                .flatten()
                .collect();
            if entry.is_empty() {
                continue;
            }
            let snap = peer.snap_writer.generate_delta(&entry);

            let pack = Packet::build_packet(
                protocol::packet::PacketId::GameMsg,
                &[vec![GameMsgId::PlayerState.to_u8(), peer.lv_seq, 0], snap].concat(),
            );

            peer.send(0, &pack, true);
        }

        Some(())
    }
}
