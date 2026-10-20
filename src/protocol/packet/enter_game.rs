//! EnterGame packet (S->C).
//!
//! ```text
//! [player_count: compressed(0..=8)]
//! repeat:
//!   [net_player_id: u8]
//!   [player_uuid: u8[16]]
//!   [session_token: u8[16]]   (zeroed by the server)
//!   [level_id: u32]
//! ```

use crate::protocol::binary::{Reader, Writer};
use crate::protocol::types::TgcUuid;

/// Maximum players in an EnterGame roster.
pub const MAX_ENTER_GAME_PLAYERS: usize = 8;

/// One roster entry.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EnterPlayer {
    pub net_id: u8,
    pub uuid: TgcUuid,
    pub level_id: u32,
}

/// Full `EnterGame` payload.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EnterGame {
    pub players: Vec<EnterPlayer>,
}

impl EnterGame {
    /// Encode an `EnterGame` payload.
    pub fn to_bytes(&self) -> Option<Vec<u8>> {
        if self.players.len() > MAX_ENTER_GAME_PLAYERS {
            return None;
        }

        let mut writer = Writer::new();
        writer.write_compressed(self.players.len() as u32, 8)?;

        for player in &self.players {
            writer.write_u8(player.net_id);
            writer.write_bytes(&player.uuid.raw());
            writer.write_bytes(&[0u8; 16]);
            writer.write_u32(player.level_id);
        }

        Some(writer.into_bytes())
    }

    /// Decode an `EnterGame` payload.
    pub fn from_bytes(data: &[u8]) -> Option<Self> {
        let mut reader = Reader::new(data);
        let count = reader.read_compressed(8)? as usize;

        if count > MAX_ENTER_GAME_PLAYERS {
            return None;
        }

        let mut players = Vec::with_capacity(count);
        for _ in 0..count {
            let net_id = reader.read_u8()?;
            let uuid = TgcUuid::from(&reader.read_bytes(16)?)?;
            reader.read_bytes(16)?;
            let level_id = reader.read_u32()?;
            players.push(EnterPlayer {
                net_id,
                uuid,
                level_id,
            });
        }

        Some(Self { players })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn uuid(byte: u8) -> TgcUuid {
        TgcUuid::from(&[byte; 16]).unwrap()
    }

    #[test]
    fn roundtrip() {
        let packet = EnterGame {
            players: vec![
                EnterPlayer {
                    net_id: 1,
                    uuid: uuid(1),
                    level_id: 0x1234_5678,
                },
                EnterPlayer {
                    net_id: 2,
                    uuid: uuid(2),
                    level_id: 0x90ab_cdef,
                },
            ],
        };

        let bytes = packet.to_bytes().unwrap();
        assert_eq!(bytes.len(), 1 + 2 * 37);
        assert_eq!(EnterGame::from_bytes(&bytes), Some(packet));
    }

    #[test]
    fn rejects_too_many_players() {
        let packet = EnterGame {
            players: (0..=MAX_ENTER_GAME_PLAYERS)
                .map(|index| EnterPlayer {
                    net_id: index as u8,
                    uuid: uuid(index as u8),
                    level_id: 0,
                })
                .collect(),
        };
        assert!(packet.to_bytes().is_none());
    }
}
