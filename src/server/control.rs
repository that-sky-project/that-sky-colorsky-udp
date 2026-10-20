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
                .map(|peer| PlayerInfo {
                    player_id: peer.player_id,
                    uuid: format!("{:?}", peer.uuid),
                    level_id: peer.level_id,
                })
                .collect(),
        }
    }

    fn control_kick_player(&mut self, player_id: u8, reason: &str) -> ControlResponse {
        let Some((uuid, level_id)) = self
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
                (peer.uuid, peer.level_id)
            })
        else {
            return ControlResponse::error(format!("player not found: {player_id}"));
        };

        self.emit_event(crate::control::ServerEvent::PlayerLeft {
            player_id,
            uuid,
            level_id,
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
    use crate::protocol::net_address::NetAddress;

    use super::*;

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
    fn move_with_unknown_room_is_rejected() {
        let mut state = ServerState::default();
        let response = state.handle_control(ControlCommand::MovePlayer {
            player_id: 1,
            room_id: "missing".to_string(),
        });

        assert!(!response.is_ok());
    }
}
