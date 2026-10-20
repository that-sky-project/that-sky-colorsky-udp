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
    /// Handles net rpc
    pub(super) fn handle_net_rpc(&mut self, peer_key: usize, mut msg: GameMsg) -> Option<()> {
        let sender_uuid = self.peers.get(&peer_key)?.uuid.raw();

        if msg.payload.len() > 1 {
            let netrpc = &mut msg.payload[1..];
            if netrpc.len() >= 34
                && netrpc.starts_with(&[0xb3, 0xb9, 0xd9, 0x20, 0x00, 0x24, 0x00])
                && netrpc[9] == 1
            {
                netrpc[18..34].copy_from_slice(&sender_uuid);
            }
        }

        tracing::debug!("[NET_RPC] hex={}", hex_preview(&msg.payload, 64));
        self.relay_game_msg(GameMsgId::NetRpc, &msg.payload, peer_key)
    }
}
