//! QWD transport helpers and small utilities.

use std::sync::Once;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use tungstenite::stream::MaybeTlsStream;

use crate::control::ControlError;
use crate::protocol::types::TgcUuid;

use super::Socket;

/// Build the node control endpoint.
///
/// QWD accepts node connections on the root path and reads the protocol version
/// and the node identity from the query string. The node id is stable for the
/// lifetime of the process so a reconnect lands inside the manager's grace
/// window and its rooms can be restored, instead of appearing as a brand-new
/// node until the old connection is swept.
pub(super) fn endpoint_url(raw: &str, node_id: &str) -> Result<String, String> {
    let raw = raw.trim();
    let (scheme, rest) = raw
        .split_once("://")
        .ok_or_else(|| "qwd.url must include ws:// or wss://".to_string())?;
    let authority = rest.split('/').next().unwrap_or(rest);
    if authority.is_empty() {
        return Err("qwd.url has no host".to_string());
    }
    Ok(format!("{scheme}://{authority}/?v=1&node={node_id}"))
}

pub(super) fn set_read_timeout(
    socket: &mut Socket,
    timeout: Option<Duration>,
) -> std::io::Result<()> {
    match socket.get_mut() {
        MaybeTlsStream::Plain(stream) => stream.set_read_timeout(timeout),
        MaybeTlsStream::Rustls(stream) => stream.sock.set_read_timeout(timeout),
        _ => Ok(()),
    }
}

pub(super) fn control_error(error: ControlError) -> String {
    error.to_string()
}

pub(super) fn init_tls() {
    static TLS_INIT: Once = Once::new();
    TLS_INIT.call_once(|| {
        let _ = rustls::crypto::ring::default_provider().install_default();
    });
}

pub(super) fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_millis() as u64)
        .unwrap_or_default()
}

/// Inclusive bounds of an 8-digit decimal room id.
const MIN_ROOM_ID: u32 = 10_000_000;
const MAX_ROOM_ID: u32 = 99_999_999;

/// Generate an 8-digit decimal room id.
///
/// QWD addresses rooms by id, and short decimal ids are readable in logs and
/// admin views. `MoveGame` still carries a 16-byte destination id, derived
/// deterministically from this value by [`destination_uuid`]. Collisions between
/// nodes are possible (90M ids) but rare; the manager's `room.id_conflict`
/// handshake makes the loser regenerate.
pub(super) fn generate_room_id() -> String {
    use rand::Rng as _;
    rand::thread_rng()
        .gen_range(MIN_ROOM_ID..=MAX_ROOM_ID)
        .to_string()
}

/// Stable identity of this node process across reconnects (`?node=`).
pub(super) fn generate_node_id() -> String {
    random_uuid()
}

/// Map a QWD room id to the 16-byte id carried by `MoveGame`.
///
/// QWD only knows room ids. Newer nodes use 8-digit decimal ids, which are not
/// valid `TgcUuid` text, so the same deterministic mapping is derived on every
/// node (mirrors XYSKY's `RoomServer.destinationUuid` and the manager's
/// `CrossCoordinator.destinationUuidText`): the number becomes the low 48 bits
/// of a fixed, version-4-looking UUID. UUID-shaped ids pass through unchanged,
/// which keeps mixed-version clusters working during a rollout.
pub(super) fn destination_uuid(room_id: &str) -> Option<TgcUuid> {
    if let Some(uuid) = TgcUuid::parse(room_id) {
        return Some(uuid);
    }

    // Only a plain decimal id is accepted; `""`, `0x10`, `1e3` and 16+ digit
    // values must not silently turn into a uuid.
    if room_id.is_empty()
        || room_id.len() > 15
        || !room_id.bytes().all(|byte| byte.is_ascii_digit())
    {
        return None;
    }
    let numeric: u64 = room_id.parse().ok()?;
    if numeric > 0xffff_ffff_ffff {
        return None;
    }

    TgcUuid::parse(&format!("00000000-0000-4000-8000-{numeric:012x}"))
}

fn random_uuid() -> String {
    let bytes = rand::random::<u128>().to_be_bytes();
    format!(
        "{:02x}{:02x}{:02x}{:02x}-{:02x}{:02x}-{:02x}{:02x}-{:02x}{:02x}-{:02x}{:02x}{:02x}{:02x}{:02x}{:02x}",
        bytes[0],
        bytes[1],
        bytes[2],
        bytes[3],
        bytes[4],
        bytes[5],
        bytes[6],
        bytes[7],
        bytes[8],
        bytes[9],
        bytes[10],
        bytes[11],
        bytes[12],
        bytes[13],
        bytes[14],
        bytes[15]
    )
}
