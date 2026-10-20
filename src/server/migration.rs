//! Migration roster: tracks a group that arrived through a move.

use std::collections::HashSet;
use std::time::Instant;

use crate::protocol::packet::move_game::Move;
use crate::protocol::types::TgcUuid;

use super::state::{
    MAX_PLAYERS, MIGRATION_ROSTER_TIMEOUT, MigrationEntry, MigrationRoster, ServerState,
};

impl ServerState {
    /// Reserve slots for a `JoinGame` that carries a move group.
    ///
    /// Returns `(own_net_id, roster_key, roster_entries)` when the join belongs
    /// to a migration roster.
    pub(crate) fn claim_migration_roster(
        &mut self,
        uuid: TgcUuid,
        level_id: u32,
        transfer: &Move,
        moving: bool,
    ) -> Result<Option<(u8, String, Vec<MigrationEntry>)>, String> {
        if !moving || transfer.players.is_empty() || !transfer.players.contains(&uuid) {
            return Ok(None);
        }

        let key = roster_key(&transfer.players);

        if let Some(roster) = self.migration_rosters.get_mut(&key) {
            roster.expires_at = Instant::now() + MIGRATION_ROSTER_TIMEOUT;
            let entries = roster.entries.clone();
            let Some(entry) = entries.iter().find(|entry| entry.uuid == uuid) else {
                return Ok(None);
            };
            return Ok(Some((entry.net_id, key, entries)));
        }

        let active_uuids: HashSet<TgcUuid> = self
            .peers
            .values()
            .filter(|peer| peer.is_player())
            .map(|peer| peer.uuid)
            .collect();

        let new_players: Vec<TgcUuid> = transfer
            .players
            .iter()
            .copied()
            .filter(|player_uuid| !active_uuids.contains(player_uuid))
            .collect();

        if new_players.is_empty() {
            return Ok(None);
        }

        let projected = self.player_count() + self.reserved_ids.len() + new_players.len();
        if projected > MAX_PLAYERS {
            return Err(format!(
                "migration roster exceeds room capacity: {projected}/{MAX_PLAYERS}"
            ));
        }

        let mut entries: Vec<MigrationEntry> = Vec::with_capacity(new_players.len());
        for player_uuid in new_players {
            let Some(net_id) = self.allocate_player_id() else {
                for entry in &entries {
                    self.reserved_ids.remove(&entry.net_id);
                }
                return Err("no player ids available".to_string());
            };
            self.reserved_ids.insert(net_id);
            entries.push(MigrationEntry {
                net_id,
                uuid: player_uuid,
                level_id,
            });
        }

        let Some(own_entry) = entries.iter().find(|entry| entry.uuid == uuid).cloned() else {
            for entry in &entries {
                self.reserved_ids.remove(&entry.net_id);
            }
            return Ok(None);
        };

        self.migration_rosters.insert(
            key.clone(),
            MigrationRoster {
                key: key.clone(),
                entries: entries.clone(),
                expires_at: Instant::now() + MIGRATION_ROSTER_TIMEOUT,
            },
        );

        Ok(Some((own_entry.net_id, key, entries)))
    }

    /// Release the roster once every member is active.
    pub(crate) fn complete_migration_roster(&mut self, key: &str) {
        let Some(roster) = self.migration_rosters.get(key) else {
            return;
        };

        let active_uuids: HashSet<TgcUuid> = self
            .peers
            .values()
            .filter(|peer| peer.is_player())
            .map(|peer| peer.uuid)
            .collect();

        if !roster
            .entries
            .iter()
            .all(|entry| active_uuids.contains(&entry.uuid))
        {
            return;
        }

        if let Some(roster) = self.migration_rosters.remove(key) {
            for entry in roster.entries {
                self.reserved_ids.remove(&entry.net_id);
            }
        }
    }

    /// Expire incomplete rosters and release their reserved ids.
    pub(crate) fn expire_migration_rosters(&mut self, now: Instant) {
        let expired: Vec<String> = self
            .migration_rosters
            .iter()
            .filter(|(_, roster)| roster.expires_at <= now)
            .map(|(key, _)| key.clone())
            .collect();

        for key in expired {
            if let Some(roster) = self.migration_rosters.remove(&key) {
                for entry in roster.entries {
                    self.reserved_ids.remove(&entry.net_id);
                }
            }
        }
    }

    /// Number of connected players (including sessions in MOVING and peers
    /// waiting for their ENet disconnect after a kick).
    pub(crate) fn player_count(&self) -> usize {
        self.peers
            .values()
            .filter(|peer| peer.occupies_slot())
            .count()
    }

    /// Allocate an unused player id, skipping reserved migration slots.
    pub(crate) fn allocate_player_id(&mut self) -> Option<u8> {
        if self.net_id_count == 0 {
            self.net_id_count = 1;
        }

        for _ in 0..255 {
            let id = self.net_id_count;
            self.net_id_count = if id == 255 { 1 } else { id + 1 };

            if id == 0 {
                continue;
            }
            if self.reserved_ids.contains(&id)
                || self
                    .peers
                    .values()
                    .any(|peer| peer.player_id == id && peer.occupies_slot())
            {
                continue;
            }
            return Some(id);
        }

        None
    }
}

fn roster_key(players: &[TgcUuid]) -> String {
    let mut keys: Vec<String> = players.iter().map(|uuid| hex::encode(uuid.raw())).collect();
    keys.sort();
    keys.join(",")
}

#[cfg(test)]
mod tests {
    use std::net::{IpAddr, Ipv4Addr};

    use crate::protocol::net_address::NetAddress;
    use crate::protocol::packet::move_game::{MAX_MOVE_PLAYERS, Move};

    use super::*;

    fn uuid(byte: u8) -> TgcUuid {
        TgcUuid::from(&[byte; 16]).unwrap()
    }

    fn transfer(players: Vec<TgcUuid>) -> Move {
        Move {
            address: NetAddress::new(IpAddr::V4(Ipv4Addr::LOCALHOST), 0),
            move_nonce: 0,
            split_type: 0,
            players,
        }
    }

    #[test]
    fn reserves_and_expires_roster() {
        let mut state = ServerState::default();
        let players = vec![uuid(1), uuid(2)];

        let (net_id, key, entries) = state
            .claim_migration_roster(players[0], 42, &transfer(players.clone()), true)
            .unwrap()
            .unwrap();

        assert_eq!(entries.len(), 2);
        assert!(state.reserved_ids.contains(&net_id));
        assert_eq!(state.reserved_ids.len(), 2);
        assert_eq!(state.migration_rosters.len(), 1);

        let expires_at = state.migration_rosters.get(&key).unwrap().expires_at;
        state.expire_migration_rosters(expires_at + MIGRATION_ROSTER_TIMEOUT);

        assert!(state.migration_rosters.is_empty());
        assert!(state.reserved_ids.is_empty());
    }

    #[test]
    fn non_moving_join_does_not_create_roster() {
        let mut state = ServerState::default();
        let players = vec![uuid(1)];

        let roster = state
            .claim_migration_roster(players[0], 1, &transfer(players), false)
            .unwrap();

        assert!(roster.is_none());
        assert!(state.reserved_ids.is_empty());
    }

    #[test]
    fn roster_rejects_room_overflow() {
        let mut state = ServerState::default();
        state.reserved_ids.insert(1);
        state.reserved_ids.insert(2);

        let players: Vec<TgcUuid> = (1..=MAX_MOVE_PLAYERS as u8).map(uuid).collect();
        let result = state.claim_migration_roster(players[0], 1, &transfer(players), true);

        assert!(result.is_err());
    }
}
