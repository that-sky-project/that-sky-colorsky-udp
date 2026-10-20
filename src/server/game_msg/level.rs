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
    utils::{hex_preview, snapshot::SnapshotWriter},
};

impl crate::server::state::ServerState {
    /// Handles Level data
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
        let level_id = peer.level_id;

        if let Some(last_level_data) = self.levels_data.get_mut(&level_id) {
            tracing::debug!("[LEVEL_DATA] last: {:?}", &last_level_data.header);
            tracing::debug!("[LEVEL_DATA] new: {:?}", &level_data.header);
            last_level_data.update(&level_data);
        } else {
            // The level cache is created by the first real authority upload.
            // Do not stream empty level data before that point.
            tracing::debug!("[LEVEL_DATA] new: {:?}", &level_data.header);
            let mut level_data = level_data;
            level_data.header.elected_player = peer.player_id;
            self.levels_data.insert(level_id, level_data);
        }

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

        if let Some(next) = next.first() {
            self.level_authority.insert(peer.level_id, next.player_id);
        }

        Some(())
    }

    /// Handle level heart beat
    pub(super) fn handle_level_heart_beat(&mut self, peer_key: usize, msg: GameMsg) -> Option<()> {
        let peer = self.peers.get(&peer_key)?;

        self.level_authority
            .entry(peer.level_id)
            .or_insert(peer.player_id);

        Some(())
    }

    /// Sync level data, this function will called by server tick
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

        let level_id = peer.level_id;

        let authority = if let Some(authority) = self.level_authority.get(&level_id) {
            *authority
        } else {
            self.level_authority.insert(level_id, peer.player_id);
            peer.player_id
        };

        let level_bytes = if let Some(level_data) = self.levels_data.get_mut(&level_id) {
            // when player enter a non-player & non-empty level, we need set its authority
            level_data.header.elected_player = authority;
            level_data.to_bytes()
        } else {
            // Keep only the Elect payload local; the cache is populated when
            // the authority uploads real level data (matches XYSKY).
            LevelData::new(authority, level_id, 0, &[]).to_bytes()
        };

        // Elect is decoded by a temporary client reader initialized with the
        // empty sequence-1 base. Use a fresh writer here so the persistent
        // NetLevelData stream keeps its sequence/window intact; resetting the
        // peer writer would make the client lose the base and stop acking.
        let mut elect_writer = SnapshotWriter::new();
        let snap = elect_writer.generate_delta(&level_bytes);

        let msg = GameMsg::new(GameMsgId::NetLevelDataElect, peer.lv_seq, 0, &snap);

        tracing::debug!(
            "[ENET:LEVEL_ECLECT] send to {}, level_id={:#x} authority={}",
            peer.player_id,
            level_id,
            authority
        );

        self.send_msg_to(msg, to)
    }
}
