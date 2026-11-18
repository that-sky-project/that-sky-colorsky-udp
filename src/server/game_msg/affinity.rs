#![allow(dead_code)]
#![allow(unused)]

use crate::protocol::game_msg::{GameMsg, affinity::Affinity};

impl crate::server::state::ServerState {
    /// Handle a client `Affinity` update.
    ///
    /// This message is client-to-server only in the current client, so it is
    /// consumed and never relayed to other players.
    ///
    /// `pub(crate)` so control-plane tests can drive the freeze behaviour.
    pub(crate) fn handle_affinity(&mut self, peer_key: usize, msg: GameMsg) -> Option<()> {
        let peer = self.peers.get(&peer_key)?;
        let affinity = Affinity::from_bytes(&msg.payload)?;

        tracing::debug!(
            player_id = peer.player_id,
            count = affinity.count,
            complete = affinity.is_complete(),
            trailing_bytes = affinity.trailing_bytes,
            entries = ?affinity.entries,
            "consumed client affinity update"
        );

        // Freeze the bonds at receive time: resolve each native net player id
        // to the bonded peer's UUID now. Resolving later (live) would let a
        // reused native id misattribute a bond to whichever player currently
        // holds it, so entries that do not resolve to a connected player are
        // dropped instead of being deferred.
        let mut frozen = Vec::with_capacity(affinity.entries.len());
        for entry in &affinity.entries {
            let Some(other) = self
                .peers
                .values()
                .find(|other| other.player_id == entry.net_player_id && other.occupies_slot())
            else {
                tracing::debug!(
                    player_id = peer.player_id,
                    net_player_id = entry.net_player_id,
                    value = entry.value,
                    "dropped affinity entry without a connected peer"
                );
                continue;
            };
            frozen.push(crate::server::peer::AffinityBond {
                uuid: other.uuid,
                session_id: other.control_session_id.clone(),
                value: entry.value,
            });
        }

        let peer = self.peers.get_mut(&peer_key)?;
        peer.affinity = frozen;

        Some(())
    }
}
