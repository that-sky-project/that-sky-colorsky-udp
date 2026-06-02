impl crate::server::state::ServerState {
    /// Broadcast data to all in-game peers in the same level, optionally
    /// excluding a specific peer.
    ///
    /// Deduplicates by UUID to prevent a single player on multiple
    /// connections from receiving duplicate packets.
    pub(super) fn broadcast_same_level(
        &self,
        level_id: u32,
        channel: u8,
        data: &[u8],
        reliable: bool,
    ) {
        for e in self.peers.values() {
            if e.level_id != level_id {
                continue;
            }
            e.send(channel, data, reliable);
        }
    }

    /// Same as [`broadcast_same_level`], but also excludes the peer
    /// with the given `player_id`.
    pub(super) fn broadcast_same_level_without(
        &self,
        channel: u8,
        data: &[u8],
        reliable: bool,
        level_id: u32,
        except_player_id: u8,
    ) {
        for e in self.peers.values() {
            if e.level_id != level_id {
                continue;
            }
            if e.player_id == except_player_id {
                continue;
            }
            e.send(channel, data, reliable);
        }
    }

    /// Broadcast data to every connected peer, regardless of level.
    ///
    /// Only sends to peers that have entered the game (`in_game == true`).
    pub(super) fn broadcast_all(&self, channel: u8, data: &[u8], reliable: bool) {
        for e in self.peers.values() {
            e.send(channel, data, reliable);
        }
    }

    /// Same as [`broadcast_all`], but also excludes the peer with the
    /// given `player_id`.
    pub(super) fn broadcast_all_without(
        &self,
        except_player_id: u8,
        channel: u8,
        data: &[u8],
        reliable: bool,
    ) {
        for e in self.peers.values() {
            if e.player_id == except_player_id {
                continue;
            }
            e.send(channel, data, reliable);
        }
    }
}
