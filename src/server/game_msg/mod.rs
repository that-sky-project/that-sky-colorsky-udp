#![allow(dead_code)]
#![allow(unused)]

use crate::protocol::{self, game_msg::GameMsgId};

mod affinity;
mod level;
mod net_rpc;
mod nominee;
mod player_state;
mod revoke_ack;
mod snapshot_ack;
mod utils;

impl crate::server::state::ServerState {
    /// Handles incoming game messages from a peer.
    pub(super) fn handle_game_msg(
        &mut self,
        peer_key: usize,
        game_msg: protocol::game_msg::GameMsg,
    ) {
        // The level generation is owned by JoinGame/LevelUpdate. A GameMsg
        // carrying a different generation was sent before the client saw the
        // new level; dropping it keeps `lv_seq` stable, which the client
        // requires before it will decode (and acknowledge) our snapshots.
        //
        // SnapshotAck is exempt: the writers validate the acknowledged
        // sequence against their own window, so a stale ack is harmless while
        // dropping it would stall the window.
        if game_msg.msg_id != GameMsgId::SnapshotAck
            && let Some(peer) = self.peers.get(&peer_key)
            && game_msg.level_seq != peer.lv_seq
        {
            tracing::debug!(
                player_id = peer.player_id,
                expected = peer.lv_seq,
                received = game_msg.level_seq,
                msg_id = game_msg.msg_id.to_u8(),
                "ignored GameMsg with stale level generation"
            );
            return;
        }

        match game_msg.msg_id {
            // NetRpc (now we just forward it to all players)
            GameMsgId::NetRpc => {
                self.handle_net_rpc(peer_key, game_msg);
            }

            // PlayerState need use snapshot
            GameMsgId::PlayerState => {
                self.handle_player_state(peer_key, game_msg);
            }

            // Client affinity update (consumed, never relayed)
            GameMsgId::Affinity => {
                self.handle_affinity(peer_key, game_msg);
            }

            // Handle level revoke
            GameMsgId::NetLevelDataRevoke => {
                self.handle_level_revoke(peer_key, game_msg);
            }

            // Final snapshot after a level revoke
            GameMsgId::NetLevelDataRevokeAck => {
                self.handle_level_revoke_ack(peer_key, game_msg);
            }

            // Handle level data
            GameMsgId::NetLevelData => {
                self.handle_level_data(peer_key, game_msg);
            }

            // Level authority heartbeat
            GameMsgId::NetLevelDataHeartbeat => {
                self.handle_level_heart_beat(peer_key, game_msg);
            }

            // SnapshotAck, really ack?
            GameMsgId::SnapshotAck => {
                self.handle_snapshot_ack(peer_key, game_msg);
            }

            // Authority nomination from a client
            GameMsgId::NetLevelElectionNominee => {
                self.handle_election_nominee(peer_key, game_msg);
            }

            // so those msg need be forward to same level
            GameMsgId::Critters | GameMsgId::MusicSync => {
                self.relay_game_msg(game_msg.msg_id, &game_msg.payload, peer_key);
            }

            // Elect is server-only. Relaying a client-sent Elect would let a
            // peer overwrite the authority view of the whole level.
            GameMsgId::NetLevelDataElect => {
                let player_id = self
                    .peers
                    .get(&peer_key)
                    .map(|peer| peer.player_id)
                    .unwrap_or(0);
                tracing::warn!(
                    player_id = player_id,
                    bytes = game_msg.payload.len(),
                    "ignored server-only NetLevelDataElect from client"
                );
            }

            // Metrics and audience messages are client-to-server only.
            GameMsgId::Metrics
            | GameMsgId::AudienceHint
            | GameMsgId::AudienceSocialBroadcast
            | GameMsgId::AudienceConsensusVote
            | GameMsgId::AudienceChat
            | GameMsgId::LevelStateSideChannel
            | GameMsgId::AudienceSpotlightReq => {
                let player_id = self
                    .peers
                    .get(&peer_key)
                    .map(|peer| peer.player_id)
                    .unwrap_or(0);
                tracing::debug!(
                    player_id = player_id,
                    msg_id = game_msg.msg_id.to_u8(),
                    bytes = game_msg.payload.len(),
                    "ignored client-only GameMsg"
                );
            }

            // Unknown or unimplemented message type.
            _ => {}
        }
    }
}
