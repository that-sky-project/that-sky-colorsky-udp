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
pub mod affinity;
pub mod heartbeat;
pub mod level_data;
pub mod nominee;

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

    /// ## Payload
    /// ```text
    /// [count: compressed(0..=8)] [net_player_id: u8][value: u8] * count
    /// ```
    ///
    /// Client-to-server only in the current client. The server consumes the
    /// update and does not relay it.
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

    /// Client reply carrying the final level snapshot after a revoke request.
    ///
    /// ## Payload
    /// ```text
    /// [save_seq:u8] [base_seq:u8] [checksum:u8] [snapshot...]
    /// ```
    NetLevelDataRevokeAck = 10,

    /// NetLevelData uses [`snapshot`](crate::utils::snapshot) for compression.
    /// So each msg have a snapshot header and payload
    /// ## Header
    /// ```text
    /// [save_seq:u8] [base_seq:u8] [checksum:u8]
    /// ```
    ///
    /// LevelData only can send by authority of the level
    /// ## Payload
    ///
    /// ```text
    /// [level_data: LevelData]
    /// ```
    /// The level_data is [LevelData](level_data::LevelData)
    NetLevelData = 11,

    /// Send by Client
    ///
    /// ## Payload
    ///
    /// ```text
    /// [ready: bool] [net_player_id: u8] [level_id: u32] [authority_version: u16]
    /// ```
    NetLevelDataHeartbeat = 12,

    /// SnapshotAck is used to confirm whether the snapshot has been accepted.
    /// ## Payload
    /// ```text
    /// [player_stat_ack:u8] [level_data_ack:u8]
    /// ```
    SnapshotAck = 14,

    /// Client music sync. Forwarded to the same level.
    MusicSync = 15,

    /// Client telemetry. Consumed and never relayed.
    Metrics = 16,

    /// Client nomination for a level authority candidate.
    ///
    /// ## Payload
    ///
    /// ```text
    /// [level_id: u32] [reason: u16]
    /// ```
    NetLevelElectionNominee = 17,

    /// Client-only audience hint. Consumed and never relayed.
    AudienceHint = 19,

    /// Client-only audience social broadcast. Consumed and never relayed.
    AudienceSocialBroadcast = 20,

    /// Client-only audience consensus vote. Consumed and never relayed.
    AudienceConsensusVote = 21,

    /// Client-only audience chat. Consumed and never relayed.
    AudienceChat = 22,

    /// Client-only level side channel. Consumed and never relayed.
    LevelStateSideChannel = 23,

    /// Client-only audience spotlight request. Consumed and never relayed.
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
            17 => Some(Self::NetLevelElectionNominee),
            19 => Some(Self::AudienceHint),
            20 => Some(Self::AudienceSocialBroadcast),
            21 => Some(Self::AudienceConsensusVote),
            22 => Some(Self::AudienceChat),
            23 => Some(Self::LevelStateSideChannel),
            24 => Some(Self::AudienceSpotlightReq),
            _ => None,
        }
    }

    /// Serialize this message type to a `u8` byte.
    pub fn to_u8(self) -> u8 {
        self as u8
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn known_ids_roundtrip() {
        for id in [
            2u8, 3, 4, 7, 8, 9, 10, 11, 12, 14, 15, 16, 17, 19, 20, 21, 22, 23, 24,
        ] {
            let msg = GameMsgId::from_u8(id)
                .unwrap_or_else(|| panic!("GameMsgId {id} should be recognized"));
            assert_eq!(msg.to_u8(), id);
        }
    }

    #[test]
    fn unknown_ids_are_rejected() {
        assert_eq!(GameMsgId::from_u8(5), None);
        assert_eq!(GameMsgId::from_u8(18), None);
        assert_eq!(GameMsgId::from_u8(25), None);
    }
}
