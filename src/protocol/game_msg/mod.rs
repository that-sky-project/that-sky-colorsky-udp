//! GameMsg — a sub-message carried inside [`PacketId::GameMsg`](super::packet::PacketId::GameMsg) packets.
//!
//! ## Wire Format
//!
//! ```text
//! [game_msg_type:u8] [level_seq:u8] [source_player:u8] [payload]
//! ```
//!
//! - `level_seq` — Level change sequence number, updated by Packet::LevelUpdate.
//!   GameMsg packets with a mismatched sequence number are ignored by the
//!   client.
//! - `source_player` — Player id of the original sender. Only meaningful for
//!   [`GameMsgId::NetRpc`].

/// Bidirectional Packet
///  - `msg_id` — The [`GameMsgId`] of the message.
///  - `level_seq` — The level change sequence number.
///  - `source_player` — The player id of the original sender.
///  - `payload` — The payload bytes.
pub mod level_data;

#[derive(Debug)]
pub struct GameMsg {
    pub msg_id: GameMsgId,
    pub level_seq: u8,
    pub source_player: u8,
    pub payload: Vec<u8>,
}

impl GameMsg {
    /// Construct a new GameMsg with the given fields.
    pub fn new(
        msg_id: GameMsgId,
        level_seq: u8,
        source_player: u8,
        payload: &dyn AsRef<[u8]>,
    ) -> Self {
        GameMsg {
            msg_id,
            level_seq,
            source_player,
            payload: payload.as_ref().to_vec(),
        }
    }

    /// Parse a GameMsg from raw bytes.
    ///
    /// Returns `None` if the first byte does not map to a valid
    /// [`GameMsgId`].
    pub fn from(v: &[u8]) -> Option<GameMsg> {
        GameMsgId::from_u8(v[0]).map(|msg_id| GameMsg {
            msg_id,
            level_seq: v[1],
            source_player: v[2],
            payload: v[3..].to_vec(),
        })
    }

    /// Build a serialized GameMsg from its fields.
    ///
    /// Returns a `Vec<u8>` containing the wire-format bytes.
    pub fn build_game_msg(&self) -> Vec<u8> {
        let mut buf = Vec::new();
        buf.push(self.msg_id.to_u8());
        buf.push(self.level_seq);
        buf.push(self.source_player);
        buf.extend(&self.payload);
        buf
    }
}

/// Sub-message type carried within a [`PacketId::GameMsg`](super::packet::PacketId::GameMsg) packet.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub enum GameMsgId {
    /// ## Payload
    /// ```text
    /// [net_player_id: u8] [net_rpcs]
    /// ```
    ///
    /// We still don't know how to handle NetRpc.
    /// fuck, relay it always
    NetRpc = 2,

    /// PlayerState uses [`snapshot`](crate::utils::snapshot) for compression.
    /// So each msg have a snapshot header and payload
    /// ## Header
    /// ```text
    /// [save_seq:u8] [base_seq:u8] [checksum:u8]
    /// ```
    /// ## Payload
    ///
    /// We need to package data from other players.
    ///
    /// #### C -> S
    /// ```
    /// [player_state]
    /// ```
    ///
    /// #### S->C
    ///
    /// repeat with
    /// ```text
    /// [player_id:u8][size:u32 LE][raw_state]
    /// ```
    PlayerState = 3,

    /// forward it to same level
    Critters = 4,

    ///
    Affinity = 7,

    /// NetLevelDataElect uses [`snapshot`](crate::utils::snapshot) for compression.
    ///
    /// the first msg need send by server
    ///
    /// ## Header
    /// ```text
    /// [save_seq:u8] [base_seq:u8] [checksum:u8]
    /// ```
    ///
    /// ## Payload
    /// ```text
    /// [elected_player: u8] [level_id: u32] [unknown: u16] [has_initial_data: bool] [net_level_data]
    /// ```
    NetLevelDataElect = 8,

    /// Send by client to revoke the net level data of a player.
    ///
    /// ## Header
    /// ```text
    /// [player_id: u8] [level_id: u32]
    /// ```
    NetLevelDataRevoke = 9,

    ///
    NetLevelDataRevokeAck = 10,

    ///
    NetLevelData = 11,

    ///
    NetLevelDataHeartbeat = 12,

    /// SnapshotAck is used to confirm whether the snapshot has been accepted.
    /// ## Payload
    /// ```text
    /// [player_stat_ack:u8] [level_data_ack:u8]
    /// ```
    SnapshotAck = 14,

    ///
    MusicSync = 15,

    ///
    Metrics = 16,

    /// forward it to same level
    NetLevelElectionNominee = 17,

    ///
    AudienceHint = 19,

    ///
    AudienceSocialBroadcast = 20,

    ///
    AudienceConsensusVote = 21,

    ///
    AudienceChat = 22,
    ///
    Unknown = 23,

    ///
    AudienceSpotlightReq = 24,
}

impl GameMsgId {
    /// Convert from the wire-format `u8` byte.
    pub fn from_u8(v: u8) -> Option<Self> {
        match v {
            2 => Some(Self::NetRpc),
            3 => Some(Self::PlayerState),
            4 => Some(Self::Critters),
            7 => Some(Self::Affinity),
            8 => Some(Self::NetLevelDataElect),
            9 => Some(Self::NetLevelDataRevoke),
            10 => Some(Self::NetLevelDataRevokeAck),
            11 => Some(Self::NetLevelData),
            12 => Some(Self::NetLevelDataHeartbeat),
            14 => Some(Self::SnapshotAck),
            15 => Some(Self::MusicSync),
            16 => Some(Self::Metrics),
            19 => Some(Self::AudienceHint),
            20 => Some(Self::AudienceSocialBroadcast),
            21 => Some(Self::AudienceConsensusVote),
            22 => Some(Self::AudienceChat),
            23 => Some(Self::Unknown),
            24 => Some(Self::AudienceSpotlightReq),
            _ => None,
        }
    }

    /// Serialize this message type to a `u8` byte.
    pub fn to_u8(self) -> u8 {
        self as u8
    }
}
