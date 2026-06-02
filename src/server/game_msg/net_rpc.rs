#![allow(dead_code)]
#![allow(unused)]

use crate::protocol::{
    self,
    game_msg::{GameMsg, GameMsgId},
    packet::Packet,
};

impl crate::server::state::ServerState {
    /// Handles net rpc
    pub(super) fn handle_net_rpc(&mut self, peer_key: usize, msg: GameMsg) -> Option<()> {
        // send to all
        self.relay_game_msg(GameMsgId::NetRpc, &msg.payload, peer_key);

        Some(())
    }
}
