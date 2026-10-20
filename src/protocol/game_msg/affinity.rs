//! Affinity update (`GameMsgId::Affinity`).
//!
//! Client-to-server only in the current client. The payload is a packed
//! entry count followed by `(net_player_id, value)` pairs.
//!
//! ```text
//! [count: compressed(0..=8)] [net_player_id: u8][value: u8] * count
//! ```

use crate::protocol::binary::Reader;

/// Maximum number of affinity entries carried by one message.
pub const AFFINITY_MAX_ENTRIES: u32 = 8;

/// One affinity entry.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct AffinityEntry {
    pub net_player_id: u8,
    pub value: u8,
}

/// Decoded affinity update.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Affinity {
    pub count: u8,
    pub entries: Vec<AffinityEntry>,
    /// Bytes left after the declared entries (0 when complete).
    pub trailing_bytes: usize,
}

impl Affinity {
    /// Decode an affinity payload.
    pub fn from_bytes(data: &[u8]) -> Option<Self> {
        let mut reader = Reader::new(data);
        let count = reader.read_compressed(AFFINITY_MAX_ENTRIES)? as u8;

        let mut entries = Vec::with_capacity(count as usize);
        for _ in 0..count {
            entries.push(AffinityEntry {
                net_player_id: reader.read_u8()?,
                value: reader.read_u8()?,
            });
        }

        Some(Self {
            count,
            entries,
            trailing_bytes: reader.remaining(),
        })
    }

    /// Whether the payload contained no trailing bytes.
    pub fn is_complete(&self) -> bool {
        self.trailing_bytes == 0
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn decode_entries() {
        let affinity = Affinity::from_bytes(&[0x02, 1, 10, 2, 20]).unwrap();
        assert_eq!(affinity.count, 2);
        assert_eq!(
            affinity.entries,
            vec![
                AffinityEntry {
                    net_player_id: 1,
                    value: 10
                },
                AffinityEntry {
                    net_player_id: 2,
                    value: 20
                }
            ]
        );
        assert!(affinity.is_complete());
    }

    #[test]
    fn decode_trailing_bytes() {
        let affinity = Affinity::from_bytes(&[0x01, 3, 50, 0xff]).unwrap();
        assert_eq!(affinity.count, 1);
        assert_eq!(affinity.trailing_bytes, 1);
        assert!(!affinity.is_complete());
    }

    #[test]
    fn rejects_out_of_range_count() {
        // max 8 needs 4 bits; 9 does not fit the declared range.
        assert_eq!(Affinity::from_bytes(&[0x09]), None);
    }

    #[test]
    fn rejects_truncated_entries() {
        assert_eq!(Affinity::from_bytes(&[0x02, 1, 10, 2]), None);
    }
}
