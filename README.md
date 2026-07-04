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

| Flag | Description | Default |
|------|-------------|---------|
| `--host <IP>` | IP address to bind to | `0.0.0.0` |
| `--port <PORT>` | Port to listen on | `5413` |
| `-c`, `--config <PATH>` | Path to config file | `./config.toml` (if present) |

Priority: **CLI arguments > config file > built-in defaults**.

### config.toml Format

```toml
[server]
host = "0.0.0.0" # also you can use ipv6 with `::`
port = 9999
```

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
