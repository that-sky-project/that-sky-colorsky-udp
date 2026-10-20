//! Move result handling, timeouts and disconnect cleanup.

use std::time::Instant;

use crate::control::ServerEvent;
use crate::protocol::packet::move_result::MoveResult;

use super::super::peer::PeerState;
use super::super::state::{MOVE_TIMEOUT, MoveRecord, MoveRecordStatus, PendingMove, ServerState};

impl ServerState {
    /// Handle a client `MoveResult`.
    pub(crate) fn handle_move_result(&mut self, peer_key: usize, payload: &[u8]) -> Option<()> {
        let result = MoveResult::from_bytes(payload)?;
        let player_id = self.peers.get(&peer_key)?.player_id;

        let Some(move_id) = self
            .pending_moves
            .values()
            .find(|pending| pending.player_id == player_id)
            .map(|pending| pending.move_id)
        else {
            tracing::debug!(
                player_id,
                result = result.result,
                "MoveResult without pending move"
            );
            return Some(());
        };

        let Some(pending) = self.pending_moves.remove(&move_id) else {
            return Some(());
        };

        let accepted = result.result == 0;
        if !accepted {
            self.return_to_active(&pending);
        }

        self.record_move(MoveRecord {
            move_id,
            player_id,
            room_id: pending.room_id.clone(),
            status: if accepted {
                MoveRecordStatus::Acknowledged
            } else {
                MoveRecordStatus::Failed
            },
            result: Some(result.result),
            finished_at: Instant::now(),
        });

        self.emit_event(ServerEvent::MoveFinished {
            move_id,
            player_id,
            player_uuid: pending.player_uuid,
            accepted,
            result: result.result,
        });

        tracing::info!(
            player_id,
            move_id,
            result = result.result,
            accepted,
            "received MoveResult"
        );

        Some(())
    }

    /// Expire moves that never completed.
    pub(crate) fn expire_moves(&mut self, now: Instant) {
        let expired: Vec<u64> = self
            .pending_moves
            .iter()
            .filter(|(_, pending)| {
                now.saturating_duration_since(pending.created_at) >= MOVE_TIMEOUT
            })
            .map(|(move_id, _)| *move_id)
            .collect();

        for move_id in expired {
            let Some(pending) = self.pending_moves.remove(&move_id) else {
                continue;
            };

            self.send_cancel_move(pending.player_id, 2);
            self.return_to_active(&pending);

            self.record_move(MoveRecord {
                move_id,
                player_id: pending.player_id,
                room_id: pending.room_id.clone(),
                status: MoveRecordStatus::TimedOut,
                result: None,
                finished_at: now,
            });

            self.emit_event(ServerEvent::MoveTimedOut {
                move_id,
                player_id: pending.player_id,
                player_uuid: pending.player_uuid,
            });

            tracing::warn!(
                move_id,
                player_id = pending.player_id,
                "move timed out; reverted player to active"
            );
        }
    }

    /// Drop pending moves when the player disconnects.
    pub(crate) fn forget_player_moves(&mut self, player_id: u8) {
        let move_ids: Vec<u64> = self
            .pending_moves
            .values()
            .filter(|pending| pending.player_id == player_id)
            .map(|pending| pending.move_id)
            .collect();

        for move_id in move_ids {
            if let Some(pending) = self.pending_moves.remove(&move_id) {
                self.record_move(MoveRecord {
                    move_id,
                    player_id,
                    room_id: pending.room_id.clone(),
                    status: MoveRecordStatus::Aborted,
                    result: None,
                    finished_at: Instant::now(),
                });
            }
        }
    }

    pub(super) fn return_to_active(&mut self, pending: &PendingMove) {
        let Some(peer_key) = self.find_player_peer_key(pending.player_id) else {
            return;
        };

        let level_id = pending.level_id;
        if let Some(peer) = self.peers.get_mut(&peer_key)
            && peer.state == PeerState::Moving
        {
            peer.state = PeerState::Active;
        }

        self.rebalance_level(level_id);
    }
}
