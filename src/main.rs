mod crypto;
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
use tracing_subscriber::EnvFilter;

#[derive(Debug, Parser)]
#[command(name = "rushway", version, about = "Rust forwarding service compatible with GoWay")]
struct Args {
    #[arg(short, long)] config: Option<PathBuf>,
    #[arg(short = 'p')] port: Option<String>,
    #[arg(short = 'u', long = "up")] upstream: Option<String>,
    #[arg(short = 'k')] key: Option<String>,
    #[arg(long = "fakehost")] fakehost: Option<String>,
    #[arg(long = "mux", default_value_t = true)] mux: bool,
    #[arg(long = "no-mux", default_value_t = false)] no_mux: bool,
    #[arg(long = "mux-sessions", default_value_t = 4, value_parser = clap::value_parser!(usize).range(1..=64))] mux_sessions: usize,
    #[arg(long = "allow-open", default_value_t = false)] allow_open: bool,
    #[arg(long = "verify-ssl", default_value_t = false)] verify_ssl: bool,
    #[arg(short = 'W')] buffer_kib: Option<usize>,
    #[arg(long = "socket-buffer")] socket_buffer_kib: Option<usize>,
    #[arg(long = "no-tcp-nodelay", default_value_t = false)] no_tcp_nodelay: bool,
    #[arg(long = "no-tcp-keepalive", default_value_t = false)] no_tcp_keepalive: bool,
    #[arg(long = "dns")] dns: Option<String>,
    #[arg(long = "block-local")] block_local: bool,
    #[arg(long = "no-block-local", default_value_t = false)] no_block_local: bool,
    #[arg(long = "max-conn", value_parser = clap::value_parser!(usize).range(1..=1_000_000))] max_conn: Option<usize>,
    #[arg(long = "connection-timeout")] connection_timeout: Option<u64>,
}

fn apply_listen_arg(cfg:&mut RuntimeConfig,value:&str)->Result<()> { if let Ok(port)=value.parse::<u16>(){cfg.proxy_port=port;return Ok(())} let trimmed=value.trim();if let Some(port_text)=trimmed.strip_prefix(':'){cfg.proxy_port=port_text.parse::<u16>().map_err(|_|anyhow!("invalid listen address: {value}"))?;return Ok(())}let addr:std::net::SocketAddr=trimmed.parse().map_err(|_|anyhow!("invalid listen address: {value}"))?;cfg.proxy_host=addr.ip().to_string();cfg.proxy_port=addr.port();Ok(()) }

async fn load_json(path:PathBuf)->Result<RuntimeConfig>{let text=tokio::fs::read_to_string(&path).await?;let v:serde_json::Value=serde_json::from_str(&text)?;let mut cfg=RuntimeConfig::default();if let Some(x)=v.get("proxy_host").or_else(||v.get("proxyHost")).and_then(|x|x.as_str()){cfg.proxy_host=x.into()}if let Some(x)=v.get("proxy_port").or_else(||v.get("proxyPort")).and_then(|x|x.as_u64()){cfg.proxy_port=u16::try_from(x).map_err(|_|anyhow!("proxy_port out of range"))?}if let Some(x)=v.get("upstream").or_else(||v.get("upstreamUrl")).and_then(|x|x.as_str()){cfg.upstream=Some(x.into())}if let Some(x)=v.get("key").and_then(|x|x.as_str()){cfg.key=Some(x.into())}if let Some(x)=v.get("fakehost").or_else(||v.get("fakeHost")).and_then(|x|x.as_str()){cfg.fakehost=Some(x.into())}if let Some(x)=v.get("mux").and_then(|x|x.as_bool()){cfg.mux=x}if let Some(x)=v.get("buffer_size").or_else(||v.get("bufferSize")).and_then(|x|x.as_u64()){cfg.buffer_size=usize::try_from(x).map_err(|_|anyhow!("buffer_size out of range"))?}if let Some(x)=v.get("connection_timeout").or_else(||v.get("connectionTimeout")).and_then(|x|x.as_u64()){cfg.connection_timeout=x}if let Some(x)=v.get("allow_open").or_else(||v.get("allowOpen")).and_then(|x|x.as_bool()){cfg.allow_open=x}if let Some(x)=v.get("max_connections").or_else(||v.get("maxConnections")).and_then(|x|x.as_u64()){cfg.max_connections=usize::try_from(x).map_err(|_|anyhow!("max_connections out of range"))?.clamp(1,1_000_000)}if let Some(x)=v.get("block_local").or_else(||v.get("blockLocal")).and_then(|x|x.as_bool()){cfg.block_local=x}if let Some(x)=v.get("tcp_nodelay").or_else(||v.get("tcpNoDelay")).and_then(|x|x.as_bool()){cfg.tcp_nodelay=x}tracing::info!(config=%path.display(),"configuration loaded");Ok(cfg)}

#[tokio::main]
async fn main()->Result<()>{tracing_subscriber::fmt().with_env_filter(EnvFilter::try_from_default_env().unwrap_or_else(|_|EnvFilter::new("info"))).with_target(false).init();let args=Args::parse();let mut cfg=if let Some(path)=args.config{load_json(path).await?}else{RuntimeConfig::default()};if let Some(v)=args.port.as_deref(){apply_listen_arg(&mut cfg,v)?}if let Some(v)=args.upstream{cfg.upstream=Some(v)}if let Some(v)=args.key{cfg.key=Some(v)}if let Some(v)=args.fakehost{cfg.fakehost=Some(v)}if let Some(v)=args.buffer_kib{cfg.buffer_size=v.saturating_mul(1024).max(16*1024)}let _socket_buffer_bytes=args.socket_buffer_kib.map(|v|v.saturating_mul(1024));cfg.tcp_nodelay=!args.no_tcp_nodelay;if args.no_mux{cfg.mux=false}else if args.mux{cfg.mux=true}if args.allow_open{cfg.allow_open=true}if let Some(v)=args.max_conn{cfg.max_connections=v}if args.no_block_local{cfg.block_local=false}else if args.block_local{cfg.block_local=true}if let Some(v)=args.connection_timeout{cfg.connection_timeout=v}let _=args.dns;let _=args.no_tcp_keepalive;std::env::set_var("RUSHWAY_MUX_SESSIONS",args.mux_sessions.to_string());
    if let Some(upstream)=cfg.upstream.as_deref(){
        if upstream.starts_with("wss://"){if cfg.mux{return wss_client::run_client_from_config(cfg,args.verify_ssl).await}return wss_client::run_non_mux_from_config(cfg,args.verify_ssl).await}
        if upstream.starts_with("quic://")||upstream.starts_with("quic+tls://"){return quic::run_client(cfg,args.verify_ssl).await}
        if cfg.mux{return mux_pool::run_client(cfg).await}return nonmux::run_client(cfg).await
    }
    if cfg.key.is_none()&&!cfg.allow_open{bail!("server mode without -k requires --allow-open")}
    if cfg.mux{let(tcp_result,_quic_result)=tokio::join!(runtime::run_server(cfg.clone()),quic::run_server(cfg));tcp_result}else{let(tcp_result,_quic_result)=tokio::join!(nonmux::run_server(cfg.clone()),quic::run_server(cfg));tcp_result}
}

#[cfg(test)]
mod tests{use super::*;use clap::Parser;#[test]fn parses_basic_runtime_args(){let a=Args::parse_from(["rushway","-p",":9192","--up","ws://127.0.0.1:8080/ws"]);assert_eq!(a.port.as_deref(),Some(":9192"));assert_eq!(a.upstream.as_deref(),Some("ws://127.0.0.1:8080/ws"));assert_eq!(a.mux_sessions,4)}#[test]fn parses_policy_args(){let a=Args::parse_from(["rushway","--no-tcp-nodelay","--no-tcp-keepalive","--max-conn","32","--no-block-local"]);assert!(a.no_tcp_nodelay);assert!(a.no_tcp_keepalive);assert_eq!(a.max_conn,Some(32));assert!(a.no_block_local)}#[test]fn parses_verify_ssl(){let a=Args::parse_from(["rushway","--up","wss://example.com/ws","--verify-ssl"]);assert!(a.verify_ssl)}#[test]fn parses_listen_forms(){let mut cfg=RuntimeConfig::default();apply_listen_arg(&mut cfg,"127.0.0.1:1080").unwrap();assert_eq!(cfg.proxy_host,"127.0.0.1");assert_eq!(cfg.proxy_port,1080);apply_listen_arg(&mut cfg,"9192").unwrap();assert_eq!(cfg.proxy_port,9192)}}
