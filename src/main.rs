use std::net::{IpAddr, Ipv4Addr, SocketAddr};
use std::path::PathBuf;
use std::thread;
use std::time::Duration;

use clap::Parser;
use serde::Deserialize;

/// ColorSky UDP game server.
#[derive(Parser, Debug)]
#[command(name = "colorsky-udp")]
struct Cli {
    /// IP address to bind to
    #[arg(long)]
    host: Option<String>,

    /// Port to listen on
    #[arg(long)]
    port: Option<u16>,

    /// Path to config file (default: ./config.toml if present)
    #[arg(short, long)]
    config: Option<PathBuf>,
}

#[derive(Deserialize, Debug, Default)]
struct Config {
    #[serde(default)]
    server: ServerConfig,
}

#[derive(Deserialize, Debug)]
struct ServerConfig {
    #[serde(default = "default_host")]
    host: String,
    #[serde(default = "default_port")]
    port: u16,
}

fn default_host() -> String {
    "0.0.0.0".to_string()
}

const fn default_port() -> u16 {
    5413
}

impl Default for ServerConfig {
    fn default() -> Self {
        Self {
            host: default_host(),
            port: default_port(),
        }
    }
}

fn load_config(path: &PathBuf) -> Option<Config> {
    match std::fs::read_to_string(path) {
        Ok(content) => match toml::from_str(&content) {
            Ok(cfg) => {
                tracing::info!("loaded config from {}", path.display());
                Some(cfg)
            }
            Err(e) => {
                tracing::warn!("failed to parse {}: {}", path.display(), e);
                None
            }
        },
        Err(e) => {
            tracing::warn!("failed to read {}: {}", path.display(), e);
            None
        }
    }
}

/// Resolve final bind address. Priority: CLI args > config file > built-in defaults.
fn resolve_addr(cli: &Cli) -> SocketAddr {
    let default_config = PathBuf::from("./config.toml");
    let config = cli
        .config
        .as_ref()
        .or_else(|| default_config.exists().then_some(&default_config))
        .and_then(load_config);

    let host: String = cli
        .host
        .clone()
        .or_else(|| config.as_ref().map(|c| c.server.host.clone()))
        .unwrap_or_else(default_host);

    let port: u16 = cli
        .port
        .or_else(|| config.as_ref().map(|c| c.server.port))
        .unwrap_or_else(default_port);

    let ip: IpAddr = host.parse().unwrap_or(IpAddr::V4(Ipv4Addr::UNSPECIFIED));

    SocketAddr::new(ip, port)
}

fn main() {
    tracing_subscriber::fmt::init();
    let cli = Cli::parse();
    let addr = resolve_addr(&cli);

    tracing::info!("starting server on {}", addr);
    let _server = colorsky_udp::server::EnetServer::start(addr);
    loop {
        thread::sleep(Duration::from_secs(1));
    }
}
