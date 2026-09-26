//! Client-side physical MUX session pool for plain WebSocket (`ws://`).
//!
//! Secure WebSocket (`wss://`) connections, including Cloudflare FakeHost and Edge
//! fallback, are handled authoritatively by [`crate::wss_client::WssSessionPool`].

use crate::crypto::{shared_cipher, XorCipher};
use crate::dns;
use crate::flow::CreditGate;
use crate::mux_writer::MuxFrameWriter;
use crate::protocol::{
    decode_version_payload, decode_window_payload, encode_version_payload, encode_window_payload,
    MuxCommand, MuxFrame, OwnedMuxFrame, SynPayload, MUX_INITIAL_WINDOW_KIB, MUX_WINDOW_REFRESH,
};
use crate::proxy::{
    parse_authority_with_default, read_client_proxy_request, socks5_success_response, SocksCommand,
    TargetAddr,
};
use crate::runtime::{
    apply_listener_options, apply_socket_options, drain_join_set, enforce_target_policy,
    recycle_buf, relay_buf, wait_shutdown, RuntimeConfig,
};
use crate::udp_batch::UdpBatchReader;
use crate::ws::{
    build_client_handshake_request, encode_ws_frame, read_frame, read_frame_owned,
    read_http_headers, validate_client_handshake_response, write_frame,
};
use anyhow::{anyhow, bail, Result};
use std::collections::HashMap;
use std::net::{IpAddr, SocketAddr};
use std::sync::{
    atomic::{AtomicBool, AtomicU32, AtomicUsize, Ordering},
    Arc, Mutex as StdMutex,
};
use tokio::io::{AsyncReadExt, AsyncWriteExt, ReadHalf, WriteHalf};
use tokio::net::{TcpListener, TcpStream, UdpSocket};
use tokio::sync::{mpsc, Mutex, Semaphore};
use tokio::time::{timeout, Duration};

const DEFAULT_SESSION_COUNT: usize = 4;
const MAX_SESSION_COUNT: usize = 64;
const MAX_STREAMS_PER_SESSION: usize = 2048;

fn configured_session_count() -> usize {
    std::env::var("RUSHWAY_MUX_SESSIONS")
        .ok()
        .and_then(|v| v.parse::<usize>().ok())
        .map(|v| v.clamp(1, MAX_SESSION_COUNT))
        .unwrap_or(DEFAULT_SESSION_COUNT)
}
fn configured_cipher(key: &Option<String>) -> Arc<XorCipher> {
    shared_cipher(key)
}

/// GoWay-compatible Cloudflare edge fallback for plain `ws://`.
/// If the upstream host is a literal IP and the primary TCP dial fails,
/// resolve `fakehost` to IPv4 edges and try each non-primary edge.
/// Mirrors `dialFallback` in goway.go.
async fn connect_with_fallback(
    cfg: &RuntimeConfig,
    target_host: &str,
    target_port: u16,
) -> Result<TcpStream> {
    let primary = dns::resolve_socket(target_host, target_port).await?;
    let conn_timeout = Duration::from_secs(cfg.connection_timeout.max(1));
    match timeout(conn_timeout, TcpStream::connect(primary)).await {
        Ok(Ok(socket)) => Ok(socket),
        Ok(Err(primary_err)) => {
            let is_ip = target_host.parse::<std::net::IpAddr>().is_ok();
            let sni = cfg
                .fakehost
                .as_deref()
                .map(|s| s.split(':').next().unwrap_or(s));
            if !(is_ip && sni.is_some()) {
                return Err(primary_err.into());
            }
            let sni = sni.unwrap();
            tracing::warn!(%primary, error=%primary_err, "[WS] Primary upstream unreachable; trying Cloudflare fallback edges via fakehost");
            let edges = dns::resolve_all_ipv4(sni).await?;
            for edge in edges {
                if std::net::IpAddr::V4(edge) == primary.ip() {
                    continue;
                }
                let candidate = SocketAddr::new(std::net::IpAddr::V4(edge), target_port);
                tracing::info!(
                    "[DNS] Trying fallback Cloudflare edge: {} (fakehost: {})",
                    candidate,
                    sni
                );
                match timeout(conn_timeout, TcpStream::connect(candidate)).await {
                    Ok(Ok(socket)) => return Ok(socket),
                    Ok(Err(e)) => {
                        tracing::debug!(%candidate, error=%e, "[WS] Fallback edge dial failed");
                    }
                    Err(_) => {
                        tracing::debug!(%candidate, "[WS] Fallback edge dial timed out");
                    }
                }
            }
            Err(anyhow!("primary {primary} unreachable and all Cloudflare fallback edges failed for fakehost: {sni}"))
        }
        Err(_) => {
            let is_ip = target_host.parse::<std::net::IpAddr>().is_ok();
            let sni = cfg
                .fakehost
                .as_deref()
                .map(|s| s.split(':').next().unwrap_or(s));
            if !(is_ip && sni.is_some()) {
                bail!("upstream connection timeout to {primary}");
            }
            let sni = sni.unwrap();
            tracing::warn!(%primary, "[WS] Primary upstream timed out; trying Cloudflare fallback edges via fakehost");
            let edges = dns::resolve_all_ipv4(sni).await?;
            for edge in edges {
                if std::net::IpAddr::V4(edge) == primary.ip() {
                    continue;
                }
                let candidate = SocketAddr::new(std::net::IpAddr::V4(edge), target_port);
                tracing::info!(
                    "[DNS] Trying fallback Cloudflare edge: {} (fakehost: {})",
                    candidate,
                    sni
                );
                match timeout(conn_timeout, TcpStream::connect(candidate)).await {
                    Ok(Ok(socket)) => return Ok(socket),
                    Ok(Err(e)) => {
                        tracing::debug!(%candidate, error=%e, "[WS] Fallback edge dial failed");
                    }
                    Err(_) => {
                        tracing::debug!(%candidate, "[WS] Fallback edge dial timed out");
                    }
                }
            }
            bail!("primary {primary} timed out and all Cloudflare fallback edges failed for fakehost: {sni}")
        }
    }
}

async fn send_mux_parts_reuse(
    writer: &Arc<MuxFrameWriter>,
    cipher: &XorCipher,
    stream_id: u32,
    command: MuxCommand,
    payload: &[u8],
    scratch: &mut Vec<u8>,
    obfs: bool,
) -> Result<()> {
    // Prefer the caller's local scratch only while it still owns capacity
    // (first frame); after `mem::take` the pool supplies the next buffer
    // so the encoder's exact `reserve` hits existing capacity.
    if scratch.capacity() == 0 {
        *scratch = crate::mux_writer::acquire_encode_buf();
    }
    crate::mux_writer::encode_mux_ws_frame(
        scratch, stream_id, command, payload, cipher, true, obfs,
    )
    .map_err(|e| anyhow!(e.to_string()))?;
    writer
        .send_mux(stream_id, command, std::mem::take(scratch))
        .await
}
async fn send_mux_parts(
    writer: &Arc<MuxFrameWriter>,
    cipher: &XorCipher,
    stream_id: u32,
    command: MuxCommand,
    payload: &[u8],
    obfs: bool,
) -> Result<()> {
    // Start from the encode pool (or an empty Vec when cold) so the
    // encoder's one exact `reserve` is a no-op on a warm pool.
    let mut bytes = crate::mux_writer::acquire_encode_buf();
    send_mux_parts_reuse(
        writer, cipher, stream_id, command, payload, &mut bytes, obfs,
    )
    .await
}
fn parse_upstream(input: &str) -> Result<(String, String)> {
    if input.starts_with("wss://") {
        bail!("pooled client in mux_pool requires ws:// upstream; use wss_client for wss://");
    }
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
    writer: Arc<MuxFrameWriter>,
    cipher: Arc<XorCipher>,
    obfs: bool,
    // Read-mostly under concurrency (one lookup per DATA frame), so a
    // RwLock: concurrent lookups, exclusive insert/remove.
    streams: Arc<std::sync::RwLock<HashMap<u32, mpsc::Sender<OwnedMuxFrame>>>>,
    /// Peer-advertised receive window in KiB once its VERSION arrived
    /// (None => un-negotiated: v1 unbounded sends, no WINDOW refunds).
    peer_window: StdMutex<Option<u16>>,
    /// Per-stream upload credit gates (populated in open_stream).
    gates: StdMutex<HashMap<u32, Arc<CreditGate>>>,
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
        let target =
            parse_authority_with_default(&authority, 80).map_err(|e| anyhow!(e.to_string()))?;
        let socket = connect_with_fallback(cfg, &target.host, target.port).await?;
        apply_socket_options(&socket, cfg);
        let (mut rd, mut wr) = tokio::io::split(socket);
        // GoWay performWSHandshake: SNI and Host both become fakehost when set,
        // Origin is http(s)://SNI, Sec-Fetch-Site compares SNI vs Host base.
        let sni_base = cfg
            .fakehost
            .as_deref()
            .map(|s| s.split(':').next().unwrap_or(s))
            .unwrap_or(&target.host);
        let header_base = actual_host.split(':').next().unwrap_or(&actual_host);
        let origin = Some(format!("http://{}", sni_base));
        let sec_fetch_site = if sni_base.eq_ignore_ascii_case(header_base) {
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
        let (writer, _writer_task) = MuxFrameWriter::spawn(wr);
        let cipher = configured_cipher(&cfg.key);
        let mut hello = b"MUX\n".to_vec();
        cipher.apply(&mut hello);
        writer
            .send(encode_ws_frame(&hello, 2, true).map_err(|e| anyhow!(e.to_string()))?)
            .await?;
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
        // W3 version negotiation: advertise our version + initial receive
        // window right after the session handshake. Old servers skip the
        // unknown command; new servers answer with their own VERSION.
        let version_payload = encode_version_payload(MUX_INITIAL_WINDOW_KIB);
        send_mux_parts(&writer, &cipher, 0, MuxCommand::Version, &version_payload, cfg.obfs)
            .await?;
        let state = Arc::new(Self {
            writer: writer.clone(),
            cipher: cipher.clone(),
            obfs: cfg.obfs,
            streams: Arc::new(std::sync::RwLock::new(HashMap::new())),
            peer_window: StdMutex::new(None),
            gates: StdMutex::new(HashMap::new()),
            next_id: AtomicU32::new(1),
            active: AtomicUsize::new(0),
            closed: AtomicBool::new(false),
        });
        let reader_state = state.clone();
        tokio::spawn(async move {
            if let Err(e) = client_reader_loop(rd, reader_state.clone()).await {
                tracing::warn!(error=%e,"pooled MUX reader stopped; closing session streams");
            }
            reader_state.closed.store(true, Ordering::Release);
            reader_state.active.store(0, Ordering::Release);
            reader_state.streams.write().unwrap_or_else(|e| e.into_inner()).clear();
            // Unblock upload tasks parked on credit: no WINDOW will arrive.
            let mut gates = reader_state.gates.lock().unwrap_or_else(|e| e.into_inner());
            for gate in gates.values() {
                gate.close();
            }
            gates.clear();
        });
        let heartbeat_state = state.clone();
        tokio::spawn(async move {
            let mut interval = tokio::time::interval(Duration::from_secs(25));
            interval.tick().await;
            loop {
                interval.tick().await;
                if heartbeat_state.closed.load(Ordering::Acquire) {
                    break;
                }
                if heartbeat_state.active.load(Ordering::Acquire) == 0 {
                    let ping = match encode_ws_frame(&[], 9, true) {
                        Ok(frame) => frame,
                        Err(_) => {
                            heartbeat_state.closed.store(true, Ordering::Release);
                            break;
                        }
                    };
                    if heartbeat_state.writer.send(ping).await.is_err() {
                        heartbeat_state.closed.store(true, Ordering::Release);
                        break;
                    }
                }
            }
        });
        Ok(state)
    }
    fn available(&self) -> bool {
        !self.closed.load(Ordering::Acquire)
            && !self.writer.is_closed()
            && self.active.load(Ordering::Acquire) < MAX_STREAMS_PER_SESSION
    }
    async fn open_stream(
        self: &Arc<Self>,
        target: &TargetAddr,
        initial_data: Vec<u8>,
    ) -> Result<(u32, mpsc::Receiver<OwnedMuxFrame>, Arc<CreditGate>)> {
        if self.closed.load(Ordering::Acquire) {
            bail!("MUX session is closed")
        }

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

        let (id, rx, gate) = {
            let mut streams = self.streams.write().unwrap_or_else(|e| e.into_inner());

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
                    .compare_exchange_weak(active, active + 1, Ordering::AcqRel, Ordering::Acquire)
                    .is_ok()
                {
                    break;
                }
            }

            let mut id;
            loop {
                id = self.next_id.fetch_add(1, Ordering::Relaxed);
                if id == 0 {
                    id = self.next_id.fetch_add(1, Ordering::Relaxed);
                }
                if !streams.contains_key(&id) {
                    break;
                }
            }
            let (tx, rx) = mpsc::channel(32);
            // Gates lock nests inside streams lock and guards the peer_window
            // check so a racing VERSION arm can never miss this stream
            // (lock order everywhere: streams -> gates -> peer_window).
            // Scoped block: the guard must be dead before the send awaits.
            let gate = {
                let mut gates = self.gates.lock().unwrap_or_else(|e| e.into_inner());
                let gate = CreditGate::new();
                if let Some(kib) = *self.peer_window.lock().unwrap_or_else(|e| e.into_inner()) {
                    gate.enable(i64::from(kib) * 1024);
                }
                gates.insert(id, gate.clone());
                gate
            };
            streams.insert(id, tx);
            (id, rx, gate)
        };

        if let Err(e) = send_mux_parts(
            &self.writer,
            &self.cipher,
            id,
            MuxCommand::Syn,
            &syn_payload,
            self.obfs,
        )
        .await
        {
            if self.streams.write().unwrap_or_else(|e| e.into_inner()).remove(&id).is_some() {
                self.active.fetch_sub(1, Ordering::AcqRel);
            }
            self.gates.lock().unwrap_or_else(|e| e.into_inner()).remove(&id);
            self.closed.store(true, Ordering::Release);
            return Err(e);
        }

        if !remaining_initial.is_empty() {
            if let Err(e) = send_mux_parts(
                &self.writer,
                &self.cipher,
                id,
                MuxCommand::Data,
                remaining_initial,
                self.obfs,
            )
            .await
            {
                if self.streams.write().unwrap_or_else(|e| e.into_inner()).remove(&id).is_some() {
                    self.active.fetch_sub(1, Ordering::AcqRel);
                }
                self.gates.lock().unwrap_or_else(|e| e.into_inner()).remove(&id);
                self.closed.store(true, Ordering::Release);
                return Err(e);
            }
        }

        Ok((id, rx, gate))
    }
    async fn close_stream(&self, id: u32) {
        if self.streams.write().unwrap_or_else(|e| e.into_inner()).remove(&id).is_some() {
            self.active.fetch_sub(1, Ordering::AcqRel);
        }
        if let Some(gate) = self.gates.lock().unwrap_or_else(|e| e.into_inner()).remove(&id) {
            gate.close();
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
        // W3 control frames are session/stream-level: handle them here
        // instead of routing into per-stream channels (VERSION uses
        // stream id 0 and would otherwise be dropped silently).
        match frame.command {
            MuxCommand::Version => {
                if let Some((version, kib)) = decode_version_payload(frame.payload()) {
                    if version >= 1 {
                        let gates = state.gates.lock().unwrap_or_else(|e| e.into_inner());
                        let mut pw = state.peer_window.lock().unwrap_or_else(|e| e.into_inner());
                        if pw.is_none() {
                            *pw = Some(kib);
                            drop(pw);
                            let window = i64::from(kib) * 1024;
                            for gate in gates.values() {
                                gate.enable(window);
                            }
                            tracing::debug!(kib, "peer VERSION received; send window enabled");
                        }
                    }
                }
                continue;
            }
            MuxCommand::Window => {
                if let Some(credit) = decode_window_payload(frame.payload()) {
                    if let Some(gate) = state.gates.lock().unwrap_or_else(|e| e.into_inner()).get(&frame.stream_id) {
                        gate.release(i64::from(credit));
                    }
                }
                continue;
            }
            _ => {}
        }
        let id = frame.stream_id;
        let tx = state.streams.read().unwrap_or_else(|e| e.into_inner()).get(&id).cloned();
        if let Some(tx) = tx {
            if tx.send(frame).await.is_err() {
                if state.streams.write().unwrap_or_else(|e| e.into_inner()).remove(&id).is_some() {
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
    session_creation: Mutex<()>,
    consecutive_failures: std::sync::atomic::AtomicU32,
}
impl MuxSessionPool {
    fn new(cfg: RuntimeConfig) -> Arc<Self> {
        Arc::new(Self {
            cfg,
            sessions: Mutex::new(Vec::new()),
            session_creation: Mutex::new(()),
            consecutive_failures: std::sync::atomic::AtomicU32::new(0),
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
            self.consecutive_failures.store(0, Ordering::Release);
            return;
        }
        match SessionState::connect(&self.cfg).await {
            Ok(s) => {
                self.sessions.lock().await.push(s);
                self.consecutive_failures.store(0, Ordering::Release);
            }
            Err(error) => {
                let failures = self.consecutive_failures.fetch_add(1, Ordering::AcqRel) + 1;
                tracing::warn!(error=%error, failures, "MUX physical session creation failed; will retry with backoff");
            }
        }
    }
    async fn maintain(self: &Arc<Self>) {
        loop {
            self.replenish().await;
            // Base 100ms; +500ms per consecutive failure, capped at ~5s.
            // Prevents DNS/TCP hammering while the upstream is down.
            let failures = self.consecutive_failures.load(Ordering::Acquire);
            let backoff = (failures.saturating_mul(500)).min(5000);
            tokio::time::sleep(Duration::from_millis(100 + backoff as u64)).await;
        }
    }
    async fn acquire(
        self: &Arc<Self>,
        target: &TargetAddr,
        initial_data: Vec<u8>,
    ) -> Result<(Arc<SessionState>, u32, mpsc::Receiver<OwnedMuxFrame>, Arc<CreditGate>)> {
        enforce_target_policy(&self.cfg, target)?;
        let n = configured_session_count();
        loop {
            let mut sessions = self.sessions.lock().await;
            sessions.retain(|s| !s.closed.load(Ordering::Acquire));
            let snapshot = sessions
                .iter()
                .filter(|s| s.available())
                .map(|s| (s.active.load(Ordering::Acquire), s.clone()))
                .collect::<Vec<_>>();
            if let Some((_active, s)) = snapshot.into_iter().min_by_key(|(active, _)| *active) {
                drop(sessions);
                match s.open_stream(target, initial_data.clone()).await {
                    Ok(r) => return Ok((s, r.0, r.1, r.2)),
                    Err(_) => continue,
                }
            }
            let need_new = sessions.len() < n;
            drop(sessions);
            if !need_new {
                tokio::time::sleep(Duration::from_millis(2)).await;
                continue;
            }

            let _creation_guard = self.session_creation.lock().await;
            let mut sessions = self.sessions.lock().await;
            sessions.retain(|s| !s.closed.load(Ordering::Acquire));
            if sessions.len() >= n {
                continue;
            }
            drop(sessions);

            let s = SessionState::connect(&self.cfg).await?;
            let r = s.open_stream(target, initial_data.clone()).await?;
            self.sessions.lock().await.push(s.clone());
            return Ok((s, r.0, r.1, r.2));
        }
    }
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
    let target =
        parse_authority_with_default(&authority, 80).map_err(|e| anyhow!(e.to_string()))?;
    let socket = connect_with_fallback(&cfg, &target.host, target.port).await?;
    apply_socket_options(&socket, &cfg);
    let (mut rd, mut wr) = tokio::io::split(socket);
    let sni_base = cfg
        .fakehost
        .as_deref()
        .map(|s| s.split(':').next().unwrap_or(s))
        .unwrap_or(&target.host);
    let header_base = actual_host.split(':').next().unwrap_or(&actual_host);
    let origin = Some(format!("http://{}", sni_base));
    let sec_fetch_site = if sni_base.eq_ignore_ascii_case(header_base) {
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
        let mut batch = UdpBatchReader::new(udp_send);
        loop {
            let count = match batch.recv().await {
                Ok(n) => n,
                Err(_) => break,
            };
            for i in 0..count {
                let (pkt, peer) = batch.packet(i);
                *latest_send.lock().await = Some(peer);
                let mut data = pkt.to_vec();
                c_send.apply(&mut data);
                crate::stats::add_bytes(data.len() as i64, 0);
                let mut w = writer_send.lock().await;
                if write_frame(&mut *w, &data, 2, true).await.is_err() {
                    return Ok::<(), anyhow::Error>(());
                }
            }
        }
        Ok::<(), anyhow::Error>(())
    });
    let mut dummy = [0u8; 1];
    tokio::select! {
        _ = control.read(&mut dummy) => {}
        _ = async {
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
                if let Some(peer) = *latest.lock().await {
                    let _ = udp.send_to(&packet, peer).await;
                    crate::stats::add_bytes(0, packet.len() as i64);
                }
            }
            Ok::<(), anyhow::Error>(())
        } => {}
    }
    upload.abort();
    let _ = control.shutdown().await;
    Ok(())
}

async fn handle_tcp_proxy(
    mut local: TcpStream,
    cfg: RuntimeConfig,
    pool: Arc<MuxSessionPool>,
) -> Result<()> {
    let req = read_client_proxy_request(&mut local).await?;
    if req.command == SocksCommand::UdpAssociate {
        return handle_udp_proxy(local, cfg, req.target).await;
    }
    if req.command != SocksCommand::Connect {
        bail!("MUX client supports CONNECT or UDP ASSOCIATE only")
    }
    let initial_data = req.initial_payload.unwrap_or_default();
    let (session, id, mut rx, gate) = pool.acquire(&req.target, initial_data).await?;
    if req.is_socks5 {
        local.write_all(&socks5_success_response()).await?;
    } else if req.is_connect {
        local
            .write_all(b"HTTP/1.1 200 Connection Established\r\n\r\n")
            .await?;
    }
    let (mut local_rd, mut local_wr) = tokio::io::split(local);
    let writer = session.writer.clone();
    let cipher = session.cipher.clone();
    let obfs = session.obfs;
    let upload = tokio::spawn(async move {
        let mut buf = relay_buf(cfg.buffer_size).await;
        // Local first-frame scratch; after each `mem::take` the next
        // buffer comes from the writer-side encode pool (capacity reused).
        let mut frame_scratch = crate::mux_writer::acquire_encode_buf();
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
                    obfs,
                )
                .await;
                break;
            }
            crate::stats::add_bytes(n as i64, 0);
            let mut off = 0;
            while off < n {
                let end = (off + u16::MAX as usize).min(n);
                // W3 upload gate: bounded once the server's VERSION is in.
                gate.acquire(end - off).await;
                send_mux_parts_reuse(
                    &writer,
                    &cipher,
                    id,
                    MuxCommand::Data,
                    &buf[off..end],
                    &mut frame_scratch,
                    obfs,
                )
                .await?;
                off = end
            }
        }
        recycle_buf(buf).await;
        Result::<()>::Ok(())
    });
    let mut result = Result::<()>::Ok(());
    let mut remote_fin = false;
    // W3 receive-side refund accumulator (client -> server WINDOW).
    let mut refund_pending: u32 = 0;
    while let Some(frame) = rx.recv().await {
        match frame.command {
            MuxCommand::Data => {
                let payload_empty = frame.payload().is_empty();
                let len = frame.payload().len();
                let write_res = if !payload_empty {
                    local_wr.write_all(frame.payload()).await
                } else {
                    Ok(())
                };
                crate::mux_writer::recycle_encode_buf(frame.into_storage());
                if let Err(e) = write_res {
                    result = Err(e.into());
                    break;
                }
                if !payload_empty {
                    crate::stats::add_bytes(0, len as i64);
                    let negotiated = session.peer_window.lock().unwrap_or_else(|e| e.into_inner()).is_some();
                    if negotiated {
                        refund_pending = refund_pending.saturating_add(len as u32);
                        if refund_pending as usize >= MUX_WINDOW_REFRESH {
                            let credit = std::mem::take(&mut refund_pending);
                            send_mux_parts(
                                &session.writer,
                                &session.cipher,
                                id,
                                MuxCommand::Window,
                                &encode_window_payload(credit),
                                session.obfs,
                            )
                            .await?;
                        }
                    } else {
                        refund_pending = 0;
                    }
                }
            }
            MuxCommand::Fin => {
                crate::mux_writer::recycle_encode_buf(frame.into_storage());
                let _ = local_wr.shutdown().await;
                remote_fin = true;
                break;
            }
            MuxCommand::Rst => {
                crate::mux_writer::recycle_encode_buf(frame.into_storage());
                result = Err(anyhow!("upstream reset after CONNECT established"));
                break;
            }
            MuxCommand::Syn | MuxCommand::Version | MuxCommand::Window => {
                crate::mux_writer::recycle_encode_buf(frame.into_storage());
            }
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
            session.obfs,
        )
        .await;
    }

    session.close_stream(id).await;
    result
}

pub async fn run_client(cfg: RuntimeConfig) -> Result<()> {
    let pool = MuxSessionPool::new(cfg.clone());
    let listener = TcpListener::bind(format!("{}:{}", cfg.proxy_host, cfg.proxy_port)).await?;
    apply_listener_options(&listener);
    let pool_maintainer = pool.clone();
    tokio::spawn(async move {
        pool_maintainer.maintain().await;
    });
    let semaphore = Arc::new(Semaphore::new(cfg.max_connections.max(1)));
    let session_count = configured_session_count();
    tracing::info!("RushWay pooled client proxy listening on {}:{} ({} physical MUX sessions, up to {} streams/session)",cfg.proxy_host,cfg.proxy_port,session_count,MAX_STREAMS_PER_SESSION);
    let mut set = tokio::task::JoinSet::new();
    loop {
        tokio::select! {
            _ = wait_shutdown() => {
                tracing::info!("shutdown requested, draining pooled client connections");
                break;
            }
            res = listener.accept() => {
                let (stream, peer) = res?;
                // Fail fast at capacity so shutdown is never stuck behind
                // a permit wait.
                let permit = match semaphore.clone().try_acquire_owned() {
                    Ok(v) => v,
                    Err(_) => {
                        tracing::debug!(%peer, "maximum client connections reached");
                        continue;
                    }
                };
                let cfg2 = cfg.clone();
                apply_socket_options(&stream, &cfg2);
                let pool2 = pool.clone();
                set.spawn(async move {
                    let _permit = permit;
                    let _conn = crate::stats::ConnGuard::new();
                    if let Err(e) = handle_tcp_proxy(stream, cfg2, pool2).await {
                        tracing::warn!(%peer,error=%e,"pooled proxy connection closed with error")
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

    #[test]
    fn parse_ws_upstream_success() {
        let (authority, path) = parse_upstream("ws://example.com:8080/ws").unwrap();
        assert_eq!(authority, "example.com:8080");
        assert_eq!(path, "/ws");
    }

    #[test]
    fn parse_ws_upstream_rejects_wss_explicitly() {
        let err = parse_upstream("wss://example.com:443/pyway").unwrap_err();
        assert!(err.to_string().contains("use wss_client for wss://"));
    }
}
