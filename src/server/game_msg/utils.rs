use crate::protocol::{
    self,
    game_msg::{GameMsg, GameMsgId},
    packet::Packet,
};

// Useful utilities for game message
impl crate::server::state::ServerState {
    /// Send game msg to all players on the same level
    pub(super) fn relay_game_msg(
        &self,
        msg_id: GameMsgId,
        payload: &[u8],
        peer_key: usize,
    ) -> Option<()> {
        let sender = self.peers.get(&peer_key)?;
        for (_, peer) in self
            .peers
            .iter()
            // we only need send msg to same level without sender
            .filter(|(pk, pr)| **pk != peer_key && pr.level_id == sender.level_id)
        {
            /// game message header need match target player lv_seq
            let game_msg = GameMsg::new(msg_id, peer.lv_seq, sender.player_id, &payload);

            let payload = Packet::build_packet(
                protocol::packet::PacketId::GameMsg,
                &game_msg.build_game_msg(),
            );

            peer.send(0, &payload, true);
        }

        Some(())
    }

    /// send a game msg to target player with peer_key
    pub(super) fn send_msg_to(&self, msg: GameMsg, to: usize) -> Option<()> {
        let payload =
            Packet::build_packet(protocol::packet::PacketId::GameMsg, &msg.build_game_msg());

        let peer = self.peers.get(&to)?;

        peer.send(0, &payload, true);

        Some(())
    }
}
