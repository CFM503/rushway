//! Runtime forwarding paths for the GoWay-compatible transport slice.

use crate::crypto::XorCipher;
use crate::dns::resolve_socket;
use crate::flow::CreditGate;
use crate::mux_writer::MuxFrameWriter;
use crate::protocol::{
    decode_version_payload, decode_window_payload, encode_version_payload, encode_window_payload,
    MuxCommand, MuxFrame, OwnedMuxFrame, SynPayload, MUX_INITIAL_WINDOW_KIB, MUX_WINDOW_REFRESH,
};
use crate::proxy::{parse_socks5_udp_datagram, parse_target_authority, TargetAddr};
use crate::udp_batch::{UdpBatchReader, UdpBatchWriter};
use crate::ws::{
    build_server_handshake_response, encode_ws_frame, read_frame, read_frame_owned,
    read_http_headers, validate_server_handshake, write_frame, write_frame_borrowed,
};
use anyhow::{anyhow, bail, Context, Result};
use socket2::SockRef;
use std::collections::HashMap;
use std::net::{IpAddr, SocketAddr};
use std::sync::{Arc, Mutex as StdMutex, OnceLock};
use tokio::io::{AsyncReadExt, AsyncWriteExt, ReadHalf, WriteHalf};
use tokio::net::{TcpListener, TcpStream, UdpSocket};
use tokio::sync::{mpsc, watch, Mutex, RwLock, Semaphore};
use tokio::time::{timeout, Duration};

#[derive(Debug, Clone)]
pub struct RuntimeConfig {
    pub proxy_host: String,
    pub proxy_port: u16,
    pub upstream: Option<String>,
    pub key: Option<String>,
    pub fakehost: Option<String>,
    pub mux: bool,
    pub buffer_size: usize,
    pub connection_timeout: u64,
    pub allow_open: bool,
    pub max_connections: usize,
    pub block_local: bool,
    pub tcp_nodelay: bool,
    pub tcp_keepalive: bool,
    pub socket_buffer: usize,
    /// Pad MUX DATA frames with random lengths (GoWay `-obfs` parity).
    pub obfs: bool,
}
impl Default for RuntimeConfig {
    fn default() -> Self {
        Self {
            proxy_host: "127.0.0.1".into(),
            proxy_port: 9192,
            upstream: None,
            key: None,
            fakehost: None,
            mux: true,
            buffer_size: 128 * 1024,
            connection_timeout: 60,
            allow_open: false,
            max_connections: 1500,
            block_local: true,
            tcp_nodelay: true,
            tcp_keepalive: true,
            socket_buffer: 0,
            obfs: false,
        }
    }
}
#[derive(Debug)]
struct StreamEntry {
    tx: mpsc::Sender<StreamCommand>,
    cancel: watch::Sender<bool>,
    /// Server→client send credit (drained by target_to_mux, replenished
    /// by WINDOW frames from the client). Unbounded until the client's
    /// VERSION frame arrives.
    gate: Arc<CreditGate>,
}
#[derive(Debug)]
enum StreamCommand {
    Data(OwnedMuxFrame),
    Fin,
    Reset,
}
fn configured_cipher(key: &Option<String>) -> XorCipher {
    XorCipher::new(key.as_deref().unwrap_or(""))
}
fn transform_payload(cipher: &XorCipher, payload: &mut [u8]) {
    cipher.apply(payload)
}
pub(crate) fn is_blocked_local_host(host: &str) -> bool {
    let clean = host.trim().trim_matches(|c| c == '[' || c == ']');
    if clean.eq_ignore_ascii_case("localhost") {
        return true;
    }
    if let Ok(ip) = clean.parse::<IpAddr>() {
        return match ip {
            IpAddr::V4(v4) => {
                v4.is_loopback() || v4.is_private() || v4.is_link_local() || v4.is_unspecified()
            }
            IpAddr::V6(v6) => {
                if let Some(v4) = v6.to_ipv4() {
                    if v4.is_loopback()
                        || v4.is_private()
                        || v4.is_link_local()
                        || v4.is_unspecified()
                    {
                        return true;
                    }
                }
                v6.is_loopback()
                    || v6.is_unspecified()
                    || v6.is_unique_local()
                    || v6.is_unicast_link_local()
                    || ((v6.segments()[0] & 0xff00) == 0xff00 && (v6.segments()[0] & 0x000f) <= 2)
            }
        };
    }
    false
}
/// GoWay-aligned relay read size: honors `-W` up to a 12 MiB ceiling
/// (GoWay BufPool scales with the configured buffer up to 12 MiB + frame
/// overhead). The 128 KiB default is unaffected; only high `-W` values
/// (e.g. `-W 1024` + large `--socket-buffer`) now take effect instead of
/// being truncated at 1 MiB.
pub(crate) fn relay_buffer_size(requested: usize) -> usize {
    requested.clamp(16 * 1024, 12 * 1024 * 1024)
}

/// Graceful-shutdown drain budget after Ctrl+C / Ctrl+Break: stop accepting
/// new connections, then wait this long for in-flight tasks before aborting.
pub(crate) const SHUTDOWN_DRAIN_SECS: u64 = 5;

/// Relay read-buffer pool (GoWay `BufPool` parity for the reuse half).
///
/// Relay tasks hold one buffer for their whole lifetime and touch the pool
/// only twice per task (get on start, put on exit), so a single global lock
/// is uncontended. Only buffers at or below 1 MiB are pooled, bounding
/// retained memory; larger `-W` buffers fall back to allocate/free (the
/// allocator's page cache absorbs those). Buffers are always fully
/// overwritten by `read` before the used prefix is consumed, so no
/// zeroing is needed on either path.
const POOLED_BUF_MAX_SIZE: usize = 1024 * 1024;
const POOLED_BUF_MAX_COUNT: usize = 128;

static RELAY_BUFS: OnceLock<StdMutex<Vec<Vec<u8>>>> = OnceLock::new();

fn relay_bufs() -> &'static StdMutex<Vec<Vec<u8>>> {
    RELAY_BUFS.get_or_init(|| StdMutex::new(Vec::new()))
}

/// Gets a zeroed relay buffer of exactly `relay_buffer_size(requested)`
/// bytes, reusing a pooled one when available.
pub(crate) async fn relay_buf(requested: usize) -> Vec<u8> {
    let size = relay_buffer_size(requested);
    if size <= POOLED_BUF_MAX_SIZE {
        let mut pool = relay_bufs().lock().unwrap();
        if let Some(pos) = pool.iter().position(|b| b.len() == size) {
            return pool.swap_remove(pos);
        }
    }
    vec![0u8; size]
}

/// Returns a relay buffer to the pool (best-effort: over-capacity or
/// oversized buffers are simply dropped).
pub(crate) async fn recycle_buf(buf: Vec<u8>) {
    if buf.len() > POOLED_BUF_MAX_SIZE {
        return;
    }
    let mut pool = relay_bufs().lock().unwrap();
    if pool.len() < POOLED_BUF_MAX_COUNT {
        pool.push(buf);
    }
}

/// Resolves when the operator requests shutdown (Ctrl+C everywhere,
/// Ctrl+Break on Windows — the latter is what console-less training
/// drivers can deliver to a child process group).
pub(crate) async fn wait_shutdown() {
    #[cfg(windows)]
    {
        let mut brk = tokio::signal::windows::ctrl_break().expect("ctrl_break handler installed");
        tokio::select! {
            _ = tokio::signal::ctrl_c() => {}
            _ = brk.recv() => {}
        }
    }
    #[cfg(not(windows))]
    {
        let _ = tokio::signal::ctrl_c().await;
    }
}

/// Drains a `JoinSet` after the accept loop exits: waits for in-flight
/// tasks up to the drain budget, then aborts leftovers so the process can
/// exit cleanly (flushing PGO profiles, releasing ports).
pub(crate) async fn drain_join_set<T: Send + 'static>(set: &mut tokio::task::JoinSet<T>) {
    let deadline = tokio::time::sleep(Duration::from_secs(SHUTDOWN_DRAIN_SECS));
    tokio::pin!(deadline);
    loop {
        tokio::select! {
            _ = &mut deadline => {
                set.abort_all();
                break;
            }
            next = set.join_next() => {
                if next.is_none() {
                    break;
                }
            }
        }
    }
}
pub(crate) fn enforce_target_policy(cfg: &RuntimeConfig, target: &TargetAddr) -> Result<()> {
    if cfg.upstream.is_some() && cfg.block_local && is_blocked_local_host(&target.host) {
        bail!("local/LAN target blocked by client policy")
    }
    if target.port == 0 {
        bail!("target port must be non-zero")
    }
    Ok(())
}
pub(crate) fn apply_socket_options_raw(
    stream: &TcpStream,
    nodelay: bool,
    socket_buffer: usize,
    keepalive: bool,
) {
    let sock = SockRef::from(stream);
    let _ = sock.set_nodelay(nodelay);
    if socket_buffer > 0 {
        let bytes = socket_buffer.saturating_mul(1024);
        let _ = sock.set_send_buffer_size(bytes);
        let _ = sock.set_recv_buffer_size(bytes);
    }
    if keepalive {
        let ka = socket2::TcpKeepalive::new().with_time(Duration::from_secs(30));
        let _ = sock.set_tcp_keepalive(&ka);
    }
    #[cfg(target_os = "linux")]
    {
        use std::os::fd::AsRawFd;
        let fd = stream.as_raw_fd();

        // 1. TCP_NOTSENT_LOWAT bounds unsent bytes buffered in the kernel send
        // queue before poll/epoll reports writable. This prevents bufferbloat
        // on high-BDP cross-border links (Cloudflare/HTTP2 standard practice: 16 KiB).
        let lowat: libc::c_uint = 16384;
        unsafe {
            let _ = libc::setsockopt(
                fd,
                libc::IPPROTO_TCP,
                libc::TCP_NOTSENT_LOWAT,
                &lowat as *const _ as *const libc::c_void,
                std::mem::size_of_val(&lowat) as libc::socklen_t,
            );
        }

        // 2. TCP_CONGESTION: Set BBR congestion control algorithm.
        // On cross-border/WAN links with random packet loss (1%~3%), loss-based algorithms
        // like Cubic collapse throughput by repeatedly halving the congestion window,
        // whereas BBR builds a model of the network to sustain maximum throughput.
        // Best-effort: if BBR is unavailable in the kernel, setsockopt fails gracefully.
        let bbr = b"bbr\0";
        unsafe {
            let _ = libc::setsockopt(
                fd,
                libc::IPPROTO_TCP,
                libc::TCP_CONGESTION,
                bbr.as_ptr() as *const libc::c_void,
                bbr.len() as libc::socklen_t,
            );
        }

        // 3. TCP_QUICKACK: Suppress delayed ACK timer during stream setup.
        // Prevents TCP receiver from delaying ACKs up to 40ms during handshake
        // and initial protocol frame exchange.
        let quickack: libc::c_int = 1;
        unsafe {
            let _ = libc::setsockopt(
                fd,
                libc::IPPROTO_TCP,
                libc::TCP_QUICKACK,
                &quickack as *const _ as *const libc::c_void,
                std::mem::size_of_val(&quickack) as libc::socklen_t,
            );
        }

        // 4. TCP_USER_TIMEOUT (RFC 5482): Forcefully resets dead connections after
        // 30 seconds of unacknowledged transmitted data, eliminating 15-minute kernel
        // retransmission hangs during silent network disconnects.
        let user_timeout_ms: libc::c_uint = 30000;
        unsafe {
            let _ = libc::setsockopt(
                fd,
                libc::IPPROTO_TCP,
                libc::TCP_USER_TIMEOUT,
                &user_timeout_ms as *const _ as *const libc::c_void,
                std::mem::size_of_val(&user_timeout_ms) as libc::socklen_t,
            );
        }
    }
}

/// Applies kernel-level optimizations to a listening TCP socket.
///
/// On Linux:
/// - TCP_FASTOPEN (RFC 7413): Enables TCP Fast Open on server-side listeners with
///   a queue depth of 256. This allows clients supporting TFO to send SYN+data,
///   eliminating 1 RTT of latency during connection establishment.
pub(crate) fn apply_listener_options(listener: &TcpListener) {
    #[cfg(target_os = "linux")]
    {
        use std::os::fd::AsRawFd;
        let qlen: libc::c_int = 256;
        unsafe {
            let _ = libc::setsockopt(
                listener.as_raw_fd(),
                libc::IPPROTO_TCP,
                libc::TCP_FASTOPEN,
                &qlen as *const _ as *const libc::c_void,
                std::mem::size_of_val(&qlen) as libc::socklen_t,
            );
        }
    }
    let _ = listener;
}
pub(crate) fn apply_socket_options(stream: &TcpStream, cfg: &RuntimeConfig) {
    apply_socket_options_raw(stream, cfg.tcp_nodelay, cfg.socket_buffer, cfg.tcp_keepalive);
}
async fn dial_target(
    target: &TargetAddr,
    timeout_secs: u64,
    cfg: &RuntimeConfig,
) -> Result<TcpStream> {
    let stream = timeout(Duration::from_secs(timeout_secs.max(1)), async {
        let addr = resolve_socket(&target.host, target.port)
            .await
            .map_err(|e| std::io::Error::other(e.to_string()))?;
        TcpStream::connect(addr).await
    })
    .await
    .context("target connection timeout")??;
    apply_socket_options(&stream, cfg);
    Ok(stream)
}
async fn send_frame_encrypted(
    writer: &Arc<MuxFrameWriter>,
    cipher: &XorCipher,
    frame: &MuxFrame,
    obfs: bool,
) -> Result<()> {
    // Exact sizing happens inside `encode_mux_ws_frame` (one reserve);
    // start from a pooled buffer when available so the reserve is a no-op.
    let mut bytes = crate::mux_writer::acquire_encode_buf();
    crate::mux_writer::encode_mux_ws_frame(
        &mut bytes,
        frame.stream_id,
        frame.command,
        &frame.payload,
        cipher,
        false,
        obfs,
    )
    .map_err(|e| anyhow!(e.to_string()))?;
    writer.send_mux(frame.stream_id, frame.command, bytes).await
}
async fn send_mux_parts_encrypted(
    writer: &Arc<MuxFrameWriter>,
    cipher: &XorCipher,
    stream_id: u32,
    command: MuxCommand,
    payload: &[u8],
    obfs: bool,
) -> Result<()> {
    // Exact sizing happens inside `encode_mux_ws_frame` (one reserve);
    // start from a pooled buffer when available so the reserve is a no-op.
    let mut bytes = crate::mux_writer::acquire_encode_buf();
    crate::mux_writer::encode_mux_ws_frame(
        &mut bytes, stream_id, command, payload, cipher, false, obfs,
    )
    .map_err(|e| anyhow!(e.to_string()))?;
    writer.send_mux(stream_id, command, bytes).await
}
async fn send_reset_encrypted(
    writer: &Arc<MuxFrameWriter>,
    cipher: &XorCipher,
    id: u32,
    obfs: bool,
) -> Result<()> {
    send_frame_encrypted(
        writer,
        cipher,
        &MuxFrame::new(id, MuxCommand::Rst, Vec::new()).map_err(|e| anyhow!(e.to_string()))?,
        obfs,
    )
    .await
}
async fn target_to_mux(
    id: u32,
    mut target: ReadHalf<TcpStream>,
    writer: Arc<MuxFrameWriter>,
    buffer_size: usize,
    cipher: XorCipher,
    obfs: bool,
    gate: Arc<CreditGate>,
) {
    let mut buf = relay_buf(buffer_size).await;
    loop {
        match target.read(&mut buf).await {
            Ok(0) => {
                let _ = send_mux_parts_encrypted(&writer, &cipher, id, MuxCommand::Fin, &[], obfs)
                    .await;
                break;
            }
            Ok(n) => {
                crate::stats::add_bytes(0, n as i64);
                let mut off = 0;
                while off < n {
                    let end = (off + u16::MAX as usize).min(n);
                    gate.acquire(end - off).await;
                    if send_mux_parts_encrypted(
                        &writer,
                        &cipher,
                        id,
                        MuxCommand::Data,
                        &buf[off..end],
                        obfs,
                    )
                    .await
                    .is_err()
                    {
                        recycle_buf(buf).await;
                        return;
                    }
                    off = end
                }
            }
            Err(_) => {
                let _ = send_reset_encrypted(&writer, &cipher, id, obfs).await;
                break;
            }
        }
    }
    recycle_buf(buf).await;
}
#[allow(dead_code)]
async fn server_stream_task(
    id: u32,
    target: TcpStream,
    mut rx: mpsc::Receiver<StreamCommand>,
    writer: Arc<MuxFrameWriter>,
    buffer_size: usize,
    cipher: XorCipher,
    obfs: bool,
) {
    let (rd, mut wr) = tokio::io::split(target);
    let mut reader = tokio::spawn(target_to_mux(
        id,
        rd,
        writer.clone(),
        buffer_size,
        cipher.clone(),
        obfs,
        CreditGate::new(),
    ));
    let mut is_fin = false;
    while let Some(cmd) = rx.recv().await {
        match cmd {
            StreamCommand::Data(frame) => {
                if wr.write_all(frame.payload()).await.is_err() {
                    break;
                }
            }
            StreamCommand::Fin => {
                let _ = wr.shutdown().await;
                is_fin = true;
                break;
            }
            StreamCommand::Reset => break,
        }
    }
    if is_fin {
        tokio::select! {
            _ = &mut reader => {}
            cmd = rx.recv() => {
                if let Some(StreamCommand::Reset) = cmd {
                    reader.abort();
                } else {
                    let _ = reader.await;
                }
            }
        }
    } else {
        reader.abort();
    }
}
/// Accumulates consumed bytes per stream and emits a WINDOW refund once
/// the refresh threshold is reached. Refund accounting only runs after the
/// peer's VERSION arrived (un-negotiated peers never send/receive WINDOW).
async fn maybe_send_window(
    writer: &Arc<MuxFrameWriter>,
    cipher: &XorCipher,
    stream_id: u32,
    obfs: bool,
    peer_window: &StdMutex<Option<u16>>,
    refund_pending: &mut u32,
    consumed: usize,
) {
    let negotiated = peer_window.lock().unwrap().is_some();
    if !negotiated {
        *refund_pending = 0;
        return;
    }
    *refund_pending = refund_pending.saturating_add(consumed as u32);
    if (*refund_pending as usize) < MUX_WINDOW_REFRESH {
        return;
    }
    let credit = std::mem::take(refund_pending);
    let _ = send_mux_parts_encrypted(
        writer,
        cipher,
        stream_id,
        MuxCommand::Window,
        &encode_window_payload(credit),
        obfs,
    )
    .await;
}

async fn handle_mux_parts(
    mut rd: ReadHalf<TcpStream>,
    wr: WriteHalf<TcpStream>,
    cfg: RuntimeConfig,
    first_payload: Vec<u8>,
) -> Result<()> {
    let cipher = configured_cipher(&cfg.key);
    let mut hello = first_payload;
    transform_payload(&cipher, &mut hello);
    if hello != b"MUX\n" {
        bail!("invalid MUX handshake")
    }

    // Hand the write half to the dedicated writer task; all MUX frames on
    // this session go through its serialized, coalescing queue.
    let (writer, _writer_task) = MuxFrameWriter::spawn(wr);
    let mut ok = b"OK\n".to_vec();
    transform_payload(&cipher, &mut ok);
    writer
        .send(encode_ws_frame(&ok, 2, false).map_err(|e| anyhow!(e.to_string()))?)
        .await?;

    // W3 version negotiation: advertise our protocol version + initial
    // receive window right after the session handshake. Old clients skip
    // the unknown command; new clients answer with their own VERSION.
    let peer_window: Arc<StdMutex<Option<u16>>> = Arc::new(StdMutex::new(None));
    let version_payload = encode_version_payload(MUX_INITIAL_WINDOW_KIB);
    send_mux_parts_encrypted(
        &writer,
        &cipher,
        0,
        MuxCommand::Version,
        &version_payload,
        cfg.obfs,
    )
    .await?;

    // Read-mostly under concurrency (one lookup per DATA frame), so a
    // RwLock: concurrent lookups, exclusive insert/remove.
    let streams: Arc<RwLock<HashMap<u32, StreamEntry>>> = Arc::new(RwLock::new(HashMap::new()));
    let mut stream_tasks = Vec::new();
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

        transform_payload(&cipher, &mut payload);

        let frame = match MuxFrame::decode_owned(payload) {
            Ok(f) => f,
            Err(_) => continue,
        };

        match frame.command {
            MuxCommand::Syn => {
                let stream_id = frame.stream_id;

                let syn = match SynPayload::decode(frame.payload()) {
                    Ok(v) => v,
                    Err(_) => {
                        tracing::warn!(stream_id, "mux RST: bad syn payload");
                        let _ = send_reset_encrypted(&writer, &cipher, stream_id, cfg.obfs).await;
                        continue;
                    }
                };

                let target_text = match String::from_utf8(syn.target) {
                    Ok(v) => v,
                    Err(_) => {
                        tracing::warn!(stream_id, "mux RST: target not utf-8");
                        let _ = send_reset_encrypted(&writer, &cipher, stream_id, cfg.obfs).await;
                        continue;
                    }
                };

                let target = match parse_target_authority(&target_text) {
                    Ok(v) => v,
                    Err(_) => {
                        tracing::warn!(stream_id, "mux RST: bad target authority");
                        let _ = send_reset_encrypted(&writer, &cipher, stream_id, cfg.obfs).await;
                        continue;
                    }
                };

                if let Err(e) = enforce_target_policy(&cfg, &target) {
                    tracing::warn!(stream_id, error=%e, "mux RST: target policy");
                    let _ = send_reset_encrypted(&writer, &cipher, stream_id, cfg.obfs).await;
                    continue;
                }

                let (tx, mut rx) = mpsc::channel(64);
                let (cancel, cancelled) = watch::channel(false);
                let gate = CreditGate::new();
                if let Some(kib) = *peer_window.lock().unwrap() {
                    gate.enable(i64::from(kib) * 1024);
                }

                // Atomically admit and register the logical stream. This removes
                // the check-then-insert race during large concurrent SYN bursts.
                let admitted = {
                    let mut guard = streams.write().await;
                    if guard.len() >= 2048 || guard.contains_key(&stream_id) {
                        false
                    } else {
                        guard.insert(
                            stream_id,
                            StreamEntry {
                                tx: tx.clone(),
                                cancel: cancel.clone(),
                                gate: gate.clone(),
                            },
                        );
                        true
                    }
                };
                if !admitted {
                    tracing::warn!(stream_id, "mux RST: stream admission rejected (table full or duplicate id)");
                    let _ = send_reset_encrypted(&writer, &cipher, stream_id, cfg.obfs).await;
                    continue;
                }

                if !syn.initial_data.is_empty() {
                    let initial = MuxFrame::new(stream_id, MuxCommand::Data, syn.initial_data)
                        .map_err(|e| anyhow!(e.to_string()))?;

                    let mut encoded = Vec::with_capacity(7 + initial.payload.len());
                    initial
                        .encode(&mut encoded)
                        .map_err(|e| anyhow!(e.to_string()))?;
                    let owned =
                        MuxFrame::decode_owned(encoded).map_err(|e| anyhow!(e.to_string()))?;

                    tx.send(StreamCommand::Data(owned))
                        .await
                        .map_err(|_| anyhow!("stream task exited before initial data"))?;
                }

                let streams_task = streams.clone();
                let writer_task = writer.clone();
                let cipher_task = cipher.clone();
                let cfg_task = cfg.clone();
                let gate_task = gate;
                let peer_window_task = peer_window.clone();

                // Bound pre-dial buffering: a malicious/buggy client could
                // otherwise spray DATA frames while dial_target is in flight
                // and grow `pending` without limit. Limits mirror goway's
                // muxServerStreamBufferLimit (8 MiB): a single eager flow
                // (e.g. 4 MiB written right after CONNECT) must survive a
                // slow target dial instead of being reset.
                const MAX_PENDING_FRAMES: usize = 256;
                const MAX_PENDING_BYTES: usize = 8 * 1024 * 1024;
                async fn reject_pending_overflow(
                    writer: &Arc<MuxFrameWriter>,
                    cipher: &XorCipher,
                    streams: &Arc<RwLock<HashMap<u32, StreamEntry>>>,
                    stream_id: u32,
                    obfs: bool,
                ) {
                    tracing::warn!(stream_id, "mux RST: pre-dial pending overflow");
                    let _ = send_reset_encrypted(writer, cipher, stream_id, obfs).await;
                    streams.write().await.remove(&stream_id);
                }
                let task = tokio::spawn(async move {
                    let mut cancelled = cancelled;
                    let mut pending = Vec::new();
                    let mut pending_bytes: usize = 0;
                    let mut client_fin = false;

                    let target_stream = loop {
                        tokio::select! {
                            dial = dial_target(
                                &target,
                                cfg_task.connection_timeout,
                                &cfg_task,
                            ) => {
                                match dial {
                                    Ok(stream) => break stream,
                                    Err(error) => {
                                        tracing::warn!(stream_id, error=%error, "mux RST: target dial failed");
                                        let _ = send_reset_encrypted(
                                            &writer_task,
                                            &cipher_task,
                                            stream_id,
                                            cfg_task.obfs,
                                        )
                                        .await;
                                        streams_task.write().await.remove(&stream_id);
                                        return;
                                    }
                                }
                            }

                            changed = cancelled.changed() => {
                                match changed {
                                    Ok(()) if *cancelled.borrow() => {
                                        streams_task.write().await.remove(&stream_id);
                                        return;
                                    }
                                    Ok(()) => {}
                                    Err(_) => {
                                        streams_task.write().await.remove(&stream_id);
                                        return;
                                    }
                                }
                            }

                            command = rx.recv() => {
                                match command {
                                    Some(StreamCommand::Data(frame)) => {
                                        pending_bytes += frame.payload().len();
                                        if pending.len() >= MAX_PENDING_FRAMES
                                            || pending_bytes > MAX_PENDING_BYTES
                                        {
                                            reject_pending_overflow(
                                                &writer_task,
                                                &cipher_task,
                                                &streams_task,
                                                stream_id,
                                                cfg_task.obfs,
                                            )
                                            .await;
                                            return;
                                        }
                                        pending.push(frame);
                                    }
                                    Some(StreamCommand::Fin) => {
                                        client_fin = true;
                                    }
                                    Some(StreamCommand::Reset)
                                    | None => {
                                        streams_task.write().await.remove(&stream_id);
                                        return;
                                    }
                                }
                            }
                        }
                    };

                    if *cancelled.borrow() {
                        streams_task.write().await.remove(&stream_id);
                        return;
                    }

                    while let Ok(command) = rx.try_recv() {
                        match command {
                            StreamCommand::Data(frame) => {
                                pending_bytes += frame.payload().len();
                                if pending.len() >= MAX_PENDING_FRAMES
                                    || pending_bytes > MAX_PENDING_BYTES
                                {
                                    reject_pending_overflow(
                                        &writer_task,
                                        &cipher_task,
                                        &streams_task,
                                        stream_id,
                                        cfg_task.obfs,
                                    )
                                    .await;
                                    return;
                                }
                                pending.push(frame);
                            }
                            StreamCommand::Fin => client_fin = true,
                            StreamCommand::Reset => {
                                streams_task.write().await.remove(&stream_id);
                                return;
                            }
                        }
                    }
                    let (rd_target, mut wr_target) = tokio::io::split(target_stream);

                    let mut reader = tokio::spawn(target_to_mux(
                        stream_id,
                        rd_target,
                        writer_task.clone(),
                        cfg_task.buffer_size,
                        cipher_task.clone(),
                        cfg_task.obfs,
                        gate_task.clone(),
                    ));

                    // W3 receive-side refunds: credit the client's upload
                    // window once we have consumed >= 64 KiB toward the
                    // target. Only accumulated once the client negotiated.
                    let mut refund_pending: u32 = 0;

                    for frame in pending {
                        if wr_target.write_all(frame.payload()).await.is_err() {
                            reader.abort();
                            streams_task.write().await.remove(&stream_id);
                            return;
                        }
                        let len = frame.payload().len();
                        crate::stats::add_bytes(len as i64, 0);
                        maybe_send_window(
                            &writer_task,
                            &cipher_task,
                            stream_id,
                            cfg_task.obfs,
                            &peer_window_task,
                            &mut refund_pending,
                            len,
                        )
                        .await;
                    }

                    if client_fin {
                        let _ = wr_target.shutdown().await;
                    } else {
                        while let Some(command) = rx.recv().await {
                            match command {
                                StreamCommand::Data(frame) => {
                                    if wr_target.write_all(frame.payload()).await.is_err() {
                                        break;
                                    }
                                    let len = frame.payload().len();
                                    crate::stats::add_bytes(len as i64, 0);
                                    maybe_send_window(
                                        &writer_task,
                                        &cipher_task,
                                        stream_id,
                                        cfg_task.obfs,
                                        &peer_window_task,
                                        &mut refund_pending,
                                        len,
                                    )
                                    .await;
                                }
                                StreamCommand::Fin => {
                                    let _ = wr_target.shutdown().await;
                                    client_fin = true;
                                    break;
                                }
                                StreamCommand::Reset => break,
                            }
                        }
                    }

                    if client_fin {
                        tokio::select! {
                            _ = &mut reader => {}
                            cmd = rx.recv() => {
                                if let Some(StreamCommand::Reset) = cmd {
                                    reader.abort();
                                } else {
                                    let _ = reader.await;
                                }
                            }
                        }
                    } else {
                        reader.abort();
                    }

                    streams_task.write().await.remove(&stream_id);
                });

                stream_tasks.push(task);
            }

            MuxCommand::Data => {
                let id = frame.stream_id;

                if let Some(tx) = streams.read().await.get(&id).map(|s| s.tx.clone()) {
                    if tx.send(StreamCommand::Data(frame)).await.is_err() {
                        streams.write().await.remove(&id);
                    }
                }
            }

            MuxCommand::Fin => {
                if let Some(tx) = streams
                    .read()
                    .await
                    .get(&frame.stream_id)
                    .map(|s| s.tx.clone())
                {
                    let _ = tx.send(StreamCommand::Fin).await;
                }
            }

            MuxCommand::Rst => {
                if let Some((tx, cancel)) = streams
                    .read()
                    .await
                    .get(&frame.stream_id)
                    .map(|s| (s.tx.clone(), s.cancel.clone()))
                {
                    let _ = tx.send(StreamCommand::Reset).await;
                    let _ = cancel.send(true);
                }
            }

            MuxCommand::Version => {
                // W3 negotiation: first VERSION wins; enable send credit on
                // every registered stream (streams created later enable at
                // admission time).
                if let Some((version, kib)) = decode_version_payload(frame.payload()) {
                    if version >= 1 {
                        let already = {
                            let mut pw = peer_window.lock().unwrap();
                            let seen = pw.is_some();
                            if !seen {
                                *pw = Some(kib);
                            }
                            seen
                        };
                        if !already {
                            let window = i64::from(kib) * 1024;
                            for entry in streams.read().await.values() {
                                entry.gate.enable(window);
                            }
                            tracing::debug!(kib, "peer VERSION received; send window enabled");
                        }
                    }
                }
            }

            MuxCommand::Window => {
                if let Some(credit) = decode_window_payload(frame.payload()) {
                    if let Some(entry) = streams.read().await.get(&frame.stream_id) {
                        entry.gate.release(i64::from(credit));
                    }
                }
            }
        }
    }

    // Unblock any target_to_mux sender parked on credit: the session is
    // over, so remaining sends must fail at the socket, not on WINDOWs
    // that will never arrive.
    for entry in streams.read().await.values() {
        entry.gate.close();
    }

    for task in stream_tasks {
        task.abort();
    }

    streams.write().await.clear();

    Ok(())
}
/// Plain non-MUX TCP relay, mirroring goway.go `handleServer`:
/// only the target hello (`host:port\n`) and `OK\n` are XOR-encrypted,
/// all subsequent data frames are plaintext.
async fn handle_server_tcp_parts(
    mut rd: ReadHalf<TcpStream>,
    writer: Arc<Mutex<WriteHalf<TcpStream>>>,
    cfg: RuntimeConfig,
    first: Vec<u8>,
) -> Result<()> {
    let cipher = configured_cipher(&cfg.key);
    let mut target_frame = first;
    transform_payload(&cipher, &mut target_frame);
    let target_text =
        String::from_utf8(target_frame).map_err(|_| anyhow!("invalid non-MUX target UTF-8"))?;
    let target = parse_target_authority(target_text.trim()).map_err(|e| anyhow!(e.to_string()))?;
    let resolved = resolve_socket(&target.host, target.port).await?;
    let target_stream = timeout(
        Duration::from_secs(cfg.connection_timeout.max(1)),
        TcpStream::connect(resolved),
    )
    .await
    .context("target connection timeout")??;
    apply_socket_options(&target_stream, &cfg);
    let mut ok = b"OK\n".to_vec();
    transform_payload(&cipher, &mut ok);
    {
        let mut w = writer.lock().await;
        write_frame(&mut *w, &ok, 2, false).await?;
    }
    let (mut target_rd, mut target_wr) = tokio::io::split(target_stream);
    let writer_down = writer.clone();
    let buffer_size = cfg.buffer_size;
    let mut download = tokio::spawn(async move {
        let mut buf = relay_buf(buffer_size).await;
        loop {
            let n = target_rd.read(&mut buf).await?;
            if n == 0 {
                break;
            }
            crate::stats::add_bytes(0, n as i64);
            // Borrow the pooled scratch instead of `buf[..n].to_vec()`:
            // the old copy allocated a fresh Vec per read on the download
            // path (data frames are plaintext on non-MUX, so masking never
            // touches the scratch).
            let mut w = writer_down.lock().await;
            write_frame_borrowed(&mut *w, &mut buf[..n], 2, false).await?;
        }
        recycle_buf(buf).await;
        Result::<()>::Ok(())
    });
    let mut frame_buf = Vec::with_capacity(64 * 1024);
    loop {
        tokio::select! {
            _ = &mut download => break,
            res = read_frame(
                &mut rd,
                Option::<&mut WriteHalf<TcpStream>>::None,
                &mut frame_buf,
            ) => {
                let Some((opcode, payload)) = res? else {
                    break;
                };
                if opcode == 8 {
                    break;
                }
                if opcode != 2 {
                    continue;
                }
                if target_wr.write_all(&payload).await.is_err() {
                    break;
                }
                crate::stats::add_bytes(payload.len() as i64, 0);
            }
        }
    }
    let _ = target_wr.shutdown().await;
    download.abort();
    Ok(())
}
async fn resolve_udp_target(target: &TargetAddr) -> Result<SocketAddr> {
    resolve_socket(&target.host, target.port).await
}
fn udp_envelope(source: SocketAddr, payload: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(22 + payload.len());
    out.extend_from_slice(&[0, 0, 0]);
    match source.ip() {
        IpAddr::V4(ip) => {
            out.push(1);
            out.extend_from_slice(&ip.octets())
        }
        IpAddr::V6(ip) => {
            out.push(4);
            out.extend_from_slice(&ip.octets())
        }
    }
    out.extend_from_slice(&source.port().to_be_bytes());
    out.extend_from_slice(payload);
    out
}
async fn handle_server_udp_parts(
    mut rd: ReadHalf<TcpStream>,
    writer: Arc<Mutex<WriteHalf<TcpStream>>>,
    cfg: RuntimeConfig,
    first_payload: Vec<u8>,
) -> Result<()> {
    let cipher = configured_cipher(&cfg.key);
    let mut hello = first_payload;
    transform_payload(&cipher, &mut hello);
    if hello != b"UDP\n" {
        bail!("invalid UDP handshake")
    };
    let mut ok = b"OK\n".to_vec();
    transform_payload(&cipher, &mut ok);
    {
        let mut w = writer.lock().await;
        write_frame(&mut *w, &ok, 2, false).await?
    };
    let udp = Arc::new(UdpSocket::bind("0.0.0.0:0").await?);
    let udp_send = udp.clone();
    let writer_send = writer.clone();
    let cipher_send = cipher.clone();
    let send_task = tokio::spawn(async move {
        let mut batch = UdpBatchReader::new(udp_send);
        loop {
            let count = match batch.recv().await {
                Ok(n) => n,
                Err(_) => break,
            };
            for i in 0..count {
                let (pkt, source) = batch.packet(i);
                crate::stats::add_bytes(0, pkt.len() as i64);
                let mut packet = udp_envelope(source, pkt);
                cipher_send.apply(&mut packet);
                let mut w = writer_send.lock().await;
                if write_frame(&mut *w, &packet, 2, false).await.is_err() {
                    return Ok::<(), anyhow::Error>(());
                }
            }
        }
        Ok::<(), anyhow::Error>(())
    });
    let mut frame_buf = Vec::with_capacity(64 * 1024);
    let mut batch_writer = UdpBatchWriter::new(udp.clone());
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
        let (target, payload) =
            parse_socks5_udp_datagram(&packet).map_err(|e| anyhow!(e.to_string()))?;
        if cfg.block_local && is_blocked_local_host(&target.host) {
            continue;
        }
        let addr = match resolve_udp_target(&target).await {
            Ok(v) => v,
            Err(_) => continue,
        };
        crate::stats::add_bytes(payload.len() as i64, 0);
        let _ = batch_writer.send(payload, addr).await;
    }
    let _ = batch_writer.flush().await;
    send_task.abort();
    Ok(())
}

pub async fn run_server(cfg: RuntimeConfig) -> Result<()> {
    let listener = TcpListener::bind(format!("{}:{}", cfg.proxy_host, cfg.proxy_port)).await?;
    apply_listener_options(&listener);
    let semaphore = Arc::new(Semaphore::new(cfg.max_connections.max(1)));
    tracing::info!(
        "RushWay server listening on {}:{}",
        cfg.proxy_host,
        cfg.proxy_port
    );
    let mut set = tokio::task::JoinSet::new();
    loop {
        tokio::select! {
            _ = wait_shutdown() => {
                tracing::info!("shutdown requested, draining server connections");
                break;
            }
            res = listener.accept() => {
                let (stream, peer) = res?;
                // Fail fast at capacity instead of stalling the accept loop and
                // letting the TCP backlog fill up.
                let permit = match semaphore.clone().try_acquire_owned() {
                    Ok(v) => v,
                    Err(_) => {
                        tracing::debug!(%peer, "maximum server connections reached");
                        continue;
                    }
                };
                let cfg2 = cfg.clone();
                apply_socket_options(&stream, &cfg2);
                set.spawn(async move {
                    let _permit = permit;
                    let _conn = crate::stats::ConnGuard::new();
                    let result = async {
                        let (mut rd, mut wr) = tokio::io::split(stream);
                        let request = read_http_headers(&mut rd).await?;
                        let key = validate_server_handshake(&request)?;
                        wr.write_all(&build_server_handshake_response(&key)).await?;
                        wr.flush().await?;
                        let writer = Arc::new(Mutex::new(wr));
                        let mut buf = Vec::with_capacity(64 * 1024);
                        let Some((opcode, first)) =
                            read_frame(&mut rd, Option::<&mut WriteHalf<TcpStream>>::None, &mut buf)
                                .await?
                        else {
                            bail!("missing transport handshake")
                        };
                        if opcode != 2 {
                            bail!("invalid transport handshake opcode")
                        };
                        let mut plain = first.clone();
                        let cipher = configured_cipher(&cfg2.key);
                        transform_payload(&cipher, &mut plain);
                        if plain == b"UDP\n" {
                            return handle_server_udp_parts(rd, writer, cfg2, first).await;
                        }
                        if plain == b"MUX\n" {
                            let wr = Arc::try_unwrap(writer)
                                .map_err(|_| anyhow!("upstream writer unexpectedly shared"))?
                                .into_inner();
                            return handle_mux_parts(rd, wr, cfg2, first).await;
                        }
                        // Anything else is a plain non-MUX target ("host:port\n"),
                        // exactly like goway.go handleServer's fallthrough branch.
                        handle_server_tcp_parts(rd, writer, cfg2, first).await
                    }
                    .await;
                    if let Err(e) = result {
                        tracing::debug!(%peer,error=%e,"transport connection closed")
                    }
                });
            }
        }
    }
    drain_join_set(&mut set).await;
    Ok(())
}

#[cfg(test)]
mod lifecycle_tests {
    use super::*;
    use tokio::time::{timeout, Instant};

    #[tokio::test]
    async fn dialing_cancel_signal_wakes_immediately() {
        let (cancel, mut cancelled) = watch::channel(false);
        cancel.send(true).expect("receiver exists");
        let result = timeout(Duration::from_millis(100), async move {
            loop {
                match cancelled.changed().await {
                    Ok(()) if *cancelled.borrow() => break,
                    Ok(()) => {}
                    Err(_) => break,
                }
            }
        })
        .await;
        assert!(result.is_ok(), "cancellation watcher did not wake in time");
    }

    #[tokio::test]
    async fn cancellation_precedes_slow_dial() {
        let (cancel, mut cancelled) = watch::channel(false);
        cancel.send(true).expect("receiver exists");
        let start = Instant::now();
        let won = tokio::select! {
            _ = std::future::pending::<Result<TcpStream>>() => false,
            changed = cancelled.changed() => changed.is_ok() && *cancelled.borrow(),
        };
        assert!(won, "cancellation must win over a pending dial");
        assert!(start.elapsed() < Duration::from_millis(100));
    }

    #[tokio::test]
    async fn relay_buf_pool_reuses_allocations() {
        let a = relay_buf(128 * 1024).await;
        assert_eq!(a.len(), 128 * 1024);
        let ptr = a.as_ptr();
        recycle_buf(a).await;
        let b = relay_buf(128 * 1024).await;
        assert_eq!(b.as_ptr(), ptr);
        // Oversized buffers bypass the pool.
        let big = relay_buf(4 * 1024 * 1024).await;
        assert_eq!(big.len(), 4 * 1024 * 1024);
        recycle_buf(big).await;
        recycle_buf(b).await;
        let c = relay_buf(128 * 1024).await;
        assert_eq!(c.as_ptr(), ptr);
    }

    #[test]
    fn relay_buffer_size_honors_goway_ceiling() {
        // Defaults and small values pass through; high -W values that used
        // to be truncated at 1 MiB now scale toward the 12 MiB ceiling.
        assert_eq!(relay_buffer_size(128 * 1024), 128 * 1024);
        assert_eq!(relay_buffer_size(0), 16 * 1024);
        assert_eq!(relay_buffer_size(1024 * 1024), 1024 * 1024);
        assert_eq!(relay_buffer_size(4 * 1024 * 1024), 4 * 1024 * 1024);
        assert_eq!(relay_buffer_size(12 * 1024 * 1024), 12 * 1024 * 1024);
        assert_eq!(relay_buffer_size(64 * 1024 * 1024), 12 * 1024 * 1024);
    }

    #[test]
    fn test_is_blocked_local_host_all_variants() {
        let locals = [
            "localhost",
            "LocalHost",
            "127.0.0.1",
            "127.0.0.100",
            "::1",
            "[::1]",
            "0.0.0.0",
            "::",
            "10.0.0.1",
            "10.254.1.1",
            "172.16.0.1",
            "172.31.255.254",
            "192.168.0.1",
            "192.168.100.200",
            "169.254.1.1",
            "fe80::1",
            "fc00::1",
            "fd00::1",
            "::ffff:127.0.0.1",
            "::ffff:192.168.1.1",
        ];
        for a in locals {
            assert!(
                is_blocked_local_host(a),
                "is_blocked_local_host({a}) should be true"
            );
        }

        let non_locals = [
            "1.1.1.1",
            "8.8.8.8",
            "172.15.255.255",
            "172.32.0.1",
            "203.0.113.1",
            "google.com",
            "example.com",
            "::ffff:8.8.8.8",
        ];
        for a in non_locals {
            assert!(
                !is_blocked_local_host(a),
                "is_blocked_local_host({a}) should be false"
            );
        }
    }

    #[tokio::test]
    async fn test_apply_socket_and_listener_options() {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        apply_listener_options(&listener);
        let addr = listener.local_addr().unwrap();
        let client_task = tokio::spawn(async move {
            let stream = TcpStream::connect(addr).await.unwrap();
            apply_socket_options_raw(&stream, true, 64, true);
            stream
        });
        let (server_stream, _) = listener.accept().await.unwrap();
        apply_socket_options_raw(&server_stream, true, 64, true);
        let client_stream = client_task.await.unwrap();
        drop(client_stream);
        drop(server_stream);
    }
}
