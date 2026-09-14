//! Client-side physical MUX session pool.

use crate::crypto::XorCipher;
use crate::dns;
use crate::protocol::{write_frame_parts, MuxCommand, MuxFrame, OwnedMuxFrame, SynPayload};
use crate::proxy::{
    parse_http_connect, parse_socks5_request, parse_socks5_udp_datagram, socks5_failure_response,
    socks5_success_response, SocksCommand, TargetAddr, SOCKS5_CONNECT, SOCKS5_UDP_ASSOCIATE,
    SOCKS5_VERSION,
};
use crate::runtime::{apply_socket_options, enforce_target_policy, RuntimeConfig};
use crate::ws::{
    build_client_handshake_request, read_frame, read_frame_owned, read_http_headers,
    validate_client_handshake_response, write_frame,
};
use anyhow::{anyhow, bail, Context, Result};
use std::collections::HashMap;
use std::net::{IpAddr, SocketAddr};
use std::sync::{
    atomic::{AtomicBool, AtomicU32, AtomicUsize, Ordering},
    Arc,
};
use tokio::io::{AsyncReadExt, AsyncWriteExt, ReadHalf, WriteHalf};
use tokio::net::{TcpListener, TcpStream, UdpSocket};
use tokio::sync::{mpsc, Mutex, Semaphore};
use tokio::time::{timeout, Duration};

const DEFAULT_SESSION_COUNT: usize = 4;
const MAX_SESSION_COUNT: usize = 64;
const MAX_STREAMS_PER_SESSION: usize = 256;

fn configured_session_count() -> usize {
    std::env::var("RUSHWAY_MUX_SESSIONS")
        .ok()
        .and_then(|v| v.parse::<usize>().ok())
        .map(|v| v.clamp(1, MAX_SESSION_COUNT))
        .unwrap_or(DEFAULT_SESSION_COUNT)
}
fn configured_cipher(key: &Option<String>) -> XorCipher {
    XorCipher::new(key.as_deref().unwrap_or(""))
}

fn split_authority(authority: &str) -> Result<(String, u16)> {
    if authority.starts_with('[') {
        let close = authority
            .find(']')
            .ok_or_else(|| anyhow!("invalid upstream IPv6 authority"))?;
        let host = authority[1..close].to_string();
        let port = authority
            .get(close + 1..)
            .and_then(|s| s.strip_prefix(':'))
            .unwrap_or("80")
            .parse::<u16>()?;
        return Ok((host, port));
    }
    match authority.rsplit_once(':') {
        Some((host, port)) => Ok((host.to_string(), port.parse::<u16>()?)),
        None => Ok((authority.to_string(), 80)),
    }
}

async fn send_mux_parts_reuse(
    writer: &Arc<Mutex<WriteHalf<TcpStream>>>,
    cipher: &XorCipher,
    stream_id: u32,
    command: MuxCommand,
    payload: &[u8],
    scratch: &mut Vec<u8>,
) -> Result<()> {
    scratch.clear();
    scratch.reserve(
        7 + payload
            .len()
            .saturating_sub(scratch.capacity().saturating_sub(7)),
    );
    write_frame_parts(scratch, stream_id, command, payload).map_err(|e| anyhow!(e.to_string()))?;
    cipher.apply(scratch);
    let mut w = writer.lock().await;
    write_frame(&mut *w, scratch, 2, true).await
}
async fn send_mux_parts(
    writer: &Arc<Mutex<WriteHalf<TcpStream>>>,
    cipher: &XorCipher,
    stream_id: u32,
    command: MuxCommand,
    payload: &[u8],
) -> Result<()> {
    let mut bytes = Vec::with_capacity(7 + payload.len());
    send_mux_parts_reuse(writer, cipher, stream_id, command, payload, &mut bytes).await
}
fn parse_upstream(input: &str) -> Result<(String, String)> {
    let rest = input
        .strip_prefix("ws://")
        .ok_or_else(|| anyhow!("pooled client requires ws:// upstream"))?;
    let (authority, path) = match rest.split_once('/') {
        Some((a, p)) => (a.to_string(), format!("/{}", p)),
        None => (rest.to_string(), "/".to_string()),
    };
    let authority = if authority.contains(':') {
        authority
    } else {
        format!("{}:80", authority)
    };
    Ok((authority, path))
}

struct SessionState {
    writer: Arc<Mutex<WriteHalf<TcpStream>>>,
    cipher: XorCipher,
    streams: Arc<Mutex<HashMap<u32, mpsc::Sender<OwnedMuxFrame>>>>,
    next_id: AtomicU32,
    active: AtomicUsize,
    closed: AtomicBool,
}
impl SessionState {
    async fn connect(cfg: &RuntimeConfig) -> Result<Arc<Self>> {
        let upstream = cfg
            .upstream
            .as_deref()
            .ok_or_else(|| anyhow!("client mode requires upstream"))?;
        let (authority, path) = parse_upstream(upstream)?;
        let actual_host = cfg.fakehost.clone().unwrap_or_else(|| authority.clone());
        let (dns_host, dns_port) = split_authority(&authority)?;
        let addr = dns::resolve_socket(&dns_host, dns_port).await?;
        let socket = timeout(
            Duration::from_secs(cfg.connection_timeout.max(1)),
            TcpStream::connect(addr),
        )
        .await
        .context("upstream connection timeout")??;
        apply_socket_options(&socket, cfg);
        let (mut rd, mut wr) = tokio::io::split(socket);
        let origin = Some(format!("https://{}", actual_host));
        let sec_fetch_site = if authority.eq_ignore_ascii_case(&actual_host) {
            "same-origin"
        } else {
            "cross-site"
        };
        let (request, key) = build_client_handshake_request(
            &actual_host,
            &path,
            origin.as_deref(),
            Some(sec_fetch_site),
        );
        wr.write_all(&request).await?;
        wr.flush().await?;
        let response = read_http_headers(&mut rd).await?;
        validate_client_handshake_response(&response, &key)?;
        let writer = Arc::new(Mutex::new(wr));
        let cipher = configured_cipher(&cfg.key);
        let mut hello = b"MUX\n".to_vec();
        cipher.apply(&mut hello);
        {
            let mut w = writer.lock().await;
            write_frame(&mut *w, &hello, 2, true).await?
        };
        let mut frame_buf = Vec::with_capacity(64 * 1024);
        let Some((opcode, mut ok)) = read_frame(
            &mut rd,
            Option::<&mut WriteHalf<TcpStream>>::None,
            &mut frame_buf,
        )
        .await?
        else {
            bail!("upstream closed during MUX handshake")
        };
        if opcode != 2 {
            bail!("invalid MUX handshake response opcode")
        };
        cipher.apply(&mut ok);
        if ok != b"OK\n" {
            bail!("upstream rejected MUX handshake")
        };
        let state = Arc::new(Self {
            writer: writer.clone(),
            cipher: cipher.clone(),
            streams: Arc::new(Mutex::new(HashMap::new())),
            next_id: AtomicU32::new(1),
            active: AtomicUsize::new(0),
            closed: AtomicBool::new(false),
        });
        let reader_state = state.clone();
        tokio::spawn(async move {
            if let Err(e) = client_reader_loop(rd, reader_state.clone()).await {
                tracing::debug!(error=%e,"pooled MUX reader stopped");
            }
            reader_state.closed.store(true, Ordering::Release);
            reader_state.active.store(0, Ordering::Release);
            reader_state.streams.lock().await.clear()
        });
        Ok(state)
    }
    fn available(&self) -> bool {
        !self.closed.load(Ordering::Acquire)
            && self.active.load(Ordering::Acquire) < MAX_STREAMS_PER_SESSION
    }
    async fn open_stream(
        self: &Arc<Self>,
        target: &TargetAddr,
    ) -> Result<(u32, mpsc::Receiver<OwnedMuxFrame>)> {
        if self.closed.load(Ordering::Acquire) {
            bail!("MUX session is closed")
        }

        let syn_payload = SynPayload {
            target: format!("{}:{}", target.host, target.port).into_bytes(),
            initial_data: Vec::new(),
        }
        .encode()
        .map_err(|e| anyhow!(e.to_string()))?;

        let mut streams = self.streams.lock().await;

        if self.closed.load(Ordering::Acquire) {
            bail!("MUX session is closed")
        }

        loop {
            let active = self.active.load(Ordering::Acquire);
            if active >= MAX_STREAMS_PER_SESSION {
                bail!("MUX session is full")
            }
            if self
                .active
                .compare_exchange_weak(
                    active,
                    active + 1,
                    Ordering::AcqRel,
                    Ordering::Acquire,
                )
                .is_ok()
            {
                break;
            }
        }

        let id = self.next_id.fetch_add(1, Ordering::Relaxed).max(1);
        let (tx, rx) = mpsc::channel(32);
        streams.insert(id, tx);
        drop(streams);

        if let Err(e) = send_mux_parts(
            &self.writer,
            &self.cipher,
            id,
            MuxCommand::Syn,
            &syn_payload,
        )
        .await
        {
            if self.streams.lock().await.remove(&id).is_some() {
                self.active.fetch_sub(1, Ordering::AcqRel);
            }
            self.closed.store(true, Ordering::Release);
            return Err(e);
        }
        Ok((id, rx))
    }
    async fn close_stream(&self, id: u32) {
        if self.streams.lock().await.remove(&id).is_some() {
            self.active.fetch_sub(1, Ordering::AcqRel);
        }
    }
}
async fn client_reader_loop(mut rd: ReadHalf<TcpStream>, state: Arc<SessionState>) -> Result<()> {
    let mut frame_buf = Vec::with_capacity(64 * 1024);
    loop {
        let Some((opcode, mut payload)) = read_frame_owned(
            &mut rd,
            Option::<&mut WriteHalf<TcpStream>>::None,
            &mut frame_buf,
        )
        .await?
        else {
            break;
        };
        if opcode != 2 {
            continue;
        }
        state.cipher.apply(&mut payload);
        let frame = match MuxFrame::decode_owned(payload) {
            Ok(v) => v,
            Err(_) => continue,
        };
        let id = frame.stream_id;
        let tx = state.streams.lock().await.get(&id).cloned();
        if let Some(tx) = tx {
            if tx.send(frame).await.is_err() {
                if state.streams.lock().await.remove(&id).is_some() {
                    state.active.fetch_sub(1, Ordering::AcqRel);
                }
            }
        }
    }
    Ok(())
}
struct MuxSessionPool {
    cfg: RuntimeConfig,
    sessions: Mutex<Vec<Arc<SessionState>>>,
}
impl MuxSessionPool {
    fn new(cfg: RuntimeConfig) -> Arc<Self> {
        Arc::new(Self {
            cfg,
            sessions: Mutex::new(Vec::new()),
        })
    }
    async fn prewarm(self: &Arc<Self>) {
        for _ in 0..configured_session_count() {
            match SessionState::connect(&self.cfg).await {
                Ok(s) => self.sessions.lock().await.push(s),
                Err(e) => {
                    tracing::debug!(error=%e,"MUX session prewarm failed");
                    break;
                }
            }
        }
    }
    async fn acquire(
        self: &Arc<Self>,
        target: &TargetAddr,
    ) -> Result<(Arc<SessionState>, u32, mpsc::Receiver<OwnedMuxFrame>)> {
        enforce_target_policy(&self.cfg, target)?;
        let n = configured_session_count();
        loop {
            let mut sessions = self.sessions.lock().await;
            sessions.retain(|s| !s.closed.load(Ordering::Acquire));
            if let Some(s) = sessions
                .iter()
                .filter(|s| s.available())
                .min_by_key(|s| s.active.load(Ordering::Acquire))
                .cloned()
            {
                drop(sessions);
                match s.open_stream(target).await {
                    Ok(r) => return Ok((s, r.0, r.1)),
                    Err(_) => continue,
                }
            }
            let need_new = sessions.len() < n;
            drop(sessions);
            if !need_new {
                bail!("all pooled MUX sessions are at stream capacity")
            };
            let s = SessionState::connect(&self.cfg).await?;
            let r = s.open_stream(target).await?;
            self.sessions.lock().await.push(s.clone());
            return Ok((s, r.0, r.1));
        }
    }
}
async fn read_proxy_request(stream: &mut TcpStream) -> Result<(SocksCommand, TargetAddr, bool)> {
    let first = stream.read_u8().await?;
    if first != SOCKS5_VERSION {
        if first == b'C' {
            let mut buf = vec![first];
            let mut tail = read_http_headers(stream).await?;
            buf.append(&mut tail);
            let target = parse_http_connect(&buf).map_err(|e| anyhow!(e.to_string()))?;
            return Ok((SocksCommand::Connect, target, false));
        }
        bail!("unsupported proxy protocol")
    }
    let n = stream.read_u8().await? as usize;
    let mut methods = vec![0u8; n];
    stream.read_exact(&mut methods).await?;
    if !methods.contains(&0) {
        stream.write_all(&[5, 0xff]).await?;
        bail!("SOCKS5 no-auth method unavailable")
    }
    stream.write_all(&[5, 0]).await?;
    let mut head = [0u8; 4];
    stream.read_exact(&mut head).await?;
    if head[1] != SOCKS5_CONNECT && head[1] != SOCKS5_UDP_ASSOCIATE {
        stream.write_all(&[5, 7, 0, 1, 0, 0, 0, 0, 0, 0]).await?;
        bail!("unsupported SOCKS5 command")
    }
    let mut rest = match head[3] {
        1 => vec![0u8; 6],
        3 => vec![0u8; 1],
        4 => vec![0u8; 18],
        _ => {
            stream.write_all(&[5, 8, 0, 1, 0, 0, 0, 0, 0, 0]).await?;
            bail!("unsupported SOCKS5 address type")
        }
    };
    stream.read_exact(&mut rest).await?;
    let mut req = head.to_vec();
    req.extend_from_slice(&rest);
    if head[3] == 3 {
        let n = rest[0] as usize;
        let mut tail = vec![0u8; n + 2];
        stream.read_exact(&mut tail).await?;
        req.extend_from_slice(&tail)
    }
    let parsed = parse_socks5_request(&req).map_err(|e| anyhow!(e.to_string()))?;
    Ok((parsed.command, parsed.target, true))
}

async fn handle_udp_proxy(
    mut control: TcpStream,
    cfg: RuntimeConfig,
    bind_hint: TargetAddr,
) -> Result<()> {
    let _ = enforce_target_policy(&cfg, &bind_hint);
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
    if let IpAddr::V4(ip) = bound.ip() {
        resp[4..8].copy_from_slice(&ip.octets())
    }
    resp[8..10].copy_from_slice(&bound.port().to_be_bytes());
    control.write_all(&resp).await?;
    let upstream = cfg
        .upstream
        .clone()
        .ok_or_else(|| anyhow!("client mode requires upstream"))?;
    let (authority, path) = parse_upstream(&upstream)?;
    let actual_host = cfg.fakehost.clone().unwrap_or_else(|| authority.clone());
    let (dns_host, dns_port) = split_authority(&authority)?;
    let addr = dns::resolve_socket(&dns_host, dns_port).await?;
    let socket = timeout(
        Duration::from_secs(cfg.connection_timeout.max(1)),
        TcpStream::connect(addr),
    )
    .await??;
    apply_socket_options(&socket, &cfg);
    let (mut rd, mut wr) = tokio::io::split(socket);
    let origin = Some(format!("https://{}", actual_host));
    let sec_fetch_site = if authority.eq_ignore_ascii_case(&actual_host) {
        "same-origin"
    } else {
        "cross-site"
    };
    let (request, key) = build_client_handshake_request(
        &actual_host,
        &path,
        origin.as_deref(),
        Some(sec_fetch_site),
    );
    wr.write_all(&request).await?;
    wr.flush().await?;
    let response = read_http_headers(&mut rd).await?;
    validate_client_handshake_response(&response, &key)?;
    let writer = Arc::new(Mutex::new(wr));
    let cipher = configured_cipher(&cfg.key);
    let mut hello = b"UDP\n".to_vec();
    cipher.apply(&mut hello);
    {
        let mut w = writer.lock().await;
        write_frame(&mut *w, &hello, 2, true).await?
    };
    let mut frame_buf = Vec::with_capacity(64 * 1024);
    let Some((opcode, mut ok)) = read_frame(
        &mut rd,
        Option::<&mut WriteHalf<TcpStream>>::None,
        &mut frame_buf,
    )
    .await?
    else {
        bail!("upstream closed during UDP handshake")
    };
    if opcode != 2 {
        bail!("invalid UDP handshake response opcode")
    };
    cipher.apply(&mut ok);
    if ok != b"OK\n" {
        bail!("upstream rejected UDP handshake")
    };
    let latest = Arc::new(Mutex::new(None::<SocketAddr>));
    let udp_send = udp.clone();
    let writer_send = writer.clone();
    let c_send = cipher.clone();
    let latest_send = latest.clone();
    let upload = tokio::spawn(async move {
        let mut buf = vec![0u8; 64 * 1024];
        loop {
            let (n, peer) = udp_send.recv_from(&mut buf).await?;
            *latest_send.lock().await = Some(peer);
            let mut data = buf[..n].to_vec();
            c_send.apply(&mut data);
            let mut w = writer_send.lock().await;
            write_frame(&mut *w, &data, 2, true).await?
        }
        Result::<()>::Ok(())
    });
    loop {
        let Some((opcode, mut packet)) = read_frame(
            &mut rd,
            Option::<&mut WriteHalf<TcpStream>>::None,
            &mut frame_buf,
        )
        .await?
        else {
            break;
        };
        if opcode != 2 {
            continue;
        }
        cipher.apply(&mut packet);
        let (_src, payload) =
            parse_socks5_udp_datagram(&packet).map_err(|e| anyhow!(e.to_string()))?;
        if let Some(peer) = *latest.lock().await {
            let _ = udp.send_to(payload, peer).await?;
        }
    }
    upload.abort();
    control.shutdown().await.ok();
    Ok(())
}

async fn handle_tcp_proxy(
    mut local: TcpStream,
    cfg: RuntimeConfig,
    pool: Arc<MuxSessionPool>,
) -> Result<()> {
    let (command, target, is_socks5) = read_proxy_request(&mut local).await?;
    if command == SocksCommand::UdpAssociate {
        return handle_udp_proxy(local, cfg, target).await;
    }
    if command != SocksCommand::Connect {
        bail!("MUX client supports CONNECT or UDP ASSOCIATE only")
    }
    let (session, id, mut rx) = pool.acquire(&target).await?;
    let first = timeout(Duration::from_secs(cfg.connection_timeout.max(1)), async {
        loop {
            match rx.recv().await {
                Some(frame) if matches!(frame.command, MuxCommand::Syn) => continue,
                other => break other,
            }
        }
    })
    .await
    .context("MUX upstream response timeout")?
    .ok_or_else(|| anyhow!("MUX upstream closed before CONNECT response"))?;
    match first.command {
        MuxCommand::Rst => {
            session.close_stream(id).await;
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
            session.close_stream(id).await;
            return Ok(());
        }
        MuxCommand::Data => {}
        MuxCommand::Syn => {
            unreachable!()
        }
    }
    if is_socks5 {
        local.write_all(&socks5_success_response()).await?
    } else {
        local
            .write_all(b"HTTP/1.1 200 Connection Established\r\n\r\n")
            .await?
    }
    let (mut local_rd, mut local_wr) = tokio::io::split(local);
    if !first.payload().is_empty() {
        local_wr.write_all(first.payload()).await?
    }
    let writer = session.writer.clone();
    let cipher = session.cipher.clone();
    let upload = tokio::spawn(async move {
        let mut buf = vec![0u8; cfg.buffer_size.clamp(16 * 1024, 1024 * 1024)];
        let mut frame_scratch = Vec::with_capacity(buf.len().min(u16::MAX as usize) + 7);
        loop {
            let n = local_rd.read(&mut buf).await?;
            if n == 0 {
                let _ = send_mux_parts_reuse(
                    &writer,
                    &cipher,
                    id,
                    MuxCommand::Fin,
                    &[],
                    &mut frame_scratch,
                )
                .await;
                break;
            }
            let mut off = 0;
            while off < n {
                let end = (off + u16::MAX as usize).min(n);
                send_mux_parts_reuse(
                    &writer,
                    &cipher,
                    id,
                    MuxCommand::Data,
                    &buf[off..end],
                    &mut frame_scratch,
                )
                .await?;
                off = end
            }
        }
        Result::<()>::Ok(())
    });
    let mut result = Result::<()>::Ok(());
    let mut remote_fin = false;
    while let Some(frame) = rx.recv().await {
        match frame.command {
            MuxCommand::Data => {
                if let Err(e) = local_wr.write_all(frame.payload()).await {
                    result = Err(e.into());
                    break;
                }
            }
            MuxCommand::Fin => {
                let _ = local_wr.shutdown().await;
                remote_fin = true;
                break;
            }
            MuxCommand::Rst => {
                result = Err(anyhow!("upstream reset after CONNECT established"));
                break;
            }
            MuxCommand::Syn => {}
        }
    }
    upload.abort();
    if remote_fin {
        let _ = send_mux_parts(
            &session.writer,
            &session.cipher,
            id,
            MuxCommand::Fin,
            &[],
        )
        .await;
    }

    session.close_stream(id).await;
    result
}

pub async fn run_client(cfg: RuntimeConfig) -> Result<()> {
    let pool = MuxSessionPool::new(cfg.clone());
    pool.prewarm().await;
    let listener = TcpListener::bind(format!("{}:{}", cfg.proxy_host, cfg.proxy_port)).await?;
    let semaphore = Arc::new(Semaphore::new(cfg.max_connections.max(1)));
    let session_count = configured_session_count();
    tracing::info!("RushWay pooled client proxy listening on {}:{} ({} physical MUX sessions, up to {} streams/session)",cfg.proxy_host,cfg.proxy_port,session_count,MAX_STREAMS_PER_SESSION);
    loop {
        let (stream, peer) = listener.accept().await?;
        let permit = match semaphore.clone().try_acquire_owned() {
            Ok(v) => v,
            Err(_) => {
                tracing::debug!(%peer,"maximum client connections reached");
                continue;
            }
        };
        let cfg2 = cfg.clone();
        let pool2 = pool.clone();
        tokio::spawn(async move {
            let _permit = permit;
            if let Err(e) = handle_tcp_proxy(stream, cfg2, pool2).await {
                tracing::debug!(%peer,error=%e,"pooled proxy connection closed")
            }
        });
    }
}
