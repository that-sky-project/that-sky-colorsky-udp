//! JoinGame packet (C->S).
//!
//! The client packet header (`token + id + seq`) is stripped before this
//! payload is parsed. The layout follows the XYSKY `JoinGamePacket` decoder.

use crate::protocol::binary::{Reader, Writer};
use crate::protocol::net_address::NetAddress;
use crate::protocol::packet::move_game::Move;
use crate::protocol::types::TgcUuid;

/// Maximum bytes accepted for the JoinGame level-data blob.
pub const MAX_JOIN_LEVEL_DATA_BYTES: usize = 0x2000;

/// Decoded `JoinGame` payload.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct JoinGame {
    pub uuid: TgcUuid,
    pub session_uuid: TgcUuid,
    pub session_token: [u8; 32],
    pub transfer: Move,
    pub join_token: u32,
    pub join_state: u8,
    pub join_flags: u32,
    pub join_signature: [u8; 16],
    pub server_version: u16,
    pub client_version: u16,
    pub rpc_hash: u32,
    pub net_version: [u16; 6],
    pub level_change_count: u8,
    pub level_id: u32,
    pub level_summary: [u8; 64],
    pub local_player_id: u32,
    pub current_player_id: u32,
    pub level_data: Vec<u8>,
    pub moving: bool,
    pub has_transfer_address: bool,
    pub merge_state: u8,
    pub transfer_address: NetAddress,
    pub move_result: u32,
    /// Bytes left after the documented payload.
    pub trailing_bytes: usize,
}

impl JoinGame {
    /// Decode a `JoinGame` payload.
    pub fn from_bytes(data: &[u8]) -> Option<Self> {
        let mut reader = Reader::new(data);

        let uuid = read_uuid(&mut reader)?;
        let session_uuid = read_uuid(&mut reader)?;
        let session_token = read_array::<32>(&mut reader)?;

        let transfer = Move::from_reader(&mut reader)?;

        let join_token = reader.read_u32()?;
        let join_state = reader.read_u8()?;
        let join_flags = reader.read_u32()?;
        let join_signature = read_array::<16>(&mut reader)?;
        let server_version = reader.read_u16()?;
        let client_version = reader.read_u16()?;
        let rpc_hash = reader.read_u32()?;

        let mut net_version = [0u16; 6];
        for value in &mut net_version {
            *value = reader.read_u16()?;
        }

        let level_change_count = reader.read_u8()?;
        let level_id = reader.read_u32()?;
        let level_summary = read_array::<64>(&mut reader)?;
        let local_player_id = reader.read_u32()?;
        let current_player_id = reader.read_u32()?;

        let level_data_length = reader.read_u32()? as usize;
        if level_data_length > MAX_JOIN_LEVEL_DATA_BYTES {
            return None;
        }
        let level_data = reader.read_bytes(level_data_length)?.to_vec();

        let moving = reader.read_bool()?;
        let has_transfer_address = reader.read_bool()?;
        let merge_state = reader.read_compressed(0xff)? as u8;
        let transfer_address = NetAddress::read_typed(&mut reader)?;
        let move_result = reader.read_u32()?;

        Some(Self {
            uuid,
            session_uuid,
            session_token,
            transfer,
            join_token,
            join_state,
            join_flags,
            join_signature,
            server_version,
            client_version,
            rpc_hash,
            net_version,
            level_change_count,
            level_id,
            level_summary,
            local_player_id,
            current_player_id,
            level_data,
            moving,
            has_transfer_address,
            merge_state,
            transfer_address,
            move_result,
            trailing_bytes: reader.remaining(),
        })
    }

    /// Encode a `JoinGame` payload.
    pub fn to_bytes(&self) -> Option<Vec<u8>> {
        if self.level_data.len() > MAX_JOIN_LEVEL_DATA_BYTES {
            return None;
        }

        let mut writer = Writer::new();
        writer.write_bytes(&self.uuid.raw());
        writer.write_bytes(&self.session_uuid.raw());
        writer.write_bytes(&self.session_token);
        self.transfer.write_to(&mut writer)?;
        writer.write_u32(self.join_token);
        writer.write_u8(self.join_state);
        writer.write_u32(self.join_flags);
        writer.write_bytes(&self.join_signature);
        writer.write_u16(self.server_version);
        writer.write_u16(self.client_version);
        writer.write_u32(self.rpc_hash);
        for value in self.net_version {
            writer.write_u16(value);
        }
        writer.write_u8(self.level_change_count);
        writer.write_u32(self.level_id);
        writer.write_bytes(&self.level_summary);
        writer.write_u32(self.local_player_id);
        writer.write_u32(self.current_player_id);
        writer.write_u32(self.level_data.len() as u32);
        writer.write_bytes(&self.level_data);
        writer.write_bool(self.moving);
        writer.write_bool(self.has_transfer_address);
        writer.write_compressed(self.merge_state as u32, 0xff)?;
        self.transfer_address.write_typed(&mut writer)?;
        writer.write_u32(self.move_result);
        Some(writer.into_bytes())
    }
}

fn read_uuid(reader: &mut Reader<'_>) -> Option<TgcUuid> {
    TgcUuid::from(&reader.read_bytes(16)?)
}

fn read_array<const N: usize>(reader: &mut Reader<'_>) -> Option<[u8; N]> {
    let bytes = reader.read_bytes(N)?;
    let mut value = [0u8; N];
    value.copy_from_slice(bytes);
    Some(value)
}

#[cfg(test)]
mod tests {
    use std::net::{IpAddr, Ipv4Addr};

    use super::*;

    fn uuid(byte: u8) -> TgcUuid {
        TgcUuid::from(&[byte; 16]).unwrap()
    }

    fn sample(moving: bool) -> JoinGame {
        JoinGame {
            uuid: uuid(1),
            session_uuid: uuid(2),
            session_token: [0x33; 32],
            transfer: Move {
                address: NetAddress::new(IpAddr::V4(Ipv4Addr::new(192, 168, 1, 4)), 19133),
                move_nonce: 0xfeed_face,
                split_type: 0,
                players: vec![uuid(1), uuid(3)],
            },
            join_token: 0x1234_5678,
            join_state: 2,
            join_flags: 0x90ab_cdef,
            join_signature: [0x44; 16],
            server_version: 1,
            client_version: 2,
            rpc_hash: 0x1122_3344,
            net_version: [1, 2, 3, 4, 5, 6],
            level_change_count: 7,
            level_id: 0x0a0b_0c0d,
            level_summary: [0x55; 64],
            local_player_id: 8,
            current_player_id: 9,
            level_data: vec![0x66, 0x67],
            moving,
            has_transfer_address: true,
            merge_state: 3,
            transfer_address: NetAddress::new(IpAddr::V4(Ipv4Addr::new(10, 0, 0, 7)), 7777),
            move_result: 0,
            trailing_bytes: 0,
        }
    }

    #[test]
    fn roundtrip() {
        for moving in [false, true] {
            let packet = sample(moving);
            let bytes = packet.to_bytes().unwrap();
            let decoded = JoinGame::from_bytes(&bytes).unwrap();
            assert_eq!(decoded, packet);
        }
    }

    #[test]
    fn rejects_oversized_level_data() {
        let mut packet = sample(false);
        packet.level_data = vec![0; MAX_JOIN_LEVEL_DATA_BYTES + 1];
        assert!(packet.to_bytes().is_none());
    }
}
