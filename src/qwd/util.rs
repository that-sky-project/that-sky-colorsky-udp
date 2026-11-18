//! QWD transport helpers and small utilities.

use std::sync::Once;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use tungstenite::stream::MaybeTlsStream;

use crate::control::ControlError;
use crate::protocol::types::TgcUuid;

use super::Socket;

/// Build the node control endpoint.
///
/// Preserve deployment paths and authentication query parameters. Reserved
/// protocol keys are replaced once and node identity is percent encoded.
/// Validation errors never include the original URL or its secrets.
pub(super) fn endpoint_url(raw: &str, node_id: &str) -> Result<String, String> {
    let raw = raw.trim();
    if !raw.contains("://") {
        return Err("qwd.url must include ws:// or wss://".to_string());
    }
    let mut endpoint =
        url::Url::parse(raw).map_err(|_| "qwd.url is not a valid WebSocket URL".to_string())?;
    if !matches!(endpoint.scheme(), "ws" | "wss") {
        return Err("qwd.url must use ws:// or wss://".to_string());
    }
    if endpoint.host_str().is_none() {
        return Err("qwd.url has no host".to_string());
    }
    // Also reject syntactically empty userinfo, which Url normalizes away.
    let authority = raw
        .split_once("://")
        .map(|(_, rest)| rest.split(['/', '?', '#']).next().unwrap_or_default())
        .unwrap_or_default();
    if !endpoint.username().is_empty() || endpoint.password().is_some() || authority.contains('@') {
        return Err("qwd.url must not contain credentials".to_string());
    }
    if endpoint.fragment().is_some() {
        return Err("qwd.url must not contain a fragment".to_string());
    }
    // Keep untouched query segments byte-for-byte (signed tokens can depend
    // on their encoding), while recognizing percent-encoded reserved keys.
    let mut query: Vec<String> = endpoint
        .query()
        .unwrap_or_default()
        .split('&')
        .filter(|part| !part.is_empty())
        .filter(|part| {
            !url::form_urlencoded::parse(part.as_bytes())
                .any(|(key, _)| key == "v" || key == "node")
        })
        .map(str::to_owned)
        .collect();
    let mut protocol = url::form_urlencoded::Serializer::new(String::new());
    protocol.append_pair("v", "1").append_pair("node", node_id);
    query.push(protocol.finish());
    endpoint.set_query(Some(&query.join("&")));
    Ok(endpoint.into())
}

/// Log only the origin. Paths and query values may contain access tokens.
pub(super) fn redacted_endpoint(endpoint: &str) -> String {
    url::Url::parse(endpoint)
        .map(|url| url.origin().ascii_serialization())
        .unwrap_or_else(|_| "<invalid WebSocket endpoint>".to_string())
}

pub(super) fn set_socket_timeouts(
    socket: &mut Socket,
    read_timeout: Option<Duration>,
    write_timeout: Option<Duration>,
) -> std::io::Result<()> {
    let stream = match socket.get_mut() {
        MaybeTlsStream::Plain(stream) => stream,
        MaybeTlsStream::Rustls(stream) => &mut stream.sock,
        _ => {
            return Err(std::io::Error::new(
                std::io::ErrorKind::Unsupported,
                "unsupported QWD TLS stream",
            ));
        }
    };
    stream.set_read_timeout(read_timeout)?;
    stream.set_write_timeout(write_timeout)
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
