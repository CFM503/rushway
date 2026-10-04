//! Client-side physical MUX session pool for plain WebSocket (`ws://`).
//!
//! Secure WebSocket (`wss://`) connections, including Cloudflare FakeHost and Edge
//! fallback, are handled authoritatively by [`crate::wss_client::WssSessionPool`].

use crate::common::{
    configured_cipher, parse_ws_url_with_port, send_mux_parts, send_mux_parts_reuse,
    socks5_udp_associate_reply, try_cloudflare_edges, HTTP_200_CONNECTION_ESTABLISHED,
};
use crate::crypto::XorCipher;
use crate::dns;
use crate::flow::CreditGate;
use crate::mux_writer::MuxFrameWriter;
use crate::protocol::{
    decode_version_payload, decode_window_payload, encode_version_payload, encode_window_payload,
    MuxCommand, MuxFrame, OwnedMuxFrame, SynPayload, MUX_INITIAL_WINDOW_KIB, MUX_WINDOW_REFRESH,
};
use crate::proxy::{
    parse_authority_with_default, parse_socks5_udp_datagram, read_client_proxy_request,
    socks5_success_response, SocksCommand, TargetAddr,
};
use crate::runtime::{
    apply_listener_options, apply_socket_options, check_target_policy, drain_join_set,
    enforce_target_policy, recycle_buf, relay_buf, wait_shutdown, RuntimeConfig,
};
use crate::udp_batch::UdpBatchReader;
use crate::ws::{
    build_client_handshake_request, encode_ws_frame, read_frame, read_frame_owned,
    read_http_headers, validate_client_handshake_response, write_frame,
};
use anyhow::{anyhow, bail, Result};
use std::collections::HashMap;
use std::net::SocketAddr;
use std::sync::{
    atomic::{AtomicBool, AtomicU32, AtomicUsize, Ordering},
    Arc, Mutex as StdMutex,
};
use tokio::io::{AsyncReadExt, AsyncWriteExt, ReadHalf, WriteHalf};
use tokio::net::{TcpListener, TcpStream, UdpSocket};
use tokio::sync::{mpsc, Mutex, Notify, Semaphore};
use tokio::time::{timeout, Duration};

const MAX_SESSION_COUNT: usize = 64;
const MAX_STREAMS_PER_SESSION: usize = 2048;

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
            if let Some(socket) =
                try_cloudflare_edges(sni, primary.ip(), target_port, conn_timeout).await?
            {
                return Ok(socket);
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
            if let Some(socket) =
                try_cloudflare_edges(sni, primary.ip(), target_port, conn_timeout).await?
            {
                return Ok(socket);
            }
            bail!("primary {primary} timed out and all Cloudflare fallback edges failed for fakehost: {sni}")
        }
    }
}

fn parse_upstream(input: &str) -> Result<(String, String)> {
    if input.starts_with("wss://") {
        bail!("pooled client in mux_pool requires ws:// upstream; use wss_client for wss://");
    }
    parse_ws_url_with_port(input, "pooled client requires ws:// upstream")
}

struct SessionState {
    writer: Arc<MuxFrameWriter>,
    cipher: XorCipher,
    obfs: bool,
    // Read-mostly under concurrency (one lookup per DATA frame), so a
    // RwLock: concurrent lookups, exclusive insert/remove.
    streams: Arc<std::sync::RwLock<HashMap<u32, mpsc::Sender<OwnedMuxFrame>>>>,
    /// Peer-advertised receive window in KiB once its VERSION arrived
    /// (0 => un-negotiated: v1 unbounded sends, no WINDOW refunds).
    peer_window_kib: AtomicU32,
    /// Per-stream upload credit gates (populated in open_stream).
    gates: StdMutex<HashMap<u32, Arc<CreditGate>>>,
    next_id: AtomicU32,
    active: AtomicUsize,
    closed: AtomicBool,
    /// Pool-shared notifier: stream release and session close wake up
    /// `acquire` waiters parked on session capacity.
    capacity_notify: Arc<Notify>,
}
impl SessionState {
    async fn connect(cfg: &RuntimeConfig, capacity_notify: Arc<Notify>) -> Result<Arc<Self>> {
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
            peer_window_kib: AtomicU32::new(0),
            gates: StdMutex::new(HashMap::new()),
            next_id: AtomicU32::new(1),
            active: AtomicUsize::new(0),
            closed: AtomicBool::new(false),
            capacity_notify,
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
            // Wake capacity waiters: this session just left the pool.
            reader_state.capacity_notify.notify_waiters();
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
                // Ping unconditionally: active-but-idle streams (a suspended
                // SSH session, for instance) carry no traffic, so waiting for
                // a fully idle session lets NAT/CDN idle timeouts reset the
                // link and every stream riding on it.
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
            let (tx, rx) = mpsc::channel(crate::protocol::MUX_STREAM_QUEUE_CAP);
            // Scoped gate registration: gates guard dies before the send awaits.
            let gate = {
                let mut gates = self.gates.lock().unwrap_or_else(|e| e.into_inner());
                let gate = CreditGate::new();
                let kib = self.peer_window_kib.load(Ordering::Acquire);
                if kib > 0 {
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
            self.capacity_notify.notify_waiters();
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
                self.capacity_notify.notify_waiters();
                return Err(e);
            }
        }

        Ok((id, rx, gate))
    }
    async fn close_stream(&self, id: u32) {
        if self.streams.write().unwrap_or_else(|e| e.into_inner()).remove(&id).is_some() {
            self.active.fetch_sub(1, Ordering::AcqRel);
            self.capacity_notify.notify_waiters();
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
                        let prev = state.peer_window_kib.swap(kib as u32, Ordering::AcqRel);
                        if prev == 0 {
                            let gates = state.gates.lock().unwrap_or_else(|e| e.into_inner());
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
            // Same non-blocking rule as the server read loop: a local client
            // that stops reading must not stall downloads for every other
            // stream on this session. `close_stream` also drops the
            // per-stream credit gate, which the previous inline teardown
            // forgot — leaking one `state.gates` entry per closed stream.
            match tx.try_send(frame) {
                Ok(()) => {}
                Err(mpsc::error::TrySendError::Full(_)) => {
                    tracing::warn!(stream_id = id, "mux RST: stream queue full; resetting stalled stream");
                    let _ = send_mux_parts(
                        &state.writer,
                        &state.cipher,
                        id,
                        MuxCommand::Rst,
                        &[],
                        state.obfs,
                    )
                    .await;
                    state.close_stream(id).await;
                }
                Err(mpsc::error::TrySendError::Closed(_)) => {
                    state.close_stream(id).await;
                }
            }
        }
    }
    Ok(())
}
struct MuxSessionPool {
    cfg: RuntimeConfig,
    /// Target physical session count, resolved once at startup (see
    /// `run_client`); re-read per acquire would touch the environment on
    /// every stream open.
    session_count: usize,
    sessions: Mutex<Vec<Arc<SessionState>>>,
    session_creation: Mutex<()>,
    consecutive_failures: std::sync::atomic::AtomicU32,
    /// Signalled whenever session capacity may have appeared: a stream was
    /// released, a session closed, or a replenished session joined the pool.
    capacity_notify: Arc<Notify>,
}
impl MuxSessionPool {
    fn new(cfg: RuntimeConfig, session_count: usize) -> Arc<Self> {
        Arc::new(Self {
            cfg,
            session_count: session_count.clamp(1, MAX_SESSION_COUNT),
            sessions: Mutex::new(Vec::new()),
            session_creation: Mutex::new(()),
            consecutive_failures: std::sync::atomic::AtomicU32::new(0),
            capacity_notify: Arc::new(Notify::new()),
        })
    }
    async fn replenish(self: &Arc<Self>) -> bool {
        let target = self.session_count;
        let _guard = self.session_creation.lock().await;
        let need_new = {
            let mut sessions = self.sessions.lock().await;
            sessions.retain(|s| !s.closed.load(Ordering::Acquire));
            sessions.len() < target
        };
        if !need_new {
            self.consecutive_failures.store(0, Ordering::Release);
            return false;
        }
        match SessionState::connect(&self.cfg, self.capacity_notify.clone()).await {
            Ok(s) => {
                let mut sessions = self.sessions.lock().await;
                sessions.push(s);
                self.consecutive_failures.store(0, Ordering::Release);
                self.capacity_notify.notify_waiters();
                sessions.len() < target
            }
            Err(error) => {
                let failures = self.consecutive_failures.fetch_add(1, Ordering::AcqRel) + 1;
                tracing::warn!(error=%error, failures, "MUX physical session creation failed; will retry with backoff");
                false
            }
        }
    }
    async fn maintain(self: &Arc<Self>) {
        loop {
            let more_needed = self.replenish().await;
            if more_needed {
                tokio::time::sleep(Duration::from_millis(10)).await;
                continue;
            }
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
        let n = self.session_count;
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
                    Ok(r) => {
                        let current_len = self.sessions.lock().await.len();
                        if current_len < n {
                            let pool = self.clone();
                            tokio::spawn(async move {
                                pool.replenish().await;
                            });
                        }
                        return Ok((s, r.0, r.1, r.2));
                    }
                    Err(_) => continue,
                }
            }
            let need_new = sessions.len() < n;
            drop(sessions);
            if !need_new {
                // Every session is at capacity: park until a stream release,
                // a session close, or a replenished session signals spare
                // capacity. The bounded wait is also the lost-wakeup backstop
                // for `notify_waiters` races (a waiter registers on first
                // poll, so a signal between the scan and the poll is lost).
                let notified = self.capacity_notify.notified();
                let _ = timeout(Duration::from_millis(500), notified).await;
                continue;
            }

            let _creation_guard = self.session_creation.lock().await;
            let mut sessions = self.sessions.lock().await;
            sessions.retain(|s| !s.closed.load(Ordering::Acquire));
            if sessions.len() >= n {
                continue;
            }
            drop(sessions);

            let s = SessionState::connect(&self.cfg, self.capacity_notify.clone()).await?;
            let r = s.open_stream(target, initial_data.clone()).await?;
            self.sessions.lock().await.push(s.clone());
            self.capacity_notify.notify_waiters();
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
    let resp = socks5_udp_associate_reply(bound);
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
    let latest = Arc::new(StdMutex::new(None::<SocketAddr>));
    let block_local = cfg.block_local;
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
                // The local leg is a plaintext SOCKS5 UDP request (the XOR
                // below happens on the way out), so the destination is still
                // readable here; enforce `-block-local` per datagram like the
                // WSS client does.
                if let Ok((target, _)) = parse_socks5_udp_datagram(pkt) {
                    if let Err(e) = check_target_policy(block_local, &target) {
                        tracing::debug!(error = %e, "WS UDP datagram dropped by target policy");
                        continue;
                    }
                }
                *latest_send.lock().unwrap_or_else(|e| e.into_inner()) = Some(peer);
                let mut data = crate::mux_writer::acquire_encode_buf();
                data.extend_from_slice(pkt);
                c_send.apply(&mut data);
                crate::stats::add_bytes(data.len() as i64, 0);
                let mut w = writer_send.lock().await;
                let res = write_frame(&mut *w, &data, 2, true).await;
                crate::mux_writer::recycle_encode_buf(data);
                if res.is_err() {
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
                // Copy the peer out first: a std Mutex guard must never be
                // held across the await below.
                let peer_opt = *latest.lock().unwrap_or_else(|e| e.into_inner());
                if let Some(peer) = peer_opt {
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
        local.write_all(HTTP_200_CONNECTION_ESTABLISHED).await?;
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
                    let negotiated = session.peer_window_kib.load(Ordering::Relaxed) > 0;
                    if negotiated {
                        refund_pending = refund_pending.saturating_add(len as u32);
                        if refund_pending as usize >= MUX_WINDOW_REFRESH {
                            let credit = std::mem::take(&mut refund_pending);
                            // A failed refund must not skip the stream
                            // teardown below: record the error and exit
                            // through the shared cleanup path instead of `?`.
                            if let Err(e) = send_mux_parts(
                                &session.writer,
                                &session.cipher,
                                id,
                                MuxCommand::Window,
                                &encode_window_payload(credit),
                                session.obfs,
                            )
                            .await
                            {
                                result = Err(e);
                                break;
                            }
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

pub async fn run_client(cfg: RuntimeConfig, session_count: usize) -> Result<()> {
    let pool = MuxSessionPool::new(cfg.clone(), session_count);
    let listener = TcpListener::bind(format!("{}:{}", cfg.proxy_host, cfg.proxy_port)).await?;
    apply_listener_options(&listener);
    let pool_maintainer = pool.clone();
    tokio::spawn(async move {
        pool_maintainer.maintain().await;
    });
    let semaphore = Arc::new(Semaphore::new(cfg.max_connections.max(1)));
    tracing::info!("RushWay pooled client proxy listening on {}:{} ({} physical MUX sessions, up to {} streams/session)",cfg.proxy_host,cfg.proxy_port,pool.session_count,MAX_STREAMS_PER_SESSION);
    let mut set = tokio::task::JoinSet::new();
    loop {
        tokio::select! {
            _ = wait_shutdown() => {
                tracing::info!("shutdown requested, draining pooled client connections");
                break;
            }
            res = listener.accept() => {
                // A transient accept error (EMFILE, ENFILE, ENOBUFS) must not
                // kill the whole proxy: log, back off briefly, and retry.
                let (stream, peer) = match res {
                    Ok(v) => v,
                    Err(e) => {
                        tracing::warn!(error=%e, "pooled client listener accept failed; retrying in 100ms");
                        tokio::time::sleep(Duration::from_millis(100)).await;
                        continue;
                    }
                };
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
