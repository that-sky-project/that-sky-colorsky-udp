use crate::protocol::{self, packet::PacketId};

mod join;
mod level_update;
mod ping;
mod utils;

impl crate::server::state::ServerState {
    /// Handles incoming packets from a peer.
    pub(super) fn handle_packet(&mut self, peer_key: usize, packet: protocol::packet::Packet) {
        // fuck, log the packet id
        // tracing::info!("[ENET:RX] packet={:?}", packet.packet_id as u8);

        match packet.packet_id {
            // ClientConnect is a empty packet. So we ignore it.
            PacketId::ClientConnect => {}

            // JoinGame contains player's uuid, session and level_id.
            PacketId::JoinGame => {
                self.handle_join_game(peer_key, &packet.payload);
            }

            // LevelUpdate
            PacketId::LevelUpdate => {
                self.handle_level_update(peer_key, &packet.payload);
            }

            // GameMsg need handle by the game_msg module.
            PacketId::GameMsg => {
                if let Some(game_msg) = protocol::game_msg::GameMsg::from(&packet.payload) {
                    self.handle_game_msg(peer_key, game_msg);
                }
            }
            _ => {
                // fuck, why we need print raw hex? fuck
                // tracing::info!(
                //     "[ENET:RX] packet raw={}",
                //     hex_preview(payload, payload.len())
                // );
            }
        }
    }
}
