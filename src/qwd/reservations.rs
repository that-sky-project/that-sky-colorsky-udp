//! Capacity holds sent by QWD for allocations and migrations.

use std::collections::{HashMap, HashSet};
use std::time::{Duration, Instant};

#[derive(Debug, Clone)]
struct PlayerReservation {
    id: String,
    expires_at: Instant,
}

#[derive(Debug, Clone)]
struct MoveReservation {
    /// Original group, kept for `move.completed` after every member joined.
    player_ids: Vec<String>,
    /// Members that have not joined yet. Capacity accounting uses this set so
    /// players who already joined are not counted twice.
    pending: HashSet<String>,
    expires_at: Instant,
}

/// Tracks QWD reservations until the corresponding players join or the TTL
/// expires. Reservations are counted as effective occupancy so a burst of
/// allocations cannot over-fill the room.
#[derive(Debug, Default)]
pub struct Reservations {
    players: HashMap<String, PlayerReservation>,
    moves: HashMap<String, MoveReservation>,
    next_id: u64,
}

impl Reservations {
    pub fn clear(&mut self) {
        self.players.clear();
        self.moves.clear();
    }

    /// Drop expired holds. Returns the move reservations that expired so the
    /// caller can publish `reservation.expired` to the manager.
    pub fn expire(&mut self, now: Instant) -> Vec<(String, Vec<String>)> {
        self.players
            .retain(|_, reservation| reservation.expires_at > now);

        let mut expired = Vec::new();
        self.moves.retain(|move_id, reservation| {
            if reservation.expires_at > now {
                true
            } else {
                expired.push((move_id.clone(), reservation.player_ids.clone()));
                false
            }
        });
        expired
    }

    /// Unique player ids currently reserved by any hold.
    pub fn reserved_players(&self, now: Instant) -> HashSet<String> {
        let mut reserved = HashSet::new();
        for (player_id, reservation) in &self.players {
            if reservation.expires_at > now {
                reserved.insert(player_id.clone());
            }
        }
        for reservation in self.moves.values() {
            if reservation.expires_at > now {
                reserved.extend(reservation.pending.iter().cloned());
            }
        }
        reserved
    }

    pub fn count(&self, now: Instant) -> usize {
        self.reserved_players(now).len()
    }

    pub fn reserve_player(&mut self, player_id: &str, ttl: Duration, now: Instant) -> String {
        if let Some(existing) = self.players.get_mut(player_id) {
            existing.expires_at = now + ttl;
            return existing.id.clone();
        }

        self.next_id = self.next_id.wrapping_add(1).max(1);
        let id = format!("allocation-{player_id}-{}", self.next_id);
        self.players.insert(
            player_id.to_string(),
            PlayerReservation {
                id: id.clone(),
                expires_at: now + ttl,
            },
        );
        id
    }

    /// Reserve a group for a move. Returns `Err(())` when the same move id is
    /// reused with a different group or when a player already belongs to
    /// another move hold.
    pub fn reserve_move(
        &mut self,
        move_id: &str,
        player_ids: &[String],
        ttl: Duration,
        now: Instant,
    ) -> Result<(), ()> {
        let requested: HashSet<&String> = player_ids.iter().collect();

        if let Some(existing) = self.moves.get_mut(move_id) {
            let existing_set: HashSet<&String> = existing.player_ids.iter().collect();
            if existing_set != requested {
                return Err(());
            }
            existing.expires_at = now + ttl;
            return Ok(());
        }

        for (other_id, reservation) in &self.moves {
            if reservation.expires_at <= now {
                continue;
            }
            if reservation
                .player_ids
                .iter()
                .any(|player_id| requested.contains(player_id))
            {
                tracing::warn!(move_id, other_id, "overlapping QWD move reservations");
                return Err(());
            }
        }

        self.moves.insert(
            move_id.to_string(),
            MoveReservation {
                player_ids: player_ids.to_vec(),
                pending: player_ids.iter().cloned().collect(),
                expires_at: now + ttl,
            },
        );
        Ok(())
    }

    pub fn release_move(&mut self, move_id: &str) {
        self.moves.remove(move_id);
    }

    /// A player actually joined; drop the corresponding allocation and move
    /// holds so occupancy is no longer double-counted.
    ///
    /// Returns every move whose whole group has now joined, so the caller can
    /// publish `move.completed` with the original player list.
    pub fn confirm_player(&mut self, player_id: &str) -> Vec<(String, Vec<String>)> {
        self.players.remove(player_id);

        let mut completed = Vec::new();
        for (move_id, reservation) in &mut self.moves {
            reservation.pending.remove(player_id);
            if reservation.pending.is_empty() {
                completed.push((move_id.clone(), reservation.player_ids.clone()));
            }
        }
        for (move_id, _) in &completed {
            self.moves.remove(move_id);
        }
        completed
    }

    #[cfg(test)]
    pub fn move_count(&self) -> usize {
        self.moves.len()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn player_reservation_is_idempotent_and_expires() {
        let mut reservations = Reservations::default();
        let now = Instant::now();
        let id = reservations.reserve_player("player-1", Duration::from_secs(5), now);
        let again = reservations.reserve_player("player-1", Duration::from_secs(5), now);

        assert_eq!(id, again);
        assert_eq!(reservations.count(now), 1);
        assert_eq!(reservations.count(now + Duration::from_secs(6)), 0);
    }

    #[test]
    fn move_reservation_counts_unique_players() {
        let mut reservations = Reservations::default();
        let now = Instant::now();
        reservations.reserve_player("player-1", Duration::from_secs(5), now);
        reservations
            .reserve_move(
                "move-1",
                &["player-1".to_string(), "player-2".to_string()],
                Duration::from_secs(5),
                now,
            )
            .unwrap();

        assert_eq!(reservations.count(now), 2);
    }

    #[test]
    fn conflicting_move_is_rejected() {
        let mut reservations = Reservations::default();
        let now = Instant::now();
        reservations
            .reserve_move(
                "move-1",
                &["player-1".to_string()],
                Duration::from_secs(5),
                now,
            )
            .unwrap();
        assert!(
            reservations
                .reserve_move(
                    "move-2",
                    &["player-1".to_string()],
                    Duration::from_secs(5),
                    now
                )
                .is_err()
        );
    }

    #[test]
    fn confirm_releases_all_holds() {
        let mut reservations = Reservations::default();
        let now = Instant::now();
        reservations.reserve_player("player-1", Duration::from_secs(5), now);
        reservations
            .reserve_move(
                "move-1",
                &["player-1".to_string()],
                Duration::from_secs(5),
                now,
            )
            .unwrap();

        let completed = reservations.confirm_player("player-1");

        assert_eq!(reservations.count(now), 0);
        assert_eq!(reservations.move_count(), 0);
        assert_eq!(
            completed,
            vec![("move-1".to_string(), vec!["player-1".to_string()])]
        );
    }

    #[test]
    fn confirm_reports_completed_move_with_original_group() {
        let mut reservations = Reservations::default();
        let now = Instant::now();
        reservations
            .reserve_move(
                "move-1",
                &["player-1".to_string(), "player-2".to_string()],
                Duration::from_secs(5),
                now,
            )
            .unwrap();

        assert!(reservations.confirm_player("player-1").is_empty());
        assert_eq!(reservations.count(now), 1);

        let completed = reservations.confirm_player("player-2");
        assert_eq!(
            completed,
            vec![(
                "move-1".to_string(),
                vec!["player-1".to_string(), "player-2".to_string()]
            )]
        );
        assert_eq!(reservations.move_count(), 0);
    }

    #[test]
    fn expire_reports_move_reservations() {
        let mut reservations = Reservations::default();
        let now = Instant::now();
        reservations
            .reserve_move(
                "move-1",
                &["player-1".to_string()],
                Duration::from_secs(5),
                now,
            )
            .unwrap();

        let expired = reservations.expire(now + Duration::from_secs(6));
        assert_eq!(
            expired,
            vec![("move-1".to_string(), vec!["player-1".to_string()])]
        );
    }
}
