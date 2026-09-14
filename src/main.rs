mod crypto;
mod dns;
mod mux_pool;
mod nonmux;
mod protocol;
mod proxy;
mod quic;
mod runtime;
mod tls;
mod udp_relay;
mod ws;
mod wss_client;

use anyhow::{anyhow, bail, Result};
use clap::Parser;
use runtime::RuntimeConfig;
use std::path::PathBuf;
use tokio::io::{copy_bidirectional, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};
use tokio::time::{sleep, Duration};
use tracing_subscriber::EnvFilter;

#[derive(Debug, Parser)]
#[command(
    name = "rushway",
    version,
    about = "Rust forwarding service compatible with GoWay"
)]
struct Args {
    #[arg(short, long)]
    config: Option<PathBuf>,
    #[arg(short = 'p')]
    port: Option<String>,
    #[arg(short = 'u', long = "up")]
    upstream: Option<String>,
    #[arg(short = 'k')]
    key: Option<String>,
    #[arg(long = "fakehost")]
    fakehost: Option<String>,
    #[arg(long = "mux", default_value_t = true)]
    mux: bool,
    #[arg(long = "no-mux", default_value_t = false)]
    no_mux: bool,
    #[arg(long = "mux-sessions", default_value_t = 4, value_parser = clap::value_parser!(usize))]
    mux_sessions: usize,
    #[arg(long = "allow-open", default_value_t = false)]
    allow_open: bool,
    #[arg(long = "verify-ssl", default_value_t = false)]
    verify_ssl: bool,
    #[arg(long = "wss-server", default_value_t = false)]
    wss_server: bool,
    #[arg(short = 'W')]
    buffer_kib: Option<usize>,
    #[arg(long = "socket-buffer")]
    socket_buffer_kib: Option<usize>,
    #[arg(long = "no-tcp-nodelay", default_value_t = false)]
    no_tcp_nodelay: bool,
    #[arg(long = "no-tcp-keepalive", default_value_t = false)]
    no_tcp_keepalive: bool,
    #[arg(long = "dns")]
    dns: Option<String>,
    #[arg(long = "block-local", default_value_t = false)]
    block_local: bool,
    #[arg(long = "no-block-local", default_value_t = false)]
    no_block_local: bool,
    #[arg(long = "max-conn", value_parser = clap::value_parser!(usize))]
    max_conn: Option<usize>,
    #[arg(long = "connection-timeout")]
    connection_timeout: Option<u64>,
    #[arg(long = "log", default_value = "INFO")]
    log_level: String,
    #[arg(long = "log-file")]
    log_file: Option<PathBuf>,
    #[arg(long = "tui", default_value_t = false)]
    tui: bool,
    #[arg(long = "cpuprofile")]
    cpu_profile: Option<PathBuf>,
    #[arg(long = "cpuprofile-duration")]
    cpu_profile_duration: Option<u64>,
}

const LEGACY_LONG_FLAGS: &[&str] = &[
    "up",
    "fakehost",
    "mux",
    "no-mux",
    "mux-sessions",
    "allow-open",
    "verify-ssl",
    "socket-buffer",
    "no-tcp-nodelay",
    "no-tcp-keepalive",
    "dns",
    "block-local",
    "no-block-local",
    "max-conn",
    "connection-timeout",
    "log",
    "log-file",
    "tui",
    "version",
    "cpuprofile",
    "cpuprofile-duration",
];

fn normalize_legacy_args<I, S>(args: I) -> Vec<String>
where
    I: IntoIterator<Item = S>,
    S: Into<String>,
{
    args.into_iter()
        .map(Into::into)
        .map(|arg| {
            for name in LEGACY_LONG_FLAGS {
                let exact = format!("-{name}");
                if arg == exact {
                    return format!("--{name}");
                }
                if let Some(value) = arg.strip_prefix(&format!("{exact}=")) {
                    let bool_value = match value.to_ascii_lowercase().as_str() {
                        "true" => Some(true),
                        "false" => Some(false),
                        _ => None,
                    };
                    if let Some(value) = bool_value {
                        match *name {
                            "mux" | "block-local" | "allow-open" | "verify-ssl" | "tui" => {
                                return if value {
                                    format!("--{name}")
                                } else {
                                    format!("--no-{name}")
                                };
                            }
                            "no-mux" | "no-block-local" | "no-tcp-nodelay" | "no-tcp-keepalive" => {
                                if value {
                                    return format!("--{name}");
                                }
                                return match *name {
                                    "no-mux" => "--mux".to_string(),
                                    "no-block-local" => "--block-local".to_string(),
                                    _ => String::new(),
                                };
                            }
                            _ => {}
                        }
                    }
                    return format!("--{name}={value}");
                }
            }
            arg
        })
        .filter(|arg| !arg.is_empty())
        .collect()
}

fn apply_listen_arg(cfg: &mut RuntimeConfig, value: &str) -> Result<()> {
    if let Ok(port) = value.parse::<u16>() {
        cfg.proxy_port = port;
        return Ok(());
    }
    let trimmed = value.trim();
    if let Some(port_text) = trimmed.strip_prefix(':') {
        cfg.proxy_port = port_text
            .parse::<u16>()
            .map_err(|_| anyhow!("invalid listen address: {value}"))?;
        return Ok(());
    }
    let addr: std::net::SocketAddr = trimmed
        .parse()
        .map_err(|_| anyhow!("invalid listen address: {value}"))?;
    cfg.proxy_host = addr.ip().to_string();
    cfg.proxy_port = addr.port();
    Ok(())
}

fn tracing_filter(level: &str) -> String {
    match level.trim().to_ascii_uppercase().as_str() {
        "DEBUG" => "debug".into(),
        "WARN" | "WARNING" => "warn".into(),
        "ERROR" => "error".into(),
        "OFF" => "off".into(),
        _ => "info".into(),
    }
}

async fn load_json(path: PathBuf) -> Result<RuntimeConfig> {
    let text = tokio::fs::read_to_string(&path).await?;
    let v: serde_json::Value = serde_json::from_str(&text)?;
    let mut cfg = RuntimeConfig::default();
    if let Some(x) = v
        .get("proxy_host")
        .or_else(|| v.get("proxyHost"))
        .and_then(|x| x.as_str())
    {
        cfg.proxy_host = x.into()
    }
    if let Some(x) = v
        .get("proxy_port")
        .or_else(|| v.get("proxyPort"))
        .and_then(|x| x.as_u64())
    {
        cfg.proxy_port = u16::try_from(x).map_err(|_| anyhow!("proxy_port out of range"))?
    }
    if let Some(x) = v
        .get("upstream")
        .or_else(|| v.get("upstreamUrl"))
        .and_then(|x| x.as_str())
    {
        cfg.upstream = Some(x.into())
    }
    if let Some(x) = v.get("key").and_then(|x| x.as_str()) {
        cfg.key = Some(x.into())
    }
    if let Some(x) = v
        .get("fakehost")
        .or_else(|| v.get("fakeHost"))
        .and_then(|x| x.as_str())
    {
        cfg.fakehost = Some(x.into())
    }
    if let Some(x) = v.get("mux").and_then(|x| x.as_bool()) {
        cfg.mux = x
    }
    if let Some(x) = v
        .get("buffer_size")
        .or_else(|| v.get("bufferSize"))
        .and_then(|x| x.as_u64())
    {
        cfg.buffer_size = usize::try_from(x).map_err(|_| anyhow!("buffer_size out of range"))?
    }
    if let Some(x) = v
        .get("connection_timeout")
        .or_else(|| v.get("connectionTimeout"))
        .and_then(|x| x.as_u64())
    {
        cfg.connection_timeout = x
    }
    if let Some(x) = v
        .get("allow_open")
        .or_else(|| v.get("allowOpen"))
        .and_then(|x| x.as_bool())
    {
        cfg.allow_open = x
    }
    if let Some(x) = v
        .get("max_connections")
        .or_else(|| v.get("maxConnections"))
        .and_then(|x| x.as_u64())
    {
        cfg.max_connections = usize::try_from(x)
            .map_err(|_| anyhow!("max_connections out of range"))?
            .clamp(1, 1_000_000)
    }
    if let Some(x) = v
        .get("block_local")
        .or_else(|| v.get("blockLocal"))
        .and_then(|x| x.as_bool())
    {
        cfg.block_local = x
    }
    if let Some(x) = v
        .get("tcp_nodelay")
        .or_else(|| v.get("tcpNoDelay"))
        .and_then(|x| x.as_bool())
    {
        cfg.tcp_nodelay = x
    }
    if let Some(x) = v
        .get("tcp_keepalive")
        .or_else(|| v.get("tcpKeepAlive"))
        .and_then(|x| x.as_bool())
    {
        cfg.tcp_keepalive = x
    }
    if let Some(x) = v
        .get("socket_buffer")
        .or_else(|| v.get("socketBuffer"))
        .and_then(|x| x.as_u64())
    {
        cfg.socket_buffer = usize::try_from(x).map_err(|_| anyhow!("socket_buffer out of range"))?
    }
    if let Some(x) = v
        .get("dns")
        .or_else(|| v.get("dnsServer"))
        .and_then(|x| x.as_str())
    {
        dns::configure(Some(x.into())).await?
    }
    tracing::info!(config=%path.display(), "configuration loaded");
    Ok(cfg)
}

async fn run_wss_server(cfg: RuntimeConfig) -> Result<()> {
    let public_bind = format!("{}:{}", cfg.proxy_host, cfg.proxy_port);
    let listener = TcpListener::bind(&public_bind).await?;
    let internal_listener = TcpListener::bind(("127.0.0.1", 0)).await?;
    let internal_port = internal_listener.local_addr()?.port();
    drop(internal_listener);

    let mut internal_cfg = cfg.clone();
    internal_cfg.proxy_host = "127.0.0.1".into();
    internal_cfg.proxy_port = internal_port;
    tokio::spawn(async move {
        if let Err(error) = runtime::run_server(internal_cfg).await {
            tracing::error!(%error, "private WS runtime stopped");
        }
    });

    let acceptor = tls::standalone_server_acceptor()?;
    tracing::info!("RushWay WSS server listening on {public_bind}");
    loop {
        let (stream, peer) = listener.accept().await?;
        let acceptor = acceptor.clone();
        tokio::spawn(async move {
            let result = async {
                let mut tls_stream = acceptor.accept(stream).await?;
                let request = ws::read_http_headers(&mut tls_stream).await?;
                let key = ws::validate_server_handshake(&request)?;
                tls_stream
                    .write_all(&ws::build_server_handshake_response(&key))
                    .await?;

                let mut internal = None;
                for _ in 0..100 {
                    match TcpStream::connect(("127.0.0.1", internal_port)).await {
                        Ok(stream) => {
                            internal = Some(stream);
                            break;
                        }
                        Err(_) => sleep(Duration::from_millis(20)).await,
                    }
                }
                let mut internal = internal.ok_or_else(|| anyhow!("private WS runtime did not start"))?;
                let (request, key) = ws::build_client_handshake_request(
                    &format!("127.0.0.1:{internal_port}"),
                    "/",
                    None,
                    Some("same-origin"),
                );
                internal.write_all(&request).await?;
                let response = ws::read_http_headers(&mut internal).await?;
                ws::validate_client_handshake_response(&response, &key)?;
                copy_bidirectional(&mut tls_stream, &mut internal).await?;
                Ok::<(), anyhow::Error>(())
            }
            .await;
            if let Err(error) = result {
                tracing::debug!(%peer, %error, "WSS connection closed");
            }
        });
    }
}

#[tokio::main]
async fn main() -> Result<()> {
    let raw_args = normalize_legacy_args(std::env::args());
    let args = Args::parse_from(&raw_args);
    if !(1..=64).contains(&args.mux_sessions) {
        return Err(anyhow!("mux-sessions must be between 1 and 64"));
    }
    if let Some(v) = args.max_conn {
        if !(1..=1_000_000).contains(&v) {
            return Err(anyhow!("max-conn must be between 1 and 1000000"));
        }
    }
    let filter = tracing_filter(&args.log_level);
    tracing_subscriber::fmt()
        .with_env_filter(EnvFilter::new(filter))
        .with_target(false)
        .init();
    let mut cfg = if let Some(path) = args.config.clone() {
        load_json(path).await?
    } else {
        RuntimeConfig::default()
    };
    if args.tui {
        tracing::warn!("-tui accepted for GoWay CLI compatibility; RushWay currently uses log output without a TUI dashboard");
    }
    if args.log_file.is_some() {
        tracing::warn!(
            "-log-file accepted for GoWay CLI compatibility; file logging is not yet enabled"
        );
    }
    if args.cpu_profile.is_some() || args.cpu_profile_duration.is_some() {
        tracing::warn!("CPU profiling flags accepted for GoWay CLI compatibility; profiling output is not yet enabled");
    }
    if let Some(v) = args.port.as_deref() {
        apply_listen_arg(&mut cfg, v)?;
    }
    if let Some(v) = args.upstream {
        cfg.upstream = Some(v);
    }
    if let Some(v) = args.key {
        cfg.key = Some(v);
    }
    if let Some(v) = args.fakehost {
        cfg.fakehost = Some(v);
    }
    if let Some(v) = args.buffer_kib {
        cfg.buffer_size = v.saturating_mul(1024).max(16 * 1024);
    }
    if let Some(v) = args.socket_buffer_kib {
        cfg.socket_buffer = v;
    }
    cfg.tcp_nodelay = !args.no_tcp_nodelay;
    cfg.tcp_keepalive = !args.no_tcp_keepalive;
    if args.no_mux {
        cfg.mux = false;
    } else if args.mux {
        cfg.mux = true;
    }
    if args.allow_open {
        cfg.allow_open = true;
    }
    if let Some(v) = args.max_conn {
        cfg.max_connections = v;
    }
    if args.no_block_local {
        cfg.block_local = false;
    } else if args.block_local {
        cfg.block_local = true;
    }
    if let Some(v) = args.connection_timeout {
        cfg.connection_timeout = v;
    }
    if let Some(v) = args.dns {
        dns::configure(Some(v)).await?;
    }
    std::env::set_var("RUSHWAY_MUX_SESSIONS", args.mux_sessions.to_string());

    if let Some(upstream) = cfg.upstream.as_deref() {
        if upstream.starts_with("wss://") {
            if cfg.mux {
                return wss_client::run_client_from_config(cfg, args.verify_ssl).await;
            }
            return wss_client::run_non_mux_from_config(cfg, args.verify_ssl).await;
        }
        if upstream.starts_with("quic://") || upstream.starts_with("quic+tls://") {
            return quic::run_client(cfg, args.verify_ssl).await;
        }
        if cfg.mux {
            return mux_pool::run_client(cfg).await;
        }
        return nonmux::run_client(cfg).await;
    }
    if cfg.key.is_none() && !cfg.allow_open {
        bail!("server mode without -k requires --allow-open");
    }
    if args.wss_server {
        return run_wss_server(cfg).await;
    }
    if cfg.mux {
        let (tcp_result, _quic_result) =
            tokio::join!(runtime::run_server(cfg.clone()), quic::run_server(cfg));
        tcp_result
    } else {
        let (tcp_result, _quic_result) =
            tokio::join!(nonmux::run_server(cfg.clone()), quic::run_server(cfg));
        tcp_result
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use clap::Parser;
    #[test]
    fn normalizes_goway_single_dash_long_flags_and_bool_values() {
        let got = normalize_legacy_args([
            "rushway",
            "-up=ws://127.0.0.1:8080/ws",
            "-fakehost",
            "edge.example.com",
            "-mux=true",
            "-block-local=false",
            "-socket-buffer",
            "128",
            "-no-tcp-keepalive=false",
            "-log",
            "ERROR",
            "-version",
        ]);
        assert_eq!(
            got,
            vec![
                "rushway",
                "--up=ws://127.0.0.1:8080/ws",
                "--fakehost",
                "edge.example.com",
                "--mux",
                "--no-block-local",
                "--socket-buffer",
                "128",
                "--log",
                "ERROR",
                "--version"
            ]
        );
    }
    #[test]
    fn preserves_short_flags_and_values() {
        let got = normalize_legacy_args(["rushway", "-p", ":9192", "-k", "secret", "-W", "256"]);
        assert_eq!(
            got,
            vec!["rushway", "-p", ":9192", "-k", "secret", "-W", "256"]
        );
    }
    #[test]
    fn parses_basic_runtime_args() {
        let args = Args::parse_from([
            "rushway",
            "-p",
            ":9192",
            "--up",
            "ws://127.0.0.1:8080/ws",
            "--log",
            "ERROR",
        ]);
        assert_eq!(args.port.as_deref(), Some(":9192"));
        assert_eq!(args.upstream.as_deref(), Some("ws://127.0.0.1:8080/ws"));
        assert_eq!(args.mux_sessions, 4);
        assert_eq!(args.log_level, "ERROR");
    }
    #[test]
    fn parses_policy_args() {
        let args = Args::parse_from([
            "rushway",
            "--no-tcp-nodelay",
            "--no-tcp-keepalive",
            "--max-conn",
            "32",
            "--no-block-local",
        ]);
        assert!(args.no_tcp_nodelay);
        assert!(args.no_tcp_keepalive);
        assert_eq!(args.max_conn, Some(32));
        assert!(args.no_block_local);
    }
    #[test]
    fn parses_verify_ssl() {
        let args = Args::parse_from(["rushway", "--up", "wss://example.com/ws", "--verify-ssl"]);
        assert!(args.verify_ssl);
    }
    #[test]
    fn parses_listen_forms() {
        let mut cfg = RuntimeConfig::default();
        apply_listen_arg(&mut cfg, "127.0.0.1:1080").unwrap();
        assert_eq!(cfg.proxy_host, "127.0.0.1");
        assert_eq!(cfg.proxy_port, 1080);
        apply_listen_arg(&mut cfg, "9192").unwrap();
        assert_eq!(cfg.proxy_port, 9192);
    }
}