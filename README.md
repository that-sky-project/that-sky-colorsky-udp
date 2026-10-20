# ColorSky UDP Server

UDP game server for *Sky: Children of the Light*, built on [ENet](http://enet.bespin.org/)
with CRC32 packet checksums.

## Quick Start

Install Rust and add build dependencies 

#### Ubuntu
```bash
sudo apt update
sudo apt install build-essential clang cmake
```

#### Archlinux
```bash
pacman -S base-devel clang cmake
```

#### Termux
```bash
apt update
apt install rust cmake
```


```bash
# Build
cargo build --release

# Run with defaults (0.0.0.0:5413)
cargo run --release

# Custom host & port
cargo run --release -- --host 127.0.0.1 --port 7777

# Config file
cargo run --release -- -c path/to/config.toml
```

## Configuration

### CLI Options

| Flag                    | Description           | Default                      |
| ----------------------- | --------------------- | ---------------------------- |
| `--host <IP>`           | IP address to bind to | `0.0.0.0`                    |
| `--port <PORT>`         | Port to listen on     | `5413`                       |
| `-c`, `--config <PATH>` | Path to config file   | `./config.toml` (if present) |

Priority: **CLI arguments > config file > built-in defaults**.

### config.toml Format

```toml
[server]
host = "0.0.0.0" # also you can use ipv6 with `::`
port = 9999

[qwd]
# Empty url disables the QWD adapter and runs standalone.
url = ""
# UDP endpoint advertised to QWD. Must be reachable by clients.
public_uri = "203.0.113.10:9999"
# Optional stable room id; generated on startup when empty.
room_id = ""
capacity = 8
```

### QWD node mode

This branch includes the QWD (Hermes room manager) node adapter. Enable it by
setting `qwd.url` or passing `--qwd-url`:

```bash
cargo run --release -- \
  --qwd-url wss://manager.example \
  --public-uri 203.0.113.10:5413
```

CLI options `--qwd-url`, `--public-uri`, and `--room-id` override the config
file. The adapter connects to the manager WebSocket (`/` with `?v=1`), sends
`node.ready`, `room.created`, `player.join`/`player.leave`, `room.telemetry`
and `move.result`, and handles `room.reserve`, `move.prepare`, `move.commit`,
`move.cancel`, `player.redirect`, `node.drain`, `room.destroy` and
`room.id_conflict`.

## Build Requirements
## Embedding & Control Plane

The server can be embedded in a larger application. `EnetServer` exposes a
cloneable `ControlHandle`; adapters (HTTP, WebSocket, QWD, CLI, ...) translate
incoming requests into `ControlCommand` values. The library does not depend on
any specific manager.

```rust
use colorsky_udp::control::RoomEndpoint;
use colorsky_udp::protocol::net_address::NetAddress;
use colorsky_udp::server::EnetServer;
use std::net::{IpAddr, Ipv4Addr};

fn main() {
    let server = EnetServer::start("0.0.0.0:5413".parse().unwrap());
    let control = server.control();

    control
        .upsert_room(RoomEndpoint::new(
            "room-b",
            NetAddress::new(IpAddr::V4(Ipv4Addr::new(10, 0, 0, 2)), 5413),
        ))
        .unwrap();

    // Player ids come from ServerEvent::PlayerJoined or your own registry.
    let _ = control.move_player(1, "room-b");
    let _ = control.status();

    control.shutdown().unwrap();
    server.wait();
}
```

Supported commands:

- `move_player`, `move_players` (atomic group), `move_level`
- `cancel_move`
- `kick_player`
- `broadcast_rpc`
- `upsert_room`, `remove_room`
- `revoke_authority`
- `status`, `shutdown`

Events (player join/leave, move started/finished/timeout, authority elected)
are available by starting with `EnetServer::start_with_events(addr, Some(tx))`.

### Threading model

The ENet host and peer pointers are owned by one server thread. Commands are
sent through a bounded `std::sync::mpsc::sync_channel` and executed serially
between ENet polls; the server never shares peer state behind a lock. This keeps
ENet access single-threaded, preserves command ordering, and provides
backpressure through `ControlError::QueueFull`. Synchronous callers use
`ControlHandle::request` with a response channel and a five-second timeout.

## Build Requirements

- Rust **1.85+** (edition 2024)
- ENet C library (linked via `enet-sys`)

### Nix

```bash
nix develop
cargo build --release
```

## Thanks

- [That Sky Project](https://github.com/that-sky-project)
