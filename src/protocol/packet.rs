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
//! [token:u32] [packet_id:u8] [seq:u8] [payload]
//! ```
//!
//! The 4-byte token is bound to the session by the first `JoinGame` packet
//! and must match on every subsequent packet (same contract as XYSKY).
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

pub mod cancel_move;
pub mod enter_game;
pub mod join_game;
pub mod kick;
pub mod move_game;
pub mod move_result;
pub mod net_time;

/// Represents a parsed packet from the client or server.
#[derive(Debug)]
pub struct Packet {
    pub packet_id: PacketId,
    pub seq: u8,
    pub payload: Vec<u8>,
}

impl Packet {
    /// Parse an unprefixed packet (`packet_id + seq + payload`).
    ///
    /// Client traffic should use [`ClientPacket::from`], which also consumes
    /// the 4-byte connection token.
    pub fn from(v: &[u8]) -> Option<Self> {
        let (&packet_id, rest) = v.split_first()?;
        let packet_id = PacketId::from_u8(packet_id)?;
        let (&seq, payload) = rest.split_first()?;
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

/// A client packet prefixed with the 4-byte connection token.
///
/// ```text
/// [token:u32 LE] [packet_id:u8] [seq:u8] [payload]
/// ```
#[derive(Debug)]
pub struct ClientPacket {
    pub token: u32,
    pub packet: Packet,
}

impl ClientPacket {
    /// Parse a client packet, including its connection token.
    pub fn from(v: &[u8]) -> Option<Self> {
        if v.len() < 6 {
            return None;
        }

        let token = u32::from_le_bytes(v[..4].try_into().ok()?);
        let packet = Packet::from(&v[4..])?;
        Some(Self { token, packet })
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
    /// ## Payload
    ///
    /// ```text
    /// [unk_1: [u8; 32]] [move: Move] [unk_2: i32] [unk_3: u8] [unk_4: i32] [unk_5: bool] [uuid: player_uuid]
    /// ```
    /// The Move is a [`Move`](move_game::Move).
    /// The UUID is a [`TgcUuid`](super::types::TgcUuid).
    MoveGame = 9,

    /// C->S.
    ///
    /// Result of a `MoveGame` request.
    ///
    /// ## Payload
    ///
    /// ```text
    /// [result: u8] [payload: ...]
    /// ```
    MoveResult = 10,

    /// S->C.
    ///
    /// Cancels a pending move and returns the client to its current room.
    ///
    /// ## Payload
    ///
    /// ```text
    /// [reason: u8] [payload: ...]
    /// ```
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn known_ids_roundtrip() {
        for id in 0u8..=14 {
            let packet = PacketId::from_u8(id).expect("known packet id");
            assert_eq!(packet.to_u8(), id);
        }
        assert_eq!(PacketId::from_u8(17).unwrap().to_u8(), 17);
        assert_eq!(PacketId::from_u8(15), None);
        assert_eq!(PacketId::from_u8(16), None);
        assert_eq!(PacketId::from_u8(18), None);
    }

    #[test]
    fn parse_unprefixed_packet() {
        let packet = Packet::from(&[3, 9, 0xaa, 0xbb]).unwrap();
        assert_eq!(packet.packet_id, PacketId::JoinGame);
        assert_eq!(packet.seq, 9);
        assert_eq!(packet.payload, vec![0xaa, 0xbb]);

        assert!(Packet::from(&[]).is_none());
        assert!(Packet::from(&[3]).is_none());
        assert!(Packet::from(&[15, 0]).is_none());
    }

    #[test]
    fn parse_client_packet_with_token() {
        let mut raw = 0x1234_5678u32.to_le_bytes().to_vec();
        raw.extend([3, 9, 0xaa, 0xbb]);

        let packet = ClientPacket::from(&raw).unwrap();
        assert_eq!(packet.token, 0x1234_5678);
        assert_eq!(packet.packet.packet_id, PacketId::JoinGame);
        assert_eq!(packet.packet.seq, 9);
        assert_eq!(packet.packet.payload, vec![0xaa, 0xbb]);

        assert!(ClientPacket::from(&raw[..5]).is_none());
    }
}
