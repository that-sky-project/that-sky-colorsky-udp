//! Kick packet (S->C).
//!
//! ```text
//! [reason_length: u32][reason: utf8 bytes, max 256]
//! ```

use crate::protocol::binary::Reader;

/// Server-to-client kick reason.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Kick {
    pub reason: String,
}

impl Kick {
    /// Encode a kick payload.
    pub fn to_bytes(&self) -> Vec<u8> {
        let reason = self.reason.as_bytes();
        let length = reason.len().min(256);
        let mut data = Vec::with_capacity(4 + length);
        data.extend_from_slice(&(length as u32).to_le_bytes());
        data.extend_from_slice(&reason[..length]);
        data
    }

    /// Decode a kick payload.
    pub fn from_bytes(data: &[u8]) -> Option<Self> {
        let mut reader = Reader::new(data);
        let length = reader.read_u32()? as usize;
        if length > 256 {
            return None;
        }
        let reason = String::from_utf8(reader.read_bytes(length)?.to_vec()).ok()?;
        Some(Self { reason })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn roundtrip() {
        let packet = Kick {
            reason: "moved".to_string(),
        };
        assert_eq!(Kick::from_bytes(&packet.to_bytes()), Some(packet));
    }
}
