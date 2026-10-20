//! QWD transport helpers and small utilities.

use std::sync::Once;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use tungstenite::stream::MaybeTlsStream;

use crate::control::ControlError;

use super::Socket;
pub(super) fn endpoint_url(raw: &str) -> Result<String, String> {
    let raw = raw.trim();
    let (scheme, rest) = raw
        .split_once("://")
        .ok_or_else(|| "qwd.url must include ws:// or wss://".to_string())?;
    let authority = rest.split('/').next().unwrap_or(rest);
    if authority.is_empty() {
        return Err("qwd.url has no host".to_string());
    }
    Ok(format!("{scheme}://{authority}/?v=1"))
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

/// Generate a lowercase hyphenated UUID-like room id.
pub(super) fn generate_room_id() -> String {
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
