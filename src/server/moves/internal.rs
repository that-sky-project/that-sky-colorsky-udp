//! Internal move primitives: packet construction, ids and authority changes.

use std::time::Instant;

use crate::control::{ControlResponse, RoomEndpoint, ServerEvent};
use crate::protocol::game_msg::{GameMsg, GameMsgId};
use crate::protocol::packet::Packet;
use crate::protocol::packet::PacketId;
use crate::protocol::packet::move_game::{Move, MoveGame, NO_EVENT_RESERVATION};
use crate::protocol::types::TgcUuid;

use super::super::peer::PeerState;
use super::super::state::{PendingMove, ServerState};

impl ServerState {
    pub(super) fn begin_single_move(
        &mut self,
        player_id: u8,
        target: &RoomEndpoint,
    ) -> Result<u64, String> {
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
            u32_field2: NO_EVENT_RESERVATION,
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

        // Log every redirect: a client that receives a second MoveGame while it
        // is still finishing the previous one aborts the move (ENet disconnect
        // data 33), so this is the first thing to check when a moved player
        // drops right after joining.
        tracing::info!(
            "[ENET:MOVE] sent MoveGame player={} move_id={} uuid={:?} level={:#x} room={} destination={} destination_uuid={:?}",
            player_id,
            move_id,
            uuid,
            level_id,
            target.id,
            target.address,
            target.uuid
        );

        if let Some(peer) = self.peers.get_mut(&peer_key) {
            peer.state = PeerState::Moving;
        }

        self.release_authority(player_id, level_id);
        self.emit_event(ServerEvent::MoveStarted {
            move_id,
            player_id,
            player_uuid: uuid,
            room_id: target.id.clone(),
        });

        Ok(move_id)
    }

    pub(super) fn begin_group_move(
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
                u32_field2: NO_EVENT_RESERVATION,
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
            tracing::info!(
                "[ENET:MOVE] sent MoveGame player={} move_id={} uuid={:?} level={:#x} group={} room={} destination={} destination_uuid={:?}",
                player_id,
                move_id,
                uuid,
                level_id,
                player_ids.len(),
                target.id,
                target.address,
                target.uuid
            );
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
                player_uuid: uuid,
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

    pub(super) fn allocate_move_id(&mut self) -> u64 {
        if self.next_move_id == 0 {
            self.next_move_id = 1;
        }
        let move_id = self.next_move_id;
        self.next_move_id = self.next_move_id.wrapping_add(1).max(1);
        move_id
    }

    pub(super) fn find_active_peer_key(&self, player_id: u8) -> Option<usize> {
        self.peers
            .iter()
            .find(|(_, peer)| peer.player_id == player_id && peer.is_active())
            .map(|(key, _)| *key)
    }

    pub(super) fn find_player_peer_key(&self, player_id: u8) -> Option<usize> {
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
