//! Thread-safe control plane for third-party callers.
//!
//! # Why channels instead of locks
//!
//! The ENet host and every `ENetPeer` pointer are owned by the server thread.
//! Sharing them behind a `Mutex` would still require the event loop to lock on
//! every packet and would make peer callbacks re-entrant. Instead, callers
//! submit [`ControlCommand`] values through a bounded channel and the server
//! thread executes them serially between ENet polls. This keeps all ENet access
//! single-threaded and gives deterministic command ordering with backpressure.
//!
//! Adapters (HTTP, WebSocket, QWD, CLI, ...) only need a [`ControlHandle`].

use std::fmt;
use std::sync::mpsc::{self, Receiver, RecvTimeoutError, SyncSender, TrySendError};
use std::time::Duration;

use crate::protocol::net_address::NetAddress;
use crate::protocol::types::TgcUuid;

/// Capacity of the control command queue.
pub const CONTROL_QUEUE_CAPACITY: usize = 64;
/// Default timeout for a synchronous request/response round-trip.
pub const CONTROL_REQUEST_TIMEOUT: Duration = Duration::from_secs(5);

/// A room that clients can be moved to.
#[derive(Debug, Clone)]
pub struct RoomEndpoint {
    pub id: String,
    pub address: NetAddress,
    /// Optional destination room UUID sent in `MoveGame`.
    pub uuid: Option<TgcUuid>,
    pub capacity: usize,
    pub players: usize,
}

impl RoomEndpoint {
    /// Create a room endpoint with the standard eight-player capacity.
    pub fn new(id: impl Into<String>, address: NetAddress) -> Self {
        Self {
            id: id.into(),
            address,
            uuid: None,
            capacity: 8,
            players: 0,
        }
    }

    /// Convert to the shape used by status responses.
    pub fn to_info(&self) -> RoomInfo {
        RoomInfo {
            id: self.id.clone(),
            host: self.address.host.to_string(),
            port: self.address.port,
            uuid: self.uuid.map(|uuid| format!("{uuid:?}")),
            capacity: self.capacity,
            players: self.players,
        }
    }
}

/// Commands accepted by the server thread.
#[derive(Debug, Clone)]
pub enum ControlCommand {
    /// Move one player to a registered room.
    MovePlayer { player_id: u8, room_id: String },
    /// Move a group of players atomically.
    MovePlayers {
        player_ids: Vec<u8>,
        room_id: String,
    },
    /// Move every active player in a level to a registered room.
    MoveLevel { level_id: u32, room_id: String },
    /// Cancel a pending move.
    CancelMove { player_id: u8, reason: u8 },
    /// Kick a player with a reason string.
    KickPlayer { player_id: u8, reason: String },
    /// Broadcast a NetRpc payload to active players.
    BroadcastRpc {
        level_id: Option<u32>,
        player_id: u8,
        data: Vec<u8>,
    },
    /// Register or replace a move target.
    UpsertRoom { room: RoomEndpoint },
    /// Remove a move target.
    RemoveRoom { room_id: String },
    /// Revoke the current level authority and elect another player.
    RevokeAuthority { level_id: u32, reason: u16 },
    /// Return a status snapshot.
    Status,
    /// Stop the server.
    Shutdown,
}

/// Result of a move request.
#[derive(Debug, Clone)]
pub struct MoveOutcome {
    pub move_ids: Vec<u64>,
    pub moved: Vec<u8>,
    pub failed: Vec<u8>,
}

/// Room summary used by status responses.
#[derive(Debug, Clone)]
pub struct RoomInfo {
    pub id: String,
    pub host: String,
    pub port: u16,
    pub uuid: Option<String>,
    pub capacity: usize,
    pub players: usize,
}

/// Server status snapshot.
#[derive(Debug, Clone)]
pub struct ServerStatus {
    pub addr: String,
    pub peers: usize,
    pub players: usize,
    pub pending_moves: usize,
    pub rooms: Vec<RoomInfo>,
}

/// Response to a control command.
#[derive(Debug, Clone)]
pub enum ControlResponse {
    Ok,
    Error { message: String },
    Move(MoveOutcome),
    Status(ServerStatus),
}

impl ControlResponse {
    /// Convert a command-level error to a response.
    pub fn error(message: impl Into<String>) -> Self {
        Self::Error {
            message: message.into(),
        }
    }

    /// Returns true when the command succeeded.
    pub fn is_ok(&self) -> bool {
        !matches!(self, Self::Error { .. })
    }
}

/// Errors that prevent a command from reaching the server or completing.
#[derive(Debug)]
pub enum ControlError {
    QueueFull,
    Disconnected,
    Timeout,
}

impl fmt::Display for ControlError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::QueueFull => write!(f, "control queue is full"),
            Self::Disconnected => write!(f, "control channel is disconnected"),
            Self::Timeout => write!(f, "control request timed out"),
        }
    }
}

impl std::error::Error for ControlError {}

/// Events emitted by the server thread.
#[derive(Debug, Clone)]
pub enum ServerEvent {
    PlayerJoined {
        player_id: u8,
        uuid: TgcUuid,
        level_id: u32,
    },
    PlayerLeft {
        player_id: u8,
        uuid: TgcUuid,
        level_id: u32,
    },
    MoveStarted {
        move_id: u64,
        player_id: u8,
        room_id: String,
    },
    MoveFinished {
        move_id: u64,
        player_id: u8,
        accepted: bool,
        result: u8,
    },
    MoveTimedOut {
        move_id: u64,
        player_id: u8,
    },
    AuthorityElected {
        level_id: u32,
        player_id: u8,
    },
}

/// Optional event sink. Events are best-effort and never block the server.
pub type EventSender = SyncSender<ServerEvent>;

pub(crate) struct ControlRequest {
    pub command: ControlCommand,
    pub reply: Option<mpsc::Sender<ControlResponse>>,
}

/// Cloneable handle used by third-party callers.
#[derive(Clone)]
pub struct ControlHandle {
    tx: SyncSender<ControlRequest>,
}

impl ControlHandle {
    pub(crate) fn new(tx: SyncSender<ControlRequest>) -> Self {
        Self { tx }
    }

    /// Submit a command without waiting for a response.
    pub fn submit(&self, command: ControlCommand) -> Result<(), ControlError> {
        self.tx
            .try_send(ControlRequest {
                command,
                reply: None,
            })
            .map_err(map_send_error)
    }

    /// Submit a command and wait for its response.
    pub fn request(&self, command: ControlCommand) -> Result<ControlResponse, ControlError> {
        let (reply_tx, reply_rx) = mpsc::channel();
        self.tx
            .try_send(ControlRequest {
                command,
                reply: Some(reply_tx),
            })
            .map_err(map_send_error)?;

        reply_rx
            .recv_timeout(CONTROL_REQUEST_TIMEOUT)
            .map_err(|error| match error {
                RecvTimeoutError::Timeout => ControlError::Timeout,
                RecvTimeoutError::Disconnected => ControlError::Disconnected,
            })
    }

    /// Move one player to a room.
    pub fn move_player(
        &self,
        player_id: u8,
        room_id: impl Into<String>,
    ) -> Result<ControlResponse, ControlError> {
        self.request(ControlCommand::MovePlayer {
            player_id,
            room_id: room_id.into(),
        })
    }

    /// Move a group atomically.
    pub fn move_players(
        &self,
        player_ids: Vec<u8>,
        room_id: impl Into<String>,
    ) -> Result<ControlResponse, ControlError> {
        self.request(ControlCommand::MovePlayers {
            player_ids,
            room_id: room_id.into(),
        })
    }

    /// Move a whole level.
    pub fn move_level(
        &self,
        level_id: u32,
        room_id: impl Into<String>,
    ) -> Result<ControlResponse, ControlError> {
        self.request(ControlCommand::MoveLevel {
            level_id,
            room_id: room_id.into(),
        })
    }

    /// Cancel a pending move.
    pub fn cancel_move(&self, player_id: u8, reason: u8) -> Result<ControlResponse, ControlError> {
        self.request(ControlCommand::CancelMove { player_id, reason })
    }

    /// Kick a player.
    pub fn kick_player(
        &self,
        player_id: u8,
        reason: impl Into<String>,
    ) -> Result<ControlResponse, ControlError> {
        self.request(ControlCommand::KickPlayer {
            player_id,
            reason: reason.into(),
        })
    }

    /// Broadcast a NetRpc payload to active players.
    pub fn broadcast_rpc(
        &self,
        level_id: Option<u32>,
        player_id: u8,
        data: Vec<u8>,
    ) -> Result<ControlResponse, ControlError> {
        self.request(ControlCommand::BroadcastRpc {
            level_id,
            player_id,
            data,
        })
    }

    /// Register or replace a room target.
    pub fn upsert_room(&self, room: RoomEndpoint) -> Result<ControlResponse, ControlError> {
        self.request(ControlCommand::UpsertRoom { room })
    }

    /// Remove a room target.
    pub fn remove_room(&self, room_id: impl Into<String>) -> Result<ControlResponse, ControlError> {
        self.request(ControlCommand::RemoveRoom {
            room_id: room_id.into(),
        })
    }

    /// Revoke the authority of a level.
    pub fn revoke_authority(
        &self,
        level_id: u32,
        reason: u16,
    ) -> Result<ControlResponse, ControlError> {
        self.request(ControlCommand::RevokeAuthority { level_id, reason })
    }

    /// Fetch a status snapshot.
    pub fn status(&self) -> Result<ControlResponse, ControlError> {
        self.request(ControlCommand::Status)
    }

    /// Ask the server to stop. This only queues the command; use
    /// `EnetServer::wait` to join the thread.
    pub fn shutdown(&self) -> Result<(), ControlError> {
        self.submit(ControlCommand::Shutdown)
    }

    /// Queue a shutdown even if the bounded control queue is momentarily full.
    pub(crate) fn shutdown_blocking(&self) -> Result<(), ControlError> {
        self.tx
            .send(ControlRequest {
                command: ControlCommand::Shutdown,
                reply: None,
            })
            .map_err(|_| ControlError::Disconnected)
    }
}

pub(crate) fn control_channel() -> (SyncSender<ControlRequest>, Receiver<ControlRequest>) {
    mpsc::sync_channel(CONTROL_QUEUE_CAPACITY)
}

fn map_send_error(error: TrySendError<ControlRequest>) -> ControlError {
    match error {
        TrySendError::Full(_) => ControlError::QueueFull,
        TrySendError::Disconnected(_) => ControlError::Disconnected,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn request_roundtrip() {
        let (tx, rx) = control_channel();
        let handle = ControlHandle::new(tx);

        let worker = std::thread::spawn(move || {
            let request = rx.recv().unwrap();
            request.reply.unwrap().send(ControlResponse::Ok).unwrap();
        });

        assert!(matches!(
            handle.request(ControlCommand::Status),
            Ok(ControlResponse::Ok)
        ));
        worker.join().unwrap();
    }

    #[test]
    fn full_queue_reports_error() {
        let (tx, _rx) = mpsc::sync_channel(1);
        let handle = ControlHandle::new(tx);
        assert!(handle.submit(ControlCommand::Status).is_ok());
        assert!(matches!(
            handle.submit(ControlCommand::Status),
            Err(ControlError::QueueFull)
        ));
    }
}
