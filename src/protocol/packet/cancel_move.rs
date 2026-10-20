//! CancelMove packet.
//!
//! ```text
//! [reason: u8][payload...]
//! ```

/// Server-to-client move cancellation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CancelMove {
    pub reason: u8,
    pub payload: Vec<u8>,
}

impl CancelMove {
    /// Decode a cancel payload.
    pub fn from_bytes(data: &[u8]) -> Option<Self> {
        let (&reason, payload) = data.split_first()?;
        Some(Self {
            reason,
            payload: payload.to_vec(),
        })
    }

    /// Encode a cancel payload.
    pub fn to_bytes(&self) -> Vec<u8> {
        let mut data = Vec::with_capacity(1 + self.payload.len());
        data.push(self.reason);
        data.extend_from_slice(&self.payload);
        data
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn roundtrip() {
        let packet = CancelMove {
            reason: 2,
            payload: vec![1, 2, 3],
        };
        let bytes = packet.to_bytes();
        assert_eq!(bytes, vec![2, 1, 2, 3]);
        assert_eq!(CancelMove::from_bytes(&bytes), Some(packet));
        assert_eq!(CancelMove::from_bytes(&[]), None);
    }
}
