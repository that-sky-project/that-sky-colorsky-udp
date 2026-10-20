//! Server-wide global state.
//!
//! Holds the peer table, level authority registry, move targets, pending
//! moves and migration rosters. All of it is owned by the ENet server thread.

use std::collections::{HashMap, HashSet, VecDeque};
use std::time::{Duration, Instant};

use crate::control::{EventSender, RoomEndpoint, ServerEvent};
use crate::protocol::game_msg::level_data::LevelData;
use crate::protocol::net_address::NetAddress;
use crate::protocol::types::TgcUuid;

use super::peer::PeerEntry;

/// Hard room player limit, matching the client's roster size.
pub const MAX_PLAYERS: usize = 8;
/// A pending move is cancelled after this long.
pub const MOVE_TIMEOUT: Duration = Duration::from_secs(20);
/// Migration rosters expire if the whole group does not arrive in time.
pub const MIGRATION_ROSTER_TIMEOUT: Duration = Duration::from_secs(15);
/// Maximum number of finished moves retained for status queries.
pub const MOVE_HISTORY_LIMIT: usize = 64;

/// A move that has been sent to a client and is awaiting a result.
#[derive(Debug, Clone)]
pub struct PendingMove {
    pub move_id: u64,
    pub player_id: u8,
    pub player_uuid: TgcUuid,
    pub level_id: u32,
    pub room_id: String,
    pub address: NetAddress,
    pub move_nonce: u64,
    pub split: bool,
    pub created_at: Instant,
}

/// Final state of a move.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MoveRecordStatus {
    Acknowledged,
    Failed,
    Cancelled,
    TimedOut,
    Aborted,
}

/// Finished move retained for diagnostics.
#[derive(Debug, Clone)]
pub struct MoveRecord {
    pub move_id: u64,
    pub player_id: u8,
    pub room_id: String,
    pub status: MoveRecordStatus,
    pub result: Option<u8>,
    pub finished_at: Instant,
}

/// One reserved slot in a migration roster.
#[derive(Debug, Clone)]
pub struct MigrationEntry {
    pub net_id: u8,
    pub uuid: TgcUuid,
    pub level_id: u32,
}

/// Group that arrived through a move and is waiting for the remaining members.
#[derive(Debug, Clone)]
pub struct MigrationRoster {
    pub key: String,
    pub entries: Vec<MigrationEntry>,
    pub expires_at: Instant,
}

/// Server-wide state shared across all connections.
///
/// Keyed by `peer as usize`, and holding per-peer [`PeerEntry`] entries.
#[derive(Default)]
pub struct ServerState {
    /// All connected peers, keyed by `peer as usize`.
    pub peers: HashMap<usize, PeerEntry>,
    /// Count player id.
    pub net_id_count: u8,
    /// Elected host player ID per level.
    pub level_authority: HashMap<u32, u8>,
    /// Level Data.
    pub levels_data: HashMap<u32, LevelData>,
    /// Move targets registered by a control caller.
    pub rooms: HashMap<String, RoomEndpoint>,
    /// Moves awaiting a client result.
    pub pending_moves: HashMap<u64, PendingMove>,
    /// Recent finished moves, newest last.
    pub move_history: VecDeque<MoveRecord>,
    /// Monotonic move id allocator.
    pub next_move_id: u64,
    /// Player ids reserved by a migration roster.
    pub reserved_ids: HashSet<u8>,
    /// Incoming migration groups.
    pub migration_rosters: HashMap<String, MigrationRoster>,
    /// Optional event sink for control adapters and observability.
    pub event_tx: Option<EventSender>,
    /// Reject new joins while draining (QWD node.drain).
    pub draining: bool,
    /// Address the server is bound to (for status responses).
    pub bind_addr: String,
}

impl ServerState {
    /// Emit a best-effort event. Never blocks the server thread.
    pub fn emit_event(&self, event: ServerEvent) {
        if let Some(tx) = &self.event_tx {
            let _ = tx.try_send(event);
        }
    }

    /// Push a finished move into the bounded history.
    pub fn record_move(&mut self, record: MoveRecord) {
        self.move_history.push_back(record);
        while self.move_history.len() > MOVE_HISTORY_LIMIT {
            self.move_history.pop_front();
        }
    }
}
