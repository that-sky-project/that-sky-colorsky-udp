#![allow(dead_code)]
#![allow(unused)]

use crate::protocol::packet::Packet;
use crate::protocol::packet::net_time::{NetTimePing, NetTimePong};
use crate::utils::unix_time_seconds;

impl crate::server::state::ServerState {
    /// Handle a `NetTimePing` and answer with a `NetTimePong`.
    pub(super) fn handle_ping(&mut self, peer_key: usize, payload: &[u8]) -> Option<()> {
        let peer = self.peers.get(&peer_key)?;
        let ping = NetTimePing::from_bytes(payload)?;

        let pong = NetTimePong {
            request_id: ping.request_id,
            server_recv_time: unix_time_seconds(),
            server_send_time: unix_time_seconds(),
        };

        let packet = Packet::build_packet(
            crate::protocol::packet::PacketId::NetTimePong,
            &pong.to_bytes(),
        );
        peer.send(0, &packet, true);

        Some(())
    }
}
