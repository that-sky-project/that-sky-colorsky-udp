//! NetTime sync packets.
//!
//! ```text
//! NetTimePing (C->S): [request_id: u16]
//! NetTimePong (S->C): [request_id: u16][server_recv_time: f64][server_send_time: f64]
//! ```

use crate::protocol::binary::Reader;

/// Client request for a server time sample.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct NetTimePing {
    pub request_id: u16,
}

impl NetTimePing {
    /// Decode a ping payload.
    pub fn from_bytes(data: &[u8]) -> Option<Self> {
        let mut reader = Reader::new(data);
        Some(Self {
            request_id: reader.read_u16()?,
        })
    }

    /// Encode a ping payload.
    pub fn to_bytes(self) -> Vec<u8> {
        self.request_id.to_le_bytes().to_vec()
    }
}

/// Server reply carrying receive/send timestamps (Unix seconds).
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct NetTimePong {
    pub request_id: u16,
    pub server_recv_time: f64,
    pub server_send_time: f64,
}

impl NetTimePong {
    /// Decode a pong payload.
    pub fn from_bytes(data: &[u8]) -> Option<Self> {
        let mut reader = Reader::new(data);
        Some(Self {
            request_id: reader.read_u16()?,
            server_recv_time: reader.read_f64()?,
            server_send_time: reader.read_f64()?,
        })
    }

    /// Encode a pong payload.
    pub fn to_bytes(self) -> Vec<u8> {
        let mut data = Vec::with_capacity(18);
        data.extend(self.request_id.to_le_bytes());
        data.extend(self.server_recv_time.to_le_bytes());
        data.extend(self.server_send_time.to_le_bytes());
        data
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ping_roundtrip() {
        let ping = NetTimePing { request_id: 0x1234 };
        assert_eq!(NetTimePing::from_bytes(&ping.to_bytes()), Some(ping));
        assert_eq!(NetTimePing::from_bytes(&[0x12]), None);
    }

    #[test]
    fn pong_roundtrip() {
        let pong = NetTimePong {
            request_id: 7,
            server_recv_time: 1_700_000_000.25,
            server_send_time: 1_700_000_000.5,
        };
        let bytes = pong.to_bytes();
        assert_eq!(bytes.len(), 18);
        assert_eq!(NetTimePong::from_bytes(&bytes), Some(pong));
    }
}
