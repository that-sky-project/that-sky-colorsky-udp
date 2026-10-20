//! Level election nominee (`GameMsgId::NetLevelElectionNominee`).
//!
//! ```text
//! [level_id: u32][reason: u16]
//! ```

use crate::protocol::binary::Reader;

/// Client nomination for a level authority candidate.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct NetLevelElectionNominee {
    pub level_id: u32,
    pub reason: u16,
    /// Bytes left after the fixed header.
    pub trailing_bytes: usize,
}

impl NetLevelElectionNominee {
    /// Decode an election nominee payload.
    pub fn from_bytes(data: &[u8]) -> Option<Self> {
        let mut reader = Reader::new(data);
        Some(Self {
            level_id: reader.read_u32()?,
            reason: reader.read_u16()?,
            trailing_bytes: reader.remaining(),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn decode_nominee() {
        let bytes = [0x78, 0x56, 0x34, 0x12, 0x03, 0x00];
        assert_eq!(
            NetLevelElectionNominee::from_bytes(&bytes),
            Some(NetLevelElectionNominee {
                level_id: 0x1234_5678,
                reason: 3,
                trailing_bytes: 0,
            })
        );
    }

    #[test]
    fn rejects_short_payload() {
        assert_eq!(NetLevelElectionNominee::from_bytes(&[0, 0, 0]), None);
    }
}
