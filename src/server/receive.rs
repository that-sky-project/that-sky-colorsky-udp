//! Connection lifecycle and packet dispatch.
//!
//! - `on_connect`    — New connection: allocate `PeerEntry`, register in
//!   the peers table.
//! - `on_disconnect` — Peer left: remove from peers, broadcast
//!   `PlayerLeft`, clear host record if applicable.
//! - `on_receive`    — Unified inbound path: strip connection magic,
//!   parse, route by `packet_id`.

use enet_sys::{ENetPacket, ENetPeer};

use super::peer::PeerEntry;
use super::state::ServerState;
use crate::protocol::{self, packet::Packet};
use crate::utils::{hex_preview, strip_connection_magic};

impl ServerState {
    /// Handle a new ENet connection.
    ///
    /// Creates a [`PeerEntry`] with default values (`player_id = 0`) and
    /// inserts it into the peers table.
    pub(super) fn on_connect(&mut self, peer: *mut ENetPeer) {
        if peer.is_null() {
            return;
        }

        let entry = PeerEntry::new(peer);

        self.peers.insert(peer as usize, entry);
        tracing::info!("[ENET:CONNECT] peer={:p}", peer);
    }

    /// Handle a disconnection.
    ///
    /// Removes the peer from the table, broadcasts `PlayerLeft` to
    /// remaining in-game peers, and clears the level host record if the
    /// leaving player was the host.
    pub(super) fn on_disconnect(&mut self, peer: *mut ENetPeer) {
        let Some(entry) = self.peers.remove(&(peer as usize)) else {
            return;
        };
        tracing::info!(
            "[ENET:DISCONNECT] player={} uuid={:?} level={:#x}",
            entry.player_id,
            entry.uuid,
            entry.level_id
        );
        let left = Packet::build_packet(
            protocol::packet::PacketId::PlayerLeft,
            &[entry.player_id, 0],
        );
        for e in self.peers.values() {
            e.send(0, &left, true);
        }

        if self.level_hosts.get(&entry.level_id) == Some(&entry.player_id) {
            self.level_hosts.remove(&entry.level_id);
        }
    }

    /// Unified inbound packet handler.
    ///
    /// 1. Auto-register if the peer is not yet in the table (handles
    ///    out-of-order CONNECT / RECEIVE events).
    /// 2. Strip the connection magic prefix and parse the packet.
    /// 3. Dispatch to [`handle_packet`].
    pub(super) fn on_receive(
        &mut self,
        peer: *mut ENetPeer,
        _channel: u8,
        packet: *mut ENetPacket,
    ) {
        if peer.is_null() || packet.is_null() {
            return;
        }
        let data = unsafe { std::slice::from_raw_parts((*packet).data, (*packet).dataLength) };
        if data.is_empty() {
            return;
        }

        let key = peer as usize;
        if !self.peers.contains_key(&key) {
            self.on_connect(peer);
        }

        let payload = strip_connection_magic(data, self.peers.get_mut(&key));

        let Some(packet) = Packet::from(payload) else {
            tracing::info!("[ENET:RX] unknown packet raw={}", hex_preview(data, 64));
            return;
        };

        self.handle_packet(key, packet);
    }
}
