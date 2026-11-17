//! Per-connection data structure.
//!
//! [`PeerEntry`] holds everything the server knows about one ENet
//! connection — both the raw peer pointer for sending and the full
//! runtime state.

use std::time::{Instant, SystemTime, UNIX_EPOCH};

use enet_sys::ENetPeer;

use crate::protocol::types::TgcUuid;
use crate::utils::snapshot::{SnapshotReader, SnapshotWriter};

/// Lifecycle state of one ENet connection.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum PeerState {
    /// Connected, but the client has not joined yet.
    #[default]
    Pending,
    /// Joined and receiving room traffic.
    Active,
    /// MoveGame sent; only MoveResult/CancelMove are accepted.
    Moving,
    /// Kick or disconnect initiated.
    Closed,
}

/// Player State
pub struct PlayerState {
    pub raw_state: Vec<u8>,
    pub snap_reader: SnapshotReader,
    pub snap_writer: SnapshotWriter,
    pub snapshot_ack: Option<u8>,
    /// Latest position decoded from the raw PlayerState header.
    pub position: Option<[f32; 3]>,
    /// Approximate speed derived from consecutive positions.
    pub velocity: Option<f32>,
    /// Unix milliseconds of the last successfully decoded PlayerState.
    pub state_at_ms: u64,
    /// Previous position and observation time, used to derive velocity.
    pub(crate) previous_position: Option<([f32; 3], Instant)>,
}

impl PlayerState {
    fn new() -> Self {
        Self {
            raw_state: Vec::new(),
            snap_reader: SnapshotReader::new(),
            snap_writer: SnapshotWriter::with_label("player_state"),
            snapshot_ack: None,
            position: None,
            velocity: None,
            state_at_ms: 0,
            previous_position: None,
        }
    }
}

/// Level Data
pub struct LevelDelta {
    pub raw_state: Vec<u8>,
    pub snap_reader: SnapshotReader,
    pub snap_writer: SnapshotWriter,
    pub snapshot_ack: Option<u8>,
}

impl LevelDelta {
    fn new() -> Self {
        Self {
            raw_state: Vec::new(),
            snap_reader: SnapshotReader::new(),
            snap_writer: SnapshotWriter::with_label("level_data"),
            snapshot_ack: None,
        }
    }
}

/// An affinity target frozen at receive time, including its connection identity.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AffinityBond {
    pub uuid: TgcUuid,
    pub session_id: String,
    pub value: u8,
}

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
    /// Client NetVersion (six u16 values).
    pub net_version: [u16; 6],
    /// Connection lifecycle state.
    pub state: PeerState,
    /// Instant when the connection was established.
    pub connected_at: Instant,
    /// Unix milliseconds of the connection's registration (not authority election).
    pub joined_at: u64,
    /// Connection token bound by the first `JoinGame` packet. Every later
    /// client packet must carry the same token.
    pub conn_magic: Option<u32>,
    /// Server-generated connection identity; independent of reusable client tokens.
    pub control_session_id: String,
    /// Unix milliseconds of the last packet received from this client.
    pub last_seen_at_ms: u64,
    /// Receive-time UUID + session targets. A snapshot resolves only these exact
    /// connections; native id or UUID reuse cannot resurrect an obsolete bond.
    pub affinity: Vec<AffinityBond>,
    /// Player State
    pub player_delta: PlayerState,
    /// Level Data
    pub level_delta: LevelDelta,
}

/// Current Unix time in milliseconds.
pub(crate) fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_millis() as u64)
        .unwrap_or(0)
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
            net_version: [0; 6],
            state: PeerState::Pending,
            connected_at: Instant::now(),
            joined_at: now_ms(),
            conn_magic: None,
            control_session_id: format!("{:032x}", rand::random::<u128>()),
            last_seen_at_ms: now_ms(),
            affinity: Vec::new(),
            player_delta: PlayerState::new(),
            level_delta: LevelDelta::new(),
        }
    }

    /// Send data to this peer over ENet.
    ///
    /// `reliable` controls whether the packet is sent with the
    /// `ENET_PACKET_FLAG_RELIABLE` flag. Returns the ENet result code.
    #[allow(clippy::unnecessary_cast)] // enet-sys binds this flag as i32 on Windows and u32 elsewhere
    pub fn send(&self, channel: u8, data: &[u8], reliable: bool) -> i32 {
        unsafe {
            let flags = if reliable {
                enet_sys::_ENetPacketFlag_ENET_PACKET_FLAG_RELIABLE as u32
            } else {
                0
            };
            let packet = enet_sys::enet_packet_create(data.as_ptr().cast(), data.len(), flags);
            enet_sys::enet_peer_send(self.peer, channel, packet)
        }
    }

    /// Request a graceful ENet disconnect after queued packets are sent.
    pub fn disconnect_later(&self, data: u32) {
        unsafe {
            enet_sys::enet_peer_disconnect_later(self.peer, data);
        }
    }

    /// Whether the peer is joined and receiving room traffic.
    pub fn is_active(&self) -> bool {
        self.state == PeerState::Active
    }

    /// Whether the peer currently occupies a player slot.
    pub fn is_player(&self) -> bool {
        matches!(self.state, PeerState::Active | PeerState::Moving)
    }

    /// Whether the peer still holds its player id until ENet disconnects.
    pub fn occupies_slot(&self) -> bool {
        matches!(
            self.state,
            PeerState::Active | PeerState::Moving | PeerState::Closed
        )
    }
}

#[cfg(test)]
mod connection_identity_tests {
    use super::PeerEntry;

    #[test]
    fn client_token_reuse_never_reuses_control_session_identity() {
        let mut old = PeerEntry::new(std::ptr::null_mut());
        let mut replacement = PeerEntry::new(std::ptr::null_mut());
        old.conn_magic = Some(7);
        replacement.conn_magic = Some(7);
        assert_ne!(old.control_session_id, replacement.control_session_id);
        assert_eq!(old.conn_magic, replacement.conn_magic);
        assert!(!replacement.control_session_id.is_empty());
    }
}
