//! Server-wide global state.
//!
//! Holds the peer table for all connected players and the level host
//! registry. Player IDs are assigned by the game layer, defaulting to 0
//! (unassigned).

use std::collections::HashMap;

use super::peer::PeerEntry;

/// Server-wide state shared across all connections.
///
/// Keyed by `peer as usize`, and holding per-peer [`PeerEntry`] entries.
#[derive(Default)]
pub struct ServerState {
    /// All connected peers, keyed by `peer as usize`.
    pub peers: HashMap<usize, PeerEntry>,
    /// Elected host player ID per level.
    pub level_hosts: HashMap<u32, u8>,
}

impl ServerState {}
