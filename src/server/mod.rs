//! # ENet Game Server — Architecture Overview
//!
//! Core module managing all ENet connections, protocol parsing, message
//! routing, and multiplayer state synchronization.

pub mod game_msg;
pub mod host;
pub mod packet;
pub mod peer;
pub mod receive;
pub mod state;
use std::sync::Once;

/// Maximum concurrent clients (ENet host capacity).
const MAX_CLIENTS: usize = 64;
/// Number of ENet channels.
const CHANNEL_COUNT: usize = 2;
/// `enet_host_service` timeout in milliseconds — controls the event loop
/// polling granularity.
const SERVICE_TIMEOUT_MS: u32 = 15;

/// Server tick rate in ticks per second.
pub(crate) const TICK_RATE: u32 = 10;
/// Interval between ticks in milliseconds.
pub(crate) const TICK_INTERVAL_MS: u64 = 1000 / TICK_RATE as u64;

/// Global one-shot guard ensuring `enet_initialize` is called exactly once.
static ENET_INIT: Once = Once::new();

pub use host::EnetServer;
pub use peer::PeerEntry;
