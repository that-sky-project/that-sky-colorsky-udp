//! Wire protocol for *Sky: Children of the Light*.
//!
//! The protocol sits on top of ENet payloads (ENet with CRC32 checksum).
//! All multi-byte integers are **little-endian**.
//!
//! ## Layers
//!
//! The top-level unit is a [`PacketId`](packet::PacketId) packet.
//!
//! ### Client -> Server
//!
//! ```text
//! [packet_id:u8] [seq:u8] [payload]
//! ```
//!
//! ### Server -> Client
//!
//! ```text
//! [packet_id:u8] [payload_len:u16] [payload]
//! ```

pub mod game_msg;
pub mod packet;
pub mod types;
