use std::net::{IpAddr, Ipv4Addr, SocketAddr};
use std::path::PathBuf;
use std::sync::mpsc;

use clap::Parser;
use serde::Deserialize;

use colorsky_udp::qwd::{QwdClient, QwdConfig};

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

    /// QWD WebSocket URL (enables the QWD node adapter)
    #[arg(long)]
    qwd_url: Option<String>,

    /// Public UDP host:port reported to QWD (e.g. 203.0.113.10:5413)
    #[arg(long)]
    public_uri: Option<String>,
}

#[derive(Deserialize, Debug, Default)]
struct Config {
    #[serde(default)]
    server: ServerConfig,
    #[serde(default)]
    qwd: QwdFileConfig,
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

const fn default_capacity() -> usize {
    8
}

/// QWD section of `config.toml`.
#[derive(Deserialize, Debug, Default)]
struct QwdFileConfig {
    #[serde(default)]
    url: String,
    #[serde(default)]
    public_uri: String,
    #[serde(default = "default_capacity")]
    capacity: usize,
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

/// Fully resolved runtime configuration.
struct ResolvedConfig {
    addr: SocketAddr,
    qwd: Option<QwdConfig>,
}

/// Resolve final bind address and QWD settings.
/// Priority: CLI args > config file > built-in defaults.
fn resolve_config(cli: &Cli) -> ResolvedConfig {
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
    let addr = SocketAddr::new(ip, port);

    let qwd_url = cli
        .qwd_url
        .clone()
        .or_else(|| config.as_ref().map(|c| c.qwd.url.clone()))
        .unwrap_or_default()
        .trim()
        .to_string();

    let qwd = if qwd_url.is_empty() {
        None
    } else {
        let public_uri = cli
            .public_uri
            .clone()
            .or_else(|| config.as_ref().map(|c| c.qwd.public_uri.clone()))
            .unwrap_or_default();

        let (public_host, public_port) = if public_uri.trim().is_empty() {
            tracing::warn!(
                "qwd.public_uri is empty; advertising 127.0.0.1:{} to QWD",
                port
            );
            ("127.0.0.1".to_string(), port)
        } else {
            match parse_public_uri(&public_uri) {
                Ok(endpoint) => endpoint,
                Err(error) => {
                    tracing::error!(error = %error, public_uri = %public_uri, "invalid qwd.public_uri");
                    std::process::exit(2);
                }
            }
        };

        let capacity = config
            .as_ref()
            .map(|c| c.qwd.capacity)
            .unwrap_or_else(default_capacity)
            .clamp(1, 8);

        Some(QwdConfig::new(qwd_url, public_host, public_port, capacity))
    };

    ResolvedConfig { addr, qwd }
}

fn parse_public_uri(value: &str) -> Result<(String, u16), String> {
    let raw = value.trim();
    if raw.is_empty() {
        return Err("public_uri is empty".to_string());
    }

    let authority = raw
        .split_once("://")
        .map(|(_, rest)| rest)
        .unwrap_or(raw)
        .split('/')
        .next()
        .unwrap_or(raw);

    let (host, port) = if let Some(rest) = authority.strip_prefix('[') {
        let (host, port) = rest
            .split_once("]:")
            .ok_or_else(|| "IPv6 public_uri must look like [::1]:5413".to_string())?;
        (host.to_string(), port)
    } else {
        let (host, port) = authority
            .rsplit_once(':')
            .ok_or_else(|| "public_uri must include a port".to_string())?;
        (host.to_string(), port)
    };

    if host.is_empty() {
        return Err("public_uri has no host".to_string());
    }

    let port = port
        .parse::<u16>()
        .map_err(|_| format!("invalid public_uri port: {port}"))?;
    Ok((host, port))
}

fn main() {
    tracing_subscriber::fmt::init();
    let cli = Cli::parse();
    let resolved = resolve_config(&cli);
    let addr = resolved.addr;

    tracing::info!("starting server on {}", addr);

    let (event_tx, event_rx) = if resolved.qwd.is_some() {
        let (tx, rx) = mpsc::sync_channel(1024);
        (Some(tx), Some(rx))
    } else {
        (None, None)
    };

    let server = colorsky_udp::server::EnetServer::start_with_events(addr, event_tx);
    tracing::info!(
        addr = %server.addr,
        "control plane ready; use EnetServer::control() from an embedding application"
    );

    if let (Some(qwd), Some(events)) = (resolved.qwd, event_rx) {
        tracing::info!(
            qwd_url = %qwd.url,
            public = %format!("{}:{}", qwd.public_host, qwd.public_port),
            "QWD adapter enabled"
        );
        let _qwd_thread = QwdClient::spawn(qwd, server.control(), events);
    }

    server.wait();
}
