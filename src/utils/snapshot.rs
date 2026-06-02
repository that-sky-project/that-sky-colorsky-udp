// #![doc = include_str!("../../docs/snapshot.md")]

/// Maximum state size in bytes. Larger states are truncated.
const MAX_STATE_SIZE: usize = 7500;
/// Maximum number of past frames kept in the sliding window.
const MAX_WINDOW: usize = 16;
/// Emit a keyframe every N deltas to keep the client in sync.
const KEYFRAME_INTERVAL: u8 = 10;
/// Enter resync mode after this many unacknowledged frames.
const MAX_MISSED_ACKS: u8 = 3;

/// A single entry in the snapshot sliding window.
#[derive(Clone)]
pub struct WindowEntry {
    /// Monotonic sequence number of this frame.
    pub sequence: u8,
    /// Full state data at this sequence point.
    pub data: Vec<u8>,
}

// ── SnapshotReader ───────────────────────────────────────────────────

/// Client->Server snapshot decoder.
///
/// Maintains a sliding window of recent frames and reconstructs full
/// player state from incoming delta/key/raw frames.
#[derive(Clone)]
pub struct SnapshotReader {
    /// Most recently decoded `save_seq`.
    pub current_sequence: u8,
    /// Sliding window of recent frames used as XOR bases.
    pub window: Vec<WindowEntry>,
    /// Latest fully reconstructed state.
    pub latest_full_state: Vec<u8>,
    /// When `true`, delta frames are ignored until a keyframe or raw
    /// frame arrives.
    waiting_for_keyframe: bool,
}

impl SnapshotReader {
    /// Create a new reader with an empty window.
    pub fn new() -> Self {
        Self {
            current_sequence: 0,
            window: Vec::with_capacity(MAX_WINDOW),
            latest_full_state: Vec::new(),
            waiting_for_keyframe: false,
        }
    }

    /// Apply an incoming delta/key/raw frame.
    ///
    /// Returns the reconstructed full state on success, or `None` if the
    /// frame is malformed, fails checksum verification, or is a delta
    /// while waiting for a keyframe.
    pub fn apply_delta(&mut self, delta: &[u8]) -> Option<Vec<u8>> {
        if delta.len() < 3 {
            return None;
        }
        let save_seq = delta[0];
        let base_seq = delta[1];
        let expected = delta[2];
        let data = &delta[3..];

        // Raw frame: save_seq ≠ 0, base_seq = 0.
        if base_seq == 0 && save_seq != 0 {
            if snapshot_checksum(data) != expected {
                return None;
            }
            let s = data.to_vec();
            self.window.clear();
            self.add_to_window(save_seq, s.clone());
            self.current_sequence = save_seq;
            self.latest_full_state = s.clone();
            self.waiting_for_keyframe = false;
            return Some(s);
        }

        // Key or delta frame: base_seq ≠ 0.
        if base_seq != 0 {
            if self.waiting_for_keyframe && save_seq == 0 {
                tracing::debug!(
                    "[SNAPSHOT] ignore delta while waiting for keyframe baseSeq={}",
                    base_seq
                );
                return None;
            }

            if snapshot_checksum(data) != expected {
                return None;
            }
            let xor_buf = rle_decode(data)?;
            if xor_buf.len() > MAX_STATE_SIZE {
                return None;
            }
            let base = self
                .window
                .iter()
                .find(|e| e.sequence == base_seq)
                .map(|e| e.data.as_slice())
                .unwrap_or(&[]);
            let mut new_state = vec![0u8; xor_buf.len()];
            for i in 0..base.len().min(xor_buf.len()) {
                new_state[i] = base[i] ^ xor_buf[i];
            }
            if xor_buf.len() > base.len() {
                new_state[base.len()..].copy_from_slice(&xor_buf[base.len()..]);
            }

            // If save_seq ≠ 0, it's a keyframe — store in window.
            if save_seq != 0 {
                self.add_to_window(save_seq, new_state.clone());
                self.current_sequence = save_seq;
                self.waiting_for_keyframe = false;
            }
            self.latest_full_state = new_state.clone();
            return Some(new_state);
        }
        None
    }

    /// Force the reader to ignore deltas until the next keyframe or raw
    /// frame. Used after a desync is detected.
    pub fn force_wait_for_keyframe(&mut self) {
        self.window.clear();
        self.latest_full_state.clear();
        self.current_sequence = 0;
        self.waiting_for_keyframe = true;
    }

    /// Insert a frame into the sliding window, evicting the oldest entry
    /// if the window is full.
    fn add_to_window(&mut self, sequence: u8, data: Vec<u8>) {
        self.window.retain(|e| e.sequence != sequence);
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
/// Generates delta, key, or raw frames for a specific target client.
/// Maintains an independent sliding window so each client receives a
/// consistent delta sequence.
#[derive(Clone)]
pub struct SnapshotWriter {
    /// Next `save_seq` to assign to a keyframe or raw frame.
    pub next_save_seq: u8,
    /// Sliding window of recent frames (keyframes and raw frames).
    window: Vec<WindowEntry>,
    /// Number of consecutive deltas emitted since the last keyframe.
    delta_count: u8,
    /// When `true`, the writer is in resync mode and will emit a raw
    /// frame on the next call.
    resync: bool,
    /// Sequence number of the frame currently awaiting acknowledgment.
    pending_ack_seq: Option<u8>,
    /// Number of consecutive unacknowledged frames.
    missed_acks: u8,
    /// Force the next frame to be a keyframe (set after recovering from
    /// resync via ack).
    force_next_keyframe: bool,
    /// Whether to track acknowledgments. Disabled for unacked writers.
    track_acks: bool,
    /// Sequence number of the most recent frame in the window, used to
    /// select a base for XOR diffing.
    last_saved_seq: Option<u8>,
}

impl SnapshotWriter {
    /// Create a new writer starting at sequence 1.
    pub fn new() -> Self {
        Self {
            next_save_seq: 1,
            window: Vec::with_capacity(MAX_WINDOW),
            delta_count: 0,
            resync: false,
            pending_ack_seq: None,
            missed_acks: 0,
            force_next_keyframe: false,
            track_acks: true,
            last_saved_seq: None,
        }
    }

    /// Create a writer that does not track acknowledgments.
    ///
    /// Useful for broadcasts where individual acks are not expected.
    pub fn new_unacked() -> Self {
        let mut writer = Self::new();
        writer.track_acks = false;
        writer
    }

    /// Generate the next frame for the given full state.
    ///
    /// Automatically chooses between raw, key, and delta based on window
    /// state, resync status, and the keyframe interval.
    pub fn generate_delta(&mut self, new_state: &[u8]) -> Vec<u8> {
        let new_len = new_state.len().min(MAX_STATE_SIZE);

        tracing::debug!(
            "[SNAPSHOT]generate_delta new_len={} resync={} windows_is_empty={} window_len={}",
            new_len,
            self.resync,
            self.window.is_empty(),
            self.window.len()
        );

        if self.resync {
            return self.emit_raw(new_state, new_len);
        }
        if self.window.is_empty() {
            return self.emit_raw(new_state, new_len);
        }
        if self.window.len() >= MAX_WINDOW {
            // Window is at capacity — let add_to_window truncate old
            // entries naturally. No need for a destructive resync.
            tracing::warn!(
                "[SNAPSHOT] window at capacity ({} entries), old entries will be evicted",
                self.window.len()
            );
        }
        if self.force_next_keyframe {
            self.force_next_keyframe = false;
            return self.emit_keyframe(new_state, new_len);
        }
        if self.delta_count >= KEYFRAME_INTERVAL {
            return self.emit_keyframe(new_state, new_len);
        }
        self.emit_delta(new_state, new_len)
    }

    /// The sequence number currently awaiting an ack, if any.
    pub fn pending_ack_seq(&self) -> Option<u8> {
        self.pending_ack_seq
    }

    /// Acknowledge receipt of the frame with the given sequence number.
    ///
    /// Returns `true` if the ack matched the pending frame.
    ///
    /// Prunes the snapshot window of entries at or before the acked
    /// sequence, keeping the window small. Uses wrapping comparison to
    /// handle `u8` overflow safely (window spread is well within 127).
    pub fn ack(&mut self, seq: u8) -> bool {
        let matched = self.pending_ack_seq == Some(seq);
        if matched {
            self.pending_ack_seq = None;
            self.missed_acks = 0;

            // Keep entries at or after the acked sequence (the acked
            // entry itself is still a valid base for future deltas).
            // Wrapping distance 0..=127 covers seq itself and anything
            // after it. Older entries (< seq) are safe to discard.
            self.window.retain(|e| e.sequence.wrapping_sub(seq) <= 127);

            if self.resync {
                self.resync = false;
                self.force_next_keyframe = true;
            }
        }
        matched
    }

    /// Reset the writer to a clean state, clearing the window and all
    /// internal counters.
    pub fn force_keyframe(&mut self) {
        self.window.clear();
        self.delta_count = 0;
        self.resync = false;
        self.pending_ack_seq = None;
        self.missed_acks = 0;
        self.force_next_keyframe = false;
        self.last_saved_seq = None;
    }

    // ── private helpers ──────────────────────────────────────────────

    /// Emit a delta frame: `[0, base_seq, checksum, rle_xor_data]`.
    fn emit_delta(&mut self, new_state: &[u8], new_len: usize) -> Vec<u8> {
        let Some((base_sequence, base_data)) = self.base_snapshot() else {
            return self.emit_raw(new_state, new_len);
        };
        let xor_buf = xor_diff(new_state, base_data.as_slice(), new_len);
        let non_zero = xor_buf.iter().filter(|&&b| b != 0).count();
        let rle_data = rle_encode(&xor_buf);
        tracing::debug!(
            "[SNAPSHOT] DELTA baseSeq={} raw={} non0={} pct={:.1}% rle={} ratio={:.0}%",
            base_sequence,
            xor_buf.len(),
            non_zero,
            non_zero as f64 / xor_buf.len() as f64 * 100.0,
            rle_data.len(),
            rle_data.len() as f64 / xor_buf.len() as f64 * 100.0
        );
        if rle_data.len() > xor_buf.len() {
            self.resync = true;
            self.window.clear();
            return self.emit_raw(new_state, new_len);
        }
        self.delta_count += 1;
        let chk = snapshot_checksum(&rle_data);
        let mut d = Vec::with_capacity(3 + rle_data.len());
        d.push(0); // delta: save_seq = 0
        d.push(base_sequence);
        d.push(chk);
        d.extend_from_slice(&rle_data);
        d
    }

    /// Emit a keyframe: `[save_seq, base_seq, checksum, rle_xor_data]`.
    fn emit_keyframe(&mut self, new_state: &[u8], new_len: usize) -> Vec<u8> {
        let Some((base_sequence, base_data)) = self.base_snapshot() else {
            return self.emit_raw(new_state, new_len);
        };
        let sv = self.next_save_seq;
        self.next_save_seq = self.next_save_seq.wrapping_add(1).max(1);
        self.delta_count = 0;
        let xor_buf = xor_diff(new_state, base_data.as_slice(), new_len);
        let non_zero = xor_buf.iter().filter(|&&b| b != 0).count();
        tracing::debug!(
            "[SNAPSHOT] KEYFRAME baseSeq={} saveSeq={} xor_total={} non_zero={} pct={:.1}%",
            base_sequence,
            sv,
            xor_buf.len(),
            non_zero,
            non_zero as f64 / xor_buf.len() as f64 * 100.0
        );
        let rle_data = rle_encode(&xor_buf);
        if rle_data.len() > xor_buf.len() {
            self.resync = true;
            self.window.clear();
            return self.emit_raw(new_state, new_len);
        }
        let chk = snapshot_checksum(&rle_data);
        let mut d = Vec::with_capacity(3 + rle_data.len());
        d.push(sv);
        d.push(base_sequence);
        d.push(chk);
        d.extend_from_slice(&rle_data);
        self.add_to_window(sv, new_state[..new_len].to_vec());
        self.mark_ack_required(sv);
        d
    }

    /// Emit a raw frame: `[save_seq, 0, checksum, state_bytes]`.
    fn emit_raw(&mut self, new_state: &[u8], new_len: usize) -> Vec<u8> {
        let sv = self.next_save_seq;
        self.next_save_seq = self.next_save_seq.wrapping_add(1).max(1);
        self.delta_count = 0;
        let chk = snapshot_checksum(&new_state[..new_len]);
        let mut d = Vec::with_capacity(3 + new_len);
        d.push(sv);
        d.push(0); // raw: base_seq = 0
        d.push(chk);
        d.extend_from_slice(&new_state[..new_len]);
        self.add_to_window(sv, new_state[..new_len].to_vec());
        self.mark_ack_required(sv);
        d
    }

    /// Mark the given sequence as requiring acknowledgment. Enters
    /// resync mode if too many consecutive acks are missed.
    fn mark_ack_required(&mut self, seq: u8) {
        if !self.track_acks {
            return;
        }

        if self.pending_ack_seq.is_some() {
            self.missed_acks = self.missed_acks.saturating_add(1);
            if self.missed_acks >= MAX_MISSED_ACKS {
                self.resync = true;
                tracing::warn!(
                    "[SNAPSHOT] missed ack pending={:?} new_seq={} count={}, enter panic/resync mode",
                    self.pending_ack_seq,
                    seq,
                    self.missed_acks
                );
            }
        }
        self.pending_ack_seq = Some(seq);
    }

    /// Select the best base frame for XOR diffing.
    ///
    /// Prefers the most recently saved frame; falls back to the oldest
    /// entry in the window.
    fn base_snapshot(&self) -> Option<(u8, Vec<u8>)> {
        self.last_saved_seq
            .and_then(|sequence| self.window.iter().find(|entry| entry.sequence == sequence))
            .or_else(|| self.window.first())
            .map(|entry| (entry.sequence, entry.data.clone()))
    }

    /// Add a frame to the front of the sliding window.
    ///
    /// Deduplicates by sequence number and truncates to `MAX_WINDOW`.
    fn add_to_window(&mut self, sequence: u8, data: Vec<u8>) {
        self.window.retain(|e| e.sequence != sequence);
        self.window.insert(0, WindowEntry { sequence, data });
        self.window.truncate(MAX_WINDOW);
        self.last_saved_seq = Some(sequence);
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
            let mut c = 0u8;
            while i < data.len() && c < 255 && data[i] == 0 {
                c += 1;
                i += 1;
            }
            out.push(0x00);
            out.push(c);
        } else {
            out.push(data[i]);
            i += 1;
        }
    }
    out
}

/// Decode an RLE-encoded byte slice.
///
/// Returns `None` if the data is truncated (0x00 without a count byte).
fn rle_decode(data: &[u8]) -> Option<Vec<u8>> {
    let mut out = Vec::with_capacity(data.len());
    let mut i = 0;
    while i < data.len() {
        if data[i] == 0x00 {
            if i + 1 >= data.len() {
                return None;
            }
            out.resize(out.len() + data[i + 1] as usize, 0);
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
    let bl = base.len().min(new_len);
    let mut buf = vec![0u8; new_len];
    for i in 0..bl {
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
        let mut w = SnapshotWriter::new();
        let mut r = SnapshotReader::new();
        let s1 = vec![0xAAu8; 100];
        let f1 = w.generate_delta(&s1);
        assert_eq!(f1[0], 1);
        assert_eq!(f1[1], 0); // Raw: save=1, base=0
        assert_eq!(r.apply_delta(&f1).unwrap(), s1);

        for i in 0..KEYFRAME_INTERVAL {
            let s = vec![0xAAu8.wrapping_add(i as u8 + 1); 100];
            let f = w.generate_delta(&s);
            assert_eq!(f[0], 0, "delta {} saveSeq", i); // Delta: save=0
            assert_eq!(f[1], 1); // base=1
            assert_eq!(r.apply_delta(&f).unwrap(), s);
        }

        let sk = vec![0xBBu8; 100];
        let fk = w.generate_delta(&sk);
        assert_eq!(fk[0], 2); // Key: save=2
        assert_eq!(fk[1], 1); // base=1
        assert_eq!(r.apply_delta(&fk).unwrap(), sk);

        w.ack(1);
        assert_eq!(w.generate_delta(&sk)[0], 0); // delta after ack
    }

    #[test]
    fn rle_roundtrip() {
        let d = vec![0u8; 40];
        assert_eq!(rle_encode(&d), vec![0x00, 40]);
        assert_eq!(rle_decode(&rle_encode(&d)).unwrap(), d);
    }
}
