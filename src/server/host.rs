//! ENet host lifecycle and the server command loop.
//!
//! Handles library initialization, host creation, the main event loop,
//! control commands and CRC32 checksum verification.

use std::net::SocketAddr;
use std::sync::mpsc::{Receiver, SyncSender};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

use enet_sys::{
    _ENetEventType_ENET_EVENT_TYPE_CONNECT as ENET_EVENT_TYPE_CONNECT,
    _ENetEventType_ENET_EVENT_TYPE_DISCONNECT as ENET_EVENT_TYPE_DISCONNECT,
    _ENetEventType_ENET_EVENT_TYPE_NONE as ENET_EVENT_TYPE_NONE,
    _ENetEventType_ENET_EVENT_TYPE_RECEIVE as ENET_EVENT_TYPE_RECEIVE, ENET_HOST_ANY, ENetAddress,
    ENetBuffer, ENetEvent, ENetHost, enet_crc32, enet_host_create, enet_host_destroy,
    enet_host_flush, enet_host_service, enet_initialize, enet_packet_destroy,
};

use crate::control::{
    ControlCommand, ControlError, ControlHandle, ControlRequest, ServerEvent, control_channel,
};

use super::state::ServerState;
use super::{CHANNEL_COUNT, ENET_INIT, MAX_CLIENTS, SERVICE_TIMEOUT_MS, TICK_INTERVAL_MS};

/// Handle to a running ENet server.
///
/// The ENet thread is started by [`EnetServer::start`]. Third-party callers use
/// [`EnetServer::control`] to submit commands; all commands execute on the ENet
/// thread between polls.
pub struct EnetServer {
    /// The socket address the server is bound to.
    pub addr: SocketAddr,
    control: ControlHandle,
    join: Option<JoinHandle<()>>,
}

impl EnetServer {
    /// Start the ENet server bound to the given address.
    pub fn start(addr: SocketAddr) -> Self {
        Self::start_with_events(addr, None)
    }

    /// Start the server and attach an optional event sink.
    pub fn start_with_events(addr: SocketAddr, events: Option<SyncSender<ServerEvent>>) -> Self {
        init_enet();
        let (control_tx, control_rx) = control_channel();
        let join = thread::spawn(move || run_server(addr, control_rx, events));
        Self {
            addr,
            control: ControlHandle::new(control_tx),
            join: Some(join),
        }
    }

    /// Cloneable control handle for third-party adapters.
    pub fn control(&self) -> ControlHandle {
        self.control.clone()
    }

    /// Queue a shutdown command.
    pub fn shutdown(&self) -> Result<(), ControlError> {
        self.control.shutdown()
    }

    /// Block until the server thread exits.
    pub fn wait(mut self) {
        if let Some(join) = self.join.take() {
            let _ = join.join();
        }
    }
}

impl Drop for EnetServer {
    fn drop(&mut self) {
        let _ = self.control.shutdown_blocking();
        if let Some(join) = self.join.take() {
            let _ = join.join();
        }
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
/// Control commands are drained before every ENet poll, so their latency is
/// bounded by [`SERVICE_TIMEOUT_MS`].
fn run_server(
    addr: SocketAddr,
    control_rx: Receiver<ControlRequest>,
    events: Option<SyncSender<ServerEvent>>,
) {
    let host = create_host(addr);
    let mut state = ServerState {
        event_tx: events,
        bind_addr: addr.to_string(),
        ..ServerState::default()
    };
    let mut last_tick = Instant::now();
    let tick_interval = Duration::from_millis(TICK_INTERVAL_MS);
    let mut running = true;

    tracing::info!("[ENET] listening on {}", addr);

    while running {
        while let Ok(request) = control_rx.try_recv() {
            let shutdown = matches!(request.command, ControlCommand::Shutdown);
            let response = state.handle_control(request.command);
            if let Some(reply) = request.reply {
                let _ = reply.send(response);
            }
            if shutdown {
                running = false;
                break;
            }
        }

        if !running {
            break;
        }

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

        // Tick: periodic frame sync, move timeouts and migration expiry.
        if last_tick.elapsed() >= tick_interval {
            state.sync_frame();
            state.sync_level();
            state.snapshot_ack();
            let now = Instant::now();
            state.expire_moves(now);
            state.expire_migration_rosters(now);
            last_tick = Instant::now();
        }
    }

    unsafe { enet_host_destroy(host) };
    tracing::info!("[ENET] server stopped");
}

/// Create and configure an ENet host bound to `addr`.
///
/// Enables CRC32 checksum on every packet via [`enet_crc32_checksum`].
fn create_host(addr: SocketAddr) -> *mut ENetHost {
    let host_ip: u32 = match addr.ip() {
        std::net::IpAddr::V4(v4) if !v4.is_unspecified() => u32::from_ne_bytes(v4.octets()),
        std::net::IpAddr::V4(_) | std::net::IpAddr::V6(_) => ENET_HOST_ANY,
    };
    let address = ENetAddress {
        host: host_ip,
        port: addr.port(),
    };
    let host = unsafe { enet_host_create(&address, MAX_CLIENTS, CHANNEL_COUNT, 0, 0) };
    if host.is_null() {
        panic!("failed to create ENet host on {}", addr);
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

#[cfg(test)]
mod tests {
    use crate::control::ControlResponse;

    use super::*;

    #[test]
    fn starts_accepts_control_and_stops() {
        let server = EnetServer::start("127.0.0.1:0".parse().unwrap());
        let control = server.control();

        let response = control.status().expect("status response");
        let ControlResponse::Status(status) = response else {
            panic!("expected status response");
        };
        assert_eq!(status.peers, 0);

        control.shutdown().expect("shutdown accepted");
        server.wait();
    }
}
