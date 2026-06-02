//! Per-connection data structure.
//!
//! [`PeerEntry`] holds everything the server knows about one ENet
//! connection — both the raw peer pointer for sending and the full
//! runtime state.

use std::time::Instant;

use enet_sys::ENetPeer;

use crate::protocol::types::TgcUuid;
use crate::utils::snapshot::{SnapshotReader, SnapshotWriter};

/// Full state the server maintains for each ENet connection.
pub struct PeerEntry {
    /// Raw ENet peer pointer, used to send data to this peer.
    pub peer: *mut ENetPeer,
    /// Player ID (1..=255).
    pub player_id: u8,
    /// Player UUID, provided by the client at login.
    pub uuid: TgcUuid,
    /// Current level the player is in.
    pub level_id: u32,
    /// Level sequence number, updated by LevelUpdate
    pub lv_seq: u8,
    /// Instant when the connection was established.
    pub connected_at: Instant,
    /// Connection magic prefix (4 bytes) used to identify packets from
    /// different client sessions.
    pub conn_magic: [u8; 4],
    /// Raw player state
    pub player_state_raw: Vec<u8>,
    /// Client->Server
    /// SnapshotReader
    pub snap_reader: SnapshotReader,
    /// Server->Client
    /// SnapshotWriter
    pub snap_writer: SnapshotWriter,
}

impl PeerEntry {
    /// Create a blank entry with safe defaults.
    pub fn new(peer: *mut ENetPeer) -> Self {
        Self {
            peer,
            player_id: 0,
            uuid: TgcUuid::default(),
            level_id: 0,
            lv_seq: 0,
            connected_at: Instant::now(),
            conn_magic: [0; 4],
            player_state_raw: Vec::new(),
            snap_reader: SnapshotReader::new(),
            snap_writer: SnapshotWriter::new(),
        }
    }

    /// Send data to this peer over ENet.
    ///
    /// `reliable` controls whether the packet is sent with the
    /// `ENET_PACKET_FLAG_RELIABLE` flag.
    pub fn send(&self, channel: u8, data: &[u8], reliable: bool) {
        unsafe {
            let flags = if reliable {
                enet_sys::_ENetPacketFlag_ENET_PACKET_FLAG_RELIABLE
            } else {
                0
            };
            let packet = enet_sys::enet_packet_create(data.as_ptr().cast(), data.len(), flags);
            enet_sys::enet_peer_send(self.peer, channel, packet);
        }
    }
}
