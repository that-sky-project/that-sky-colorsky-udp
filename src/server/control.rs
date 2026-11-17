//! Control command execution on the server thread.

use crate::control::{ControlCommand, ControlResponse, PlayerInfo, ServerStatus};
use crate::protocol::game_msg::{GameMsg, GameMsgId};
use crate::protocol::packet::Packet;
use crate::protocol::packet::PacketId;
use crate::protocol::packet::kick::Kick;

use super::peer::PeerState;
use super::state::ServerState;

impl ServerState {
    /// Execute one control command.
    pub(crate) fn handle_control(&mut self, command: ControlCommand) -> ControlResponse {
        match command {
            ControlCommand::MovePlayer { player_id, room_id } => {
                self.move_player(player_id, &room_id)
            }
            ControlCommand::MovePlayers {
                player_ids,
                room_id,
            } => self.move_players(player_ids, &room_id),
            ControlCommand::MovePlayersGuarded {
                player_ids,
                room_id,
                expected,
                deadline_ms,
            } => {
                if deadline_ms.is_some_and(|deadline| deadline <= super::peer::now_ms()) {
                    return ControlResponse::error("move delivery deadline expired");
                }
                if player_ids.is_empty()
                    || player_ids.len() > 7
                    || player_ids
                        .iter()
                        .collect::<std::collections::HashSet<_>>()
                        .len()
                        != player_ids.len()
                {
                    return ControlResponse::error("invalid guarded move group");
                }
                if !expected.is_empty()
                    && (expected.len() != player_ids.len()
                        || expected
                            .iter()
                            .map(|guard| guard.player_id)
                            .collect::<std::collections::HashSet<_>>()
                            .len()
                            != player_ids.len())
                {
                    return ControlResponse::error("incomplete source participant guards");
                }
                for guard in &expected {
                    if !player_ids.contains(&guard.player_id)
                        || !self.peers.values().any(|peer| {
                            peer.is_active()
                                && peer.player_id == guard.player_id
                                && format!("{:?}", peer.uuid) == guard.uuid
                                && peer.control_session_id == guard.session_id
                                && peer.level_id == guard.level_id
                                && peer.lv_seq == guard.lv_seq
                        })
                    {
                        return ControlResponse::error(
                            "source participant changed during move preparation",
                        );
                    }
                }
                let selected: std::collections::HashSet<u8> = player_ids.iter().copied().collect();
                for peer in self.peers.values().filter(|peer| peer.is_player()) {
                    let peer_selected = selected.contains(&peer.player_id);
                    for bond in &peer.affinity {
                        if bond.value < 230 {
                            continue;
                        }
                        let Some(target) = self.peers.values().find(|target| {
                            target.is_player()
                                && target.uuid == bond.uuid
                                && target.control_session_id == bond.session_id
                        }) else {
                            continue;
                        };
                        if peer_selected != selected.contains(&target.player_id) {
                            return ControlResponse::error(
                                "hard-bonded peer is outside the guarded move group",
                            );
                        }
                    }
                }
                self.move_players(player_ids, &room_id)
            }
            ControlCommand::MoveLevel { level_id, room_id } => self.move_level(level_id, &room_id),
            ControlCommand::CancelMove { player_id, reason } => {
                self.cancel_player_move(player_id, reason)
            }
            ControlCommand::KickPlayer { player_id, reason } => {
                self.control_kick_player(player_id, &reason)
            }
            ControlCommand::BroadcastRpc {
                level_id,
                player_id,
                data,
            } => self.control_broadcast_rpc(level_id, player_id, &data),
            ControlCommand::UpsertRoom { room } => {
                self.rooms.insert(room.id.clone(), room);
                ControlResponse::Ok
            }
            ControlCommand::RemoveRoom { room_id } => {
                self.rooms.remove(&room_id);
                ControlResponse::Ok
            }
            ControlCommand::RevokeAuthority { level_id, reason } => {
                self.revoke_authority(level_id, reason)
            }
            ControlCommand::SetDraining { draining } => {
                self.draining = draining;
                ControlResponse::Ok
            }
            ControlCommand::Status => ControlResponse::Status(self.status()),
            ControlCommand::Shutdown => ControlResponse::Ok,
        }
    }

    /// Snapshot used by control callers.
    pub(crate) fn status(&self) -> ServerStatus {
        ServerStatus {
            addr: self.bind_addr.clone(),
            peers: self.peers.len(),
            players: self.peers.values().filter(|peer| peer.is_player()).count(),
            pending_moves: self.pending_moves.len(),
            draining: self.draining,
            rooms: self.rooms.values().map(|room| room.to_info()).collect(),
            player_list: self
                .peers
                .values()
                .filter(|peer| peer.is_player())
                .map(|peer| {
                    let affinity: Vec<(String, f32)> = peer
                        .affinity
                        .iter()
                        .filter(|bond| {
                            self.peers.values().any(|target| {
                                target.is_player()
                                    && target.uuid == bond.uuid
                                    && target.control_session_id == bond.session_id
                            })
                        })
                        .map(|bond| (format!("{:?}", bond.uuid), f32::from(bond.value) / 255.0))
                        .collect();
                    PlayerInfo {
                        player_id: peer.player_id,
                        uuid: format!("{:?}", peer.uuid),
                        level_id: peer.level_id,
                        lv_seq: peer.lv_seq,
                        moving: peer.state == PeerState::Moving,
                        session_id: peer.control_session_id.clone(),
                        is_host: self.level_authority.get(&peer.level_id) == Some(&peer.player_id),
                        level_hash: self
                            .levels_data
                            .get(&peer.level_id)
                            .map(|level| level.header.level_hash),
                        position: peer.player_delta.position,
                        velocity: peer.player_delta.velocity,
                        state_at: peer.player_delta.state_at_ms,
                        last_move_at: peer.last_seen_at_ms,
                        affinity,
                    }
                })
                .collect(),
        }
    }

    fn control_kick_player(&mut self, player_id: u8, reason: &str) -> ControlResponse {
        let Some((uuid, level_id, session_id, lv_seq)) = self
            .peers
            .values_mut()
            .find(|peer| peer.player_id == player_id && peer.is_player())
            .map(|peer| {
                let payload = Kick {
                    reason: reason.to_string(),
                }
                .to_bytes();
                let wire = Packet::build_packet(PacketId::Kick, &payload);
                peer.send(0, &wire, true);
                peer.state = PeerState::Closed;
                peer.disconnect_later(0);
                (
                    peer.uuid,
                    peer.level_id,
                    peer.control_session_id.clone(),
                    peer.lv_seq,
                )
            })
        else {
            return ControlResponse::error(format!("player not found: {player_id}"));
        };

        self.emit_event(crate::control::ServerEvent::PlayerLeft {
            player_id,
            uuid,
            level_id,
            session_id,
            lv_seq,
        });

        ControlResponse::Ok
    }

    fn control_broadcast_rpc(
        &mut self,
        level_id: Option<u32>,
        player_id: u8,
        data: &[u8],
    ) -> ControlResponse {
        let mut payload = Vec::with_capacity(1 + data.len());
        payload.push(player_id);
        payload.extend_from_slice(data);

        let targets: Vec<(usize, u8)> = self
            .peers
            .iter()
            .filter(|(_, peer)| {
                peer.is_active() && level_id.is_none_or(|level| peer.level_id == level)
            })
            .map(|(key, peer)| (*key, peer.lv_seq))
            .collect();

        let mut sent = 0;
        for (peer_key, lv_seq) in targets {
            let msg = GameMsg::new(GameMsgId::NetRpc, lv_seq, 0, &payload);
            let wire = Packet::build_packet(PacketId::GameMsg, &msg.build_game_msg());
            if let Some(peer) = self.peers.get(&peer_key)
                && peer.send(0, &wire, true) >= 0
            {
                sent += 1;
            }
        }

        if sent == 0 {
            ControlResponse::error("no active recipients")
        } else {
            ControlResponse::Ok
        }
    }
}

#[cfg(test)]
mod tests {
    use std::net::{IpAddr, Ipv4Addr};

    use crate::control::RoomEndpoint;
    use crate::protocol::game_msg::{GameMsg, GameMsgId};
    use crate::protocol::net_address::NetAddress;
    use crate::protocol::types::TgcUuid;

    use super::*;

    fn active_peer(player_id: u8, uuid: TgcUuid) -> crate::server::peer::PeerEntry {
        let mut peer = crate::server::peer::PeerEntry::new(std::ptr::null_mut());
        peer.player_id = player_id;
        peer.uuid = uuid;
        peer.state = PeerState::Active;
        peer
    }

    /// Build a client `Affinity` payload: a compressed count (four bits, one
    /// byte for up to eight entries) followed by `(net_player_id, value)`.
    fn affinity_msg(entries: &[(u8, u8)]) -> GameMsg {
        let mut payload = vec![entries.len() as u8];
        for (net_player_id, value) in entries {
            payload.push(*net_player_id);
            payload.push(*value);
        }
        GameMsg::new(GameMsgId::Affinity, 0, 0, &payload)
    }

    fn player_of<'a>(status: &'a ServerStatus, uuid: &TgcUuid) -> &'a PlayerInfo {
        let text = format!("{uuid:?}");
        status
            .player_list
            .iter()
            .find(|player| player.uuid == text)
            .unwrap()
    }

    #[test]
    fn status_lists_registered_rooms() {
        let mut state = ServerState {
            bind_addr: "127.0.0.1:7777".to_string(),
            ..ServerState::default()
        };

        let room = RoomEndpoint::new(
            "room-1",
            NetAddress::new(IpAddr::V4(Ipv4Addr::new(127, 0, 0, 1)), 19133),
        );
        assert!(
            state
                .handle_control(ControlCommand::UpsertRoom { room })
                .is_ok()
        );

        let ControlResponse::Status(status) = state.handle_control(ControlCommand::Status) else {
            panic!("expected status response");
        };

        assert_eq!(status.addr, "127.0.0.1:7777");
        assert_eq!(status.rooms.len(), 1);
        assert_eq!(status.rooms[0].id, "room-1");
        assert_eq!(status.rooms[0].port, 19133);
    }

    #[test]
    fn status_uses_server_connection_identity_instead_of_client_token() {
        let mut state = ServerState::default();
        let mut peer = crate::server::peer::PeerEntry::new(std::ptr::null_mut());
        peer.player_id = 1;
        peer.state = PeerState::Active;
        peer.conn_magic = Some(7);
        let identity = peer.control_session_id.clone();
        state.peers.insert(1, peer);
        let status = state.status();
        assert_eq!(status.player_list[0].session_id, identity);
        assert_ne!(identity, "7");
    }

    #[test]
    fn guarded_execution_refuses_replacement_session_generation_and_expired_delivery() {
        let mut state = ServerState::default();
        let mut peer = crate::server::peer::PeerEntry::new(std::ptr::null_mut());
        peer.player_id = 1;
        peer.state = PeerState::Active;
        peer.level_id = 7;
        peer.lv_seq = 2;
        let session_id = peer.control_session_id.clone();
        state.peers.insert(1, peer);
        for expected in [
            crate::control::ExpectedPlayerGuard {
                player_id: 1,
                uuid: format!("{:?}", state.peers[&1].uuid),
                session_id: "old".into(),
                level_id: 7,
                lv_seq: 2,
            },
            crate::control::ExpectedPlayerGuard {
                player_id: 1,
                uuid: format!("{:?}", state.peers[&1].uuid),
                session_id: session_id.clone(),
                level_id: 6,
                lv_seq: 2,
            },
            crate::control::ExpectedPlayerGuard {
                player_id: 1,
                uuid: format!("{:?}", state.peers[&1].uuid),
                session_id: session_id.clone(),
                level_id: 7,
                lv_seq: 1,
            },
        ] {
            let response = state.handle_control(ControlCommand::MovePlayersGuarded {
                player_ids: vec![1],
                room_id: "target".into(),
                expected: vec![expected],
                deadline_ms: None,
            });
            assert!(
                matches!(response, ControlResponse::Error { message } if message.contains("participant changed"))
            );
        }
        let response = state.handle_control(ControlCommand::MovePlayersGuarded {
            player_ids: vec![1],
            room_id: "target".into(),
            expected: vec![],
            deadline_ms: Some(1),
        });
        assert!(
            matches!(response, ControlResponse::Error { message } if message.contains("deadline expired"))
        );
        assert_eq!(state.pending_moves.len(), 0);
        assert!(state.peers[&1].is_active());
    }

    #[test]
    fn move_with_unknown_room_is_rejected() {
        let mut state = ServerState::default();
        let response = state.handle_control(ControlCommand::MovePlayer {
            player_id: 1,
            room_id: "missing".to_string(),
        });

        assert!(!response.is_ok());
    }

    #[test]
    fn affinity_freezes_bonded_uuid_and_survives_net_id_reuse() {
        let mut state = ServerState::default();
        let first = TgcUuid::from(&[1u8; 16]).unwrap();
        let second = TgcUuid::from(&[2u8; 16]).unwrap();
        let replacement = TgcUuid::from(&[3u8; 16]).unwrap();
        state.peers.insert(1, active_peer(1, first));
        state.peers.insert(2, active_peer(2, second));

        assert!(
            state
                .handle_affinity(1, affinity_msg(&[(2, 230)]))
                .is_some()
        );
        let status = state.status();
        assert_eq!(
            player_of(&status, &first).affinity,
            vec![(format!("{second:?}"), 230.0f32 / 255.0)]
        );

        // Player 2 leaves and a different client takes native id 2. The frozen
        // bond must still point at the original uuid, not the newcomer's.
        state.peers.remove(&2);
        state.peers.insert(2, active_peer(2, replacement));
        let status = state.status();
        assert_eq!(
            player_of(&status, &first).affinity,
            vec![(format!("{second:?}"), 230.0f32 / 255.0)]
        );
    }

    #[test]
    fn affinity_entries_without_a_connected_peer_are_not_frozen() {
        let mut state = ServerState::default();
        let first = TgcUuid::from(&[1u8; 16]).unwrap();
        state.peers.insert(1, active_peer(1, first));

        // Native id 5 belongs to nobody; deferring the entry would let a later
        // id reuse misattribute the bond, so it is dropped instead.
        assert!(
            state
                .handle_affinity(1, affinity_msg(&[(5, 255)]))
                .is_some()
        );
        let status = state.status();
        assert!(player_of(&status, &first).affinity.is_empty());

        // A second update replaces the whole frozen set.
        let second = TgcUuid::from(&[2u8; 16]).unwrap();
        state.peers.insert(2, active_peer(2, second));
        assert!(state.handle_affinity(1, affinity_msg(&[(2, 10)])).is_some());
        let status = state.status();
        assert_eq!(
            player_of(&status, &first).affinity,
            vec![(format!("{second:?}"), 10.0f32 / 255.0)]
        );
    }

    #[test]
    fn host_is_earliest_active_player_and_hands_over_automatically() {
        let mut state = ServerState::default();
        let first = TgcUuid::from(&[1u8; 16]).unwrap();
        let second = TgcUuid::from(&[2u8; 16]).unwrap();
        let mut host = active_peer(1, first);
        host.joined_at = 1_000;
        let mut joining = active_peer(2, second);
        joining.joined_at = 2_000;
        state.peers.insert(1, host);
        state.peers.insert(2, joining);

        let status = state.status();
        assert!(player_of(&status, &first).is_host);
        assert!(!player_of(&status, &second).is_host);

        // A replacement session for the same client is a new peer entry with a
        // later registration time, so the older player keeps the host role.
        let mut replacement = active_peer(2, second);
        replacement.joined_at = 3_000;
        state.peers.remove(&2);
        state.peers.insert(2, replacement);
        let status = state.status();
        assert!(player_of(&status, &first).is_host);
        assert!(!player_of(&status, &second).is_host);

        // The host departs: the remaining player takes over on the next
        // snapshot without any explicit handover step.
        state.peers.remove(&1);
        let status = state.status();
        assert!(player_of(&status, &second).is_host);
    }

    #[test]
    fn moving_player_is_never_host() {
        let mut state = ServerState::default();
        let first = TgcUuid::from(&[1u8; 16]).unwrap();
        let second = TgcUuid::from(&[2u8; 16]).unwrap();
        let mut migrating = active_peer(1, first);
        migrating.joined_at = 500;
        migrating.state = PeerState::Moving;
        let mut staying = active_peer(2, second);
        staying.joined_at = 600;
        state.peers.insert(1, migrating);
        state.peers.insert(2, staying);

        let status = state.status();
        assert!(!player_of(&status, &first).is_host);
        assert!(player_of(&status, &second).is_host);
    }

    #[test]
    fn host_tie_breaks_to_lowest_player_id() {
        let mut state = ServerState::default();
        let fifth = TgcUuid::from(&[5u8; 16]).unwrap();
        let third = TgcUuid::from(&[3u8; 16]).unwrap();
        let mut a = active_peer(5, fifth);
        a.joined_at = 700;
        let mut b = active_peer(3, third);
        b.joined_at = 700;
        state.peers.insert(5, a);
        state.peers.insert(3, b);

        let status = state.status();
        assert!(player_of(&status, &third).is_host);
        assert!(!player_of(&status, &fifth).is_host);
    }
}
