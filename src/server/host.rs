//! ENet host lifecycle management.
//!
//! Handles library initialization, host creation, the main event loop,
//! and CRC32 checksum verification.

use std::net::{Ipv4Addr, SocketAddr, SocketAddrV4};
use std::thread;
use std::time::{Duration, Instant};

use enet_sys::{
    _ENetEventType_ENET_EVENT_TYPE_CONNECT as ENET_EVENT_TYPE_CONNECT,
    _ENetEventType_ENET_EVENT_TYPE_DISCONNECT as ENET_EVENT_TYPE_DISCONNECT,
    _ENetEventType_ENET_EVENT_TYPE_NONE as ENET_EVENT_TYPE_NONE,
    _ENetEventType_ENET_EVENT_TYPE_RECEIVE as ENET_EVENT_TYPE_RECEIVE, ENET_HOST_ANY, ENetAddress,
    ENetBuffer, ENetEvent, ENetHost, enet_crc32, enet_host_create, enet_host_flush,
    enet_host_service, enet_initialize, enet_packet_destroy,
};

use super::state::ServerState;
use super::{CHANNEL_COUNT, ENET_INIT, MAX_CLIENTS, SERVICE_TIMEOUT_MS, TICK_INTERVAL_MS};

/// Handle to a running ENet server.
///
/// Created by [`EnetServer::start`], which spawns the event loop on a
/// dedicated thread.
pub struct EnetServer {
    /// The socket address the server is bound to.
    pub addr: SocketAddr,
}

impl EnetServer {
    /// Start the ENet server on the given port.
    ///
    /// Spawns the event loop on a new thread and returns immediately.
    pub fn start(port: u16) -> Self {
        init_enet();
        let addr = SocketAddr::V4(SocketAddrV4::new(Ipv4Addr::UNSPECIFIED, port));
        thread::spawn(move || run_server(port));
        Self { addr }
    }
}

/// One-shot ENet library initialization (thread-safe).
fn init_enet() {
    ENET_INIT.call_once(|| unsafe {
        if enet_initialize() != 0 {
            panic!("failed to initialize ENet");
        }
    });
}

/// Main event loop running on a dedicated thread.
///
/// Polls ENet for events and dispatches them to [`ServerState`] handlers:
///
/// | Event | Handler |
/// |-------|---------|
/// | `CONNECT` | `state.on_connect(peer)` |
/// | `RECEIVE` | `state.on_receive(peer, chan, packet)` |
/// | `DISCONNECT` | `state.on_disconnect(peer)` |
fn run_server(port: u16) {
    let host = create_host(port);
    let mut state = ServerState::default();
    let mut last_tick = Instant::now();
    let tick_interval = Duration::from_millis(TICK_INTERVAL_MS);
    tracing::info!("[ENET] listening on 0.0.0.0:{}", port);

    loop {
        let mut event = unsafe { std::mem::zeroed::<ENetEvent>() };
        let result = unsafe { enet_host_service(host, &mut event, SERVICE_TIMEOUT_MS) };
        if result < 0 {
            tracing::error!("[ENET] host service failed");
            thread::sleep(Duration::from_millis(100));
            continue;
        }

        if result > 0 && event.type_ != ENET_EVENT_TYPE_NONE {
            match event.type_ {
                ENET_EVENT_TYPE_CONNECT => state.on_connect(event.peer),
                ENET_EVENT_TYPE_RECEIVE => {
                    state.on_receive(event.peer, event.channelID, event.packet);
                    unsafe { enet_packet_destroy(event.packet) };
                }
                ENET_EVENT_TYPE_DISCONNECT => state.on_disconnect(event.peer),
                other => tracing::debug!("[ENET] ignored event type={}", other),
            }

            unsafe { enet_host_flush(host) };
        }

        // Tick: periodic frame sync
        if last_tick.elapsed() >= tick_interval {
            // fku
            state.sync_frame();
            last_tick = Instant::now();
        }
    }
}

/// Create and configure an ENet host bound to `port`.
///
/// Enables CRC32 checksum on every packet via [`enet_crc32_checksum`].
fn create_host(port: u16) -> *mut ENetHost {
    let address = ENetAddress {
        host: ENET_HOST_ANY,
        port,
    };
    let host = unsafe { enet_host_create(&address, MAX_CLIENTS, CHANNEL_COUNT, 0, 0) };
    if host.is_null() {
        panic!("failed to create ENet host on port {}", port);
    }
    unsafe {
        (*host).checksum = Some(enet_crc32_checksum);
    }
    host
}

/// ENet CRC32 callback invoked for every incoming and outgoing packet.
unsafe extern "C" fn enet_crc32_checksum(buffers: *const ENetBuffer, buffer_count: usize) -> u32 {
    unsafe { enet_crc32(buffers, buffer_count) }
}
