//! Sky snapshot frame reader/writer.
//!
//! Wire format:
//!
//! ```text
//! [save_seq: u8][base_seq: u8][checksum: u8][payload...]
//! ```
//!
//! Frame kinds:
//!
//! - raw:      `save_seq != 0 && base_seq == 0` — full state, replaces the window
//! - keyframe: `save_seq != 0 && base_seq != 0` — XOR-RLE diff against `base_seq`
//! - delta:    `save_seq == 0 && base_seq != 0` — XOR-RLE diff, not stored
//! - invalid:  both sequence numbers are zero
//!
//! Both sides start with an empty sequence-1 base. This lets the first server
//! frame be a keyframe (`save_seq = 2, base_seq = 1`) rather than a raw frame,
//! matching the current client snapshot implementation.

/// Maximum state size in bytes. Larger states are truncated when writing and
/// rejected when reading.
const MAX_STATE_SIZE: usize = 0x2100;
/// Maximum number of past saved frames kept in the sliding window.
const MAX_WINDOW: usize = 16;
/// Emit a saved frame every N writes outside resync mode.
const KEYFRAME_INTERVAL: usize = 10;

/// A single entry in the snapshot sliding window.
#[derive(Clone, Debug)]
pub struct WindowEntry {
    /// Monotonic sequence number of this frame.
    pub sequence: u8,
    /// Full state data at this sequence point.
    pub data: Vec<u8>,
}

// ── SnapshotReader ───────────────────────────────────────────────────

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

// ── SnapshotWriter ───────────────────────────────────────────────────

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

// ── RLE ──────────────────────────────────────────────────────────────

/// Run-length encode a byte slice, compressing runs of zeroes.
///
/// Each zero run of length `n` is encoded as `[0x00, n]`. Non-zero bytes
/// are copied as-is. Maximum run length is 255.
fn rle_encode(data: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(data.len());
    let mut i = 0;
    while i < data.len() {
        if data[i] == 0 {
            let mut count = 0u8;
            while i < data.len() && count < 255 && data[i] == 0 {
                count += 1;
                i += 1;
            }
            out.push(0x00);
            out.push(count);
        } else {
            out.push(data[i]);
            i += 1;
        }
    }
    out
}

/// Decode an RLE-encoded byte slice.
///
/// Returns `None` if the data is truncated (0x00 without a valid count).
fn rle_decode(data: &[u8]) -> Option<Vec<u8>> {
    let mut out = Vec::with_capacity(data.len());
    let mut i = 0;
    while i < data.len() {
        if data[i] == 0x00 {
            if i + 1 >= data.len() {
                return None;
            }
            let count = data[i + 1];
            if count == 0 {
                return None;
            }
            out.resize(out.len() + count as usize, 0);
            i += 2;
        } else {
            out.push(data[i]);
            i += 1;
        }
    }
    Some(out)
}

/// Compute the XOR difference between `new` and `base`.
///
/// Bytes beyond `base.len()` in `new` are copied as-is (they have no
/// base to XOR against).
fn xor_diff(new: &[u8], base: &[u8], new_len: usize) -> Vec<u8> {
    let base_len = base.len().min(new_len);
    let mut buf = vec![0u8; new_len];
    for i in 0..base_len {
        buf[i] = new[i] ^ base[i];
    }
    if new_len > base.len() {
        buf[base.len()..].copy_from_slice(&new[base.len()..]);
    }
    buf
}

/// Compute an 8-bit rolling checksum for snapshot frame integrity.
///
/// Used to validate frames before decoding.
pub fn snapshot_checksum(data: &[u8]) -> u8 {
    let mut h: u8 = 0xC5;
    for &b in data {
        h = (0x93u16.wrapping_mul((h ^ b) as u16) & 0xFF) as u8;
    }
    h
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn checksums() {
        for (hex, exp) in &[
            (
                "00a750008507001d05001d0b00ff00ff00ff00ff00ff00ff00ff00ff00ff00ff00ff00ff00ff00ff00ff00ff003f",
                0x50u8,
            ),
            (
                "00a7b3008506001d06001d0a00ff00ff00ff00ff00ff00ff00ff00ff00ff00ff00ff00ff00ff00ff00ff00ff003f",
                0xb3,
            ),
        ] {
            let d = hex::decode(hex).unwrap();
            assert_eq!(snapshot_checksum(&d[3..]), d[2]);
            assert_eq!(snapshot_checksum(&d[3..]), *exp);
        }
    }

    #[test]
    fn roundtrip() {
        let mut writer = SnapshotWriter::new();
        let mut reader = SnapshotReader::new();

        // First frame is a keyframe against the seeded empty seq-1 base.
        let state1 = vec![0xAAu8; 100];
        let frame1 = writer.generate_delta(&state1);
        assert_eq!(frame1[0], 2);
        assert_eq!(frame1[1], 1);
        assert_eq!(reader.apply_delta(&frame1).unwrap(), state1);

        // Before an ack, deltas still base on the oldest unacked frame (seq 1).
        let state2 = vec![0xBBu8; 100];
        let frame2 = writer.generate_delta(&state2);
        assert_eq!(frame2[0], 0);
        assert_eq!(frame2[1], 1);
        assert_eq!(reader.apply_delta(&frame2).unwrap(), state2);

        // Acking the keyframe prunes the seed, so the next delta bases on seq 2.
        assert!(writer.ack(2));
        let state3 = vec![0xCCu8; 100];
        let frame3 = writer.generate_delta(&state3);
        assert_eq!(frame3[0], 0);
        assert_eq!(frame3[1], 2);
        assert_eq!(reader.apply_delta(&frame3).unwrap(), state3);

        // An unknown sequence is not acknowledged.
        assert!(!writer.ack(0x7f));
    }

    #[test]
    fn keyframe_interval() {
        let mut writer = SnapshotWriter::new();
        let mut reader = SnapshotReader::new();

        for i in 0..12u8 {
            let state = vec![i; 64];
            let frame = writer.generate_delta(&state);
            if i == 0 {
                assert_eq!(frame[0], 2);
                assert_eq!(frame[1], 1);
            } else if i == 10 {
                assert_eq!(frame[0], 3);
                assert_eq!(frame[1], 1);
            } else {
                assert_eq!(frame[0], 0);
            }
            assert_eq!(reader.apply_delta(&frame).unwrap(), state);
        }
    }

    #[test]
    fn force_keyframe_resets_to_seed() {
        let mut writer = SnapshotWriter::new();
        let _ = writer.generate_delta(&[0x11u8; 32]);
        writer.force_keyframe();

        let frame = writer.generate_delta(&[0x22u8; 32]);
        assert_eq!(frame[0], 2);
        assert_eq!(frame[1], 1);
    }

    #[test]
    fn missing_base_requires_raw_frame() {
        let mut reader = SnapshotReader::new();
        let mut writer = SnapshotWriter::new();

        let state1 = vec![0x11u8; 32];
        let keyframe = writer.generate_delta(&state1);
        let delta = writer.generate_delta(&[0x22u8; 32]);

        // A delta referencing an unknown base resets the reader.
        let mut bad = vec![0u8, 99];
        let payload = vec![0x33u8; 8];
        bad.push(snapshot_checksum(&payload));
        bad.extend_from_slice(&payload);
        assert!(reader.apply_delta(&bad).is_none());
        assert!(reader.apply_delta(&delta).is_none());

        // Only a raw frame restores the reader.
        let raw_state = vec![0x44u8; 32];
        let mut raw = vec![7u8, 0];
        raw.push(snapshot_checksum(&raw_state));
        raw.extend_from_slice(&raw_state);
        assert_eq!(reader.apply_delta(&raw).unwrap(), raw_state);

        // The original keyframe is no longer usable after the reset.
        assert!(reader.apply_delta(&keyframe).is_none());
    }

    #[test]
    fn window_overflow_enters_resync() {
        let mut writer = SnapshotWriter::new();
        let mut saw_raw = false;

        for i in 0..200u8 {
            let frame = writer.generate_delta(&[i; 16]);
            if i > 0 && frame[0] != 0 && frame[1] == 0 {
                saw_raw = true;
                break;
            }
        }

        assert!(
            saw_raw,
            "writer should emit a raw frame after the window fills"
        );
    }

    #[test]
    fn rle_roundtrip() {
        let data = vec![0u8; 40];
        assert_eq!(rle_encode(&data), vec![0x00, 40]);
        assert_eq!(rle_decode(&rle_encode(&data)).unwrap(), data);
        assert!(rle_decode(&[0x00]).is_none());
        assert!(rle_decode(&[0x00, 0x00]).is_none());
    }
}
