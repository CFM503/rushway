//! WSS client paths for GoWay-compatible upstreams.

use crate::crypto::XorCipher;
use crate::dns;
use crate::mux_writer::MuxFrameWriter;
use crate::protocol::{MuxCommand, MuxFrame, OwnedMuxFrame, SynPayload};
use crate::proxy::{
    parse_authority_with_default, read_client_proxy_request, socks5_success_response, SocksCommand,
    TargetAddr,
};
use crate::runtime::{
    apply_socket_options, drain_join_set, recycle_buf, relay_buf, wait_shutdown, RuntimeConfig,
};
use crate::tls;
use crate::ws::{
    build_client_handshake_request, encode_ws_frame, read_frame, read_frame_owned,
    read_http_headers_timeout, redact_handshake_request, validate_client_handshake_response,
    write_frame, write_frame_borrowed,
};
use anyhow::{anyhow, bail, Context, Result};
use std::collections::HashMap;
use std::net::SocketAddr;
use std::sync::{
    atomic::{AtomicBool, AtomicU32, AtomicUsize, Ordering},
    Arc,
};
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream, UdpSocket};
use tokio::sync::{mpsc, Mutex, RwLock};
use tokio::time::{timeout, Duration};

pub(crate) trait Transport: AsyncRead + AsyncWrite + Unpin + Send {}
impl<T: AsyncRead + AsyncWrite + Unpin + Send> Transport for T {}
pub(crate) type BoxTransport = Box<dyn Transport>;
pub(crate) type BoxReader = tokio::io::ReadHalf<BoxTransport>;
pub(crate) type BoxWriter = tokio::io::WriteHalf<BoxTransport>;

const DEFAULT_SESSION_COUNT: usize = 4;
const MAX_SESSION_COUNT: usize = 64;
const MAX_STREAMS_PER_SESSION: usize = 2048;

#[derive(Debug, Clone)]
pub(crate) struct WssConfig {
    pub(crate) proxy_host: String,
    pub(crate) proxy_port: u16,
    pub(crate) upstream: String,
    pub(crate) key: Option<String>,
    pub(crate) fakehost: Option<String>,
    pub(crate) buffer_size: usize,
    pub(crate) connection_timeout: u64,
    pub(crate) verify_ssl: bool,
    pub(crate) tcp_nodelay: bool,
    pub(crate) tcp_keepalive: bool,
    pub(crate) socket_buffer: usize,
    pub(crate) obfs: bool,
}
impl WssConfig {
    #[allow(dead_code)]
    pub(crate) fn from_runtime_config(cfg: &RuntimeConfig, verify_ssl: bool) -> Result<Self> {
        let upstream = cfg
            .upstream
            .clone()
            .ok_or_else(|| anyhow!("WSS client requires upstream"))?;
        Ok(Self {
            proxy_host: cfg.proxy_host.clone(),
            proxy_port: cfg.proxy_port,
            upstream,
            key: cfg.key.clone(),
            fakehost: cfg.fakehost.clone(),
            buffer_size: cfg.buffer_size,
            connection_timeout: cfg.connection_timeout,
            verify_ssl,
            tcp_nodelay: cfg.tcp_nodelay,
            tcp_keepalive: cfg.tcp_keepalive,
            socket_buffer: cfg.socket_buffer,
            obfs: cfg.obfs,
        })
    }
}
fn cipher(key: &Option<String>) -> XorCipher {
    XorCipher::new(key.as_deref().unwrap_or(""))
}
fn configured_session_count() -> usize {
    std::env::var("RUSHWAY_MUX_SESSIONS")
        .ok()
        .and_then(|v| v.parse::<usize>().ok())
        .unwrap_or(DEFAULT_SESSION_COUNT)
        .clamp(1, MAX_SESSION_COUNT)
}

fn split_authority(authority: &str) -> Result<(String, u16)> {
    let target =
        parse_authority_with_default(authority, 443).map_err(|e| anyhow!(e.to_string()))?;
    Ok((target.host, target.port))
}

fn parse_wss_url(input: &str) -> Result<(String, String, String)> {
    let rest = input
        .strip_prefix("wss://")
        .ok_or_else(|| anyhow!("WSS client requires wss:// upstream"))?;
    let (authority, path) = match rest.split_once('/') {
        Some((a, p)) => (a, format!("/{}", p)),
        None => (rest, "/".to_string()),
    };
    if authority.is_empty() {
        bail!("empty WSS upstream authority")
    }
    let target =
        parse_authority_with_default(authority, 443).map_err(|e| anyhow!(e.to_string()))?;
    let connect_addr = if target.host.contains(':') {
        format!("[{}]:{}", target.host, target.port)
    } else {
        format!("{}:{}", target.host, target.port)
    };
    Ok((connect_addr, target.host, path))
}

async fn connect_tls(
    addr: SocketAddr,
    tls_name: &str,
    cfg: &WssConfig,
) -> Result<tls::RushTlsStream> {
    let tcp = timeout(
        Duration::from_secs(cfg.connection_timeout.max(1)),
        TcpStream::connect(addr),
    )
    .await
    .context("WSS upstream TCP timeout")?
    .map_err(|e| anyhow!("TCP connect failed: {e}"))?;

    tracing::info!("[WSS] TCP connected");

    let policy = RuntimeConfig {
        proxy_host: "127.0.0.1".into(),
        proxy_port: 0,
        upstream: None,
        key: None,
        fakehost: None,
        mux: true,
        buffer_size: cfg.buffer_size,
        connection_timeout: cfg.connection_timeout,
        allow_open: false,
        max_connections: 1,
        block_local: false,
        tcp_nodelay: cfg.tcp_nodelay,
        tcp_keepalive: cfg.tcp_keepalive,
        socket_buffer: cfg.socket_buffer,
        obfs: false,
    };
    apply_socket_options(&tcp, &policy);
    let tls_stream = tls::connect(tcp, tls_name, cfg.verify_ssl)
        .await
        .map_err(|e| anyhow!("TLS handshake failed: {e}"))?;

    let (_, conn) = tls_stream.get_ref();
    let alpn = conn
        .alpn_protocol()
        .map(|p| String::from_utf8_lossy(p).to_string())
        .unwrap_or_else(|| "none".to_string());
    let cipher = conn
        .negotiated_cipher_suite()
        .map(|c| format!("{:?}", c.suite()))
        .unwrap_or_else(|| "unknown".to_string());
    let proto = conn
        .protocol_version()
        .map(|v| format!("{:?}", v))
        .unwrap_or_else(|| "unknown".to_string());

    tracing::info!(
        protocol = %proto,
        cipher = %cipher,
        alpn = %alpn,
        "[WSS] TLS handshake completed"
    );
    Ok(tls_stream)
}

async fn dial_cloudflare_fallback(
    port: u16,
    primary_ip: std::net::IpAddr,
    tls_name: &str,
    cfg: &WssConfig,
) -> Result<tls::RushTlsStream> {
    tracing::info!("[DNS] Resolving fakehost: {}", tls_name);
    let ips = match dns::resolve_all_ipv4(tls_name).await {
        Ok(v) => v,
        Err(e) => {
            tracing::warn!(fakehost=%tls_name, error=%e, "[Cloudflare] Failed to resolve fakehost fallback IPs");
            return Err(e);
        }
    };
    for ip in ips {
        let candidate_ip = std::net::IpAddr::V4(ip);
        if candidate_ip == primary_ip {
            continue;
        }
        let fallback_addr = SocketAddr::new(candidate_ip, port);
        tracing::info!(
            "[Cloudflare] Trying fallback edge: {} (fakehost: {})",
            fallback_addr,
            tls_name
        );
        match connect_tls(fallback_addr, tls_name, cfg).await {
            Ok(stream) => return Ok(stream),
            Err(err) => {
                tracing::debug!(
                    fallback_addr = %fallback_addr,
                    error = %err,
                    "[Cloudflare] Fallback edge dial failed"
                );
            }
        }
    }
    bail!("all Cloudflare fallback edges failed for fakehost: {tls_name}")
}

pub(crate) async fn connect_wss_upstream(
    cfg: &WssConfig,
) -> Result<(BoxReader, Arc<Mutex<BoxWriter>>)> {
    let (addr, host, path) = parse_wss_url(&cfg.upstream)?;
    let (dns_host, dns_port) = split_authority(&addr)?;
    let resolved = dns::resolve_socket(&dns_host, dns_port).await?;

    let sni_hostname = cfg
        .fakehost
        .as_deref()
        .map(|s| s.split(':').next().unwrap_or(s))
        .unwrap_or(&host);
    let header_host = cfg.fakehost.as_deref().unwrap_or(&host);
    let tls_name = sni_hostname;

    tracing::info!("[WSS] Connecting to {}", addr);
    tracing::info!("[WSS] TLS ServerName: {}", tls_name);
    tracing::info!("[WSS] WebSocket Host: {}", header_host);
    tracing::info!("[WSS] Path: {}", path);

    let primary_result = connect_tls(resolved, tls_name, cfg).await;
    let tls_stream = match primary_result {
        Ok(stream) => stream,
        Err(primary_err) => {
            let is_ip_host = host.parse::<std::net::IpAddr>().is_ok();
            if cfg.fakehost.is_some() && is_ip_host {
                tracing::warn!("[WSS] Primary upstream failed: {}", addr);
                dial_cloudflare_fallback(dns_port, resolved.ip(), tls_name, cfg).await?
            } else {
                return Err(primary_err);
            }
        }
    };

    let boxed: BoxTransport = Box::new(tls_stream);
    let (mut rd, mut wr) = tokio::io::split(boxed);
    let origin = format!("https://{}", tls_name);
    let header_host_name = header_host.split(':').next().unwrap_or(header_host);
    let sec_fetch_site = if tls_name.eq_ignore_ascii_case(header_host_name) {
        "same-origin"
    } else {
        "cross-site"
    };
    let (request, key) =
        build_client_handshake_request(header_host, &path, Some(&origin), Some(sec_fetch_site));

    tracing::debug!(
        "[WSS] Outgoing WebSocket upgrade request:\n{}",
        redact_handshake_request(&request)
    );

    tracing::info!("[WSS] Sending WebSocket upgrade");
    wr.write_all(&request)
        .await
        .context("WebSocket request write failed")?;
    wr.flush()
        .await
        .context("WebSocket request flush failed")?;

    tracing::info!("[WSS] Waiting for WebSocket 101");
    let handshake_timeout = Duration::from_secs(cfg.connection_timeout.max(1));
    let response = read_http_headers_timeout(&mut rd, handshake_timeout).await?;
    validate_client_handshake_response(&response, &key)?;
    tracing::info!("[WSS] WebSocket handshake completed");

    Ok((rd, Arc::new(Mutex::new(wr))))
}

pub(crate) async fn open_upstream(
    cfg: &WssConfig,
) -> Result<(BoxReader, Arc<Mutex<BoxWriter>>)> {
    connect_wss_upstream(cfg).await
}

/// Probe helper to test standalone WSS TCP -> TLS -> HTTP Upgrade -> 101 without entering MUX.
#[allow(dead_code)]
pub(crate) async fn probe_wss_handshake(cfg: &WssConfig) -> Result<String> {
    let (rd, wr) = connect_wss_upstream(cfg).await?;
    drop(rd);
    drop(wr);
    Ok("HTTP 101 Switching Protocols".to_string())
}

async fn handle_udp_proxy(
    mut control: TcpStream,
    cfg: WssConfig,
    bind_hint: TargetAddr,
) -> Result<()> {
    let bind_ip = if bind_hint.host == "0.0.0.0" || bind_hint.host.is_empty() {
        "0.0.0.0"
    } else {
        bind_hint.host.as_str()
    };
    let udp = Arc::new(UdpSocket::bind(format!("{}:0", bind_ip)).await?);
    let bound = udp.local_addr()?;
    let mut resp = [0u8; 10];
    resp[0] = 5;
    resp[1] = 0;
    resp[2] = 0;
    resp[3] = 1;
    if let std::net::IpAddr::V4(ip) = bound.ip() {
        resp[4..8].copy_from_slice(&ip.octets())
    }
    resp[8..10].copy_from_slice(&bound.port().to_be_bytes());
    control.write_all(&resp).await?;
    let (mut rd, writer) = open_upstream(&cfg).await?;
    let c = cipher(&cfg.key);
    let mut hello = b"UDP\n".to_vec();
    c.apply(&mut hello);
    {
        let mut w = writer.lock().await;
        write_frame(&mut *w, &hello, 2, true).await?
    };
    let mut frame_buf = Vec::with_capacity(64 * 1024);
    let Some((opcode, mut ok)) =
        read_frame(&mut rd, Option::<&mut BoxWriter>::None, &mut frame_buf).await?
    else {
        bail!("WSS upstream closed during UDP handshake")
    };
    if opcode != 2 {
        bail!("invalid WSS UDP handshake response opcode")
    };
    c.apply(&mut ok);
    if ok != b"OK\n" {
        bail!("WSS upstream rejected UDP handshake")
    };
    let latest_client = Arc::new(Mutex::new(None::<SocketAddr>));
    let udp_send = udp.clone();
    let writer_send = writer.clone();
    let cipher_send = c.clone();
    let latest_send = latest_client.clone();
    let upload = tokio::spawn(async move {
        let mut buf = vec![0u8; 64 * 1024];
        while let Ok((n, peer)) = udp_send.recv_from(&mut buf).await {
            *latest_send.lock().await = Some(peer);
            let mut packet = buf[..n].to_vec();
            cipher_send.apply(&mut packet);
            let mut w = writer_send.lock().await;
            if write_frame(&mut *w, &packet, 2, true).await.is_err() {
                break;
            }
        }
        Ok::<(), anyhow::Error>(())
    });
    let mut dummy = [0u8; 1];
    tokio::select! {
        _ = control.read(&mut dummy) => {}
        _ = async {
            loop {
                let Some((opcode, mut packet)) =
                    read_frame(&mut rd, Option::<&mut BoxWriter>::None, &mut frame_buf).await?
                else {
                    break;
                };
                if opcode != 2 {
                    continue;
                }
                c.apply(&mut packet);
                if let Some(peer) = *latest_client.lock().await {
                    let _ = udp.send_to(&packet, peer).await;
                }
            }
            Ok::<(), anyhow::Error>(())
        } => {}
    }
    upload.abort();
    let _ = control.shutdown().await;
    Ok(())
}

/// Pre-warmed WSS non-MUX upstream pool (GoWay `ConnPool` parity).
/// Transports complete TCP+TLS+WS-handshake and are single-use: grabbed
/// once, relayed, then closed, with a background task refilling the pool.
struct PooledWssUpstream {
    rd: BoxReader,
    wr: Arc<Mutex<BoxWriter>>,
    created: std::time::Instant,
    last_used: std::time::Instant,
}

const NON_MUX_WSS_POOL_SIZE: usize = 4;
const NON_MUX_WSS_MAX_AGE: Duration = Duration::from_secs(5 * 60);
const NON_MUX_WSS_IDLE_TIMEOUT: Duration = Duration::from_secs(30);
const NON_MUX_WSS_MAINTAIN_INTERVAL: Duration = Duration::from_secs(5);

fn pooled_wss_usable(
    created: std::time::Instant,
    last_used: std::time::Instant,
    now: std::time::Instant,
) -> bool {
    now.duration_since(created) <= NON_MUX_WSS_MAX_AGE
        && now.duration_since(last_used) <= NON_MUX_WSS_IDLE_TIMEOUT
}

struct NonMuxWssPool {
    cfg: WssConfig,
    conns: Mutex<Vec<PooledWssUpstream>>,
    creation: Mutex<()>,
}

impl NonMuxWssPool {
    fn new(cfg: WssConfig) -> Arc<Self> {
        Arc::new(Self {
            cfg,
            conns: Mutex::new(Vec::new()),
            creation: Mutex::new(()),
        })
    }

    async fn dial_pooled(cfg: &WssConfig) -> Result<PooledWssUpstream> {
        let (rd, wr) = open_upstream(cfg).await?;
        let now = std::time::Instant::now();
        Ok(PooledWssUpstream {
            rd,
            wr,
            created: now,
            last_used: now,
        })
    }

    async fn get_or_dial(self: &Arc<Self>) -> Result<(BoxReader, Arc<Mutex<BoxWriter>>)> {
        let pooled = {
            let mut conns = self.conns.lock().await;
            let now = std::time::Instant::now();
            conns.retain(|c| pooled_wss_usable(c.created, c.last_used, now));
            conns.pop()
        };
        if let Some(mut c) = pooled {
            c.last_used = std::time::Instant::now();
            tracing::debug!("[WSS non-MUX] using pre-warmed upstream connection");
            return Ok((c.rd, c.wr));
        }
        let c = Self::dial_pooled(&self.cfg).await?;
        Ok((c.rd, c.wr))
    }

    async fn replenish(self: &Arc<Self>) {
        let _guard = self.creation.lock().await;
        loop {
            let need = {
                let mut conns = self.conns.lock().await;
                conns.retain(|c| {
                    pooled_wss_usable(c.created, c.last_used, std::time::Instant::now())
                });
                NON_MUX_WSS_POOL_SIZE.saturating_sub(conns.len())
            };
            if need == 0 {
                return;
            }
            match Self::dial_pooled(&self.cfg).await {
                Ok(c) => self.conns.lock().await.push(c),
                Err(error) => {
                    tracing::debug!(error=%error, "[WSS non-MUX] pre-warm dial failed; will retry next tick");
                    return;
                }
            }
        }
    }

    async fn maintain(self: Arc<Self>) {
        loop {
            self.replenish().await;
            tokio::time::sleep(NON_MUX_WSS_MAINTAIN_INTERVAL).await;
        }
    }
}

async fn handle_non_mux_connection(
    mut local: TcpStream,
    cfg: WssConfig,
    pool: Arc<NonMuxWssPool>,
) -> Result<()> {
    let req = read_client_proxy_request(&mut local).await?;
    if req.command == SocksCommand::UdpAssociate {
        return handle_udp_proxy(local, cfg, req.target).await;
    }
    if req.command != SocksCommand::Connect {
        bail!("WSS non-MUX supports CONNECT or UDP ASSOCIATE only")
    };
    let (mut rd, writer) = pool.get_or_dial().await?;
    let c = cipher(&cfg.key);
    let mut hello = format!("{}:{}\n", req.target.host, req.target.port).into_bytes();
    c.apply(&mut hello);
    {
        let mut w = writer.lock().await;
        write_frame(&mut *w, &hello, 2, true).await?
    };
    let mut frame_buf = Vec::with_capacity(64 * 1024);
    let Some((opcode, mut ok)) =
        read_frame(&mut rd, Option::<&mut BoxWriter>::None, &mut frame_buf).await?
    else {
        bail!("WSS upstream closed before non-MUX OK")
    };
    if opcode != 2 {
        bail!("invalid WSS non-MUX handshake opcode")
    };
    c.apply(&mut ok);
    if ok != b"OK\n" {
        if req.is_socks5 {
            local.write_all(&[5, 1, 0, 1, 0, 0, 0, 0, 0, 0]).await?
        } else {
            local
                .write_all(
                    b"HTTP/1.1 502 Bad Gateway\r\nConnection: close\r\nContent-Length: 0\r\n\r\n",
                )
                .await?
        }
        local.shutdown().await.ok();
        return Ok(());
    };
    if req.is_socks5 {
        local.write_all(&socks5_success_response()).await?
    } else if req.is_connect {
        local
            .write_all(b"HTTP/1.1 200 Connection Established\r\n\r\n")
            .await?
    };
    let (mut local_rd, mut local_wr) = tokio::io::split(local);
    let writer_up = writer.clone();
    // GoWay non-MUX TCP: only handshake ("host:port\n"/"OK\n") is XOR-encrypted.
    // Data frames are plaintext.
    if let Some(initial) = req.initial_payload {
        let payload = initial;
        let mut w = writer_up.lock().await;
        write_frame(&mut *w, &payload, 2, true).await?;
    }
    let buffer_size = cfg.buffer_size;
    let mut upload = tokio::spawn(async move {
        let mut buf = relay_buf(buffer_size).await;
        loop {
            let n = local_rd.read(&mut buf).await?;
            if n == 0 {
                break;
            }
            let mut w = writer_up.lock().await;
            write_frame_borrowed(&mut *w, &mut buf[..n], 2, true).await?
        }
        recycle_buf(buf).await;
        Ok::<(), anyhow::Error>(())
    });
    tokio::select! {
        _ = &mut upload => {}
        _ = async {
            loop {
                let Some((opcode, payload)) =
                    read_frame(&mut rd, Option::<&mut BoxWriter>::None, &mut frame_buf).await?
                else {
                    break;
                };
                if opcode == 8 {
                    break;
                }
                if opcode != 2 {
                    continue;
                }
                if local_wr.write_all(&payload).await.is_err() {
                    break;
                }
            }
            Ok::<(), anyhow::Error>(())
        } => {}
    }
    upload.abort();
    Ok(())
}

pub async fn run_non_mux_from_config(cfg: RuntimeConfig, verify_ssl: bool) -> Result<()> {
    let upstream = cfg
        .upstream
        .clone()
        .ok_or_else(|| anyhow!("WSS client requires upstream"))?;
    let wc = WssConfig {
        proxy_host: cfg.proxy_host,
        proxy_port: cfg.proxy_port,
        upstream,
        key: cfg.key,
        fakehost: cfg.fakehost,
        buffer_size: cfg.buffer_size,
        connection_timeout: cfg.connection_timeout,
        verify_ssl,
        tcp_nodelay: cfg.tcp_nodelay,
        tcp_keepalive: cfg.tcp_keepalive,
        socket_buffer: cfg.socket_buffer,
        obfs: cfg.obfs,
    };
    let pool = NonMuxWssPool::new(wc.clone());
    let maintainer = pool.clone();
    tokio::spawn(async move {
        maintainer.maintain().await;
    });
    let listener = TcpListener::bind(format!("{}:{}", wc.proxy_host, wc.proxy_port)).await?;
    tracing::info!(
        "RushWay WSS non-MUX client proxy listening on {}:{} ({} pre-warmed upstream connections)",
        wc.proxy_host,
        wc.proxy_port,
        NON_MUX_WSS_POOL_SIZE,
    );
    let mut set = tokio::task::JoinSet::new();
    loop {
        tokio::select! {
            _ = wait_shutdown() => {
                tracing::info!("shutdown requested, draining WSS non-MUX connections");
                break;
            }
            res = listener.accept() => {
                let (stream, peer) = res?;
                let cfg2 = wc.clone();
                let pool2 = pool.clone();
                set.spawn(async move {
                    if let Err(e) = handle_non_mux_connection(stream, cfg2, pool2).await {
                        tracing::debug!(%peer,error=%e,"WSS non-MUX connection closed")
                    }
                });
            }
        }
    }
    drain_join_set(&mut set).await;
    Ok(())
}

struct WssSessionState {
    writer: Arc<MuxFrameWriter>,
    cipher: XorCipher,
    obfs: bool,
    // Read-mostly under concurrency (one lookup per DATA frame), so a
    // RwLock: concurrent lookups, exclusive insert/remove.
    streams: Arc<RwLock<HashMap<u32, mpsc::Sender<OwnedMuxFrame>>>>,
    next_id: AtomicU32,
    active: AtomicUsize,
    closed: AtomicBool,
}
impl WssSessionState {
    async fn connect(cfg: &WssConfig) -> Result<Arc<Self>> {
        let (mut rd, writer) = open_upstream(cfg).await?;
        let c = cipher(&cfg.key);
        let mut hello = b"MUX\n".to_vec();
        c.apply(&mut hello);

        tracing::info!("[WSS] Sending MUX handshake");
        {
            let mut w = writer.lock().await;
            write_frame(&mut *w, &hello, 2, true)
                .await
                .context("MUX handshake request write failed")?;
        };
        let mut frame_buf = Vec::with_capacity(64 * 1024);
        let mux_timeout = Duration::from_secs(cfg.connection_timeout.max(1));
        let read_result = timeout(
            mux_timeout,
            read_frame(&mut rd, Option::<&mut BoxWriter>::None, &mut frame_buf),
        )
        .await
        .context("MUX OK timeout")??;

        let Some((opcode, mut ok)) = read_result else {
            bail!("WSS upstream closed during MUX handshake")
        };
        if opcode != 2 {
            bail!("invalid WSS MUX response opcode: {opcode}")
        };
        c.apply(&mut ok);
        if ok != b"OK\n" {
            bail!(
                "MUX rejected: {:?}",
                String::from_utf8_lossy(&ok).trim()
            )
        };
        tracing::info!("[WSS] MUX handshake completed");
        // Hand the write half to the dedicated writer task; from here on
        // all MUX frames go through the serialized, coalescing queue.
        let writer = Arc::try_unwrap(writer)
            .map_err(|_| anyhow!("upstream writer unexpectedly shared"))?
            .into_inner();
        let (writer, _writer_task) = MuxFrameWriter::spawn(writer);
        let session = Arc::new(Self {
            writer,
            cipher: c.clone(),
            obfs: cfg.obfs,
            streams: Arc::new(RwLock::new(HashMap::new())),
            next_id: AtomicU32::new(1),
            active: AtomicUsize::new(0),
            closed: AtomicBool::new(false),
        });
        let reader_session = session.clone();
        tokio::spawn(async move {
            if let Err(error) = wss_reader_loop(&mut rd, reader_session.clone()).await {
                tracing::debug!(%error,"WSS physical session reader stopped")
            };
            reader_session.closed.store(true, Ordering::Release);
            let mut streams = reader_session.streams.write().await;
            streams.clear();
            reader_session.active.store(0, Ordering::Release)
        });
        let heartbeat_session = session.clone();
        tokio::spawn(async move {
            let mut interval = tokio::time::interval(Duration::from_secs(25));
            interval.tick().await;
            loop {
                interval.tick().await;
                if heartbeat_session.closed.load(Ordering::Acquire) {
                    break;
                }
                if heartbeat_session.active.load(Ordering::Acquire) == 0 {
                    let ping = match encode_ws_frame(&[], 9, true) {
                        Ok(frame) => frame,
                        Err(_) => {
                            heartbeat_session.closed.store(true, Ordering::Release);
                            break;
                        }
                    };
                    if heartbeat_session.writer.send(ping).await.is_err() {
                        heartbeat_session.closed.store(true, Ordering::Release);
                        break;
                    }
                }
            }
        });
        Ok(session)
    }
    fn available(&self) -> bool {
        !self.closed.load(Ordering::Acquire)
            && !self.writer.is_closed()
            && self.active.load(Ordering::Acquire) < MAX_STREAMS_PER_SESSION
    }
    fn try_reserve(&self) -> bool {
        if self.closed.load(Ordering::Acquire) {
            return false;
        }
        self.active
            .fetch_update(Ordering::AcqRel, Ordering::Acquire, |current| {
                (current < MAX_STREAMS_PER_SESSION).then_some(current + 1)
            })
            .is_ok()
    }
    async fn open_stream(
        self: &Arc<Self>,
        target: &TargetAddr,
        initial_data: Vec<u8>,
    ) -> Result<(u32, mpsc::Receiver<OwnedMuxFrame>)> {
        if !self.try_reserve() {
            bail!("WSS physical session is full or closed")
        };
        let (id, rx) = {
            let mut streams = self.streams.write().await;
            if self.closed.load(Ordering::Acquire) {
                self.active.fetch_sub(1, Ordering::AcqRel);
                bail!("WSS physical session closed")
            }
            let mut stream_id;
            loop {
                stream_id = self.next_id.fetch_add(1, Ordering::Relaxed);
                if stream_id == 0 {
                    stream_id = self.next_id.fetch_add(1, Ordering::Relaxed);
                }
                if !streams.contains_key(&stream_id) {
                    break;
                }
            }
            let (tx, rx) = mpsc::channel(64);
            streams.insert(stream_id, tx);
            (stream_id, rx)
        };
        let target_bytes = format!("{}:{}", target.host, target.port).into_bytes();
        let max_syn_initial = (u16::MAX as usize).saturating_sub(2 + target_bytes.len());
        let (syn_initial, remaining_initial) = if initial_data.len() > max_syn_initial {
            (
                initial_data[..max_syn_initial].to_vec(),
                &initial_data[max_syn_initial..],
            )
        } else {
            (initial_data, &[][..])
        };

        let syn_payload = SynPayload {
            target: target_bytes,
            initial_data: syn_initial,
        }
        .encode()
        .map_err(|e| anyhow!(e.to_string()))?;
        let syn =
            MuxFrame::new(id, MuxCommand::Syn, syn_payload).map_err(|e| anyhow!(e.to_string()))?;
        if let Err(error) = send_mux(&self.writer, &self.cipher, &syn, self.obfs).await {
            self.streams.write().await.remove(&id);
            self.active.fetch_sub(1, Ordering::AcqRel);
            return Err(error);
        }
        if !remaining_initial.is_empty() {
            if let Err(error) = send_mux_parts(
                &self.writer,
                &self.cipher,
                id,
                MuxCommand::Data,
                remaining_initial,
                self.obfs,
            )
            .await
            {
                self.streams.write().await.remove(&id);
                self.active.fetch_sub(1, Ordering::AcqRel);
                return Err(error);
            }
        }
        Ok((id, rx))
    }
}
async fn send_mux(
    writer: &Arc<MuxFrameWriter>,
    cipher: &XorCipher,
    frame: &MuxFrame,
    obfs: bool,
) -> Result<()> {
    let mut data = Vec::with_capacity(7 + frame.payload.len());
    crate::mux_writer::encode_mux_ws_frame(
        &mut data,
        frame.stream_id,
        frame.command,
        &frame.payload,
        cipher,
        true,
        obfs,
    )
    .map_err(|e| anyhow!(e.to_string()))?;
    writer.send(data).await
}
async fn send_mux_parts(
    writer: &Arc<MuxFrameWriter>,
    cipher: &XorCipher,
    stream_id: u32,
    command: MuxCommand,
    payload: &[u8],
    obfs: bool,
) -> Result<()> {
    let mut data = Vec::with_capacity(7 + payload.len());
    crate::mux_writer::encode_mux_ws_frame(
        &mut data, stream_id, command, payload, cipher, true, obfs,
    )
    .map_err(|e| anyhow!(e.to_string()))?;
    writer.send(data).await
}
async fn wss_reader_loop(rd: &mut BoxReader, session: Arc<WssSessionState>) -> Result<()> {
    let mut frame_buf = Vec::with_capacity(64 * 1024);
    loop {
        let Some((opcode, mut payload)) =
            read_frame_owned(rd, Option::<&mut BoxWriter>::None, &mut frame_buf).await?
        else {
            return Ok(());
        };
        if opcode != 2 {
            continue;
        }
        session.cipher.apply(&mut payload);
        let frame = MuxFrame::decode_owned(payload).map_err(|e| anyhow!(e.to_string()))?;
        let id = frame.stream_id;
        let sender = { session.streams.read().await.get(&id).cloned() };
        if let Some(tx) = sender {
            // Terminal FIN/RST is forwarded; accounting is done once by the
            // stream owner (see handle_connection cleanup below), mirroring
            // mux_pool::client_reader_loop.
            if tx.send(frame).await.is_err()
                && session.streams.write().await.remove(&id).is_some()
            {
                session.active.fetch_sub(1, Ordering::AcqRel);
            }
        }
    }
}

struct WssSessionPool {
    cfg: WssConfig,
    sessions: Mutex<Vec<Arc<WssSessionState>>>,
    session_creation: Mutex<()>,
    consecutive_failures: AtomicU32,
}
impl WssSessionPool {
    fn new(cfg: WssConfig) -> Arc<Self> {
        Arc::new(Self {
            cfg,
            sessions: Mutex::new(Vec::new()),
            session_creation: Mutex::new(()),
            consecutive_failures: AtomicU32::new(0),
        })
    }
    async fn replenish(self: &Arc<Self>) {
        let target = configured_session_count();
        let _guard = self.session_creation.lock().await;
        let need_new = {
            let mut sessions = self.sessions.lock().await;
            sessions.retain(|s| !s.closed.load(Ordering::Acquire));
            sessions.len() < target
        };
        if !need_new {
            self.consecutive_failures
                .store(0, Ordering::Release);
            return;
        }
        match WssSessionState::connect(&self.cfg).await {
            Ok(s) => {
                self.sessions.lock().await.push(s);
                self.consecutive_failures
                    .store(0, Ordering::Release);
            }
            Err(error) => {
                let failures = self
                    .consecutive_failures
                    .fetch_add(1, Ordering::AcqRel)
                    + 1;
                let (_, host, path) = parse_wss_url(&self.cfg.upstream).unwrap_or_default();
                let sni = self
                    .cfg
                    .fakehost
                    .as_deref()
                    .map(|s| s.split(':').next().unwrap_or(s))
                    .unwrap_or(&host);
                let header_host = self.cfg.fakehost.as_deref().unwrap_or(&host);
                tracing::warn!(
                    error = %error,
                    upstream = %self.cfg.upstream,
                    fakehost = ?self.cfg.fakehost,
                    sni = %sni,
                    host = %header_host,
                    path = %path,
                    failures,
                    "WSS physical session creation failed; will retry with backoff"
                );
            }
        }
    }
    async fn maintain(self: &Arc<Self>) {
        loop {
            self.replenish().await;
            let failures = self.consecutive_failures.load(Ordering::Acquire);
            let backoff = (failures.saturating_mul(500)).min(5000);
            tokio::time::sleep(Duration::from_millis(500 + backoff as u64)).await;
        }
    }
    async fn acquire(
        self: &Arc<Self>,
        target: &TargetAddr,
        initial_data: Vec<u8>,
    ) -> Result<(Arc<WssSessionState>, u32, mpsc::Receiver<OwnedMuxFrame>)> {
        let snapshot = {
            let mut sessions = self.sessions.lock().await;
            sessions.retain(|s| !s.closed.load(Ordering::Acquire));
            sessions.clone()
        };
        let mut ordered = snapshot
            .into_iter()
            .map(|session| (session.active.load(Ordering::Acquire), session))
            .collect::<Vec<_>>();
        ordered.sort_unstable_by_key(|(active, _)| *active);
        for (_active, session) in ordered {
            if !session.available() {
                continue;
            }
            if let Ok((id, rx)) = session.open_stream(target, initial_data.clone()).await {
                return Ok((session, id, rx));
            }
        }
        let limit = configured_session_count();
        let _guard = self.session_creation.lock().await;
        let can_create = {
            let mut sessions = self.sessions.lock().await;
            sessions.retain(|s| !s.closed.load(Ordering::Acquire));
            sessions.len() < limit
        };
        if can_create {
            let session = WssSessionState::connect(&self.cfg).await.map_err(|e| {
                let (_, host, path) = parse_wss_url(&self.cfg.upstream).unwrap_or_default();
                let sni = self
                    .cfg
                    .fakehost
                    .as_deref()
                    .map(|s| s.split(':').next().unwrap_or(s))
                    .unwrap_or(&host);
                let header_host = self.cfg.fakehost.as_deref().unwrap_or(&host);
                tracing::warn!(
                    error = %e,
                    upstream = %self.cfg.upstream,
                    fakehost = ?self.cfg.fakehost,
                    sni = %sni,
                    host = %header_host,
                    path = %path,
                    "WSS physical session creation on-demand failed"
                );
                e
            })?;
            let opened = session.open_stream(target, initial_data).await?;
            self.sessions.lock().await.push(session.clone());
            return Ok((session, opened.0, opened.1));
        }
        bail!("all WSS physical MUX sessions are full or unavailable")
    }
}

async fn handle_connection(mut local: TcpStream, pool: Arc<WssSessionPool>) -> Result<()> {
    let req = read_client_proxy_request(&mut local).await?;
    if req.command == SocksCommand::UdpAssociate {
        return handle_udp_proxy(local, pool.cfg.clone(), req.target).await;
    }
    if req.command != SocksCommand::Connect {
        bail!("WSS path only supports CONNECT or UDP ASSOCIATE")
    };
    let initial_data = req.initial_payload.unwrap_or_default();
    let (session, stream_id, mut rx) = pool.acquire(&req.target, initial_data).await?;
    let writer = session.writer.clone();
    let cipher = session.cipher.clone();
    let obfs = session.obfs;
    if req.is_socks5 {
        local.write_all(&socks5_success_response()).await?;
    } else if req.is_connect {
        local
            .write_all(b"HTTP/1.1 200 Connection Established\r\n\r\n")
            .await?;
    };
    let (mut local_rd, mut local_wr) = tokio::io::split(local);
    let buffer_size = pool.cfg.buffer_size;
    let upload = tokio::spawn(async move {
        let mut buf = relay_buf(buffer_size).await;
        loop {
            let n = local_rd.read(&mut buf).await?;
            if n == 0 {
                let _ = send_mux_parts(&writer, &cipher, stream_id, MuxCommand::Fin, &[], obfs).await;
                break;
            }
            let mut off = 0;
            while off < n {
                let end = (off + u16::MAX as usize).min(n);
                send_mux_parts(
                    &writer,
                    &cipher,
                    stream_id,
                    MuxCommand::Data,
                    &buf[off..end],
                    obfs,
                )
                .await?;
                off = end
            }
        }
        recycle_buf(buf).await;
        Result::<()>::Ok(())
    });
    while let Some(frame) = rx.recv().await {
        match frame.command {
            MuxCommand::Data => {
                if !frame.payload().is_empty() {
                    local_wr.write_all(frame.payload()).await?;
                }
            }
            MuxCommand::Fin => {
                local_wr.shutdown().await?;
                break;
            }
            MuxCommand::Rst => break,
            MuxCommand::Syn => {}
        }
    }
    upload.abort();
    // Conditional decrement: the reader may have already reaped the stream
    // (send failure) or the session may have been torn down (active zeroed).
    if session.streams.write().await.remove(&stream_id).is_some() {
        session.active.fetch_sub(1, Ordering::AcqRel);
    }
    Ok(())
}

pub async fn run_client_from_config(cfg: RuntimeConfig, verify_ssl: bool) -> Result<()> {
    let upstream = cfg
        .upstream
        .clone()
        .ok_or_else(|| anyhow!("WSS client requires upstream"))?;
    let wc = WssConfig {
        proxy_host: cfg.proxy_host,
        proxy_port: cfg.proxy_port,
        upstream,
        key: cfg.key,
        fakehost: cfg.fakehost,
        buffer_size: cfg.buffer_size,
        connection_timeout: cfg.connection_timeout,
        verify_ssl,
        tcp_nodelay: cfg.tcp_nodelay,
        tcp_keepalive: cfg.tcp_keepalive,
        socket_buffer: cfg.socket_buffer,
        obfs: cfg.obfs,
    };
    let pool = WssSessionPool::new(wc.clone());
    let listener = TcpListener::bind(format!("{}:{}", wc.proxy_host, wc.proxy_port)).await?;
    let pool_maintainer = pool.clone();
    tokio::spawn(async move {
        pool_maintainer.maintain().await;
    });
    tracing::info!(
        "RushWay WSS client proxy listening on {}:{}",
        wc.proxy_host,
        wc.proxy_port
    );
    let mut set = tokio::task::JoinSet::new();
    loop {
        tokio::select! {
            _ = wait_shutdown() => {
                tracing::info!("shutdown requested, draining WSS client connections");
                break;
            }
            res = listener.accept() => {
                let (stream, peer) = res?;
                let pool2 = pool.clone();
                set.spawn(async move {
                    if let Err(e) = handle_connection(stream, pool2).await {
                        tracing::debug!(%peer,error=%e,"WSS proxy connection closed")
                    }
                });
            }
        }
    }
    drain_join_set(&mut set).await;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ws::read_http_headers;

    #[test]
    fn pooled_wss_transports_expire_by_age_and_idleness() {
        let now = std::time::Instant::now();
        assert!(pooled_wss_usable(now, now, now));
        assert!(!pooled_wss_usable(
            now,
            now - NON_MUX_WSS_IDLE_TIMEOUT - Duration::from_secs(1),
            now
        ));
        assert!(!pooled_wss_usable(
            now - NON_MUX_WSS_MAX_AGE - Duration::from_secs(1),
            now,
            now
        ));
    }

    #[test]
    fn parse_wss_url_with_ip_and_path() {
        let (connect_addr, host, path) =
            parse_wss_url("wss://172.64.229.105:443/pyway").expect("valid wss url");
        assert_eq!(connect_addr, "172.64.229.105:443");
        assert_eq!(host, "172.64.229.105");
        assert_eq!(path, "/pyway");
    }

    #[test]
    fn parse_wss_url_standard_port_and_root_path() {
        let (connect_addr, host, path) =
            parse_wss_url("wss://example.com").expect("valid wss url");
        assert_eq!(connect_addr, "example.com:443");
        assert_eq!(host, "example.com");
        assert_eq!(path, "/");
    }

    #[test]
    fn parse_wss_url_rejects_non_wss() {
        assert!(parse_wss_url("ws://172.64.229.105:443/pyway").is_err());
        assert!(parse_wss_url("http://example.com").is_err());
    }

    #[test]
    fn fakehost_overrides_tls_and_headers_matching_goway() {
        let upstream = "wss://172.64.229.105:443/pyway";
        let fakehost = "colo.4467107.xyz";

        let (connect_addr, host, path) = parse_wss_url(upstream).unwrap();
        let sni_hostname = fakehost.split(':').next().unwrap_or(fakehost);
        let header_host = fakehost;
        let tls_name = sni_hostname;
        let origin = format!("https://{}", tls_name);
        let sec_fetch_site = if tls_name.eq_ignore_ascii_case(header_host.split(':').next().unwrap_or(header_host)) {
            "same-origin"
        } else {
            "cross-site"
        };

        assert_eq!(connect_addr, "172.64.229.105:443");
        assert_eq!(host, "172.64.229.105");
        assert_eq!(tls_name, "colo.4467107.xyz");
        assert_eq!(header_host, "colo.4467107.xyz");
        assert_eq!(origin, "https://colo.4467107.xyz");
        assert_eq!(sec_fetch_site, "same-origin");
        assert_eq!(path, "/pyway");

        let (request_bytes, key) = build_client_handshake_request(
            header_host,
            &path,
            Some(&origin),
            Some(sec_fetch_site),
        );
        let req_text = std::str::from_utf8(&request_bytes).unwrap();

        assert!(req_text.starts_with("GET /pyway HTTP/1.1\r\n"));
        assert!(req_text.contains("Host: colo.4467107.xyz\r\n"));
        assert!(req_text.contains("Origin: https://colo.4467107.xyz\r\n"));
        assert!(req_text.contains("Upgrade: websocket\r\n"));
        assert!(req_text.contains("Connection: Upgrade\r\n"));
        assert!(req_text.contains("Sec-WebSocket-Version: 13\r\n"));
        assert!(req_text.contains(&format!("Sec-WebSocket-Key: {key}\r\n")));
        assert!(req_text.contains("Sec-Fetch-Site: same-origin\r\n"));

        // Must NOT leak raw IP into Host or Origin
        assert!(!req_text.contains("Host: 172.64.229.105"));
        assert!(!req_text.contains("Origin: https://172.64.229.105"));
    }

    #[test]
    fn without_fakehost_uses_upstream_host() {
        let upstream = "wss://gateway.example.com:443/ws";
        let (connect_addr, host, path) = parse_wss_url(upstream).unwrap();
        let fakehost: Option<&str> = None;

        let sni_hostname = fakehost
            .map(|s| s.split(':').next().unwrap_or(s))
            .unwrap_or(&host);
        let header_host = fakehost.unwrap_or(&host);
        let tls_name = sni_hostname;
        let origin = format!("https://{}", tls_name);

        assert_eq!(connect_addr, "gateway.example.com:443");
        assert_eq!(tls_name, "gateway.example.com");
        assert_eq!(header_host, "gateway.example.com");
        assert_eq!(origin, "https://gateway.example.com");
        assert_eq!(path, "/ws");
    }

    #[test]
    fn fallback_eligibility_criteria() {
        // IP host + fakehost -> eligible for Cloudflare fallback
        let host_ip = "172.64.229.105";
        let has_fakehost = true;
        let is_ip = host_ip.parse::<std::net::IpAddr>().is_ok();
        assert!(has_fakehost && is_ip);

        // Domain host + fakehost -> NOT eligible (standard DNS handles domain)
        let host_domain = "example.com";
        let is_ip_domain = host_domain.parse::<std::net::IpAddr>().is_ok();
        assert!(!(has_fakehost && is_ip_domain));

        // IP host + no fakehost -> NOT eligible
        let no_fakehost = false;
        assert!(!(no_fakehost && is_ip));
    }

    #[tokio::test]
    async fn mock_wss_server_fakehost_mux_handshake() {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();
        let acceptor = tls::standalone_server_acceptor().unwrap();

        let server_task = tokio::spawn(async move {
            let (stream, _) = listener.accept().await.unwrap();
            let mut tls_stream = acceptor.accept(stream).await.unwrap();
            let headers_bytes = read_http_headers(&mut tls_stream).await.unwrap();
            let headers_str = std::str::from_utf8(&headers_bytes).unwrap();

            assert!(headers_str.starts_with("GET /pyway HTTP/1.1\r\n"));
            assert!(headers_str.contains("Host: colo.4467107.xyz\r\n"));
            assert!(headers_str.contains("Origin: https://colo.4467107.xyz\r\n"));
            assert!(headers_str.contains("Upgrade: websocket\r\n"));
            assert!(!headers_str.contains("Host: 127.0.0.1"));

            let key = crate::ws::validate_server_handshake(&headers_bytes).unwrap();
            tls_stream
                .write_all(&crate::ws::build_server_handshake_response(&key))
                .await
                .unwrap();

            let mut buf = Vec::new();
            let (opcode, mut payload) = read_frame(
                &mut tls_stream,
                Option::<&mut tokio::io::DuplexStream>::None,
                &mut buf,
            )
            .await
            .unwrap()
            .unwrap();
            assert_eq!(opcode, 2);
            let c = cipher(&Some("secretkey".into()));
            c.apply(&mut payload);
            assert_eq!(payload, b"MUX\n");

            let mut ok_payload = b"OK\n".to_vec();
            c.apply(&mut ok_payload);
            write_frame(&mut tls_stream, &ok_payload, 2, false)
                .await
                .unwrap();
        });

        let cfg = WssConfig {
            proxy_host: "127.0.0.1".into(),
            proxy_port: 0,
            upstream: format!("wss://127.0.0.1:{port}/pyway"),
            key: Some("secretkey".into()),
            fakehost: Some("colo.4467107.xyz".into()),
            buffer_size: 128 * 1024,
            connection_timeout: 10,
            verify_ssl: false,
            tcp_nodelay: true,
            tcp_keepalive: true,
            socket_buffer: 0,
            obfs: false,
        };

        let session = WssSessionState::connect(&cfg).await.unwrap();
        assert_eq!(session.active.load(Ordering::Acquire), 0);
        assert!(!session.closed.load(Ordering::Acquire));

        server_task.await.unwrap();
    }

    #[tokio::test]
    async fn mock_wss_standalone_handshake_probe() {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();
        let acceptor = tls::standalone_server_acceptor().unwrap();

        let server_task = tokio::spawn(async move {
            let (stream, _) = listener.accept().await.unwrap();
            let mut tls_stream = acceptor.accept(stream).await.unwrap();
            let headers_bytes = read_http_headers(&mut tls_stream).await.unwrap();
            let headers_str = std::str::from_utf8(&headers_bytes).unwrap();

            assert!(headers_str.starts_with("GET /pyway HTTP/1.1\r\n"));
            assert!(headers_str.contains("Host: dedi.4467107.xyz\r\n"));
            assert!(headers_str.contains("Origin: https://dedi.4467107.xyz\r\n"));
            assert!(headers_str.contains("Upgrade: websocket\r\n"));
            assert!(!headers_str.contains("Host: 127.0.0.1"));

            let key = crate::ws::validate_server_handshake(&headers_bytes).unwrap();
            tls_stream
                .write_all(&crate::ws::build_server_handshake_response(&key))
                .await
                .unwrap();
        });

        let cfg = WssConfig {
            proxy_host: "127.0.0.1".into(),
            proxy_port: 0,
            upstream: format!("wss://127.0.0.1:{port}/pyway"),
            key: Some("secretkey".into()),
            fakehost: Some("dedi.4467107.xyz".into()),
            buffer_size: 128 * 1024,
            connection_timeout: 10,
            verify_ssl: false,
            tcp_nodelay: true,
            tcp_keepalive: true,
            socket_buffer: 0,
            obfs: false,
        };

        let res = probe_wss_handshake(&cfg).await.unwrap();
        assert_eq!(res, "HTTP 101 Switching Protocols");
        server_task.await.unwrap();
    }
}
