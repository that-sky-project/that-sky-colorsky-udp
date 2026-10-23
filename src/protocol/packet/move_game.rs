//! MoveGame packet (S->C) and the embedded `Move` structure.
//!
//! ```text
//! [raw_bytes_32: u8[32]]
//! [address: IPv4 + u16 port]
//! [move_nonce: u64]
//! [split_type: u8]
//! [player_count: u32]
//! [player_uuid: u8[16]] * player_count
//! [u32_field: u32]
//! [u08_field: u8]
//! [u32_field2: u32]
//! [is_split: bool]
//! [destination_uuid: u8[16]]
//! ```
//!
//! `JoinGame` embeds the same `Move` fields without the 32-byte prefix and
//! trailing fields.

use crate::protocol::binary::{Reader, Writer};
use crate::protocol::net_address::NetAddress;
use crate::protocol::types::TgcUuid;

/// Maximum number of players carried by one move.
pub const MAX_MOVE_PLAYERS: usize = 7;

/// Value for `MoveGame::u32_field2` meaning "no matchmaking event reservation".
///
/// The client stores this in its reservation slot (state+420) when the move is
/// accepted. `u32::MAX` clears it; zero leaves a phantom reservation that makes
/// later generic matchmaking signals get rejected.
pub const NO_EVENT_RESERVATION: u32 = u32::MAX;

/// Move destination and participant list.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Move {
    pub address: NetAddress,
    pub move_nonce: u64,
    pub split_type: u8,
    pub players: Vec<TgcUuid>,
}

impl Move {
    /// Decode the embedded `Move` structure.
    pub fn from_reader(reader: &mut Reader<'_>) -> Option<Self> {
        let address = NetAddress::read(reader)?;
        let move_nonce = reader.read_u64()?;
        let split_type = reader.read_u8()?;
        let player_count = reader.read_u32()? as usize;

        if player_count > MAX_MOVE_PLAYERS {
            return None;
        }

        let mut players = Vec::with_capacity(player_count);
        for _ in 0..player_count {
            players.push(read_uuid(reader)?);
        }

        Some(Self {
            address,
            move_nonce,
            split_type,
            players,
        })
    }

    /// Encode the embedded `Move` structure.
    pub fn write_to(&self, writer: &mut Writer) -> Option<()> {
        if self.players.len() > MAX_MOVE_PLAYERS {
            return None;
        }

        self.address.write(writer)?;
        writer.write_u64(self.move_nonce);
        writer.write_u8(self.split_type);
        writer.write_u32(self.players.len() as u32);
        for player in &self.players {
            writer.write_bytes(&player.raw());
        }
        Some(())
    }
}

/// Full server-to-client `MoveGame` payload.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MoveGame {
    pub raw_bytes32: [u8; 32],
    pub transfer: Move,
    pub u32_field: u32,
    pub u08_field: u8,
    pub u32_field2: u32,
    pub is_split: bool,
    pub destination_uuid: TgcUuid,
}

impl MoveGame {
    /// Decode a `MoveGame` payload.
    pub fn from_bytes(data: &[u8]) -> Option<Self> {
        let mut reader = Reader::new(data);

        let mut raw_bytes32 = [0u8; 32];
        raw_bytes32.copy_from_slice(reader.read_bytes(32)?);

        let transfer = Move::from_reader(&mut reader)?;
        let u32_field = reader.read_u32()?;
        let u08_field = reader.read_u8()?;
        let u32_field2 = reader.read_u32()?;
        let is_split = reader.read_bool()?;
        let destination_uuid = read_uuid(&mut reader)?;

        Some(Self {
            raw_bytes32,
            transfer,
            u32_field,
            u08_field,
            u32_field2,
            is_split,
            destination_uuid,
        })
    }

    /// Encode a `MoveGame` payload.
    pub fn to_bytes(&self) -> Option<Vec<u8>> {
        let mut writer = Writer::new();
        writer.write_bytes(&self.raw_bytes32);
        self.transfer.write_to(&mut writer)?;
        writer.write_u32(self.u32_field);
        writer.write_u8(self.u08_field);
        writer.write_u32(self.u32_field2);
        writer.write_bool(self.is_split);
        writer.write_bytes(&self.destination_uuid.raw());
        Some(writer.into_bytes())
    }
}

fn read_uuid(reader: &mut Reader<'_>) -> Option<TgcUuid> {
    TgcUuid::from(&reader.read_bytes(16)?)
}

#[cfg(test)]
mod tests {
    use std::net::{IpAddr, Ipv4Addr};

    use super::*;

    fn uuid(byte: u8) -> TgcUuid {
        TgcUuid::from(&[byte; 16]).unwrap()
    }

    #[test]
    fn move_game_roundtrip() {
        let packet = MoveGame {
            raw_bytes32: [0x11; 32],
            transfer: Move {
                address: NetAddress::new(IpAddr::V4(Ipv4Addr::new(192, 168, 1, 4)), 19133),
                move_nonce: 0x1234_5678_9abc_def0,
                split_type: 0,
                players: vec![uuid(1), uuid(2)],
            },
            u32_field: 7,
            u08_field: 8,
            u32_field2: 9,
            is_split: true,
            destination_uuid: uuid(3),
        };

        let bytes = packet.to_bytes().unwrap();
        assert_eq!(MoveGame::from_bytes(&bytes), Some(packet));
    }

    #[test]
    fn rejects_too_many_players() {
        let transfer = Move {
            address: NetAddress::new(IpAddr::V4(Ipv4Addr::LOCALHOST), 1),
            move_nonce: 0,
            split_type: 0,
            players: vec![uuid(1); MAX_MOVE_PLAYERS + 1],
        };
        let packet = MoveGame {
            raw_bytes32: [0; 32],
            transfer,
            u32_field: 0,
            u08_field: 0,
            u32_field2: 0,
            is_split: false,
            destination_uuid: TgcUuid::default(),
        };
        assert!(packet.to_bytes().is_none());
    }

    #[test]
    fn embedded_move_roundtrip() {
        let transfer = Move {
            address: NetAddress::new(IpAddr::V4(Ipv4Addr::new(10, 0, 0, 7)), 7777),
            move_nonce: 42,
            split_type: 1,
            players: vec![uuid(9)],
        };
        let mut writer = Writer::new();
        transfer.write_to(&mut writer).unwrap();
        let bytes = writer.into_bytes();

        let mut reader = Reader::new(&bytes);
        assert_eq!(Move::from_reader(&mut reader), Some(transfer));
    }
}
