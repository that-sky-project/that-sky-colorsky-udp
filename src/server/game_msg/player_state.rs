#![allow(dead_code)]
#![allow(unused)]

use std::time::Instant;

use crate::{
    protocol::{
        self,
        game_msg::{GameMsg, GameMsgId},
        packet::Packet,
    },
    utils::hex_preview,
};

/// PlayerState starts with `state_kind: u8` followed by three little-endian
/// floats. That prefix has been stable across client revisions.
fn decode_position(state: &[u8]) -> Option<[f32; 3]> {
    let x = f32::from_le_bytes(state.get(1..5)?.try_into().ok()?);
    let y = f32::from_le_bytes(state.get(5..9)?.try_into().ok()?);
    let z = f32::from_le_bytes(state.get(9..13)?.try_into().ok()?);
    [x, y, z]
        .iter()
        .all(|value| value.is_finite())
        .then_some([x, y, z])
}

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

        let now = Instant::now();
        if let Some(position) = decode_position(&raw_state) {
            if let Some((previous, previous_at)) = peer.player_delta.previous_position {
                let dt = now.duration_since(previous_at).as_secs_f32();
                if dt > 0.001 {
                    let dx = position[0] - previous[0];
                    let dy = position[1] - previous[1];
                    let dz = position[2] - previous[2];
                    // The native client reports a velocity vector; the raw
                    // layout is version-specific, so approximate it from
                    // consecutive positions and clamp level-change teleports.
                    let speed = (dx * dx + dy * dy + dz * dz).sqrt() / dt;
                    if speed.is_finite() {
                        peer.player_delta.velocity = Some(speed.min(150.0));
                    }
                }
            }
            peer.player_delta.previous_position = Some((position, now));
            peer.player_delta.position = Some(position);
        }
        peer.player_delta.state_at_ms = crate::server::peer::now_ms();

        peer.player_delta.raw_state.clear();
        peer.player_delta.raw_state.extend(raw_state);

        Some(())
    }

    // Sync Player State frame, this function will called by server tick
    pub(crate) fn sync_frame(&mut self) {
        let all_entry: Vec<(u8, u32, Vec<u8>)> = self
            .peers
            .values()
            .filter(|pr| pr.is_active() && !(pr.player_delta.raw_state.is_empty()))
            .map(|pr| (pr.player_id, pr.level_id, pr.player_delta.raw_state.clone()))
            .collect();

        for peer in self.peers.values_mut() {
            if !peer.is_active() {
                continue;
            }
            let entry: Vec<u8> = all_entry
                .iter()
                .filter(|(player, level_id, _)| *player != peer.player_id)
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
