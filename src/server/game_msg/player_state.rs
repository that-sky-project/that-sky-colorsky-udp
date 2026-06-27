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
        let raw_state = peer.player_delta.snap_reader.apply_delta(&msg.payload)?;

        // ack will reply by tick
        if save_seq != 0 {
            peer.player_delta.snapshot_ack = Some(save_seq);
        }

        peer.player_delta.raw_state.clear();
        peer.player_delta.raw_state.extend(raw_state);

        Some(())
    }

    // Sync Player State frame, this function will called by server tick
    pub(crate) fn sync_frame(&mut self) {
        let all_entry: Vec<(u8, u32, Vec<u8>)> = self
            .peers
            .values()
            .filter(|pr| !(pr.player_delta.raw_state.is_empty()))
            .map(|pr| (pr.player_id, pr.level_id, pr.player_delta.raw_state.clone()))
            .collect();

        for peer in self.peers.values_mut() {
            let entry: Vec<u8> = all_entry
                .iter()
                .filter(|(player, level_id, _)| {
                    *player != peer.player_id && *level_id == peer.level_id
                })
                .flat_map(|(id, _, state)| {
                    let mut buf = Vec::with_capacity(1 + 4 + state.len());
                    buf.push(*id);
                    buf.extend((state.len() as u32).to_le_bytes());
                    buf.extend(state);
                    buf
                })
                .collect();
            if entry.is_empty() {
                continue;
            }
            let snap = peer.player_delta.snap_writer.generate_delta(&entry);

            let pack = Packet::build_packet(
                protocol::packet::PacketId::GameMsg,
                &[vec![GameMsgId::PlayerState.to_u8(), peer.lv_seq, 0], snap].concat(),
            );

            peer.send(0, &pack, true);
        }
    }
}
