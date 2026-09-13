mod crypto;
mod mux_pool;
mod protocol;
mod proxy;
mod runtime;
mod tls;
mod ws;
mod wss_client;

use anyhow::{anyhow, Result};
use clap::Parser;
use runtime::RuntimeConfig;
use std::path::PathBuf;
use tracing_subscriber::EnvFilter;

#[derive(Debug, Parser)]
#[command(name = "rushway", version, about = "Rust forwarding service compatible with GoWay")]
struct Args {
    #[arg(short, long)] config: Option<PathBuf>,
    #[arg(short = 'p')] port: Option<u16>,
    #[arg(short = 'u', long = "up")] upstream: Option<String>,
    #[arg(short = 'k')] key: Option<String>,
    #[arg(long = "fakehost")] fakehost: Option<String>,
    #[arg(long = "mux", default_value_t = true)] mux: bool,
    #[arg(long = "no-mux", default_value_t = false)] no_mux: bool,
    #[arg(long = "allow-open", default_value_t = false)] allow_open: bool,
    #[arg(long = "verify-ssl", default_value_t = false)] verify_ssl: bool,
    #[arg(short = 'W')] buffer_size: Option<usize>,
    #[arg(long = "connection-timeout")] connection_timeout: Option<u64>,
}

async fn load_json(path: PathBuf) -> Result<RuntimeConfig> {
    let text = tokio::fs::read_to_string(&path).await?;
    let v: serde_json::Value = serde_json::from_str(&text)?;
    let mut cfg = RuntimeConfig::default();
    if let Some(x) = v.get("proxy_host").or_else(|| v.get("proxyHost")).and_then(|x| x.as_str()) { cfg.proxy_host = x.into(); }
    if let Some(x) = v.get("proxy_port").or_else(|| v.get("proxyPort")).and_then(|x| x.as_u64()) { cfg.proxy_port = u16::try_from(x).map_err(|_| anyhow!("proxy_port out of range"))?; }
    if let Some(x) = v.get("upstream").or_else(|| v.get("upstreamUrl")).and_then(|x| x.as_str()) { cfg.upstream = Some(x.into()); }
    if let Some(x) = v.get("key").and_then(|x| x.as_str()) { cfg.key = Some(x.into()); }
    if let Some(x) = v.get("fakehost").or_else(|| v.get("fakeHost")).and_then(|x| x.as_str()) { cfg.fakehost = Some(x.into()); }
    if let Some(x) = v.get("mux").and_then(|x| x.as_bool()) { cfg.mux = x; }
    if let Some(x) = v.get("buffer_size").or_else(|| v.get("bufferSize")).and_then(|x| x.as_u64()) { cfg.buffer_size = usize::try_from(x).map_err(|_| anyhow!("buffer_size out of range"))?; }
    if let Some(x) = v.get("connection_timeout").or_else(|| v.get("connectionTimeout")).and_then(|x| x.as_u64()) { cfg.connection_timeout = x; }
    if let Some(x) = v.get("allow_open").or_else(|| v.get("allowOpen")).and_then(|x| x.as_bool()) { cfg.allow_open = x; }
    tracing::info!(config = %path.display(), "configuration loaded");
    Ok(cfg)
}

#[tokio::main]
async fn main() -> Result<()> {
    tracing_subscriber::fmt().with_env_filter(EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info"))).with_target(false).init();
    let args = Args::parse();
    let mut cfg = if let Some(path) = args.config { load_json(path).await? } else { RuntimeConfig::default() };
    if let Some(v) = args.port { cfg.proxy_port = v; }
    if let Some(v) = args.upstream { cfg.upstream = Some(v); }
    if let Some(v) = args.key { cfg.key = Some(v); }
    if let Some(v) = args.fakehost { cfg.fakehost = Some(v); }
    if let Some(v) = args.buffer_size { cfg.buffer_size = v; }
    if let Some(v) = args.connection_timeout { cfg.connection_timeout = v; }
    if args.no_mux { cfg.mux = false; } else { cfg.mux = args.mux; }
    if args.allow_open { cfg.allow_open = true; }
    if let Some(upstream) = cfg.upstream.as_deref() {
        if upstream.starts_with("wss://") {
            return wss_client::run_client_from_config(cfg, args.verify_ssl).await;
        }
        mux_pool::run_client(cfg).await
    } else {
        runtime::run_server(cfg).await
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use clap::Parser;
    #[test]
    fn parses_basic_runtime_args() {
        let a = Args::parse_from(["rushway", "-p", "9192", "--up", "ws://127.0.0.1:8080/ws"]);
        assert_eq!(a.port, Some(9192));
        assert_eq!(a.upstream.as_deref(), Some("ws://127.0.0.1:8080/ws"));
    }
    #[test]
    fn parses_verify_ssl() {
        let a = Args::parse_from(["rushway", "--up", "wss://example.com/ws", "--verify-ssl"]);
        assert!(a.verify_ssl);
    }
}
