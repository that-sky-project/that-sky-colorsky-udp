//! Move settlement: reporting outcomes to the manager and bridging the ENet
//! thread's server events into QWD messages.
//!
//! Settle events must echo the reservation token the manager issued: a
//! transaction whose token does not match is ignored, which would leave every
//! move open until its timeout.

use std::sync::mpsc::TryRecvError;

use serde_json::{Value, json};

use crate::control::ServerEvent;

use super::protocol::event;
use super::reservations::{CompletedMove, ExpiredMove};
use super::util::now_ms;
use super::{PendingQwdMove, QwdClient, Socket};

/// One `move.result` payload.
pub(super) struct MoveReport<'a> {
    pub move_id: &'a str,
    pub accepted: bool,
    pub moved: &'a [String],
    pub failed: &'a [String],
    pub reason: Option<&'a str>,
    pub token: Option<&'a str>,
}

impl QwdClient {
    /// Tell the manager that every player of a move group has landed. This is
    /// stronger than inferring completion from join/leave pairs, so the move
    /// transaction can settle even if a leave event was lost.
    pub(super) fn send_move_completed(
        &self,
        socket: &mut Socket,
        completed: &CompletedMove,
    ) -> Result<(), String> {
        if !self.caps.move_completed {
            // Older managers have no `move.completed` event and settle the
            // transaction from the join/leave pair instead. A positive
            // `move.result` is the strongest signal they do understand.
            return self.send_move_result(
                socket,
                MoveReport {
                    move_id: &completed.move_id,
                    accepted: true,
                    moved: &completed.player_ids,
                    failed: &[],
                    reason: None,
                    token: Some(&completed.token),
                },
            );
        }
        let value = event(
            "move.completed",
            self.settle_data(json!({
                "roomId": self.room_id,
                "moveId": completed.move_id,
                "playerIds": completed.player_ids,
                "admittedPlayerIds": completed.player_ids,
                "reservationToken": completed.token,
                "timestamp": now_ms()
            })),
        );
        self.send_json(socket, &value)
    }

    /// Tell the manager that a target reservation expired before its group
    /// arrived, so it can fail the transaction immediately.
    pub(super) fn send_reservation_expired(
        &self,
        socket: &mut Socket,
        expired: &ExpiredMove,
    ) -> Result<(), String> {
        if !self.caps.reservation_expired {
            // Older managers reject the event and run their own reservation
            // TTL, so sending it only produces a rejection per expired move.
            return Ok(());
        }
        let value = event(
            "reservation.expired",
            self.settle_data(json!({
                "roomId": self.room_id,
                "moveId": expired.move_id,
                "playerIds": expired.player_ids,
                "admittedPlayerIds": expired.admitted_player_ids,
                "reservationToken": expired.token,
                "expiresAt": now_ms(),
                "timestamp": now_ms()
            })),
        );
        self.send_json(socket, &value)
    }

    pub(super) fn send_move_result(
        &self,
        socket: &mut Socket,
        report: MoveReport<'_>,
    ) -> Result<(), String> {
        let mut data = json!({
            "roomId": self.room_id,
            "moveId": report.move_id,
            "accepted": report.accepted,
            "movedPlayerIds": report.moved,
            "failedPlayerIds": report.failed,
            "timestamp": now_ms()
        });
        if let Some(reason) = report.reason {
            data["reason"] = Value::String(reason.to_string());
        }
        if let Some(token) = report.token
            && self.caps.settle_extras
        {
            data["reservationToken"] = Value::String(token.to_string());
            data["phase"] = json!("source_commit");
        }
        self.send_json(socket, &event("move.result", data))
    }

    /// Strip the fields an older manager's strict schema does not know. The
    /// manager answers with a validation error and drops the whole frame, so
    /// extras must disappear rather than break the minimal shape.
    fn settle_data(&self, data: Value) -> Value {
        if self.caps.settle_extras {
            return data;
        }
        let mut object = data;
        for key in [
            "phase",
            "reservationToken",
            "admittedPlayerIds",
            "expiresAt",
        ] {
            object.as_object_mut().map(|value| value.remove(key));
        }
        object
    }

    pub(super) fn drain_events(&mut self, socket: &mut Socket) -> Result<(), String> {
        loop {
            match self.events.try_recv() {
                Ok(server_event) => self.handle_server_event(socket, server_event)?,
                Err(TryRecvError::Empty | TryRecvError::Disconnected) => return Ok(()),
            }
        }
    }

    fn handle_server_event(
        &mut self,
        socket: &mut Socket,
        server_event: ServerEvent,
    ) -> Result<(), String> {
        match server_event {
            ServerEvent::PlayerJoined { uuid, level_id, .. } => {
                let player_id = format!("{uuid:?}");
                let completed = self.reservations.confirm_player(&player_id);
                let value = event(
                    "player.join",
                    json!({
                        "roomId": self.room_id,
                        "player": {
                            "playerId": player_id,
                            "level": level_id
                        }
                    }),
                );
                self.send_json(socket, &value)?;
                for completed_move in &completed {
                    self.send_move_completed(socket, completed_move)?;
                }
                Ok(())
            }
            ServerEvent::PlayerLeft { uuid, .. } => {
                let player_id = format!("{uuid:?}");
                let _ = self.reservations.confirm_player(&player_id);
                let value = event(
                    "player.leave",
                    json!({
                        "roomId": self.room_id,
                        "player": {
                            "playerId": player_id
                        }
                    }),
                );
                self.send_json(socket, &value)
            }
            ServerEvent::MoveFinished {
                move_id,
                player_uuid,
                accepted,
                result,
                ..
            } => self.finish_qwd_move(
                socket,
                move_id,
                &format!("{player_uuid:?}"),
                accepted,
                Some(format!("client_move_result_{result}")),
            ),
            ServerEvent::MoveTimedOut {
                move_id,
                player_uuid,
                ..
            } => self.finish_qwd_move(
                socket,
                move_id,
                &format!("{player_uuid:?}"),
                false,
                Some("move_timeout".to_string()),
            ),
            ServerEvent::MoveStarted { .. } | ServerEvent::AuthorityElected { .. } => Ok(()),
        }
    }

    fn finish_qwd_move(
        &mut self,
        socket: &mut Socket,
        internal_move_id: u64,
        player_id: &str,
        accepted: bool,
        reason: Option<String>,
    ) -> Result<(), String> {
        let Some(qwd_move_id) = self.internal_to_qwd.remove(&internal_move_id) else {
            return Ok(());
        };
        let Some(state) = self.qwd_moves.get_mut(&qwd_move_id) else {
            return Ok(());
        };

        state.pending.remove(&internal_move_id);
        if accepted {
            if !state.moved.iter().any(|id| id == player_id) {
                state.moved.push(player_id.to_string());
            }
        } else {
            if !state.failed.iter().any(|id| id == player_id) {
                state.failed.push(player_id.to_string());
            }
            if state.reason.is_none() {
                state.reason = reason;
            }
        }

        if !state.pending.is_empty() {
            return Ok(());
        }

        let state = self
            .qwd_moves
            .remove(&qwd_move_id)
            .unwrap_or(PendingQwdMove {
                pending: Default::default(),
                moved: Vec::new(),
                failed: Vec::new(),
                reason: None,
                token: None,
            });

        self.send_move_result(
            socket,
            MoveReport {
                move_id: &qwd_move_id,
                accepted: state.failed.is_empty(),
                moved: &state.moved,
                failed: &state.failed,
                reason: state.reason.as_deref(),
                token: state.token.as_deref(),
            },
        )
    }
}
