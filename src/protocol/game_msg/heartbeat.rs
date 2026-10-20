//! Level authority heartbeat (`GameMsgId::NetLevelDataHeartbeat`).
//!
//! ```text
//! [ready: bool][net_player_id: u8][level_id: u32][authority_version: u16]
//! ```

use crate::protocol::binary::Reader;

/// Client heartbeat for the current level authority.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct NetLevelDataHeartbeat {
    pub ready: bool,
    pub net_player_id: u8,
    pub level_id: u32,
    pub authority_version: u16,
    /// Bytes left after the fixed header.
    pub trailing_bytes: usize,
}

impl NetLevelDataHeartbeat {
    /// Decode a heartbeat payload.
    pub fn from_bytes(data: &[u8]) -> Option<Self> {
        let mut reader = Reader::new(data);
        Some(Self {
            ready: reader.read_bool()?,
            net_player_id: reader.read_u8()?,
            level_id: reader.read_u32()?,
            authority_version: reader.read_u16()?,
            trailing_bytes: reader.remaining(),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn decode_heartbeat() {
        // bool(1) + padding, player id, level id, authority version.
        let bytes = [0x01, 7, 0x78, 0x56, 0x34, 0x12, 0x02, 0x00];
        let heartbeat = NetLevelDataHeartbeat::from_bytes(&bytes).unwrap();
        assert_eq!(
            heartbeat,
            NetLevelDataHeartbeat {
                ready: true,
                net_player_id: 7,
                level_id: 0x1234_5678,
                authority_version: 2,
                trailing_bytes: 0,
            }
        );
    }

    #[test]
    fn rejects_truncated_heartbeat() {
        assert_eq!(NetLevelDataHeartbeat::from_bytes(&[0x01, 7, 0, 0]), None);
    }
}
