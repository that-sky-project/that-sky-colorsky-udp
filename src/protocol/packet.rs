//! Top-level packet identifiers.
//!
//! Each packet has a 1-byte [`PacketId`] followed by a direction-dependent
//! header (see the [module-level docs](super)).
//!
//! ## Wire Format
//!
//! ### Client -> Server
//!
//! ```text
//! [packet_id:u8] [seq:u8] [payload]
//! ```
//!
//! ### Server -> Client
//!
//! ```text
//! [packet_id:u8] [payload_len:u16] [payload]
//! ```
//!
//! ## Referenced Types
//!
//! - [`TgcUuid`](super::types::TgcUuid) — 16-byte player identifier
//! - `net_player_id` — `u8`, derived from [`TgcUuid`](super::types::TgcUuid)
//!   via `fnv1a_player_id`

/// Represents a parsed packet from the client or server.
pub struct Packet {
    pub packet_id: PacketId,
    pub seq: u8,
    pub payload: Vec<u8>,
}

impl Packet {
    /// Parse a raw packet into a `Packet` struct.
    pub fn from(v: &[u8]) -> Option<Self> {
        let packet_id = PacketId::from_u8(v[0])?;
        let seq = v[1];
        let payload = &v[2..];
        Some(Self {
            packet_id,
            seq,
            payload: payload.to_vec(),
        })
    }

    /// Build a complete S->C packet (packet_id + length prefix + payload).
    pub fn build_packet(packet_id: PacketId, data: &[u8]) -> Vec<u8> {
        let mut buf = Vec::new();
        buf.push(packet_id.to_u8());
        buf.extend(&(data.len() as u16).to_le_bytes());
        buf.extend(data);
        buf
    }
}

/// Top-level packet type.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub enum PacketId {
    /// C->S.
    ///
    /// Sent automatically by the client on connect. The server does not
    /// need to send this packet — the client creates it internally once
    /// the ENet connection is established.
    ClientConnect = 0,

    /// S->C.
    ///
    /// Sent by the server to disconnect a client.
    Disconnect = 1,

    /// S->C.
    ///
    /// Sent by the server to kick a client.
    Kick = 2,

    /// C->S.
    ///
    /// Sent by the client after the server receives `ClientConnect`.
    ///
    /// ## Payload
    ///
    /// ```text
    /// [player_uuid: [u8; 16]] [session: [u8; 16]] [unknown: [...]] [level_id: u32]
    /// ```
    ///
    /// The UUID is a [`TgcUuid`](super::types::TgcUuid).
    /// The session UUID is set by the HTTP server.
    JoinGame = 3,

    /// C->S.
    ///
    /// Sent by the client when changing levels.
    ///
    /// ## Payload
    ///
    /// ```text
    /// [unknown: [u8; 14]] [level_id: u32]
    /// ```
    LevelUpdate = 4,

    /// S->C (deprecated).
    ///
    /// This packet is deprecated and its function is unknown.
    EnterGameDeprecated = 5,

    /// S->C.
    ///
    /// Broadcast to existing players when a new player joins the level.
    ///
    /// ## Payload
    ///
    /// ```text
    /// [net_player_id: u8] [unk_1: u8]
    /// ```
    PlayerJoined = 6,

    /// S->C.
    ///
    /// Broadcast to existing players when a player leaves the level.
    ///
    /// ## Payload
    ///
    /// ```text
    /// [net_player_id: u8] [unk_1: u8]
    /// ```
    PlayerLeft = 7,

    /// S->C.
    ///
    /// Broadcast when a player changes level.
    ///
    /// ## Payload
    ///
    /// ```text
    /// [net_player_id: u8] [level_id: u32]
    /// ```
    PlayerChangedLevel = 8,

    /// S->C.
    ///
    /// *TODO: payload format unknown.*
    MoveGame = 9,

    /// C->S.
    ///
    /// *TODO: payload format unknown.*
    MoveResult = 10,

    /// S->C.
    ///
    /// *TODO: payload format unknown.*
    CancelMove = 11,

    /// C->S.
    ///
    /// Client requests a time sync from the server.
    ///
    /// ## Payload
    ///
    /// ```text
    /// [request_id: u16]
    /// ```
    NetTimePing = 12,

    /// S->C.
    ///
    /// Server replies to a [`PacketId::NetTimePing`] with its current timestamps.
    ///
    /// ## Payload
    ///
    /// ```text
    /// [request_id: u16] [server_recv_time: f64] [server_send_time: f64]
    /// ```
    NetTimePong = 13,

    /// Bidirectional.
    ///
    /// Wrap [`GameMsg`](super::game_msg::GameMsg) sub-messages.
    GameMsg = 14,

    /// S->C.
    ///
    /// Full player list for the room, sent when joining a level. Also
    /// sent to update the player list when a new player arrives.
    ///
    /// ## Payload
    ///
    /// ```text
    /// [player_count: compressed] [player_list: ...]
    /// ```
    ///
    /// Each entry in `player_list`:
    ///
    /// | Field | Type | Note |
    /// |-------|------|------|
    /// | `net_player_id` | `u8` | |
    /// | `player_uuid` | [`TgcUuid`](super::types::TgcUuid) | |
    /// | `<unknown>` | `[u8; 16]` | |
    /// | `level_id` | `i32` | FNV-1a hash of the level name |
    EnterGame = 17,
}

impl PacketId {
    /// Convert from the wire-format `u8` byte.
    pub fn from_u8(v: u8) -> Option<Self> {
        match v {
            0 => Some(Self::ClientConnect),
            1 => Some(Self::Disconnect),
            2 => Some(Self::Kick),
            3 => Some(Self::JoinGame),
            4 => Some(Self::LevelUpdate),
            5 => Some(Self::EnterGameDeprecated),
            6 => Some(Self::PlayerJoined),
            7 => Some(Self::PlayerLeft),
            8 => Some(Self::PlayerChangedLevel),
            9 => Some(Self::MoveGame),
            10 => Some(Self::MoveResult),
            11 => Some(Self::CancelMove),
            12 => Some(Self::NetTimePing),
            13 => Some(Self::NetTimePong),
            14 => Some(Self::GameMsg),
            17 => Some(Self::EnterGame),
            _ => None,
        }
    }

    /// Serialize this packet type to a `u8` byte.
    pub fn to_u8(self) -> u8 {
        self as u8
    }
}
