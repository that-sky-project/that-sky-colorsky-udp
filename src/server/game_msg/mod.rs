#![allow(dead_code)]
#![allow(unused)]

use crate::protocol::{self, game_msg::GameMsgId};

mod level;
mod net_rpc;
mod player_state;
mod snapshot_ack;
mod utils;

impl crate::server::state::ServerState {
    /// Handles incoming game messages from a peer.
    pub(super) fn handle_game_msg(
        &mut self,
        peer_key: usize,
        game_msg: protocol::game_msg::GameMsg,
    ) {
        match game_msg.msg_id {
            // NetRpc (now we just forward it to all players)
            GameMsgId::NetRpc => {
                self.handle_net_rpc(peer_key, game_msg);
            }

            // PlayerState need use snapshot
            GameMsgId::PlayerState => {
                self.handle_player_state(peer_key, game_msg);
            }

            // SnapshotAck, really ack?
            GameMsgId::SnapshotAck => {
                self.handle_snapshot_ack(peer_key, game_msg);
            }

            // try handle level data
            GameMsgId::NetLevelDataElect => {
                self.handle_level_elect(peer_key, game_msg);
            }

            // fuck u I don't known how to process this msg
            _ => {}
        }
    }
}
