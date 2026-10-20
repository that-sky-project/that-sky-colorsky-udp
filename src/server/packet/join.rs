use std::collections::HashSet;

use crate::control::ServerEvent;
use crate::protocol::packet::Packet;
use crate::protocol::packet::PacketId;
use crate::protocol::packet::enter_game::{EnterGame, EnterPlayer};
use crate::protocol::packet::join_game::JoinGame;

use crate::server::peer::PeerState;
use crate::server::state::{MAX_PLAYERS, MigrationEntry, ServerState};

impl ServerState {
    /// Handle a `JoinGame` packet.
    ///
    /// The client token is already validated by `on_receive`. This function
    /// assigns a player id, handles a migration roster when the join carries a
    /// move group, and broadcasts the resulting `EnterGame` roster.
    pub(super) fn handle_join_game(&mut self, peer_key: usize, payload: &[u8]) -> Option<()> {
        let join = JoinGame::from_bytes(payload)?;

        let peer_state = self.peers.get(&peer_key)?.state;
        if peer_state != PeerState::Pending {
            tracing::warn!(
                peer = ?peer_key,
                state = ?peer_state,
                "ignored JoinGame outside Pending state"
            );
            return None;
        }

        if self.draining {
            tracing::warn!(uuid = ?join.uuid, "rejected JoinGame: node is draining");
            return None;
        }

        if self
            .peers
            .iter()
            .any(|(key, peer)| *key != peer_key && peer.is_player() && peer.uuid == join.uuid)
        {
            tracing::warn!(uuid = ?join.uuid, "ignored JoinGame for an active uuid");
            return None;
        }

        let roster = match self.claim_migration_roster(
            join.uuid,
            join.level_id,
            &join.transfer,
            join.moving,
        ) {
            Ok(roster) => roster,
            Err(message) => {
                tracing::warn!(uuid = ?join.uuid, error = %message, "rejected migration join");
                return None;
            }
        };

        if roster.is_none() && self.player_count() + self.reserved_ids.len() >= MAX_PLAYERS {
            tracing::warn!(
                uuid = ?join.uuid,
                players = self.player_count(),
                reserved = self.reserved_ids.len(),
                "rejected JoinGame: room is full"
            );
            return None;
        }

        let roster_net_id = roster.as_ref().map(|(net_id, _, _)| *net_id);
        let roster_key = roster.as_ref().map(|(_, key, _)| key.clone());
        let roster_entries = roster
            .as_ref()
            .map(|(_, _, entries)| entries.clone())
            .unwrap_or_default();

        let player_id = if let Some(net_id) = roster_net_id {
            self.reserved_ids.remove(&net_id);
            net_id
        } else {
            match self.allocate_player_id() {
                Some(player_id) => player_id,
                None => {
                    tracing::warn!(uuid = ?join.uuid, "rejected JoinGame: no player ids");
                    return None;
                }
            }
        };

        {
            let peer = self.peers.get_mut(&peer_key)?;
            peer.player_id = player_id;
            peer.uuid = join.uuid;
            peer.level_id = join.level_id;
            peer.lv_seq = join.level_change_count;
            peer.net_version = join.net_version;
            peer.state = PeerState::Active;
        }

        let enter_game = self.build_enter_game(&roster_entries);
        if let Some(bytes) = enter_game.to_bytes() {
            let packet = Packet::build_packet(PacketId::EnterGame, &bytes);
            self.broadcast_all(0, &packet, true);
        } else {
            tracing::warn!(
                uuid = ?join.uuid,
                players = enter_game.players.len(),
                "failed to encode EnterGame roster"
            );
        }

        // Tell the joiner which authority owns the level (and create one if
        // this is the first player in the level).
        self.send_elect_to(peer_key);

        if let Some(key) = roster_key {
            self.complete_migration_roster(&key);
        }

        self.emit_event(ServerEvent::PlayerJoined {
            player_id,
            uuid: join.uuid,
            level_id: join.level_id,
        });

        tracing::info!(
            player_id,
            uuid = ?join.uuid,
            level_id = join.level_id,
            lv_seq = join.level_change_count,
            moving = join.moving,
            move_players = join.transfer.players.len(),
            "player joined"
        );

        Some(())
    }

    fn build_enter_game(&self, roster_entries: &[MigrationEntry]) -> EnterGame {
        let mut seen = HashSet::new();
        let mut players = Vec::with_capacity(MAX_PLAYERS);

        for entry in roster_entries {
            if seen.insert(entry.uuid) {
                players.push(EnterPlayer {
                    net_id: entry.net_id,
                    uuid: entry.uuid,
                    level_id: entry.level_id,
                });
            }
        }

        for peer in self.peers.values().filter(|peer| peer.is_player()) {
            if seen.insert(peer.uuid) {
                players.push(EnterPlayer {
                    net_id: peer.player_id,
                    uuid: peer.uuid,
                    level_id: peer.level_id,
                });
            }
        }

        EnterGame { players }
    }
}
