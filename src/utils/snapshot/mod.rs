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

mod codec;
mod reader;
mod writer;

pub use codec::snapshot_checksum;
pub use reader::SnapshotReader;
pub use writer::SnapshotWriter;

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

#[cfg(test)]
mod tests;
