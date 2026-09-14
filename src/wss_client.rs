//! WSS client paths for GoWay-compatible upstreams.

use crate::crypto::XorCipher;
use crate::dns;
use crate::protocol::{write_frame_parts, MuxCommand, MuxFrame, OwnedMuxFrame, SynPayload};
use crate::proxy::{
    parse_http_connect, parse_socks5_request, parse_socks5_udp_datagram, socks5_failure_response,
    socks5_success_response, SocksCommand, TargetAddr, SOCKS5_CONNECT, SOCKS5_UDP_ASSOCIATE,
    SOCKS5_VERSION,
};
use crate::runtime::{apply_socket_options, RuntimeConfig};
use crate::tls;
use crate::ws::{
    build_client_handshake_request, read_frame, read_frame_owned, read_http_headers,
    validate_client_handshake_response, write_frame,
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
use tokio::sync::{mpsc, Mutex};
use tokio::time::{timeout, Duration};

trait Transport: AsyncRead + AsyncWrite + Unpin + Send {}
impl<T: AsyncRead + AsyncWrite + Unpin + Send> Transport for T {}
type BoxTransport = Box<dyn Transport>;
type BoxReader = tokio::io::ReadHalf<BoxTransport>;
type BoxWriter = tokio::io::WriteHalf<BoxTransport>;

const DEFAULT_SESSION_COUNT: usize = 4;
const MAX_SESSION_COUNT: usize = 64;
const MAX_STREAMS_PER_SESSION: usize = 256;

#[derive(Debug, Clone)]
struct WssConfig {
    proxy_host: String,
    proxy_port: u16,
    upstream: String,
    key: Option<String>,
    fakehost: Option<String>,
    buffer_size: usize,
    connection_timeout: u64,
    verify_ssl: bool,
    tcp_nodelay: bool,
    tcp_keepalive: bool,
    socket_buffer: usize,
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
    if authority.starts_with('[') {
        let close = authority
            .find(']')
            .ok_or_else(|| anyhow!("invalid WSS IPv6 authority"))?;
        let host = authority[1..close].to_string();
        let port = authority
            .get(close + 1..)
            .and_then(|s| s.strip_prefix(':'))
            .unwrap_or("443")
            .parse::<u16>()?;
        return Ok((host, port));
    }
    match authority.rsplit_once(':') {
        Some((host, port)) => Ok((host.to_string(), port.parse::<u16>()?)),
        None => Ok((authority.to_string(), 443)),
    }
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
    let host = if authority.starts_with('[') {
        let close = authority
            .find(']')
            .ok_or_else(|| anyhow!("invalid IPv6 WSS authority"))?;
        authority[1..close].to_string()
    } else {
        authority
            .rsplit_once(':')
            .map(|(h, _)| h.to_string())
            .unwrap_or_else(|| authority.to_string())
    };
    let connect_addr = if authority.starts_with('[') {
        if authority.contains("]:") {
            authority.to_string()
        } else {
            format!("{}:443", authority)
        }
    } else if authority.matches(':').count() == 1 {
        authority.to_string()
    } else {
        format!("{}:443", authority)
    };
    Ok((connect_addr, host, path))
}

async fn open_upstream(cfg: &WssConfig) -> Result<(BoxReader, Arc<Mutex<BoxWriter>>)> {
    let (addr, host, path) = parse_wss_url(&cfg.upstream)?;
    let (dns_host, dns_port) = split_authority(&addr)?;
    let resolved = dns::resolve_socket(&dns_host, dns_port).await?;
    let tcp = timeout(
        Duration::from_secs(cfg.connection_timeout.max(1)),
        TcpStream::connect(resolved),
    )
    .await
    .context("WSS upstream TCP timeout")??;
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
    };
    apply_socket_options(&tcp, &policy);
    let tls_name = cfg.fakehost.as_deref().unwrap_or(&host);
    let tls_stream = tls::connect(tcp, tls_name, cfg.verify_ssl).await?;
    let boxed: BoxTransport = Box::new(tls_stream);
    let (mut rd, mut wr) = tokio::io::split(boxed);
    let header_host = cfg.fakehost.as_deref().unwrap_or(&host);
    let origin = format!("https://{}", host);
    let sec_fetch_site = if tls_name.eq_ignore_ascii_case(header_host) {
        "same-origin"
    } else {
        "cross-site"
    };
    let (request, key) =
        build_client_handshake_request(header_host, &path, Some(&origin), Some(sec_fetch_site));
    wr.write_all(&request).await?;
    wr.flush().await?;
    let response = read_http_headers(&mut rd).await?;
    validate_client_handshake_response(&response, &key)?;
    Ok((rd, Arc::new(Mutex::new(wr))))
}

async fn read_proxy_request(local: &mut TcpStream) -> Result<(SocksCommand, TargetAddr, bool)> {
    let first = local.read_u8().await?;
    if first == SOCKS5_VERSION {
        let n = local.read_u8().await? as usize;
        let mut methods = vec![0u8; n];
        local.read_exact(&mut methods).await?;
        if !methods.contains(&0) {
            local.write_all(&[5, 0xff]).await?;
            bail!("SOCKS5 no-auth unavailable")
        }
        local.write_all(&[5, 0]).await?;
        let mut head = [0u8; 4];
        local.read_exact(&mut head).await?;
        if head[1] != SOCKS5_CONNECT && head[1] != SOCKS5_UDP_ASSOCIATE {
            local.write_all(&[5, 7, 0, 1, 0, 0, 0, 0, 0, 0]).await?;
            bail!("unsupported SOCKS5 command")
        }
        let mut req = head.to_vec();
        match head[3] {
            1 => {
                let mut b = [0u8; 6];
                local.read_exact(&mut b).await?;
                req.extend_from_slice(&b)
            }
            3 => {
                let mut n = [0u8; 1];
                local.read_exact(&mut n).await?;
                req.extend_from_slice(&n);
                let mut b = vec![0u8; n[0] as usize + 2];
                local.read_exact(&mut b).await?;
                req.extend_from_slice(&b)
            }
            4 => {
                let mut b = [0u8; 18];
                local.read_exact(&mut b).await?;
                req.extend_from_slice(&b)
            }
            _ => {
                local.write_all(&[5, 8, 0, 1, 0, 0, 0, 0, 0, 0]).await?;
                bail!("unsupported SOCKS5 address type")
            }
        }
        let parsed = parse_socks5_request(&req).map_err(|e| anyhow!(e.to_string()))?;
        return Ok((parsed.command, parsed.target, true));
    }
    if first == b'C' {
        let mut buf = vec![first];
        let mut one = [0u8; 1];
        while buf.len() < 8192 {
            local.read_exact(&mut one).await?;
            buf.push(one[0]);
            if buf.ends_with(b"\r\n\r\n") || buf.ends_with(b"\n\n") {
                break;
            }
        }
        return Ok((
            SocksCommand::Connect,
            parse_http_connect(&buf).map_err(|e| anyhow!(e.to_string()))?,
            false,
        ));
    }
    bail!("unsupported local proxy protocol")
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
        loop {
            let (n, peer) = udp_send.recv_from(&mut buf).await?;
            *latest_send.lock().await = Some(peer);
            let mut packet = buf[..n].to_vec();
            cipher_send.apply(&mut packet);
            let mut w = writer_send.lock().await;
            write_frame(&mut *w, &packet, 2, true).await?;
        }
        Result::<()>::Ok(())
    });
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
        let ((_target, payload)) =
            parse_socks5_udp_datagram(&packet).map_err(|e| anyhow!(e.to_string()))?;
        if let Some(peer) = *latest_client.lock().await {
            let _ = udp.send_to(payload, peer).await?;
        }
    }
    upload.abort();
    control.shutdown().await.ok();
    Ok(())
}

async fn handle_non_mux_connection(mut local: TcpStream, cfg: WssConfig) -> Result<()> {
    let (command, target, is_socks5) = read_proxy_request(&mut local).await?;
    if command == SocksCommand::UdpAssociate {
        return handle_udp_proxy(local, cfg, target).await;
    }
    if command != SocksCommand::Connect {
        bail!("WSS non-MUX supports CONNECT or UDP ASSOCIATE only")
    };
    let (mut rd, writer) = open_upstream(&cfg).await?;
    let c = cipher(&cfg.key);
    let mut hello = format!("{}:{}\n", target.host, target.port).into_bytes();
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
        if is_socks5 {
            local.write_all(&socks5_failure_response()).await?
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
    if is_socks5 {
        local.write_all(&socks5_success_response()).await?
    } else {
        local
            .write_all(b"HTTP/1.1 200 Connection Established\r\n\r\n")
            .await?
    };
    let (mut local_rd, mut local_wr) = tokio::io::split(local);
    let writer_up = writer.clone();
    let up_cipher = c.clone();
    let buffer_size = cfg.buffer_size;
    let upload = tokio::spawn(async move {
        let mut buf = vec![0u8; buffer_size.clamp(16 * 1024, 1024 * 1024)];
        loop {
            let n = local_rd.read(&mut buf).await?;
            if n == 0 {
                break;
            }
            let mut payload = buf[..n].to_vec();
            up_cipher.apply(&mut payload);
            let mut w = writer_up.lock().await;
            write_frame(&mut *w, &payload, 2, true).await?
        }
        Result::<()>::Ok(())
    });
    loop {
        let Some((opcode, mut payload)) =
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
        c.apply(&mut payload);
        local_wr.write_all(&payload).await?
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
    };
    let listener = TcpListener::bind(format!("{}:{}", wc.proxy_host, wc.proxy_port)).await?;
    tracing::info!(
        "RushWay WSS non-MUX client proxy listening on {}:{}",
        wc.proxy_host,
        wc.proxy_port
    );
    loop {
        let (stream, peer) = listener.accept().await?;
        let cfg2 = wc.clone();
        tokio::spawn(async move {
            if let Err(e) = handle_non_mux_connection(stream, cfg2).await {
                tracing::debug!(%peer,error=%e,"WSS non-MUX connection closed")
            }
        });
    }
}

struct WssSessionState {
    writer: Arc<Mutex<BoxWriter>>,
    cipher: XorCipher,
    streams: Arc<Mutex<HashMap<u32, mpsc::Sender<OwnedMuxFrame>>>>,
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
        {
            let mut w = writer.lock().await;
            write_frame(&mut *w, &hello, 2, true).await?
        };
        let mut frame_buf = Vec::with_capacity(64 * 1024);
        let Some((opcode, mut ok)) =
            read_frame(&mut rd, Option::<&mut BoxWriter>::None, &mut frame_buf).await?
        else {
            bail!("WSS upstream closed during MUX handshake")
        };
        if opcode != 2 {
            bail!("invalid WSS MUX response opcode")
        };
        c.apply(&mut ok);
        if ok != b"OK\n" {
            bail!("WSS upstream rejected MUX handshake")
        };
        let session = Arc::new(Self {
            writer,
            cipher: c.clone(),
            streams: Arc::new(Mutex::new(HashMap::new())),
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
            let mut streams = reader_session.streams.lock().await;
            streams.clear();
            reader_session.active.store(0, Ordering::Release)
        });
        Ok(session)
    }
    fn available(&self) -> bool {
        !self.closed.load(Ordering::Acquire)
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
    ) -> Result<(u32, mpsc::Receiver<OwnedMuxFrame>)> {
        if !self.try_reserve() {
            bail!("WSS physical session is full or closed")
        };
        let id = self.next_id.fetch_add(1, Ordering::Relaxed).max(1);
        let (tx, rx) = mpsc::channel(64);
        {
            let mut streams = self.streams.lock().await;
            if self.closed.load(Ordering::Acquire) {
                self.active.fetch_sub(1, Ordering::AcqRel);
                bail!("WSS physical session closed")
            }
            streams.insert(id, tx);
        }
        let syn_payload = SynPayload {
            target: format!("{}:{}", target.host, target.port).into_bytes(),
            initial_data: Vec::new(),
        }
        .encode()
        .map_err(|e| anyhow!(e.to_string()))?;
        let syn =
            MuxFrame::new(id, MuxCommand::Syn, syn_payload).map_err(|e| anyhow!(e.to_string()))?;
        if let Err(error) = send_mux(&self.writer, &self.cipher, &syn).await {
            self.streams.lock().await.remove(&id);
            self.active.fetch_sub(1, Ordering::AcqRel);
            return Err(error);
        }
        Ok((id, rx))
    }
}
async fn send_mux(
    writer: &Arc<Mutex<BoxWriter>>,
    cipher: &XorCipher,
    frame: &MuxFrame,
) -> Result<()> {
    let mut data = Vec::with_capacity(7 + frame.payload.len());
    frame
        .encode(&mut data)
        .map_err(|e| anyhow!(e.to_string()))?;
    cipher.apply(&mut data);
    let mut w = writer.lock().await;
    write_frame(&mut *w, &data, 2, true).await
}
async fn send_mux_parts(
    writer: &Arc<Mutex<BoxWriter>>,
    cipher: &XorCipher,
    stream_id: u32,
    command: MuxCommand,
    payload: &[u8],
) -> Result<()> {
    let mut data = Vec::with_capacity(7 + payload.len());
    write_frame_parts(&mut data, stream_id, command, payload)
        .map_err(|e| anyhow!(e.to_string()))?;
    cipher.apply(&mut data);
    let mut w = writer.lock().await;
    write_frame(&mut *w, &data, 2, true).await
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
        let terminal = matches!(frame.command, MuxCommand::Fin | MuxCommand::Rst);
        let sender = { session.streams.lock().await.get(&id).cloned() };
        if let Some(tx) = sender {
            if tx.send(frame).await.is_err() || terminal {
                session.streams.lock().await.remove(&id);
                session.active.fetch_sub(1, Ordering::AcqRel);
            }
        }
    }
}

struct WssSessionPool {
    cfg: WssConfig,
    sessions: Mutex<Vec<Arc<WssSessionState>>>,
}
impl WssSessionPool {
    fn new(cfg: WssConfig) -> Arc<Self> {
        Arc::new(Self {
            cfg,
            sessions: Mutex::new(Vec::new()),
        });
    }
    async fn prewarm(self: &Arc<Self>) {
        for _ in 0..configured_session_count() {
            match WssSessionState::connect(&self.cfg).await {
                Ok(s) => self.sessions.lock().await.push(s),
                Err(error) => {
                    tracing::debug!(%error,"WSS physical session prewarm failed");
                    break;
                }
            }
        }
    }
    async fn acquire(
        self: &Arc<Self>,
        target: &TargetAddr,
    ) -> Result<(Arc<WssSessionState>, u32, mpsc::Receiver<OwnedMuxFrame>)> {
        let snapshot = {
            let mut sessions = self.sessions.lock().await;
            sessions.retain(|s| !s.closed.load(Ordering::Acquire));
            sessions.clone()
        };
        let mut ordered = snapshot;
        ordered.sort_by_key(|s| s.active.load(Ordering::Acquire));
        for session in ordered {
            if !session.available() {
                continue;
            }
            if let Ok((id, rx)) = session.open_stream(target).await {
                return Ok((session, id, rx));
            }
        }
        let limit = configured_session_count();
        let can_create = {
            let sessions = self.sessions.lock().await;
            sessions.len() < limit
        };
        if can_create {
            let session = WssSessionState::connect(&self.cfg).await?;
            let opened = session.open_stream(target).await?;
            self.sessions.lock().await.push(session.clone());
            return Ok((session, opened.0, opened.1));
        }
        bail!("all WSS physical MUX sessions are full or unavailable")
    }
}

async fn handle_connection(mut local: TcpStream, pool: Arc<WssSessionPool>) -> Result<()> {
    let (command, target, is_socks5) = read_proxy_request(&mut local).await?;
    if command == SocksCommand::UdpAssociate {
        return handle_udp_proxy(local, pool.cfg.clone(), target).await;
    }
    if command != SocksCommand::Connect {
        bail!("WSS path only supports CONNECT or UDP ASSOCIATE")
    };
    let (session, stream_id, mut rx) = pool.acquire(&target).await?;
    let first = timeout(
        Duration::from_secs(pool.cfg.connection_timeout.max(1)),
        async {
            loop {
                match rx.recv().await {
                    Some(frame) if matches!(frame.command, MuxCommand::Syn) => continue,
                    other => break other,
                }
            }
        },
    )
    .await
    .context("WSS upstream response timeout")?
    .ok_or_else(|| anyhow!("WSS upstream closed before CONNECT response"))?;
    match first.command {
        MuxCommand::Rst => {
            if is_socks5 {
                local.write_all(&socks5_failure_response()).await?
            } else {
                local.write_all(b"HTTP/1.1 502 Bad Gateway\r\nConnection: close\r\nContent-Length: 0\r\n\r\n").await?
            }
            local.shutdown().await.ok();
            return Ok(());
        }
        MuxCommand::Fin => {
            if is_socks5 {
                local.write_all(&socks5_success_response()).await?
            } else {
                local
                    .write_all(b"HTTP/1.1 200 Connection Established\r\n\r\n")
                    .await?
            }
            local.shutdown().await.ok();
            return Ok(());
        }
        MuxCommand::Data => {}
        MuxCommand::Syn => {
            unreachable!()
        }
    }
    let writer = session.writer.clone();
    let cipher = session.cipher.clone();
    if is_socks5 {
        local.write_all(&socks5_success_response()).await?
    } else {
        local
            .write_all(b"HTTP/1.1 200 Connection Established\r\n\r\n")
            .await?
    };
    let (mut local_rd, mut local_wr) = tokio::io::split(local);
    if !first.payload().is_empty() {
        local_wr.write_all(first.payload()).await?
    }
    let buffer_size = pool.cfg.buffer_size.clamp(16 * 1024, 1024 * 1024);
    let upload = tokio::spawn(async move {
        let mut buf = vec![0u8; buffer_size];
        loop {
            let n = local_rd.read(&mut buf).await?;
            if n == 0 {
                let _ = send_mux_parts(&writer, &cipher, stream_id, MuxCommand::Fin, &[]).await;
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
                )
                .await?;
                off = end
            }
        }
        Result::<()>::Ok(())
    });
    while let Some(frame) = rx.recv().await {
        match frame.command {
            MuxCommand::Data => local_wr.write_all(frame.payload()).await?,
            MuxCommand::Fin => {
                local_wr.shutdown().await?;
                break;
            }
            MuxCommand::Rst => break,
            MuxCommand::Syn => {}
        }
    }
    upload.abort();
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
    };
    let pool = WssSessionPool::new(wc.clone());
    pool.prewarm().await;
    let listener = TcpListener::bind(format!("{}:{}", wc.proxy_host, wc.proxy_port)).await?;
    tracing::info!(
        "RushWay WSS client proxy listening on {}:{}",
        wc.proxy_host,
        wc.proxy_port
    );
    loop {
        let (stream, peer) = listener.accept().await?;
        let pool2 = pool.clone();
        tokio::spawn(async move {
            if let Err(e) = handle_connection(stream, pool2).await {
                tracing::debug!(%peer,error=%e,"WSS proxy connection closed")
            }
        });
    }
}

async fn run_non_mux_dummy() {}
