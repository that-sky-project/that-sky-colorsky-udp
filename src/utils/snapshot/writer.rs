//! Server->client snapshot encoder.

use super::codec::{rle_encode, snapshot_checksum, xor_diff};
use super::{KEYFRAME_INTERVAL, MAX_STATE_SIZE, MAX_WINDOW, WindowEntry};

/// Server->Client snapshot encoder.
///
/// Generates raw, key, or delta frames for a specific target client.
/// Entries are ordered oldest-first; deltas are based on the oldest
/// unacknowledged saved frame, because that is the frame the client is most
/// likely to have. `ack()` prunes everything older than the acknowledged
/// sequence.
#[derive(Clone)]
pub struct SnapshotWriter {
    /// Stream name used in diagnostics (e.g. `player_state`, `level_data`).
    label: &'static str,
    /// Next `save_seq` to assign to a save (raw/key) frame.
    next_save_seq: u8,
    /// Saved frames, oldest-first. The first entry is the preferred base.
    window: Vec<WindowEntry>,
    /// Number of frames written so far, used to schedule save frames.
    write_count: usize,
    /// True after the window filled up; saved frames are emitted twice as
    /// often until an ack arrives.
    resync: bool,
    /// Sequence number of the saved frame currently awaiting acknowledgment.
    pending_ack_seq: Option<u8>,
    /// Force the next write to be a save frame.
    force_next_keyframe: bool,
}

impl SnapshotWriter {
    /// Create a writer seeded with the empty sequence-1 base.
    pub fn new() -> Self {
        Self::with_label("snapshot")
    }

    /// Create a labeled writer seeded with the empty sequence-1 base.
    pub fn with_label(label: &'static str) -> Self {
        let mut writer = Self {
            label,
            next_save_seq: 1,
            window: Vec::with_capacity(MAX_WINDOW),
            write_count: 0,
            resync: false,
            pending_ack_seq: None,
            force_next_keyframe: false,
        };
        writer.reset_window();
        writer
    }

    /// Generate the next frame for the given full state.
    ///
    /// Automatically chooses between raw, key, and delta based on window
    /// state, resync status, and the keyframe interval.
    #[allow(clippy::manual_is_multiple_of)] // `usize::is_multiple_of` needs Rust 1.87
    pub fn generate_delta(&mut self, new_state: &[u8]) -> Vec<u8> {
        let new_len = new_state.len().min(MAX_STATE_SIZE);

        // A full window means the client is not acknowledging save frames
        // fast enough. Reset the sequence and fall back to a raw frame.
        if self.window.len() >= MAX_WINDOW {
            if self.resync {
                tracing::warn!(
                    "[SNAPSHOT:{}] window still full during resync ({} entries), dropping oldest half",
                    self.label,
                    self.window.len()
                );
                let keep = MAX_WINDOW / 2;
                let excess = self.window.len().saturating_sub(keep);
                self.window.drain(..excess);
            } else {
                tracing::warn!(
                    "[SNAPSHOT:{}] window full ({} entries), entering resync",
                    self.label,
                    self.window.len()
                );
                self.resync = true;
                self.window.clear();
                self.write_count = 0;
                self.next_save_seq = 1;
                self.pending_ack_seq = None;
            }
        }

        if self.force_next_keyframe {
            self.force_next_keyframe = false;
            let frame = if self.window.is_empty() {
                self.emit_raw(new_state, new_len)
            } else {
                self.emit_keyframe(new_state, new_len)
            };
            self.write_count += 1;
            return frame;
        }

        let period = if self.resync {
            (KEYFRAME_INTERVAL / 2).max(1)
        } else {
            KEYFRAME_INTERVAL.max(1)
        };
        let save_frame = self.window.is_empty() || self.write_count % period == 0;
        let frame = if save_frame {
            if self.window.is_empty() {
                self.emit_raw(new_state, new_len)
            } else {
                self.emit_keyframe(new_state, new_len)
            }
        } else {
            self.emit_delta(new_state, new_len)
        };

        self.write_count += 1;
        frame
    }

    /// The sequence number currently awaiting an ack, if any.
    pub fn pending_ack_seq(&self) -> Option<u8> {
        self.pending_ack_seq
    }

    /// Acknowledge a previously emitted `save_seq`.
    ///
    /// Returns `true` if the sequence is still in the window. Prunes entries
    /// older than the acknowledged sequence (wrapping-safe for the u8 range)
    /// and leaves resync mode.
    pub fn ack(&mut self, seq: u8) -> bool {
        if !self.window.iter().any(|entry| entry.sequence == seq) {
            return false;
        }

        self.pending_ack_seq = None;
        self.window
            .retain(|entry| entry.sequence.wrapping_sub(seq) <= 127);

        if self.resync {
            self.resync = false;
            tracing::debug!(
                "[SNAPSHOT:{}] resync complete after ack seq={}",
                self.label,
                seq
            );
        }

        true
    }

    /// Reset the writer to the seeded empty base and force the next write to
    /// be a keyframe.
    pub fn force_keyframe(&mut self) {
        self.reset_window();
    }

    // ── private helpers ──────────────────────────────────────────────

    /// Reset all sequence/window state to the empty sequence-1 base.
    fn reset_window(&mut self) {
        self.next_save_seq = 2;
        self.window.clear();
        self.window.push(WindowEntry {
            sequence: 1,
            data: Vec::new(),
        });
        self.write_count = 0;
        self.resync = false;
        self.pending_ack_seq = None;
        self.force_next_keyframe = false;
    }

    /// Allocate the next `save_seq`, skipping zero on wrap.
    fn allocate_save_seq(&mut self) -> u8 {
        let seq = self.next_save_seq;
        let next = self.next_save_seq.wrapping_add(1);
        self.next_save_seq = if next == 0 { 1 } else { next };
        seq
    }

    /// Select the oldest saved frame as the XOR base.
    fn base_snapshot(&self) -> Option<(u8, Vec<u8>)> {
        self.window
            .first()
            .map(|entry| (entry.sequence, entry.data.clone()))
    }

    /// Emit a raw frame: `[save_seq, 0, checksum, state_bytes]`.
    fn emit_raw(&mut self, new_state: &[u8], new_len: usize) -> Vec<u8> {
        let state = &new_state[..new_len];
        let seq = self.allocate_save_seq();
        let checksum = snapshot_checksum(state);
        let mut frame = Vec::with_capacity(3 + new_len);
        frame.push(seq);
        frame.push(0);
        frame.push(checksum);
        frame.extend_from_slice(state);
        self.add_to_window(seq, state.to_vec());
        self.pending_ack_seq = Some(seq);
        frame
    }

    /// Emit a keyframe: `[save_seq, base_seq, checksum, rle_xor_data]`.
    fn emit_keyframe(&mut self, new_state: &[u8], new_len: usize) -> Vec<u8> {
        let Some((base_seq, base)) = self.base_snapshot() else {
            return self.emit_raw(new_state, new_len);
        };

        let xor_buf = xor_diff(new_state, &base, new_len);
        let rle_data = rle_encode(&xor_buf);

        if rle_data.len() > xor_buf.len() {
            self.resync = true;
            self.window.clear();
            return self.emit_raw(new_state, new_len);
        }

        let seq = self.allocate_save_seq();
        let checksum = snapshot_checksum(&rle_data);
        let mut frame = Vec::with_capacity(3 + rle_data.len());
        frame.push(seq);
        frame.push(base_seq);
        frame.push(checksum);
        frame.extend_from_slice(&rle_data);
        self.add_to_window(seq, new_state[..new_len].to_vec());
        self.pending_ack_seq = Some(seq);
        frame
    }

    /// Emit a delta: `[0, base_seq, checksum, rle_xor_data]`.
    fn emit_delta(&mut self, new_state: &[u8], new_len: usize) -> Vec<u8> {
        let Some((base_seq, base)) = self.base_snapshot() else {
            return self.emit_raw(new_state, new_len);
        };

        let xor_buf = xor_diff(new_state, &base, new_len);
        let rle_data = rle_encode(&xor_buf);

        if rle_data.len() > xor_buf.len() {
            self.resync = true;
            self.window.clear();
            return self.emit_raw(new_state, new_len);
        }

        let checksum = snapshot_checksum(&rle_data);
        let mut frame = Vec::with_capacity(3 + rle_data.len());
        frame.push(0); // delta frames are not stored or acknowledged
        frame.push(base_seq);
        frame.push(checksum);
        frame.extend_from_slice(&rle_data);
        frame
    }

    /// Add a saved frame to the window, evicting the oldest entry.
    fn add_to_window(&mut self, sequence: u8, data: Vec<u8>) {
        self.window.retain(|entry| entry.sequence != sequence);
        self.window.push(WindowEntry { sequence, data });
        while self.window.len() > MAX_WINDOW {
            self.window.remove(0);
        }
    }
}

impl Default for SnapshotWriter {
    fn default() -> Self {
        Self::new()
    }
}
