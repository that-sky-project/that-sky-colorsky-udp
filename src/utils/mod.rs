//! Shared utilities for the server.
//!
//! - `peer_key` — Converts an ENet peer pointer to a `usize` key for
//!   `HashMap` indexing.
//! - `fnv1a_player_id` — FNV-1a hash mapping a 16-byte UUID to a
//!   1..=255 player ID.
use crate::protocol::packet::PacketId;
use crate::server::peer::PeerEntry;
pub mod snapshot;

/// Convert an ENet peer pointer to a `usize` key for `HashMap` indexing.
pub fn peer_key(peer: *mut enet_sys::ENetPeer) -> usize {
    peer as usize
}

/// FNV-1a hash mapping a 16-byte UUID to a `1..=255` player ID.
///
/// Uses the 32-bit FNV-1a algorithm. The result is the low 8 bits,
/// clamped to a minimum of 1 (0 is reserved for "unassigned").
pub fn fnv1a_player_id(uuid: &[u8; 16]) -> u8 {
    let mut h: u32 = 0x811c_9dc5;
    for &b in uuid.iter() {
        h = (h ^ b as u32).wrapping_mul(0x0100_0193);
    }
    (h as u8).max(1)
}

/// Strip the 4-byte connection magic prefix from incoming data.
///
/// - If the data starts with a known [`PacketId`] (e.g. `ClientConnect`
///   or `JoinGame`), returns the data as-is and records the magic bytes.
/// - Otherwise, if a known [`PacketId`] appears at offset 4, skips the
///   first 4 bytes.
pub fn strip_connection_magic<'a>(data: &'a [u8], entry: Option<&mut PeerEntry>) -> &'a [u8] {
    if let Some(packet_id) = data.first().and_then(|id| PacketId::from_u8(*id)) {
        if matches!(packet_id, PacketId::ClientConnect | PacketId::JoinGame)
            && data.len() >= 4
            && let Some(e) = entry
        {
            e.conn_magic.copy_from_slice(&data[..4]);
        }
        return data;
    }

    if data.len() >= 5 && PacketId::from_u8(data[4]).is_some() {
        if let Some(e) = entry {
            e.conn_magic.copy_from_slice(&data[..4]);
        }
        &data[4..]
    } else {
        data
    }
}

/// Convert bytes to hex preview, display "..." beyond `max_len`
pub fn hex_preview(data: &[u8], max_len: usize) -> String {
    let shown = data.len().min(max_len);
    let mut out = hex::encode(&data[..shown]);
    if data.len() > shown {
        out.push_str("...");
    }
    out
}

/// Get current Unix timestamp（seconds，f64）。
pub fn unix_time_seconds() -> f64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|duration| duration.as_secs_f64())
        .unwrap_or_default()
}
