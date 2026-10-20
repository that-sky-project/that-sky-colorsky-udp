//! Move service: single/group moves, results, cancellation and timeouts.

use std::time::Instant;

use crate::control::{ControlResponse, MoveOutcome, RoomEndpoint, ServerEvent};
use crate::protocol::game_msg::{GameMsg, GameMsgId};
use crate::protocol::packet::Packet;
use crate::protocol::packet::PacketId;
use crate::protocol::packet::cancel_move::CancelMove;
use crate::protocol::packet::move_game::{MAX_MOVE_PLAYERS, Move, MoveGame};
use crate::protocol::packet::move_result::MoveResult;
use crate::protocol::types::TgcUuid;

use super::peer::PeerState;
use super::state::{MOVE_TIMEOUT, MoveRecord, MoveRecordStatus, PendingMove, ServerState};

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
            accepted: false,
            result: reason,
        });

        ControlResponse::Move(MoveOutcome {
            move_ids: vec![move_id],
            moved: Vec::new(),
            failed: vec![player_id],
        })
    }

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

    /// Find a move target by id.
    #[allow(dead_code)]
    pub(crate) fn room_target(&self, room_id: &str) -> Option<RoomEndpoint> {
        self.rooms.get(room_id).cloned()
    }

    fn begin_single_move(&mut self, player_id: u8, target: &RoomEndpoint) -> Result<u64, String> {
        if target.capacity > 0 && target.players >= target.capacity {
            return Err(format!("target room is full: {}", target.id));
        }

        let Some(peer_key) = self.find_active_peer_key(player_id) else {
            return Err(format!("player not active: {player_id}"));
        };

        if self
            .pending_moves
            .values()
            .any(|pending| pending.player_id == player_id)
        {
            return Err(format!("player already moving: {player_id}"));
        }

        let (uuid, level_id) = {
            let peer = self
                .peers
                .get(&peer_key)
                .ok_or_else(|| format!("player not found: {player_id}"))?;
            (peer.uuid, peer.level_id)
        };

        let move_id = self.allocate_move_id();
        let nonce = create_move_nonce(player_id, &target.id);

        let packet = MoveGame {
            raw_bytes32: [0; 32],
            transfer: Move {
                address: target.address,
                move_nonce: nonce,
                split_type: 0,
                players: vec![uuid],
            },
            u32_field: 0,
            u08_field: 0,
            u32_field2: 0,
            is_split: false,
            destination_uuid: target.uuid.unwrap_or_default(),
        };

        let bytes = packet
            .to_bytes()
            .ok_or_else(|| "failed to encode MoveGame".to_string())?;
        let wire = Packet::build_packet(PacketId::MoveGame, &bytes);

        let send_result = self
            .peers
            .get(&peer_key)
            .ok_or_else(|| format!("player not found: {player_id}"))?
            .send(0, &wire, true);

        if send_result < 0 {
            return Err(format!("failed to send MoveGame to player {player_id}"));
        }

        self.pending_moves.insert(
            move_id,
            PendingMove {
                move_id,
                player_id,
                player_uuid: uuid,
                level_id,
                room_id: target.id.clone(),
                address: target.address,
                move_nonce: nonce,
                split: false,
                created_at: Instant::now(),
            },
        );

        if let Some(peer) = self.peers.get_mut(&peer_key) {
            peer.state = PeerState::Moving;
        }

        self.release_authority(player_id, level_id);
        self.emit_event(ServerEvent::MoveStarted {
            move_id,
            player_id,
            room_id: target.id.clone(),
        });

        Ok(move_id)
    }

    fn begin_group_move(
        &mut self,
        player_ids: &[u8],
        target: &RoomEndpoint,
    ) -> Result<Vec<u64>, String> {
        if target.capacity > 0 && target.players + player_ids.len() > target.capacity {
            return Err(format!("target room has no room for group: {}", target.id));
        }

        let mut members = Vec::with_capacity(player_ids.len());

        for &player_id in player_ids {
            let Some(peer_key) = self.find_active_peer_key(player_id) else {
                return Err(format!("player not active: {player_id}"));
            };
            if self
                .pending_moves
                .values()
                .any(|pending| pending.player_id == player_id)
            {
                return Err(format!("player already moving: {player_id}"));
            }

            let peer = self
                .peers
                .get(&peer_key)
                .ok_or_else(|| format!("player not found: {player_id}"))?;
            members.push((peer_key, player_id, peer.uuid, peer.level_id));
        }

        let uuids: Vec<TgcUuid> = members.iter().map(|(_, _, uuid, _)| *uuid).collect();
        let mut prepared: Vec<(u64, u8, TgcUuid, u32, u64)> = Vec::with_capacity(members.len());

        for &(_, player_id, uuid, level_id) in &members {
            let move_id = self.allocate_move_id();
            let nonce = create_move_nonce(player_id, &target.id);
            prepared.push((move_id, player_id, uuid, level_id, nonce));
        }

        let mut outgoing = Vec::with_capacity(members.len());
        for (index, &(peer_key, player_id, _, _)) in members.iter().enumerate() {
            let move_id = prepared[index].0;
            let nonce = prepared[index].4;
            let packet = MoveGame {
                raw_bytes32: [0; 32],
                transfer: Move {
                    address: target.address,
                    move_nonce: nonce,
                    split_type: 0,
                    players: uuids.clone(),
                },
                u32_field: 0,
                u08_field: 0,
                u32_field2: 0,
                is_split: true,
                destination_uuid: target.uuid.unwrap_or_default(),
            };

            let bytes = packet
                .to_bytes()
                .ok_or_else(|| "failed to encode MoveGame".to_string())?;
            outgoing.push((
                peer_key,
                player_id,
                move_id,
                Packet::build_packet(PacketId::MoveGame, &bytes),
            ));
        }

        let mut sent_players = Vec::with_capacity(outgoing.len());
        for (peer_key, player_id, _, wire) in outgoing {
            let send_result = self
                .peers
                .get(&peer_key)
                .ok_or_else(|| format!("player not found: {player_id}"))?
                .send(0, &wire, true);

            if send_result < 0 {
                for sent_player in &sent_players {
                    self.send_cancel_move(*sent_player, 2);
                }
                return Err(format!("failed to send MoveGame to player {player_id}"));
            }
            sent_players.push(player_id);
        }

        let mut move_ids = Vec::with_capacity(prepared.len());
        for (index, (move_id, player_id, uuid, level_id, nonce)) in prepared.into_iter().enumerate()
        {
            let peer_key = members[index].0;
            self.pending_moves.insert(
                move_id,
                PendingMove {
                    move_id,
                    player_id,
                    player_uuid: uuid,
                    level_id,
                    room_id: target.id.clone(),
                    address: target.address,
                    move_nonce: nonce,
                    split: true,
                    created_at: Instant::now(),
                },
            );
            move_ids.push(move_id);

            self.emit_event(ServerEvent::MoveStarted {
                move_id,
                player_id,
                room_id: target.id.clone(),
            });

            if let Some(peer) = self.peers.get_mut(&peer_key) {
                peer.state = PeerState::Moving;
            }
        }

        for &(_, player_id, _, level_id) in &members {
            self.release_authority(player_id, level_id);
        }

        Ok(move_ids)
    }

    fn send_cancel_move(&self, player_id: u8, reason: u8) {
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

    fn return_to_active(&mut self, pending: &PendingMove) {
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

    fn allocate_move_id(&mut self) -> u64 {
        if self.next_move_id == 0 {
            self.next_move_id = 1;
        }
        let move_id = self.next_move_id;
        self.next_move_id = self.next_move_id.wrapping_add(1).max(1);
        move_id
    }

    fn find_active_peer_key(&self, player_id: u8) -> Option<usize> {
        self.peers
            .iter()
            .find(|(_, peer)| peer.player_id == player_id && peer.is_active())
            .map(|(key, _)| *key)
    }

    fn find_player_peer_key(&self, player_id: u8) -> Option<usize> {
        self.peers
            .iter()
            .find(|(_, peer)| peer.player_id == player_id && peer.is_player())
            .map(|(key, _)| *key)
    }

    pub(crate) fn rebalance_level(&mut self, level_id: u32) {
        let current = self.level_authority.get(&level_id).copied();
        let still_active = current.is_some_and(|player_id| {
            self.peers.values().any(|peer| {
                peer.player_id == player_id && peer.is_active() && peer.level_id == level_id
            })
        });
        if still_active {
            return;
        }

        self.level_authority.remove(&level_id);
        let next = self
            .peers
            .iter()
            .find(|(_, peer)| peer.is_active() && peer.level_id == level_id)
            .map(|(key, _)| *key);

        if let Some(peer_key) = next {
            self.send_elect_to(peer_key);
            if let Some(player_id) = self.level_authority.get(&level_id).copied() {
                self.emit_event(ServerEvent::AuthorityElected {
                    level_id,
                    player_id,
                });
            }
        }
    }

    /// Release authority before a player moves out of a level.
    pub(crate) fn release_authority(&mut self, player_id: u8, level_id: u32) {
        if self.level_authority.get(&level_id) == Some(&player_id) {
            self.level_authority.remove(&level_id);
            self.rebalance_level(level_id);
        }
    }

    /// Server-initiated authority revocation.
    pub(crate) fn revoke_authority(&mut self, level_id: u32, reason: u16) -> ControlResponse {
        let Some(authority) = self.level_authority.get(&level_id).copied() else {
            return ControlResponse::error(format!("no authority for level {level_id}"));
        };

        let payload = [authority, (reason & 0xff) as u8, (reason >> 8) as u8];

        let targets: Vec<(usize, u8)> = self
            .peers
            .iter()
            .filter(|(_, peer)| peer.is_active() && peer.level_id == level_id)
            .map(|(key, peer)| (*key, peer.lv_seq))
            .collect();

        for (peer_key, lv_seq) in targets {
            let msg = GameMsg::new(GameMsgId::NetLevelDataRevoke, lv_seq, authority, &payload);
            let wire = Packet::build_packet(PacketId::GameMsg, &msg.build_game_msg());
            if let Some(peer) = self.peers.get(&peer_key) {
                peer.send(0, &wire, true);
            }
        }

        self.level_authority.remove(&level_id);
        self.rebalance_level(level_id);
        ControlResponse::Ok
    }
}

fn create_move_nonce(player_id: u8, room_id: &str) -> u64 {
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|duration| duration.as_millis() as u64)
        .unwrap_or_default();
    (now << 16) ^ (u64::from(player_id) << 8) ^ (room_id.len() as u64 & 0xff)
}
