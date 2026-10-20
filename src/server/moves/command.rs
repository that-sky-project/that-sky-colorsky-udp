//! Move commands exposed to the control plane.

use std::time::Instant;

use crate::control::{ControlResponse, MoveOutcome, ServerEvent};
use crate::protocol::packet::Packet;
use crate::protocol::packet::PacketId;
use crate::protocol::packet::cancel_move::CancelMove;
use crate::protocol::packet::move_game::MAX_MOVE_PLAYERS;

use super::super::state::{MoveRecord, MoveRecordStatus, ServerState};

impl ServerState {
    /// Move one player to a registered room.
    pub(crate) fn move_player(&mut self, player_id: u8, room_id: &str) -> ControlResponse {
        let Some(target) = self.rooms.get(room_id).cloned() else {
            return ControlResponse::error(format!("room not found: {room_id}"));
        };

        match self.begin_single_move(player_id, &target) {
            Ok(move_id) => ControlResponse::Move(MoveOutcome {
                move_ids: vec![move_id],
                moved: vec![player_id],
                failed: Vec::new(),
            }),
            Err(message) => ControlResponse::error(message),
        }
    }

    /// Move a group atomically: either every player starts moving or none do.
    pub(crate) fn move_players(&mut self, player_ids: Vec<u8>, room_id: &str) -> ControlResponse {
        let Some(target) = self.rooms.get(room_id).cloned() else {
            return ControlResponse::error(format!("room not found: {room_id}"));
        };

        let mut unique = Vec::with_capacity(player_ids.len());
        for player_id in player_ids {
            if unique.contains(&player_id) {
                return ControlResponse::error(format!("duplicate player id: {player_id}"));
            }
            unique.push(player_id);
        }

        if unique.is_empty() {
            return ControlResponse::error("empty player group");
        }
        if unique.len() > MAX_MOVE_PLAYERS {
            return ControlResponse::error(format!(
                "move group exceeds {MAX_MOVE_PLAYERS} players"
            ));
        }

        match self.begin_group_move(&unique, &target) {
            Ok(move_ids) => ControlResponse::Move(MoveOutcome {
                move_ids,
                moved: unique,
                failed: Vec::new(),
            }),
            Err(message) => ControlResponse::error(message),
        }
    }

    /// Move every active player in a level, one at a time (XYSKY parity).
    pub(crate) fn move_level(&mut self, level_id: u32, room_id: &str) -> ControlResponse {
        let Some(target) = self.rooms.get(room_id).cloned() else {
            return ControlResponse::error(format!("room not found: {room_id}"));
        };

        let player_ids: Vec<u8> = self
            .peers
            .values()
            .filter(|peer| peer.is_active() && peer.level_id == level_id)
            .map(|peer| peer.player_id)
            .collect();

        if player_ids.is_empty() {
            return ControlResponse::error(format!("no active players in level {level_id}"));
        }

        let mut move_ids = Vec::new();
        let mut moved = Vec::new();
        let mut failed = Vec::new();

        for player_id in player_ids {
            match self.begin_single_move(player_id, &target) {
                Ok(move_id) => {
                    move_ids.push(move_id);
                    moved.push(player_id);
                }
                Err(message) => {
                    tracing::warn!(player_id, error = %message, "level move skipped player");
                    failed.push(player_id);
                }
            }
        }

        if moved.is_empty() {
            return ControlResponse::error("no player could be moved");
        }

        ControlResponse::Move(MoveOutcome {
            move_ids,
            moved,
            failed,
        })
    }

    /// Cancel a pending move and return the player to its current room.
    pub(crate) fn cancel_player_move(&mut self, player_id: u8, reason: u8) -> ControlResponse {
        let Some(move_id) = self
            .pending_moves
            .values()
            .find(|pending| pending.player_id == player_id)
            .map(|pending| pending.move_id)
        else {
            return ControlResponse::error(format!("no pending move for player {player_id}"));
        };

        let Some(pending) = self.pending_moves.remove(&move_id) else {
            return ControlResponse::error(format!("no pending move for player {player_id}"));
        };

        self.send_cancel_move(player_id, reason);
        self.return_to_active(&pending);

        self.record_move(MoveRecord {
            move_id,
            player_id,
            room_id: pending.room_id.clone(),
            status: MoveRecordStatus::Cancelled,
            result: Some(reason),
            finished_at: Instant::now(),
        });

        self.emit_event(ServerEvent::MoveFinished {
            move_id,
            player_id,
            player_uuid: pending.player_uuid,
            accepted: false,
            result: reason,
        });

        ControlResponse::Move(MoveOutcome {
            move_ids: vec![move_id],
            moved: Vec::new(),
            failed: vec![player_id],
        })
    }

    pub(super) fn send_cancel_move(&self, player_id: u8, reason: u8) {
        let Some(peer) = self
            .peers
            .values()
            .find(|peer| peer.player_id == player_id && peer.is_player())
        else {
            return;
        };
        let payload = CancelMove {
            reason,
            payload: Vec::new(),
        }
        .to_bytes();
        let wire = Packet::build_packet(PacketId::CancelMove, &payload);
        peer.send(0, &wire, true);
    }
}
