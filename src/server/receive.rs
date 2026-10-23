//! Connection lifecycle and packet dispatch.
//!
//! - `on_connect`    — New connection: allocate `PeerEntry`, register in
//!   the peers table.
//! - `on_disconnect` — Peer left: remove from peers, broadcast
//!   `PlayerLeft`, clear host record if applicable.
//! - `on_receive`    — Unified inbound path: parse the client token,
//!   validate it against the session, then route by `packet_id`.

use enet_sys::{ENetPacket, ENetPeer};

use super::peer::{PeerEntry, PeerState};
use super::state::ServerState;
use crate::protocol::packet::{ClientPacket, Packet, PacketId};
use crate::utils::hex_preview;

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
    pub(super) fn on_disconnect(&mut self, peer: *mut ENetPeer, reason: u32) {
        let Some(entry) = self.peers.remove(&(peer as usize)) else {
            return;
        };
        tracing::info!(
            "[ENET:DISCONNECT] player={} uuid={:?} level={:#x} reason={}",
            entry.player_id,
            entry.uuid,
            entry.level_id,
            reason
        );

        if entry.player_id != 0 {
            self.forget_player_moves(entry.player_id);

            let left = Packet::build_packet(PacketId::PlayerLeft, &[entry.player_id, 0]);
            for remaining in self.peers.values().filter(|peer| peer.is_active()) {
                remaining.send(0, &left, true);
            }

            self.release_authority(entry.player_id, entry.level_id);

            if entry.state != PeerState::Closed {
                self.emit_event(crate::control::ServerEvent::PlayerLeft {
                    player_id: entry.player_id,
                    uuid: entry.uuid,
                    level_id: entry.level_id,
                });
            }
        }
    }

    /// Unified inbound packet handler.
    ///
    /// 1. Auto-register if the peer is not yet in the table (handles
    ///    out-of-order CONNECT / RECEIVE events).
    /// 2. Parse the 4-byte connection token plus packet header.
    /// 3. Only `JoinGame` may establish the token; later packets must match.
    /// 4. Dispatch to [`handle_packet`].
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

        let Some(client_packet) = ClientPacket::from(data) else {
            tracing::info!(
                "[ENET:RX] malformed client packet raw={}",
                hex_preview(data, 64)
            );
            return;
        };

        let Some(entry) = self.peers.get_mut(&key) else {
            return;
        };
        entry.last_seen_at_ms = super::peer::now_ms();

        match entry.conn_magic {
            // The first packet may only be JoinGame; its token becomes the
            // session token. This mirrors XYSKY's connMagic handling.
            None if client_packet.packet.packet_id != PacketId::JoinGame => {
                tracing::warn!(
                    player_id = entry.player_id,
                    packet_id = client_packet.packet.packet_id.to_u8(),
                    "rejected client packet before JoinGame"
                );
                return;
            }
            None => {
                entry.conn_magic = Some(client_packet.token);
            }
            Some(expected) if expected != client_packet.token => {
                tracing::warn!(
                    player_id = entry.player_id,
                    expected = expected,
                    received = client_packet.token,
                    "rejected client packet with mismatched connection token"
                );
                return;
            }
            Some(_) => {}
        }

        self.handle_packet(key, client_packet.packet);
    }
}
