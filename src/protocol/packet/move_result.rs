//! MoveResult packet (C->S).
//!
//! ```text
//! [result: u8][payload...]
//! ```

/// Result of a cross-room move initiated by `MoveGame`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MoveResult {
    pub result: u8,
    pub payload: Vec<u8>,
}

impl MoveResult {
    /// Decode a move result payload.
    pub fn from_bytes(data: &[u8]) -> Option<Self> {
        let (&result, payload) = data.split_first()?;
        Some(Self {
            result,
            payload: payload.to_vec(),
        })
    }

    /// Encode a move result payload.
    pub fn to_bytes(&self) -> Vec<u8> {
        let mut data = Vec::with_capacity(1 + self.payload.len());
        data.push(self.result);
        data.extend_from_slice(&self.payload);
        data
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn decode_result_and_trailing_bytes() {
        let result = MoveResult::from_bytes(&[1, 0xaa, 0xbb]).unwrap();
        assert_eq!(result.result, 1);
        assert_eq!(result.payload, vec![0xaa, 0xbb]);
        assert_eq!(result.to_bytes(), vec![1, 0xaa, 0xbb]);
        assert_eq!(MoveResult::from_bytes(&[]), None);
    }
}
