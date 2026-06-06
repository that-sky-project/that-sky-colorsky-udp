#![allow(dead_code)]
#![allow(unused)]

use std::collections::HashMap;

use crate::{
    protocol::{
        self,
        game_msg::{
            GameMsg, GameMsgId,
            level_data::{self, LevelData},
        },
        packet::Packet,
    },
    server::{PeerEntry, game_msg::level},
    utils::hex_preview,
};

impl crate::server::state::ServerState {
    /// Handles player state
    pub(super) fn handle_level_data(&mut self, peer_key: usize, msg: GameMsg) -> Option<()> {
        let peer = self.peers.get_mut(&peer_key)?;

        if Some(&peer.player_id) != self.level_authority.get(&peer.level_id) {
            // only the level authority can send level data
            return None;
        }

        let save_seq = msg.payload[0];
        let base_seq = msg.payload[1];
        let checksum = msg.payload[2];
        let raw_level_data = peer.level_delta.snap_reader.apply_delta(&msg.payload)?;

        // ack will reply by tick
        if save_seq != 0 {
            peer.level_delta.snapshot_ack = Some(save_seq);
        }

        let level_data = LevelData::from_bytes(&raw_level_data)?;

        let last_level_data = self.levels_data.get_mut(&peer.level_id)?;

        tracing::debug!("[LEVEL_DATA] last: {:?}", &last_level_data.header);
        tracing::debug!("[LEVEL_DATA] new: {:?}", &level_data.header);

        last_level_data.update(&level_data);

        Some(())
    }

    /// Handle level revoke
    pub(super) fn handle_level_revoke(&mut self, peer_key: usize, _msg: GameMsg) -> Option<()> {
        let peer = self.peers.get(&peer_key)?;
        if *self.level_authority.get(&peer.level_id)? == peer.player_id {
            self.level_authority.remove(&peer.level_id);
        }

        let next = self
            .peers
            .values()
            .filter(|pr| pr.level_id == peer.level_id && pr.player_id != peer.player_id)
            .collect::<Vec<_>>();

        if let Some(next) = next.iter().next() {
            self.level_authority.insert(peer.level_id, next.player_id);
        }

        Some(())
    }

    /// Handle level heart beat
    pub(super) fn handle_level_heart_beat(&mut self, peer_key: usize, msg: GameMsg) -> Option<()> {
        let peer = self.peers.get(&peer_key)?;

        if self.level_authority.get(&peer.level_id).is_none() {
            self.level_authority.insert(peer.level_id, peer.player_id);
        }

        Some(())
    }

    /// Sync level data
    pub(crate) fn sync_level(&mut self) -> Option<()> {
        for peer in self.peers.values_mut() {
            if peer.player_id == *self.level_authority.get(&peer.level_id)? {
                continue;
            }
            let level_id = peer.level_id;
            if let Some(level_data) = self.levels_data.get(&level_id) {
                tracing::debug!(
                    "level state hex={}",
                    hex_preview(&level_data.to_bytes(), level_data.to_bytes().len())
                );
                let snap = peer
                    .level_delta
                    .snap_writer
                    .generate_delta(&level_data.to_bytes());

                let msg = GameMsg::new(GameMsgId::NetLevelData, peer.lv_seq, 0, &snap);

                let payload = Packet::build_packet(
                    protocol::packet::PacketId::GameMsg,
                    &msg.build_game_msg(),
                );

                peer.send(0, &payload, true);
            }
        }

        Some(())
    }

    /// Send level elect msg to target player with **peer_key**
    pub(crate) fn send_elect_to(&mut self, to: usize) -> Option<()> {
        let peer = self.peers.get_mut(&to)?;

        // Cannot send delta frame, idk why.
        peer.level_delta.snap_writer.force_keyframe();

        let level_id = peer.level_id;

        let authority = if let Some(authority) = self.level_authority.get(&level_id) {
            *authority
        } else {
            self.level_authority.insert(level_id, peer.player_id);
            peer.player_id
        };

        let level_data = if let Some(level_data) = self.levels_data.get_mut(&level_id) {
            level_data
        } else {
            let level_data = LevelData::new(authority, level_id, 0, &[]);
            self.levels_data.insert(level_id, level_data);
            self.levels_data.get_mut(&level_id)?
        };

        // when player enter a non-player & non-empty level, we need set its authority
        level_data.header.elected_player = authority;

        let mut snap = peer
            .level_delta
            .snap_writer
            .generate_delta(&level_data.to_bytes());

        let msg = GameMsg::new(GameMsgId::NetLevelDataElect, peer.lv_seq, 0, &snap);

        tracing::info!(
            "[ENET:LEVEL_ECLECT] send to {}, level data header={:?}",
            peer.player_id,
            level_data.header
        );

        self.send_msg_to(msg, to)
    }
}
