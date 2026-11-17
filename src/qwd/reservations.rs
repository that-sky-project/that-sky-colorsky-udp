//! Capacity holds sent by QWD for allocations and migrations.
//!
//! Every hold carries a `reservationToken`. The manager echoes it back on
//! `move.commit`/`move.cancel` and the node must echo it on the settle events
//! (`move.result`, `move.completed`, `reservation.expired`); a transaction whose
//! token does not match is ignored by the manager, so losing the token would
//! leave every move open until its timeout.

use std::collections::{HashMap, HashSet};
use std::time::{Duration, Instant};

use crate::control::ServerStatus;

/// Number of physical players currently holding a slot in the room.
#[derive(Debug, Clone)]
struct PlayerReservation {
    token: String,
    expires_at: Instant,
}

#[derive(Debug, Clone)]
struct MoveReservation {
    /// Original group, kept for `move.completed` after every member joined.
    player_ids: Vec<String>,
    /// Members that have not joined yet. Capacity accounting uses this set so
    /// players who already joined are not counted twice.
    pending: HashSet<String>,
    /// Members that already joined, reported as `admittedPlayerIds`.
    joined: HashSet<String>,
    expires_at: Instant,
    token: String,
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

/// A move hold that expired before its group arrived.
#[derive(Debug, Clone)]
pub struct ExpiredMove {
    pub move_id: String,
    pub player_ids: Vec<String>,
    pub admitted_player_ids: Vec<String>,
    pub token: String,
}

/// A move whose whole group has landed.
#[derive(Debug, Clone)]
pub struct CompletedMove {
    pub move_id: String,
    pub player_ids: Vec<String>,
    pub token: String,
}

/// Occupancy facts appended to command acks and room lifecycle events. The
/// manager validates and applies them to its cached room snapshot, so a stale
/// planner view corrects itself within one round trip.
#[derive(Debug, Clone, Copy)]
pub struct CapacityFacts {
    pub physical_players: usize,
    pub active_players: usize,
    pub reserved_slots: usize,
    pub capacity: usize,
    pub version: u64,
}

impl Reservations {
    pub fn clear(&mut self) {
        self.players.clear();
        self.moves.clear();
    }

    /// Preserve business leases across a short socket loss without extending them.
    pub fn cap_transport_loss(&mut self, now: Instant, grace: Duration) {
        let limit = now + grace;
        for hold in self.players.values_mut() {
            hold.expires_at = hold.expires_at.min(limit);
        }
        for hold in self.moves.values_mut() {
            hold.expires_at = hold.expires_at.min(limit);
        }
    }

    #[allow(dead_code)] // retained for targeted partial-cancellation follow-up
    pub fn cancel_members(&mut self, move_id: &str, player_ids: &[String]) {
        if let Some(hold) = self.moves.get_mut(move_id) {
            for id in player_ids {
                hold.pending.remove(id);
            }
            if hold.pending.is_empty() {
                self.moves.remove(move_id);
            }
        }
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

    /// Slots held by reservations whose players have not arrived yet.
    pub fn outstanding_slots(&self, now: Instant) -> usize {
        self.moves
            .values()
            .filter(|reservation| reservation.expires_at > now)
            .map(|reservation| reservation.pending.len())
            .sum::<usize>()
            + self
                .players
                .values()
                .filter(|reservation| reservation.expires_at > now)
                .count()
    }

    /// Occupancy facts derived from the live status and this hold set.
    /// `version` is the node's telemetry sequence; the manager ignores facts
    /// older than its cached snapshot, so the seq must keep increasing.
    ///
    /// `reserved_slots` is clamped to the manager's invariant
    /// `physical + reserved <= capacity`: when the room is physically full, a
    /// hold whose player has not shown up yet must not make the facts invalid,
    /// or the manager rejects every telemetry frame and spams `room.sync`.
    pub fn facts(
        &self,
        status: &ServerStatus,
        capacity: usize,
        version: u64,
        now: Instant,
    ) -> CapacityFacts {
        let physical_players = status.player_list.len();
        let reserved = self
            .outstanding_slots(now)
            .min(capacity.saturating_sub(physical_players));
        CapacityFacts {
            physical_players,
            active_players: status
                .player_list
                .iter()
                .filter(|player| !player.moving)
                .count(),
            reserved_slots: reserved,
            capacity,
            version,
        }
    }

    fn next_token(&mut self, kind: &str) -> String {
        self.next_id = self.next_id.wrapping_add(1).max(1);
        let random: u64 = rand::random();
        format!("{kind}-{}-{:016x}", self.next_id, random)
    }

    pub fn reserve_player(&mut self, player_id: &str, ttl: Duration, now: Instant) -> String {
        self.players.retain(|_, hold| hold.expires_at > now);
        if let Some(existing) = self.players.get_mut(player_id) {
            existing.expires_at = now + ttl;
            return existing.token.clone();
        }

        let token = self.next_token("alloc");
        self.players.insert(
            player_id.to_string(),
            PlayerReservation {
                token: token.clone(),
                expires_at: now + ttl,
            },
        );
        token
    }

    /// Reserve a group for a move. Returns the hold's token; renewing an
    /// existing move returns its original token so the manager's transaction
    /// keeps matching. Errors when the same move id is reused with a different
    /// group, when a player already belongs to another move hold, or when the
    /// caller presents a foreign token for an existing hold.
    pub fn reserve_move(
        &mut self,
        move_id: &str,
        player_ids: &[String],
        ttl: Duration,
        now: Instant,
        requested_token: Option<&str>,
    ) -> Result<String, ReserveMoveError> {
        self.moves.retain(|_, hold| hold.expires_at > now);
        let requested: HashSet<&String> = player_ids.iter().collect();

        if let Some(existing) = self.moves.get_mut(move_id) {
            let existing_set: HashSet<&String> = existing.player_ids.iter().collect();
            if existing_set != requested {
                return Err(ReserveMoveError::Conflict);
            }
            if !matches_token(Some(&existing.token), requested_token) {
                return Err(ReserveMoveError::ForeignToken);
            }
            existing.expires_at = now + ttl;
            return Ok(existing.token.clone());
        }

        if let Some(reservation) = self.moves.values().find(|reservation| {
            reservation.expires_at > now
                && reservation
                    .player_ids
                    .iter()
                    .any(|player_id| requested.contains(player_id))
        }) {
            if !matches_token(None, requested_token) {
                tracing::warn!(other = %reservation.token, "overlapping QWD move reservations");
            }
            return Err(ReserveMoveError::Conflict);
        }

        let token = self.next_token("move");
        self.moves.insert(
            move_id.to_string(),
            MoveReservation {
                player_ids: player_ids.to_vec(),
                pending: player_ids.iter().cloned().collect(),
                joined: HashSet::new(),
                expires_at: now + ttl,
                token: token.clone(),
            },
        );
        Ok(token)
    }

    pub fn token_of(&self, move_id: &str) -> Option<&str> {
        Some(self.moves.get(move_id)?.token.as_str())
    }

    /// Token of a player allocation hold, if one is outstanding.
    pub fn player_token_of(&self, player_id: &str, now: Instant) -> Option<&str> {
        let reservation = self.players.get(player_id)?;
        (reservation.expires_at > now).then_some(reservation.token.as_str())
    }

    pub fn release_move(&mut self, move_id: &str) {
        self.moves.remove(move_id);
    }

    /// A player actually joined; drop the corresponding allocation and move
    /// holds so occupancy is no longer double-counted.
    ///
    /// Returns every move whose whole group has now joined, so the caller can
    /// publish `move.completed` with the original player list.
    pub fn confirm_player(&mut self, player_id: &str) -> Vec<CompletedMove> {
        self.players.remove(player_id);

        let mut completed = Vec::new();
        for (move_id, reservation) in &mut self.moves {
            if !reservation.pending.remove(player_id) {
                continue;
            }
            reservation.joined.insert(player_id.to_string());
            if reservation.pending.is_empty() {
                completed.push(CompletedMove {
                    move_id: move_id.clone(),
                    player_ids: reservation.player_ids.clone(),
                    token: reservation.token.clone(),
                });
            }
        }
        for completed_move in &completed {
            self.moves.remove(&completed_move.move_id);
        }
        completed
    }

    /// Drop expired holds. Returns the move reservations that expired so the
    /// caller can publish `reservation.expired` to the manager.
    pub fn expire(&mut self, now: Instant) -> Vec<ExpiredMove> {
        self.players
            .retain(|_, reservation| reservation.expires_at > now);

        let mut expired = Vec::new();
        self.moves.retain(|move_id, reservation| {
            if reservation.expires_at > now {
                true
            } else {
                expired.push(ExpiredMove {
                    move_id: move_id.clone(),
                    player_ids: reservation.player_ids.clone(),
                    admitted_player_ids: reservation.joined.iter().cloned().collect(),
                    token: reservation.token.clone(),
                });
                false
            }
        });
        expired
    }
}

/// Whether a hold accepts the token presented by the caller. A hold always
/// accepts a caller that presents no token (older managers), and rejects a
/// foreign token so one manager cannot drive another's reservation.
pub fn matches_token(hold_token: Option<&str>, presented: Option<&str>) -> bool {
    match (hold_token, presented) {
        (_, None) => true,
        (None, Some(_)) => true,
        (Some(hold), Some(presented)) => hold == presented,
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ReserveMoveError {
    /// Same identity, different group — or a player already in another hold.
    Conflict,
    /// A token was presented that does not own the hold.
    ForeignToken,
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::control::PlayerInfo;
    use std::time::Duration;

    fn ids<const N: usize>(values: [&str; N]) -> Vec<String> {
        values.iter().map(|value| value.to_string()).collect()
    }

    #[test]
    fn move_reservation_counts_unique_players() {
        let mut reservations = Reservations::default();
        let now = Instant::now();
        reservations.reserve_player("player-1", Duration::from_secs(5), now);
        reservations
            .reserve_move(
                "move-1",
                &ids(["player-1", "player-2"]),
                Duration::from_secs(5),
                now,
                None,
            )
            .unwrap();

        assert_eq!(reservations.reserved_players(now).len(), 2);
        assert_eq!(reservations.outstanding_slots(now), 3);
    }

    #[test]
    fn conflicting_move_is_rejected() {
        let mut reservations = Reservations::default();
        let now = Instant::now();
        reservations
            .reserve_move(
                "move-1",
                &ids(["player-1"]),
                Duration::from_secs(5),
                now,
                None,
            )
            .unwrap();
        assert_eq!(
            reservations.reserve_move(
                "move-2",
                &ids(["player-1"]),
                Duration::from_secs(5),
                now,
                None
            ),
            Err(ReserveMoveError::Conflict)
        );
    }

    #[test]
    fn renewing_returns_original_token_and_rejects_foreign_tokens() {
        let mut reservations = Reservations::default();
        let now = Instant::now();
        let token = reservations
            .reserve_move(
                "move-1",
                &ids(["player-1"]),
                Duration::from_secs(5),
                now,
                None,
            )
            .unwrap();

        let renewed = reservations
            .reserve_move(
                "move-1",
                &ids(["player-1"]),
                Duration::from_secs(9),
                now,
                Some(&token),
            )
            .unwrap();
        assert_eq!(renewed, token);
        assert_eq!(
            reservations.reserve_move(
                "move-1",
                &ids(["player-1"]),
                Duration::from_secs(9),
                now,
                Some("other")
            ),
            Err(ReserveMoveError::ForeignToken)
        );
    }

    #[test]
    fn confirm_tracks_admitted_players() {
        let mut reservations = Reservations::default();
        let now = Instant::now();
        let token = reservations
            .reserve_move(
                "move-1",
                &ids(["player-1", "player-2"]),
                Duration::from_secs(5),
                now,
                None,
            )
            .unwrap();

        assert!(reservations.confirm_player("player-1").is_empty());
        assert_eq!(reservations.outstanding_slots(now), 1);

        let completed = reservations.confirm_player("player-2");
        assert_eq!(completed.len(), 1);
        assert_eq!(completed[0].player_ids, ids(["player-1", "player-2"]));
        assert_eq!(completed[0].token, token);
        assert_eq!(reservations.outstanding_slots(now), 0);
    }

    #[test]
    fn expire_reports_admitted_players() {
        let mut reservations = Reservations::default();
        let now = Instant::now();
        reservations
            .reserve_move(
                "move-1",
                &ids(["player-1", "player-2"]),
                Duration::from_secs(5),
                now,
                None,
            )
            .unwrap();
        reservations.confirm_player("player-1");

        let expired = reservations.expire(now + Duration::from_secs(6));
        assert_eq!(expired.len(), 1);
        assert_eq!(expired[0].player_ids, ids(["player-1", "player-2"]));
        assert_eq!(expired[0].admitted_player_ids, ids(["player-1"]));
        assert!(!expired[0].token.is_empty());
    }

    /// A full room holding a slot for a player who never joined used to report
    /// `physical + reserved > capacity`. The manager treats that as an invalid
    /// frame: every telemetry update was rejected and it spammed `room.sync`
    /// until the hold expired. The facts must satisfy the invariant instead.
    #[test]
    fn facts_clamp_reserved_slots_to_the_room_invariant() {
        let mut reservations = Reservations::default();
        let now = Instant::now();
        reservations.reserve_player("incoming", Duration::from_secs(30), now);

        let status = ServerStatus {
            addr: String::new(),
            peers: 8,
            players: 8,
            pending_moves: 0,
            draining: false,
            rooms: Vec::new(),
            player_list: (1..=8)
                .map(|index| PlayerInfo {
                    player_id: index,
                    uuid: format!("player-{index}"),
                    level_id: 1,
                    lv_seq: 0,
                    moving: false,
                    session_id: format!("session-{index}"),
                    is_host: false,
                    position: None,
                    velocity: None,
                    state_at: 0,
                    last_move_at: 0,
                    affinity: Vec::new(),
                })
                .collect(),
        };

        let facts = reservations.facts(&status, 8, 42, now);
        assert_eq!(facts.physical_players, 8);
        assert_eq!(facts.reserved_slots, 0);

        // With one free slot the hold is reported normally.
        let status = ServerStatus {
            player_list: status.player_list[..7].to_vec(),
            ..status
        };
        let facts = reservations.facts(&status, 8, 43, now);
        assert_eq!(facts.physical_players, 7);
        assert_eq!(facts.reserved_slots, 1);
    }
}
