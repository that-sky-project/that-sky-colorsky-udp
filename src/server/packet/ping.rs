#![allow(dead_code)]
#![allow(unused)]
use crate::protocol;
use crate::protocol::packet::Packet;
use crate::utils::{hex_preview, unix_time_seconds};

impl crate::server::state::ServerState {
    pub(super) fn handle_ping(&mut self, peer_key: usize, payload: &[u8]) -> Option<()> {
        let peer = self.peers.get(&peer_key)?;

        // ping pong boom

        let mut payload = Vec::from_iter(payload.to_vec());

        payload.extend(unix_time_seconds().to_le_bytes());
        payload.extend(unix_time_seconds().to_le_bytes());

        let pong = Packet::build_packet(protocol::packet::PacketId::NetTimePong, &payload);

        peer.send(0, &pong, true);

        Some(())
    }
}
