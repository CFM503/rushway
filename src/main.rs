mod protocol;
mod proxy;
mod runtime;
mod ws;

use anyhow::Result;
use clap::Parser;
use runtime::RuntimeConfig;
use std::path::PathBuf;
use tracing_subscriber::EnvFilter;

#[derive(Debug, Parser)]
#[command(name = "rushway", version, about = "Rust forwarding service compatible with GoWay")]
struct Args {
    #[arg(short, long)]
    config: Option<PathBuf>,
    #[arg(short = 'p')]
    port: Option<u16>,
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
    #[arg(long = "allow-open", default_value_t = false)]
    allow_open: bool,
    #[arg(short = 'W')]
    buffer_size: Option<usize>,
    #[arg(long = "connection-timeout")]
    connection_timeout: Option<u64>,
}

#[tokio::main]
async fn main() -> Result<()> {
    tracing_subscriber::fmt().with_env_filter(EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info"))).with_target(false).init();
    let args = Args::parse();
    let mut cfg = if let Some(path) = args.config {
        serde_json::from_str::<RuntimeConfig>(&tokio::fs::read_to_string(path).await?)?
    } else { RuntimeConfig::default() };
    if let Some(v) = args.port { cfg.proxy_port = v; }
    if let Some(v) = args.upstream { cfg.upstream = Some(v); }
    if let Some(v) = args.key { cfg.key = Some(v); }
    if let Some(v) = args.fakehost { cfg.fakehost = Some(v); }
    if let Some(v) = args.buffer_size { cfg.buffer_size = v; }
    if let Some(v) = args.connection_timeout { cfg.connection_timeout = v; }
    if args.no_mux { cfg.mux = false; } else { cfg.mux = args.mux; }
    if args.allow_open { cfg.allow_open = true; }
    if cfg.upstream.is_some() { runtime::run_client(cfg).await } else { runtime::run_server(cfg).await }
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
}
