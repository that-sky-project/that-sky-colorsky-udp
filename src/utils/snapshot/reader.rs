//! Client->server snapshot decoder.

use super::codec::{rle_decode, snapshot_checksum};
use super::{MAX_STATE_SIZE, MAX_WINDOW, WindowEntry};

/// Client->Server snapshot decoder.
///
/// Maintains a sliding window of saved frames and reconstructs full state
/// from incoming raw/keyframe/delta frames. Entries are ordered oldest-first.
#[derive(Clone)]
pub struct SnapshotReader {
    /// Recent saved frames used as XOR bases.
    window: Vec<WindowEntry>,
    /// When `true`, deltas are ignored until a raw frame arrives.
    waiting_for_keyframe: bool,
}

impl SnapshotReader {
    /// Create a new reader seeded with the empty sequence-1 base.
    pub fn new() -> Self {
        Self {
            window: vec![WindowEntry {
                sequence: 1,
                data: Vec::new(),
            }],
            waiting_for_keyframe: false,
        }
    }

    /// Most recently decoded `save_seq` (0 if none).
    pub fn latest_sequence(&self) -> u8 {
        self.window
            .iter()
            .rev()
            .find(|entry| entry.sequence != 0)
            .map(|entry| entry.sequence)
            .unwrap_or(0)
    }

    /// Apply an incoming raw/keyframe/delta frame.
    ///
    /// Returns the reconstructed full state on success, or `None` if the
    /// frame is malformed, fails checksum verification, is a delta while
    /// waiting for a keyframe, or references a base that is not available.
    pub fn apply_delta(&mut self, delta: &[u8]) -> Option<Vec<u8>> {
        if delta.len() < 3 {
            return None;
        }

        let save_seq = delta[0];
        let base_seq = delta[1];
        let expected = delta[2];
        let payload = &delta[3..];

        if save_seq == 0 && base_seq == 0 {
            tracing::debug!("[SNAPSHOT] ignore frame with zero save/base sequence");
            return None;
        }

        if snapshot_checksum(payload) != expected {
            tracing::debug!(
                "[SNAPSHOT] checksum mismatch saveSeq={} baseSeq={}",
                save_seq,
                base_seq
            );
            return None;
        }

        // Raw frame: replaces the whole window.
        if base_seq == 0 {
            if payload.len() > MAX_STATE_SIZE {
                tracing::debug!(
                    "[SNAPSHOT] ignore oversized raw frame bytes={} max={}",
                    payload.len(),
                    MAX_STATE_SIZE
                );
                return None;
            }

            let state = payload.to_vec();
            self.window.clear();
            self.add_to_window(save_seq, state.clone());
            self.waiting_for_keyframe = false;
            return Some(state);
        }

        if self.waiting_for_keyframe && save_seq == 0 {
            tracing::debug!(
                "[SNAPSHOT] ignore delta while waiting for keyframe baseSeq={}",
                base_seq
            );
            return None;
        }

        let Some(base) = self
            .window
            .iter()
            .find(|entry| entry.sequence == base_seq)
            .map(|entry| entry.data.clone())
        else {
            tracing::debug!(
                "[SNAPSHOT] missing base sequence {}; waiting for raw frame",
                base_seq
            );
            self.window.clear();
            self.waiting_for_keyframe = true;
            return None;
        };

        let Some(xor_buf) = rle_decode(payload) else {
            tracing::debug!("[SNAPSHOT] truncated RLE payload");
            return None;
        };

        if xor_buf.len() > MAX_STATE_SIZE {
            tracing::debug!(
                "[SNAPSHOT] ignore oversized delta bytes={} max={}",
                xor_buf.len(),
                MAX_STATE_SIZE
            );
            return None;
        }

        let mut new_state = vec![0u8; xor_buf.len()];
        let copy_len = base.len().min(xor_buf.len());
        for i in 0..copy_len {
            new_state[i] = base[i] ^ xor_buf[i];
        }
        if xor_buf.len() > base.len() {
            new_state[base.len()..].copy_from_slice(&xor_buf[base.len()..]);
        }

        // A non-zero save_seq marks a keyframe, which becomes a future base.
        if save_seq != 0 {
            self.add_to_window(save_seq, new_state.clone());
            self.waiting_for_keyframe = false;
        }

        Some(new_state)
    }

    /// Force the reader to ignore deltas until the next raw frame.
    pub fn force_wait_for_keyframe(&mut self) {
        self.window.clear();
        self.waiting_for_keyframe = true;
    }

    /// Insert a frame into the sliding window, evicting the oldest entry
    /// if the window is full.
    fn add_to_window(&mut self, sequence: u8, data: Vec<u8>) {
        self.window.retain(|entry| entry.sequence != sequence);
        self.window.push(WindowEntry { sequence, data });
        while self.window.len() > MAX_WINDOW {
            self.window.remove(0);
        }
    }
}

impl Default for SnapshotReader {
    fn default() -> Self {
        Self::new()
    }
}
