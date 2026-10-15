// RushWay - Single-file amalgamation

pub mod common {
//! Helpers shared verbatim by the transport paths (WS/WSS/MUX/UDP relay).
//!
//! Only code that is byte-for-byte identical across call sites lives here;
//! call-site specifics (error texts, default-port predicates, log prefixes)
//! stay with the callers and are passed in as arguments when they are the
//! only difference.

use crate::crypto::XorCipher;
use crate::dns::resolve_all_ipv4;
use crate::mux_writer::MuxFrameWriter;
use crate::protocol::MuxCommand;
use crate::proxy::{socks5_success_response, TargetAddr};
use crate::runtime::{recycle_buf, relay_buf};
use crate::ws::{read_frame, write_frame, write_frame_borrowed};
use anyhow::{anyhow, bail, Result};
use std::future::Future;
use std::net::{IpAddr, SocketAddr};
use std::pin::Pin;
use std::sync::Arc;
use std::time::Instant;
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};
use tokio::net::TcpStream;
use tokio::sync::Mutex;
use tokio::time::{timeout, Duration};

/// Session cipher from the configured `-k` key; an absent key maps to the
/// empty-key cipher, exactly like the inline helpers it replaces.
pub(crate) fn configured_cipher(key: &Option<String>) -> XorCipher {
    XorCipher::new(key.as_deref().unwrap_or(""))
}

/// HTTP CONNECT responses handed to the local SOCKS5/HTTP proxy client.
pub(crate) const HTTP_200_CONNECTION_ESTABLISHED: &[u8] =
    b"HTTP/1.1 200 Connection Established\r\n\r\n";
pub(crate) const HTTP_502_BAD_GATEWAY: &[u8] =
    b"HTTP/1.1 502 Bad Gateway\r\nConnection: close\r\nContent-Length: 0\r\n\r\n";

/// Builds the 10-byte SOCKS5 UDP ASSOCIATE reply advertising `bound`.
/// A non-IPv4 bind address leaves BND.ADDR zeroed, as before.
pub(crate) fn socks5_udp_associate_reply(bound: SocketAddr) -> [u8; 10] {
    let mut resp = [0u8; 10];
    resp[0] = 5;
    resp[1] = 0;
    resp[2] = 0;
    resp[3] = 1;
    if let IpAddr::V4(ip) = bound.ip() {
        resp[4..8].copy_from_slice(&ip.octets())
    }
    resp[8..10].copy_from_slice(&bound.port().to_be_bytes());
    resp
}

/// Shared `ws://` upstream core: strips the scheme (failing with
/// `scheme_error` otherwise) and splits the authority from the path,
/// defaulting the path to `/`. Port-default predicates differ between call
/// sites and stay with them.
pub(crate) fn split_ws_url(input: &str, scheme_error: &str) -> Result<(String, String)> {
    let rest = input
        .strip_prefix("ws://")
        .ok_or_else(|| anyhow!("{scheme_error}"))?;
    let (authority, path) = match rest.split_once('/') {
        Some((a, p)) => (a.to_string(), format!("/{}", p)),
        None => (rest.to_string(), "/".to_string()),
    };
    Ok((authority, path))
}

/// [`split_ws_url`] plus the port default shared by the pooled MUX client
/// and the UDP relay: an authority without a port gets `:80`.
pub(crate) fn parse_ws_url_with_port(input: &str, scheme_error: &str) -> Result<(String, String)> {
    let (authority, path) = split_ws_url(input, scheme_error)?;
    let authority = if authority.contains(':') {
        authority
    } else {
        format!("{}:80", authority)
    };
    Ok((authority, path))
}

/// Cloudflare edge fallback shared by the plain-WS dials: resolves `sni`
/// to IPv4 edges, skips the primary IP, and returns the first TCP
/// connection that completes within `conn_timeout`. `Ok(None)` means every
/// edge failed; DNS failures propagate to the caller.
pub(crate) async fn try_cloudflare_edges(
    sni: &str,
    primary_ip: IpAddr,
    target_port: u16,
    conn_timeout: Duration,
) -> Result<Option<TcpStream>> {
    for edge in resolve_all_ipv4(sni).await? {
        let candidate = SocketAddr::new(IpAddr::V4(edge), target_port);
        if candidate.ip() == primary_ip {
            continue;
        }
        tracing::info!(
            "[DNS] Trying fallback Cloudflare edge: {} (fakehost: {})",
            candidate,
            sni
        );
        match timeout(conn_timeout, TcpStream::connect(candidate)).await {
            Ok(Ok(socket)) => return Ok(Some(socket)),
            Ok(Err(e)) => {
                tracing::debug!(%candidate, error=%e, "[WS] Fallback edge dial failed")
            }
            Err(_) => tracing::debug!(%candidate, "[WS] Fallback edge dial timed out"),
        }
    }
    Ok(None)
}

/// Encodes one MUX frame into a pooled scratch buffer and queues it on the
/// session writer. Client sessions mask WebSocket frames; the server-side
/// runtime keeps its own unmasked variants.
pub(crate) async fn send_mux_parts_reuse(
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
    writer.send_mux(stream_id, command, std::mem::take(scratch)).await
}

/// One-shot variant of [`send_mux_parts_reuse`] that starts from the encode
/// pool so the encoder's single exact `reserve` is a no-op on a warm pool.
pub(crate) async fn send_mux_parts(
    writer: &Arc<MuxFrameWriter>,
    cipher: &XorCipher,
    stream_id: u32,
    command: MuxCommand,
    payload: &[u8],
    obfs: bool,
) -> Result<()> {
    let mut bytes = crate::mux_writer::acquire_encode_buf();
    send_mux_parts_reuse(
        writer, cipher, stream_id, command, payload, &mut bytes, obfs,
    )
    .await
}

/// Pre-warmed non-MUX upstream pool, mirroring GoWay `ConnPool`.
///
/// Each pooled transport has completed its dial — TCP, plus TLS and the
/// WebSocket upgrade where applicable — but has NOT sent the target frame
/// yet. Transports are single-use: a grabbed transport is relayed once and
/// then closed, while a background task refills the pool (GoWay parity: the
/// pool is a pre-warmed dial cache, not a reuse pool).
pub(crate) struct PooledUpstream<R, W> {
    rd: R,
    wr: W,
    created: Instant,
    last_used: Instant,
}

pub(crate) const NON_MUX_POOL_SIZE: usize = 32;
pub(crate) const NON_MUX_MAX_AGE: Duration = Duration::from_secs(5 * 60);
pub(crate) const NON_MUX_IDLE_TIMEOUT: Duration = Duration::from_secs(30);
pub(crate) const NON_MUX_MAINTAIN_INTERVAL: Duration = Duration::from_millis(500);

pub(crate) fn pooled_usable(created: Instant, last_used: Instant, now: Instant) -> bool {
    now.duration_since(created) <= NON_MUX_MAX_AGE
        && now.duration_since(last_used) <= NON_MUX_IDLE_TIMEOUT
}

/// Dials one pre-warmed upstream for [`NonMuxPool`]. The transport-specific
/// openers are plugged in as boxed futures: dialing happens only on pool
/// refill or on a pool miss, so the boxing never touches the relay hot path.
pub(crate) type NonMuxDial<C, R, W> = Box<
    dyn for<'a> Fn(&'a C) -> Pin<Box<dyn Future<Output = Result<(R, W)>> + Send + 'a>>
        + Send
        + Sync,
>;

pub(crate) struct NonMuxPool<C, R, W> {
    cfg: C,
    dial: NonMuxDial<C, R, W>,
    /// Call-site log namespace (`"[non-MUX]"` / `"[WSS non-MUX]"`).
    log_tag: &'static str,
    conns: Mutex<Vec<PooledUpstream<R, W>>>,
    creation: Mutex<()>,
}

impl<C, R, W> NonMuxPool<C, R, W> {
    pub(crate) fn new(cfg: C, dial: NonMuxDial<C, R, W>, log_tag: &'static str) -> Arc<Self> {
        Arc::new(Self {
            cfg,
            dial,
            log_tag,
            conns: Mutex::new(Vec::new()),
            creation: Mutex::new(()),
        })
    }

    async fn dial_pooled(&self) -> Result<PooledUpstream<R, W>> {
        let (rd, wr) = (self.dial)(&self.cfg).await?;
        let now = Instant::now();
        Ok(PooledUpstream {
            rd,
            wr,
            created: now,
            last_used: now,
        })
    }

    /// Pops the freshest usable pre-warmed transport, or dials a fresh one
    /// when the pool is empty (all failures propagate to the caller).
    pub(crate) async fn get_or_dial(self: &Arc<Self>) -> Result<(R, W)> {
        let pooled = {
            let mut conns = self.conns.lock().await;
            let now = Instant::now();
            conns.retain(|c| pooled_usable(c.created, c.last_used, now));
            conns.pop()
        };
        if let Some(mut c) = pooled {
            c.last_used = Instant::now();
            tracing::debug!("{} using pre-warmed upstream connection", self.log_tag);
            return Ok((c.rd, c.wr));
        }
        let c = self.dial_pooled().await?;
        Ok((c.rd, c.wr))
    }

    async fn replenish(self: &Arc<Self>) {
        let _guard = self.creation.lock().await;
        loop {
            let need = {
                let mut conns = self.conns.lock().await;
                conns.retain(|c| pooled_usable(c.created, c.last_used, Instant::now()));
                NON_MUX_POOL_SIZE.saturating_sub(conns.len())
            };
            if need == 0 {
                return;
            }
            match self.dial_pooled().await {
                Ok(c) => self.conns.lock().await.push(c),
                // GoWay parity: stop refilling for this tick on first
                // failure instead of hammering a downed upstream.
                Err(error) => {
                    tracing::debug!(
                        error=%error,
                        "{} pre-warm dial failed; will retry next tick",
                        self.log_tag
                    );
                    return;
                }
            }
        }
    }

    pub(crate) async fn maintain(self: Arc<Self>) {
        loop {
            self.replenish().await;
            tokio::time::sleep(NON_MUX_MAINTAIN_INTERVAL).await;
        }
    }
}

/// How a non-MUX relay reacts when the upstream's OK line does not match.
#[derive(Clone, Copy)]
pub(crate) enum NonMuxOkRejection {
    /// Nothing has been written to the local client yet: propagate the
    /// upstream failure to the caller (plain-WS path).
    Propagate(&'static str),
    /// Answer the local SOCKS5/HTTP client with a failure response, shut its
    /// connection down and report success (WSS path). The SOCKS5 reply bytes
    /// are call-site specific; HTTP always gets [`HTTP_502_BAD_GATEWAY`].
    RespondLocal { socks5_failure: [u8; 10] },
}

/// Call-site specifics of [`relay_non_mux_ws`]: error texts plus the
/// rejection handling that differs between the plain-WS and WSS relays.
pub(crate) struct NonMuxRelaySpec {
    /// Error when the upstream closes before answering the target frame.
    pub(crate) closed_error: &'static str,
    /// Error when the OK frame's opcode is not binary.
    pub(crate) bad_opcode_error: &'static str,
    /// Reaction to an upstream rejection of the target frame.
    pub(crate) ok_rejection: NonMuxOkRejection,
}

/// Shared non-MUX client relay: XOR-encrypted target hello and OK handshake
/// over WebSocket frames, then a plaintext byte relay (GoWay non-MUX TCP).
pub(crate) async fn relay_non_mux_ws<R, W>(
    mut local: TcpStream,
    target: &TargetAddr,
    is_socks5: bool,
    is_connect: bool,
    initial_payload: Option<Vec<u8>>,
    key: &Option<String>,
    buffer_size: usize,
    mut rd: R,
    mut wr: W,
    spec: NonMuxRelaySpec,
) -> Result<()>
where
    R: AsyncRead + Unpin,
    W: AsyncWrite + Unpin + Send + 'static,
{
    let c = configured_cipher(key);
    let mut hello = format!("{}:{}\n", target.host, target.port).into_bytes();
    c.apply(&mut hello);
    write_frame(&mut wr, &hello, 2, true).await?;
    let mut frame_buf = crate::mux_writer::acquire_encode_buf();
    let Some((opcode, mut ok)) = read_frame(&mut rd, Option::<&mut W>::None, &mut frame_buf).await?
    else {
        crate::mux_writer::recycle_encode_buf(frame_buf);
        bail!(spec.closed_error)
    };
    if opcode != 2 {
        crate::mux_writer::recycle_encode_buf(ok);
        crate::mux_writer::recycle_encode_buf(frame_buf);
        bail!(spec.bad_opcode_error)
    };
    c.apply(&mut ok);
    let is_ok = ok == b"OK\n";
    crate::mux_writer::recycle_encode_buf(ok);
    if !is_ok {
        crate::mux_writer::recycle_encode_buf(frame_buf);
        match spec.ok_rejection {
            NonMuxOkRejection::Propagate(error) => bail!(error),
            NonMuxOkRejection::RespondLocal { socks5_failure } => {
                if is_socks5 {
                    local.write_all(&socks5_failure).await?
                } else {
                    local.write_all(HTTP_502_BAD_GATEWAY).await?
                }
                local.shutdown().await.ok();
                return Ok(());
            }
        }
    };
    if is_socks5 {
        local.write_all(&socks5_success_response()).await?
    } else if is_connect {
        local.write_all(HTTP_200_CONNECTION_ESTABLISHED).await?
    };
    let (mut local_rd, mut local_wr) = local.into_split();
    // GoWay v1.8.5 non-MUX TCP: only the target handshake ("host:port\n")
    // and "OK\n" are XOR-encrypted. All subsequent data frames are plaintext.
    if let Some(initial) = initial_payload {
        write_frame(&mut wr, &initial, 2, true).await?;
    }
    let mut upload = tokio::spawn(async move {
        let mut buf = relay_buf(buffer_size).await;
        loop {
            let n = local_rd.read(&mut buf).await?;
            if n == 0 {
                break;
            }
            crate::stats::add_bytes(n as i64, 0);
            write_frame_borrowed(&mut wr, &mut buf[..n], 2, true).await?
        }
        recycle_buf(buf).await;
        Result::<()>::Ok(())
    });
    tokio::select! {
        _ = &mut upload => {}
        _ = async {
            loop {
                let Some((opcode, payload)) =
                    read_frame(&mut rd, Option::<&mut W>::None, &mut frame_buf).await?
                else {
                    break;
                };
                match opcode {
                    2 => {
                        // Non-MUX data frames are plaintext per GoWay server.
                        let len = payload.len();
                        let write_res = local_wr.write_all(&payload).await;
                        crate::mux_writer::recycle_encode_buf(payload);
                        if write_res.is_err() {
                            break;
                        }
                        crate::stats::add_bytes(0, len as i64);
                    }
                    8 => {
                        crate::mux_writer::recycle_encode_buf(payload);
                        break;
                    }
                    _ => {
                        crate::mux_writer::recycle_encode_buf(payload);
                    }
                }
            }
            Result::<()>::Ok(())
        } => {}
    }
    crate::mux_writer::recycle_encode_buf(frame_buf);
    upload.abort();
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pooled_transports_expire_by_age_and_idleness() {
        let now = Instant::now();
        // Fresh transport is usable.
        assert!(pooled_usable(now, now, now));
        // Idle past 30 s is reaped even when young.
        assert!(!pooled_usable(
            now,
            now - NON_MUX_IDLE_TIMEOUT - Duration::from_secs(1),
            now
        ));
        // Old transport is reaped even when recently used.
        assert!(!pooled_usable(
            now - NON_MUX_MAX_AGE - Duration::from_secs(1),
            now,
            now
        ));
    }
}
} // mod common

pub mod crypto {
use ring::digest::{digest, SHA256};
use std::sync::Arc;

/// GoWay v1.8.4-compatible XOR transform.
/// Each TransformInPlace call starts at key offset zero. The SHA-256 digest
/// of the configured key is repeated to 256 KiB and applied to the buffer.
pub const XOR_KEY_SIZE: usize = 256 * 1024;

#[derive(Clone)]
pub struct XorCipher {
    /// Shared keystream. `Arc<[u8]>` keeps `Clone` O(1): the expansion is
    /// built once per `XorCipher::new` and never mutated afterwards, so
    /// handing a cipher to a task or session no longer copies 256 KiB.
    key: Arc<[u8]>,
}

impl XorCipher {
    pub fn new(key: &str) -> Self {
        if key.is_empty() {
            return Self { key: Vec::new().into() };
        }
        let digest = digest(&SHA256, key.as_bytes());
        let digest = digest.as_ref();
        let mut expanded = Vec::with_capacity(XOR_KEY_SIZE);
        while expanded.len() < XOR_KEY_SIZE {
            let take = (XOR_KEY_SIZE - expanded.len()).min(digest.len());
            expanded.extend_from_slice(&digest[..take]);
        }
        Self { key: expanded.into() }
    }

    #[inline]
    pub fn is_enabled(&self) -> bool {
        !self.key.is_empty()
    }

    /// Raw repeated keystream (`key[j]` == stream byte for data offset `j`).
    /// Exposed for fused single-pass transforms (e.g. cipher + WS mask in
    /// one loop); same-crate use only.
    pub(crate) fn keystream(&self) -> &[u8] {
        &self.key
    }

    /// Applies one GoWay TransformInPlace operation. Offset is reset to zero
    /// for every call, matching the v1.8.4 implementation.
    ///
    /// Runtime dynamic dispatch: detects CPU capabilities at runtime.
    /// - AVX2 path: 256-bit YMM registers, 4-way unrolled (128 bytes/iter).
    /// - SSE2 path: 128-bit XMM registers, 4-way unrolled (64 bytes/iter).
    /// - Fallback: 32-byte chunks and scalar tail.
    pub fn apply(&self, data: &mut [u8]) {
        if self.key.is_empty() || data.is_empty() {
            return;
        }
        let key: &[u8] = &self.key;
        #[cfg(target_arch = "x86_64")]
        {
            if has_avx2() {
                for chunk in data.chunks_mut(XOR_KEY_SIZE) {
                    let clen = chunk.len();
                    unsafe {
                        apply_chunk_avx2(chunk, &key[..clen]);
                    }
                }
                return;
            } else {
                for chunk in data.chunks_mut(XOR_KEY_SIZE) {
                    let clen = chunk.len();
                    unsafe {
                        apply_chunk_sse2(chunk, &key[..clen]);
                    }
                }
                return;
            }
        }
        #[cfg(target_arch = "aarch64")]
        {
            for chunk in data.chunks_mut(XOR_KEY_SIZE) {
                let clen = chunk.len();
                unsafe {
                    apply_chunk_neon(chunk, &key[..clen]);
                }
            }
            return;
        }
        #[cfg(not(any(target_arch = "x86_64", target_arch = "aarch64")))]
        {
            for chunk in data.chunks_mut(XOR_KEY_SIZE) {
                let clen = chunk.len();
                apply_chunk_fallback(chunk, &key[..clen]);
            }
        }
    }
}

#[inline(always)]
pub(crate) fn has_avx2() -> bool {
    #[cfg(target_arch = "x86_64")]
    {
        is_x86_feature_detected!("avx2")
    }
    #[cfg(not(target_arch = "x86_64"))]
    {
        false
    }
}

#[cfg(target_arch = "x86_64")]
#[target_feature(enable = "avx2")]
/// # Safety
/// The caller must guarantee the CPU supports the matching SIMD extension
/// (AVX2 / SSE2 / NEON). Every call site dispatches through runtime feature
/// detection (`has_avx2()`) or a compile-time `#[cfg(target_arch)]` gate.
/// `chunk` and `key` are ordinary borrowed slices, so all pointer arithmetic
/// stays within their bounds.
pub(crate) unsafe fn apply_chunk_avx2(chunk: &mut [u8], key: &[u8]) {
    use std::arch::x86_64::*;
    let len = chunk.len();
    debug_assert!(key.len() >= len);
    let mut i = 0;
    while i + 128 <= len {
        let k0 = _mm256_loadu_si256(key.as_ptr().add(i) as *const __m256i);
        let k1 = _mm256_loadu_si256(key.as_ptr().add(i + 32) as *const __m256i);
        let k2 = _mm256_loadu_si256(key.as_ptr().add(i + 64) as *const __m256i);
        let k3 = _mm256_loadu_si256(key.as_ptr().add(i + 96) as *const __m256i);

        let d0 = _mm256_loadu_si256(chunk.as_ptr().add(i) as *const __m256i);
        let d1 = _mm256_loadu_si256(chunk.as_ptr().add(i + 32) as *const __m256i);
        let d2 = _mm256_loadu_si256(chunk.as_ptr().add(i + 64) as *const __m256i);
        let d3 = _mm256_loadu_si256(chunk.as_ptr().add(i + 96) as *const __m256i);

        _mm256_storeu_si256(chunk.as_mut_ptr().add(i) as *mut __m256i, _mm256_xor_si256(d0, k0));
        _mm256_storeu_si256(chunk.as_mut_ptr().add(i + 32) as *mut __m256i, _mm256_xor_si256(d1, k1));
        _mm256_storeu_si256(chunk.as_mut_ptr().add(i + 64) as *mut __m256i, _mm256_xor_si256(d2, k2));
        _mm256_storeu_si256(chunk.as_mut_ptr().add(i + 96) as *mut __m256i, _mm256_xor_si256(d3, k3));
        i += 128;
    }
    while i + 32 <= len {
        let k = _mm256_loadu_si256(key.as_ptr().add(i) as *const __m256i);
        let d = _mm256_loadu_si256(chunk.as_ptr().add(i) as *const __m256i);
        _mm256_storeu_si256(chunk.as_mut_ptr().add(i) as *mut __m256i, _mm256_xor_si256(d, k));
        i += 32;
    }
    while i + 16 <= len {
        let k = _mm_loadu_si128(key.as_ptr().add(i) as *const __m128i);
        let d = _mm_loadu_si128(chunk.as_ptr().add(i) as *const __m128i);
        _mm_storeu_si128(chunk.as_mut_ptr().add(i) as *mut __m128i, _mm_xor_si128(d, k));
        i += 16;
    }
    while i + 8 <= len {
        let k = (key.as_ptr().add(i) as *const u64).read_unaligned();
        let d = (chunk.as_ptr().add(i) as *const u64).read_unaligned();
        (chunk.as_mut_ptr().add(i) as *mut u64).write_unaligned(d ^ k);
        i += 8;
    }
    while i < len {
        *chunk.get_unchecked_mut(i) ^= *key.get_unchecked(i);
        i += 1;
    }
}

#[cfg(target_arch = "x86_64")]
#[target_feature(enable = "sse2")]
/// # Safety
/// The caller must guarantee the CPU supports the matching SIMD extension
/// (AVX2 / SSE2 / NEON). Every call site dispatches through runtime feature
/// detection (`has_avx2()`) or a compile-time `#[cfg(target_arch)]` gate.
/// `chunk` and `key` are ordinary borrowed slices, so all pointer arithmetic
/// stays within their bounds.
pub(crate) unsafe fn apply_chunk_sse2(chunk: &mut [u8], key: &[u8]) {
    use std::arch::x86_64::*;
    let len = chunk.len();
    debug_assert!(key.len() >= len);
    let mut i = 0;
    while i + 64 <= len {
        let k0 = _mm_loadu_si128(key.as_ptr().add(i) as *const __m128i);
        let k1 = _mm_loadu_si128(key.as_ptr().add(i + 16) as *const __m128i);
        let k2 = _mm_loadu_si128(key.as_ptr().add(i + 32) as *const __m128i);
        let k3 = _mm_loadu_si128(key.as_ptr().add(i + 48) as *const __m128i);

        let d0 = _mm_loadu_si128(chunk.as_ptr().add(i) as *const __m128i);
        let d1 = _mm_loadu_si128(chunk.as_ptr().add(i + 16) as *const __m128i);
        let d2 = _mm_loadu_si128(chunk.as_ptr().add(i + 32) as *const __m128i);
        let d3 = _mm_loadu_si128(chunk.as_ptr().add(i + 48) as *const __m128i);

        _mm_storeu_si128(chunk.as_mut_ptr().add(i) as *mut __m128i, _mm_xor_si128(d0, k0));
        _mm_storeu_si128(chunk.as_mut_ptr().add(i + 16) as *mut __m128i, _mm_xor_si128(d1, k1));
        _mm_storeu_si128(chunk.as_mut_ptr().add(i + 32) as *mut __m128i, _mm_xor_si128(d2, k2));
        _mm_storeu_si128(chunk.as_mut_ptr().add(i + 48) as *mut __m128i, _mm_xor_si128(d3, k3));
        i += 64;
    }
    while i + 16 <= len {
        let k = _mm_loadu_si128(key.as_ptr().add(i) as *const __m128i);
        let d = _mm_loadu_si128(chunk.as_ptr().add(i) as *const __m128i);
        _mm_storeu_si128(chunk.as_mut_ptr().add(i) as *mut __m128i, _mm_xor_si128(d, k));
        i += 16;
    }
    while i + 8 <= len {
        let k = (key.as_ptr().add(i) as *const u64).read_unaligned();
        let d = (chunk.as_ptr().add(i) as *const u64).read_unaligned();
        (chunk.as_mut_ptr().add(i) as *mut u64).write_unaligned(d ^ k);
        i += 8;
    }
    while i < len {
        *chunk.get_unchecked_mut(i) ^= *key.get_unchecked(i);
        i += 1;
    }
}

#[cfg(target_arch = "aarch64")]
/// # Safety
/// The caller must guarantee the CPU supports the matching SIMD extension
/// (AVX2 / SSE2 / NEON). Every call site dispatches through runtime feature
/// detection (`has_avx2()`) or a compile-time `#[cfg(target_arch)]` gate.
/// `chunk` and `key` are ordinary borrowed slices, so all pointer arithmetic
/// stays within their bounds.
pub(crate) unsafe fn apply_chunk_neon(chunk: &mut [u8], key: &[u8]) {
    use std::arch::aarch64::*;
    let len = chunk.len();
    debug_assert!(key.len() >= len);
    let mut i = 0;
    while i + 64 <= len {
        let k0 = vld1q_u8(key.as_ptr().add(i));
        let k1 = vld1q_u8(key.as_ptr().add(i + 16));
        let k2 = vld1q_u8(key.as_ptr().add(i + 32));
        let k3 = vld1q_u8(key.as_ptr().add(i + 48));

        let d0 = vld1q_u8(chunk.as_ptr().add(i));
        let d1 = vld1q_u8(chunk.as_ptr().add(i + 16));
        let d2 = vld1q_u8(chunk.as_ptr().add(i + 32));
        let d3 = vld1q_u8(chunk.as_ptr().add(i + 48));

        vst1q_u8(chunk.as_mut_ptr().add(i), veorq_u8(d0, k0));
        vst1q_u8(chunk.as_mut_ptr().add(i + 16), veorq_u8(d1, k1));
        vst1q_u8(chunk.as_mut_ptr().add(i + 32), veorq_u8(d2, k2));
        vst1q_u8(chunk.as_mut_ptr().add(i + 48), veorq_u8(d3, k3));
        i += 64;
    }
    while i + 16 <= len {
        let k = vld1q_u8(key.as_ptr().add(i));
        let d = vld1q_u8(chunk.as_ptr().add(i));
        vst1q_u8(chunk.as_mut_ptr().add(i), veorq_u8(d, k));
        i += 16;
    }
    while i + 8 <= len {
        let k = (key.as_ptr().add(i) as *const u64).read_unaligned();
        let d = (chunk.as_ptr().add(i) as *const u64).read_unaligned();
        (chunk.as_mut_ptr().add(i) as *mut u64).write_unaligned(d ^ k);
        i += 8;
    }
    while i < len {
        *chunk.get_unchecked_mut(i) ^= *key.get_unchecked(i);
        i += 1;
    }
}

pub(crate) fn apply_chunk_fallback(chunk: &mut [u8], key: &[u8]) {
    let clen = chunk.len();
    let key = &key[..clen];
    let (dst_words, dst_tail) = chunk.as_chunks_mut::<8>();
    let (src_words, src_tail) = key.as_chunks::<8>();
    for (d, s) in dst_words.iter_mut().zip(src_words) {
        let dw = u64::from_ne_bytes(*d);
        let sw = u64::from_ne_bytes(*s);
        *d = (dw ^ sw).to_ne_bytes();
    }
    for (d, s) in dst_tail.iter_mut().zip(src_tail) {
        *d ^= *s;
    }
}

/// Applies fused keystream XOR + WebSocket 4-byte mask in a single pass.
/// Mathematically identical to `cipher.apply(data)` followed by `apply_ws_mask(data, key)`.
pub(crate) fn apply_fused_xor(region: &mut [u8], ks: &[u8], key: [u8; 4]) {
    if region.is_empty() || ks.is_empty() {
        return;
    }
    // The 4-byte WebSocket mask phase restarts at every keystream chunk, so
    // correctness silently depends on the keystream length being a multiple
    // of 4. Catch a bad `XOR_KEY_SIZE` here instead of shipping corrupt frames.
    debug_assert!(
        ks.len().is_multiple_of(4),
        "fused XOR requires ks.len() % 4 == 0, got {}",
        ks.len()
    );
    #[cfg(target_arch = "x86_64")]
    {
        if has_avx2() {
            for chunk in region.chunks_mut(ks.len()) {
                unsafe { apply_fused_xor_avx2(chunk, ks, key) };
            }
        } else {
            for chunk in region.chunks_mut(ks.len()) {
                unsafe { apply_fused_xor_sse2(chunk, ks, key) };
            }
        }
        return;
    }
    #[cfg(target_arch = "aarch64")]
    {
        for chunk in region.chunks_mut(ks.len()) {
            unsafe { apply_fused_xor_neon(chunk, ks, key) };
        }
        return;
    }
    #[cfg(not(any(target_arch = "x86_64", target_arch = "aarch64")))]
    {
        for chunk in region.chunks_mut(ks.len()) {
            apply_fused_xor_fallback(chunk, ks, key);
        }
    }
}

#[cfg(target_arch = "x86_64")]
#[target_feature(enable = "avx2")]
/// # Safety
/// The caller must guarantee the CPU supports the matching SIMD extension
/// (AVX2 / SSE2 / NEON). Every call site dispatches through runtime feature
/// detection (`has_avx2()`) or a compile-time `#[cfg(target_arch)]` gate.
/// `region` and `ks` are ordinary borrowed slices, so all pointer arithmetic
/// stays within their bounds. `ks.len()` is a multiple of 4 (debug-asserted
/// in `apply_fused_xor`), keeping the 4-byte mask phase aligned per chunk.
pub(crate) unsafe fn apply_fused_xor_avx2(region: &mut [u8], ks: &[u8], key: [u8; 4]) {
    use std::arch::x86_64::*;
    let len = region.len().min(ks.len());
    let k32 = u32::from_ne_bytes(key) as i32;
    let k_vec = _mm256_set1_epi32(k32);
    let mut i = 0;
    while i + 128 <= len {
        let k0 = _mm256_loadu_si256(ks.as_ptr().add(i) as *const __m256i);
        let k1 = _mm256_loadu_si256(ks.as_ptr().add(i + 32) as *const __m256i);
        let k2 = _mm256_loadu_si256(ks.as_ptr().add(i + 64) as *const __m256i);
        let k3 = _mm256_loadu_si256(ks.as_ptr().add(i + 96) as *const __m256i);

        let d0 = _mm256_loadu_si256(region.as_ptr().add(i) as *const __m256i);
        let d1 = _mm256_loadu_si256(region.as_ptr().add(i + 32) as *const __m256i);
        let d2 = _mm256_loadu_si256(region.as_ptr().add(i + 64) as *const __m256i);
        let d3 = _mm256_loadu_si256(region.as_ptr().add(i + 96) as *const __m256i);

        _mm256_storeu_si256(
            region.as_mut_ptr().add(i) as *mut __m256i,
            _mm256_xor_si256(_mm256_xor_si256(d0, k0), k_vec),
        );
        _mm256_storeu_si256(
            region.as_mut_ptr().add(i + 32) as *mut __m256i,
            _mm256_xor_si256(_mm256_xor_si256(d1, k1), k_vec),
        );
        _mm256_storeu_si256(
            region.as_mut_ptr().add(i + 64) as *mut __m256i,
            _mm256_xor_si256(_mm256_xor_si256(d2, k2), k_vec),
        );
        _mm256_storeu_si256(
            region.as_mut_ptr().add(i + 96) as *mut __m256i,
            _mm256_xor_si256(_mm256_xor_si256(d3, k3), k_vec),
        );
        i += 128;
    }
    while i + 32 <= len {
        let k = _mm256_loadu_si256(ks.as_ptr().add(i) as *const __m256i);
        let d = _mm256_loadu_si256(region.as_ptr().add(i) as *const __m256i);
        _mm256_storeu_si256(
            region.as_mut_ptr().add(i) as *mut __m256i,
            _mm256_xor_si256(_mm256_xor_si256(d, k), k_vec),
        );
        i += 32;
    }
    let k_sse = _mm_set1_epi32(k32);
    while i + 16 <= len {
        let k = _mm_loadu_si128(ks.as_ptr().add(i) as *const __m128i);
        let d = _mm_loadu_si128(region.as_ptr().add(i) as *const __m128i);
        _mm_storeu_si128(
            region.as_mut_ptr().add(i) as *mut __m128i,
            _mm_xor_si128(_mm_xor_si128(d, k), k_sse),
        );
        i += 16;
    }
    let k64 = (u32::from_ne_bytes(key) as u64) | ((u32::from_ne_bytes(key) as u64) << 32);
    while i + 8 <= len {
        let k = (ks.as_ptr().add(i) as *const u64).read_unaligned();
        let d = (region.as_ptr().add(i) as *const u64).read_unaligned();
        (region.as_mut_ptr().add(i) as *mut u64).write_unaligned(d ^ k ^ k64);
        i += 8;
    }
    while i < len {
        *region.get_unchecked_mut(i) ^= *ks.get_unchecked(i) ^ key[i & 3];
        i += 1;
    }
}

#[cfg(target_arch = "x86_64")]
#[target_feature(enable = "sse2")]
/// # Safety
/// The caller must guarantee the CPU supports the matching SIMD extension
/// (AVX2 / SSE2 / NEON). Every call site dispatches through runtime feature
/// detection (`has_avx2()`) or a compile-time `#[cfg(target_arch)]` gate.
/// `region` and `ks` are ordinary borrowed slices, so all pointer arithmetic
/// stays within their bounds. `ks.len()` is a multiple of 4 (debug-asserted
/// in `apply_fused_xor`), keeping the 4-byte mask phase aligned per chunk.
pub(crate) unsafe fn apply_fused_xor_sse2(region: &mut [u8], ks: &[u8], key: [u8; 4]) {
    use std::arch::x86_64::*;
    let len = region.len().min(ks.len());
    let k32 = u32::from_ne_bytes(key) as i32;
    let k_vec = _mm_set1_epi32(k32);
    let mut i = 0;
    while i + 64 <= len {
        let k0 = _mm_loadu_si128(ks.as_ptr().add(i) as *const __m128i);
        let k1 = _mm_loadu_si128(ks.as_ptr().add(i + 16) as *const __m128i);
        let k2 = _mm_loadu_si128(ks.as_ptr().add(i + 32) as *const __m128i);
        let k3 = _mm_loadu_si128(ks.as_ptr().add(i + 48) as *const __m128i);

        let d0 = _mm_loadu_si128(region.as_ptr().add(i) as *const __m128i);
        let d1 = _mm_loadu_si128(region.as_ptr().add(i + 16) as *const __m128i);
        let d2 = _mm_loadu_si128(region.as_ptr().add(i + 32) as *const __m128i);
        let d3 = _mm_loadu_si128(region.as_ptr().add(i + 48) as *const __m128i);

        _mm_storeu_si128(
            region.as_mut_ptr().add(i) as *mut __m128i,
            _mm_xor_si128(_mm_xor_si128(d0, k0), k_vec),
        );
        _mm_storeu_si128(
            region.as_mut_ptr().add(i + 16) as *mut __m128i,
            _mm_xor_si128(_mm_xor_si128(d1, k1), k_vec),
        );
        _mm_storeu_si128(
            region.as_mut_ptr().add(i + 32) as *mut __m128i,
            _mm_xor_si128(_mm_xor_si128(d2, k2), k_vec),
        );
        _mm_storeu_si128(
            region.as_mut_ptr().add(i + 48) as *mut __m128i,
            _mm_xor_si128(_mm_xor_si128(d3, k3), k_vec),
        );
        i += 64;
    }
    while i + 16 <= len {
        let k = _mm_loadu_si128(ks.as_ptr().add(i) as *const __m128i);
        let d = _mm_loadu_si128(region.as_ptr().add(i) as *const __m128i);
        _mm_storeu_si128(
            region.as_mut_ptr().add(i) as *mut __m128i,
            _mm_xor_si128(_mm_xor_si128(d, k), k_vec),
        );
        i += 16;
    }
    let k64 = (u32::from_ne_bytes(key) as u64) | ((u32::from_ne_bytes(key) as u64) << 32);
    while i + 8 <= len {
        let k = (ks.as_ptr().add(i) as *const u64).read_unaligned();
        let d = (region.as_ptr().add(i) as *const u64).read_unaligned();
        (region.as_mut_ptr().add(i) as *mut u64).write_unaligned(d ^ k ^ k64);
        i += 8;
    }
    while i < len {
        *region.get_unchecked_mut(i) ^= *ks.get_unchecked(i) ^ key[i & 3];
        i += 1;
    }
}

#[cfg(target_arch = "aarch64")]
/// # Safety
/// The caller must guarantee the CPU supports the matching SIMD extension
/// (AVX2 / SSE2 / NEON). Every call site dispatches through runtime feature
/// detection (`has_avx2()`) or a compile-time `#[cfg(target_arch)]` gate.
/// `region` and `ks` are ordinary borrowed slices, so all pointer arithmetic
/// stays within their bounds. `ks.len()` is a multiple of 4 (debug-asserted
/// in `apply_fused_xor`), keeping the 4-byte mask phase aligned per chunk.
pub(crate) unsafe fn apply_fused_xor_neon(region: &mut [u8], ks: &[u8], key: [u8; 4]) {
    use std::arch::aarch64::*;
    let len = region.len().min(ks.len());
    let mask16 = [
        key[0], key[1], key[2], key[3], key[0], key[1], key[2], key[3],
        key[0], key[1], key[2], key[3], key[0], key[1], key[2], key[3],
    ];
    let mask_vec = vld1q_u8(mask16.as_ptr());
    let mut i = 0;
    while i + 64 <= len {
        let k0 = vld1q_u8(ks.as_ptr().add(i));
        let k1 = vld1q_u8(ks.as_ptr().add(i + 16));
        let k2 = vld1q_u8(ks.as_ptr().add(i + 32));
        let k3 = vld1q_u8(ks.as_ptr().add(i + 48));

        let d0 = vld1q_u8(region.as_ptr().add(i));
        let d1 = vld1q_u8(region.as_ptr().add(i + 16));
        let d2 = vld1q_u8(region.as_ptr().add(i + 32));
        let d3 = vld1q_u8(region.as_ptr().add(i + 48));

        vst1q_u8(region.as_mut_ptr().add(i), veorq_u8(veorq_u8(d0, k0), mask_vec));
        vst1q_u8(
            region.as_mut_ptr().add(i + 16),
            veorq_u8(veorq_u8(d1, k1), mask_vec),
        );
        vst1q_u8(
            region.as_mut_ptr().add(i + 32),
            veorq_u8(veorq_u8(d2, k2), mask_vec),
        );
        vst1q_u8(
            region.as_mut_ptr().add(i + 48),
            veorq_u8(veorq_u8(d3, k3), mask_vec),
        );
        i += 64;
    }
    while i + 16 <= len {
        let k = vld1q_u8(ks.as_ptr().add(i));
        let d = vld1q_u8(region.as_ptr().add(i));
        vst1q_u8(region.as_mut_ptr().add(i), veorq_u8(veorq_u8(d, k), mask_vec));
        i += 16;
    }
    let mask64 = u64::from_ne_bytes([
        key[0], key[1], key[2], key[3], key[0], key[1], key[2], key[3],
    ]);
    while i + 8 <= len {
        let k = (ks.as_ptr().add(i) as *const u64).read_unaligned();
        let d = (region.as_ptr().add(i) as *const u64).read_unaligned();
        (region.as_mut_ptr().add(i) as *mut u64).write_unaligned(d ^ k ^ mask64);
        i += 8;
    }
    while i < len {
        *region.get_unchecked_mut(i) ^= *ks.get_unchecked(i) ^ key[i & 3];
        i += 1;
    }
}

pub(crate) fn apply_fused_xor_fallback(region: &mut [u8], ks: &[u8], key: [u8; 4]) {
    let len = region.len().min(ks.len());
    let mask64 = u64::from_ne_bytes([
        key[0], key[1], key[2], key[3], key[0], key[1], key[2], key[3],
    ]);
    let (dst_words, dst_tail) = region[..len].as_chunks_mut::<8>();
    let (ks_words, ks_tail) = ks[..len].as_chunks::<8>();
    for (d, k) in dst_words.iter_mut().zip(ks_words) {
        let dw = u64::from_ne_bytes(*d);
        let kw = u64::from_ne_bytes(*k);
        *d = (dw ^ kw ^ mask64).to_ne_bytes();
    }
    let offset = dst_words.len() * 8;
    for (j, (d, k)) in dst_tail.iter_mut().zip(ks_tail).enumerate() {
        *d ^= *k ^ key[(offset + j) & 3];
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn empty_key_is_noop() {
        let cipher = XorCipher::new("");
        let mut data = b"hello rushway".to_vec();
        let original = data.clone();
        cipher.apply(&mut data);
        assert_eq!(data, original);
        assert!(!cipher.is_enabled());
    }

    #[test]
    fn sha256_digest_is_repeated() {
        let cipher = XorCipher::new("test-key");
        assert!(cipher.is_enabled());
        assert_eq!(cipher.key.len(), XOR_KEY_SIZE);
        assert_eq!(&cipher.key[..32], &cipher.key[32..64]);
    }

    #[test]
    fn repeated_transform_round_trips() {
        let plaintext = b"a longer payload that crosses the digest boundary";
        let mut encrypted = plaintext.to_vec();
        let cipher = XorCipher::new("rushway-key");
        cipher.apply(&mut encrypted);
        assert_ne!(encrypted, plaintext);
        cipher.apply(&mut encrypted);
        assert_eq!(encrypted, plaintext);
    }

    fn bytewise_reference(key_digest: &[u8], data: &mut [u8]) {
        for (i, byte) in data.iter_mut().enumerate() {
            *byte ^= key_digest[i % XOR_KEY_SIZE];
        }
    }

    #[test]
    fn bulk_path_matches_bytewise_reference() {
        let cipher = XorCipher::new("bulk-key");
        // Unaligned lengths, digest-boundary crossing, and multi-chunk (>256KiB).
        for len in [
            1usize,
            7,
            8,
            9,
            15,
            31,
            33,
            1000,
            65535,
            100003,
            300 * 1024 + 7,
        ] {
            let plain: Vec<u8> = (0..len)
                .map(|i| (i as u64 * 2654435761 % 251 + 1) as u8)
                .collect();
            let mut fast = plain.clone();
            let mut reference = plain.clone();
            cipher.apply(&mut fast);
            // Rebuild the expanded keystream the same way `new` does.
            let digest = ring::digest::digest(&ring::digest::SHA256, b"bulk-key");
            let d = digest.as_ref();
            let mut stream = Vec::with_capacity(XOR_KEY_SIZE);
            while stream.len() < XOR_KEY_SIZE {
                let take = (XOR_KEY_SIZE - stream.len()).min(d.len());
                stream.extend_from_slice(&d[..take]);
            }
            bytewise_reference(&stream, &mut reference);
            assert_eq!(fast, reference, "mismatch at len {len}");
            // Round-trip back to plaintext.
            cipher.apply(&mut fast);
            assert_eq!(fast, plain, "round-trip failed at len {len}");
        }
    }

    #[test]
    fn each_call_resets_offset() {
        let mut whole = b"0123456789abcdef".to_vec();
        let mut first = b"0123456789".to_vec();
        let mut second = b"abcdef".to_vec();
        let cipher = XorCipher::new("offset-key");
        cipher.apply(&mut whole);
        cipher.apply(&mut first);
        cipher.apply(&mut second);
        let mut joined = first;
        joined.extend_from_slice(&second);
        assert_ne!(whole, joined);
    }

    #[cfg(target_arch = "x86_64")]
    #[test]
    fn test_sse2_and_avx2_paths_match_reference() {
        let key: Vec<u8> = (0..2048).map(|x| (x * 13 + 7) as u8).collect();
        for len in [
            0usize, 1, 2, 7, 8, 9, 15, 16, 17, 31, 32, 33, 63, 64, 65, 127, 128, 129, 255, 256,
            1000, 2048,
        ] {
            let orig: Vec<u8> = (0..len).map(|x| (x * 7 + 3) as u8).collect();
            let mut ref_buf = orig.clone();
            for (i, b) in ref_buf.iter_mut().enumerate() {
                *b ^= key[i % key.len()];
            }
            let key_expanded: Vec<u8> = (0..len).map(|i| key[i % key.len()]).collect();

            // Test SSE2 path directly.
            let mut sse_buf = orig.clone();
            unsafe {
                apply_chunk_sse2(&mut sse_buf, &key_expanded);
            }
            assert_eq!(sse_buf, ref_buf, "SSE2 mismatch at len {len}");

            // Test AVX2 path directly if supported by host CPU.
            if has_avx2() {
                let mut avx_buf = orig.clone();
                unsafe {
                    apply_chunk_avx2(&mut avx_buf, &key_expanded);
                }
                assert_eq!(avx_buf, ref_buf, "AVX2 mismatch at len {len}");
            }

            // Test fallback path directly.
            let mut fb_buf = orig.clone();
            apply_chunk_fallback(&mut fb_buf, &key_expanded);
            assert_eq!(fb_buf, ref_buf, "fallback mismatch at len {len}");
        }
    }

    #[test]
    fn test_fused_xor_matches_reference() {
        let key_data: Vec<u8> = (0..2048).map(|x| (x * 13 + 7) as u8).collect();
        let ws_mask = [0x12, 0x34, 0x56, 0x78];
        for len in [
            0usize, 1, 2, 7, 8, 9, 15, 16, 17, 31, 32, 33, 63, 64, 65, 127, 128, 129, 255, 256,
            1000, 2048, 2049, 4096, 4097, 8192, 65536,
        ] {
            let orig: Vec<u8> = (0..len).map(|x| (x * 7 + 3) as u8).collect();
            let mut ref_buf = orig.clone();
            for (i, b) in ref_buf.iter_mut().enumerate() {
                *b ^= key_data[i % key_data.len()] ^ ws_mask[i & 3];
            }

            let mut fused_buf = orig.clone();
            apply_fused_xor(&mut fused_buf, &key_data, ws_mask);
            assert_eq!(fused_buf, ref_buf, "fused dispatcher mismatch at len {len}");

            if len <= key_data.len() {
                #[cfg(target_arch = "x86_64")]
                {
                    let mut sse_buf = orig.clone();
                    unsafe {
                        apply_fused_xor_sse2(&mut sse_buf, &key_data, ws_mask);
                    }
                    assert_eq!(sse_buf, ref_buf, "fused SSE2 mismatch at len {len}");

                    if has_avx2() {
                        let mut avx_buf = orig.clone();
                        unsafe {
                            apply_fused_xor_avx2(&mut avx_buf, &key_data, ws_mask);
                        }
                        assert_eq!(avx_buf, ref_buf, "fused AVX2 mismatch at len {len}");
                    }
                }

                let mut fb_buf = orig.clone();
                apply_fused_xor_fallback(&mut fb_buf, &key_data, ws_mask);
                assert_eq!(fb_buf, ref_buf, "fused fallback mismatch at len {len}");
            }
        }
    }
}

} // mod crypto

pub mod dns {
//! GoWay-compatible remote DNS resolver: 5s timeout, UDP->TCP fallback,
//! system-DNS fallback and a 5-minute positive cache.

use anyhow::{anyhow, bail, Result};
use rand::RngCore;
use std::collections::HashMap;
use std::hash::Hash;
use std::net::{IpAddr, Ipv4Addr, Ipv6Addr, SocketAddr};
use std::sync::{Mutex as StdMutex, OnceLock, RwLock};
use std::time::{Duration, Instant};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{lookup_host, TcpStream, UdpSocket};
use tokio::sync::watch;
use tokio::time::timeout;

const RESOLVE_TIMEOUT: Duration = Duration::from_millis(1500);
const CACHE_TTL: Duration = Duration::from_secs(5 * 60);
const DNS_PORT: u16 = 53;
const MAX_PACKET: usize = 4096;
const CACHE_MAX_ENTRIES: usize = 4096;

static SERVER: RwLock<Option<IpAddr>> = RwLock::new(None);
static CACHE: OnceLock<RwLock<HashMap<String, (IpAddr, Instant)>>> = OnceLock::new();
static ALL_V4_CACHE: OnceLock<RwLock<HashMap<String, (Vec<Ipv4Addr>, Instant)>>> = OnceLock::new();
static IN_FLIGHT: OnceLock<StdMutex<HashMap<String, watch::Receiver<Option<IpAddr>>>>> =
    OnceLock::new();

fn cache() -> &'static RwLock<HashMap<String, (IpAddr, Instant)>> {
    CACHE.get_or_init(|| RwLock::new(HashMap::new()))
}

fn all_v4_cache() -> &'static RwLock<HashMap<String, (Vec<Ipv4Addr>, Instant)>> {
    ALL_V4_CACHE.get_or_init(|| RwLock::new(HashMap::new()))
}

fn in_flight() -> &'static StdMutex<HashMap<String, watch::Receiver<Option<IpAddr>>>> {
    IN_FLIGHT.get_or_init(|| StdMutex::new(HashMap::new()))
}

/// Insert into a bounded cache map: once the entry cap is reached the whole
/// table is cleared before inserting. Clearing everything instead of evicting
/// per-entry trades cache hit-rate for simplicity and a hard bound on memory
/// growth.
fn bounded_insert<K: Hash + Eq, T>(
    map: &mut HashMap<K, (T, Instant)>,
    key: K,
    value: (T, Instant),
) {
    if map.len() >= CACHE_MAX_ENTRIES {
        let now = Instant::now();
        map.retain(|_, (_, expires)| *expires > now);
        if map.len() >= CACHE_MAX_ENTRIES {
            map.clear();
        }
    }
    map.insert(key, value);
}

pub(crate) async fn configure(server: Option<String>) -> Result<()> {
    let parsed = match server.as_deref() {
        Some(value) => Some(
            value
                .parse::<IpAddr>()
                .map_err(|_| anyhow!("-dns requires a valid IP address, got '{value}'"))?,
        ),
        None => None,
    };
    *SERVER.write().unwrap_or_else(|e| e.into_inner()) = parsed;
    Ok(())
}

pub(crate) async fn resolve_host(host: &str) -> Result<IpAddr> {
    let clean = host.trim().trim_matches(|c| c == '[' || c == ']');
    if let Ok(ip) = clean.parse::<IpAddr>() {
        return Ok(ip);
    }
    let trimmed = clean.trim_end_matches('.');
    let key: std::borrow::Cow<'_, str> = if trimmed.bytes().any(|b| b.is_ascii_uppercase()) {
        std::borrow::Cow::Owned(trimmed.to_ascii_lowercase())
    } else {
        std::borrow::Cow::Borrowed(trimmed)
    };
    // Fast path: synchronous read lock, zero tokio yield, zero heap allocation on lowercase cache hit
    if let Ok(guard) = cache().read() {
        if let Some(&(ip, expires)) = guard.get(key.as_ref()) {
            if expires > Instant::now() {
                return Ok(ip);
            }
        }
    }

    let key = key.into_owned();
    // SingleFlight coordination: deduplicate concurrent in-flight DNS lookups for the same host.
    let mut waiter = None;
    let mut tx = None;
    {
        let mut inflight = in_flight().lock().unwrap_or_else(|e| e.into_inner());
        // Double check cache inside lock
        if let Ok(guard) = cache().read() {
            if let Some(&(ip, expires)) = guard.get(&key) {
                if expires > Instant::now() {
                    return Ok(ip);
                }
            }
        }
        if let Some(rx) = inflight.get(&key) {
            waiter = Some(rx.clone());
        } else {
            let (sender, receiver) = watch::channel(None);
            inflight.insert(key.clone(), receiver);
            tx = Some(sender);
        }
    }

    if let Some(mut rx) = waiter {
        while rx.borrow().is_none() {
            if rx.changed().await.is_err() {
                break;
            }
        }
        if let Some(ip) = *rx.borrow() {
            return Ok(ip);
        }
        if let Ok(guard) = cache().read() {
            if let Some(&(ip, expires)) = guard.get(&key) {
                if expires > Instant::now() {
                    return Ok(ip);
                }
            }
        }
        bail!("concurrent DNS resolution failed for {host}");
    }

    struct InFlightGuard<'a>(&'a str);
    impl<'a> Drop for InFlightGuard<'a> {
        fn drop(&mut self) {
            if let Ok(mut inflight) = in_flight().lock() {
                inflight.remove(self.0);
            }
        }
    }
    let _guard = InFlightGuard(&key);

    let server = *SERVER.read().unwrap_or_else(|e| e.into_inner());
    let query_result = match server {
        Some(server_ip) => match resolve_remote(&key, server_ip).await {
            Ok(ip) => Ok(ip),
            Err(remote_error) => {
                tracing::warn!(host=%host, error=%remote_error, "remote DNS failed; falling back to system DNS");
                resolve_system_dns(clean, host).await
            }
        },
        None => resolve_system_dns(clean, host).await,
    };

    match query_result {
        Ok(resolved) => {
            let mut guard = cache().write().unwrap_or_else(|e| e.into_inner());
            bounded_insert(&mut guard, key.clone(), (resolved, Instant::now() + CACHE_TTL));
            if let Some(sender) = tx {
                let _ = sender.send(Some(resolved));
            }
            Ok(resolved)
        }
        Err(e) => {
            if let Some(sender) = tx {
                let _ = sender.send(None);
            }
            Err(e)
        }
    }
}

pub(crate) async fn resolve_socket(host: &str, port: u16) -> Result<SocketAddr> {
    Ok(SocketAddr::new(resolve_host(host).await?, port))
}

pub(crate) async fn resolve_all_ipv4(host: &str) -> Result<Vec<Ipv4Addr>> {
    let clean = host.trim().trim_matches(|c| c == '[' || c == ']');
    if let Ok(ip) = clean.parse::<Ipv4Addr>() {
        return Ok(vec![ip]);
    }
    if clean.parse::<Ipv6Addr>().is_ok() {
        return Ok(Vec::new());
    }
    let trimmed = clean.trim_end_matches('.');
    let key: std::borrow::Cow<'_, str> = if trimmed.bytes().any(|b| b.is_ascii_uppercase()) {
        std::borrow::Cow::Owned(trimmed.to_ascii_lowercase())
    } else {
        std::borrow::Cow::Borrowed(trimmed)
    };
    // Fast path: synchronous read lock, zero tokio yield, zero key allocation on lowercase cache hit
    if let Ok(guard) = all_v4_cache().read() {
        if let Some((ips, expires)) = guard.get(key.as_ref()) {
            if *expires > Instant::now() && !ips.is_empty() {
                return Ok(ips.clone());
            }
        }
    }
    let key = key.into_owned();
    let server = *SERVER.read().unwrap_or_else(|e| e.into_inner());
    let ips = match server {
        Some(server_ip) => match query_remote_all_ipv4(&key, server_ip).await {
            Ok(list) if !list.is_empty() => list,
            Ok(_) => resolve_system_all_ipv4(clean).await,
            Err(err) => {
                tracing::warn!(host=%host, error=%err, "remote DNS failed for fakehost; falling back to system DNS");
                resolve_system_all_ipv4(clean).await
            }
        },
        None => resolve_system_all_ipv4(clean).await,
    };
    let mut unique = Vec::new();
    for ip in ips {
        if !unique.contains(&ip) {
            unique.push(ip);
        }
    }
    if unique.is_empty() {
        bail!("no IPv4 addresses resolved for {host}");
    }
    let mut guard = all_v4_cache().write().unwrap_or_else(|e| e.into_inner());
    bounded_insert(&mut guard, key, (unique.clone(), Instant::now() + CACHE_TTL));
    Ok(unique)
}

async fn resolve_system_dns(clean: &str, host: &str) -> Result<IpAddr> {
    match timeout(RESOLVE_TIMEOUT, lookup_host((clean, 0))).await {
        Ok(Ok(addresses)) => {
            let mut first_v6 = None;
            for addr in addresses {
                let ip = addr.ip();
                if ip.is_ipv4() {
                    return Ok(ip);
                } else if first_v6.is_none() {
                    first_v6 = Some(ip);
                }
            }
            first_v6.ok_or_else(|| anyhow!("system DNS returned no addresses for {host}"))
        }
        Ok(Err(e)) => Err(e.into()),
        Err(_) => Err(anyhow!("system DNS timeout for {host}")),
    }
}

async fn resolve_system_all_ipv4(clean: &str) -> Vec<Ipv4Addr> {
    let mut result = Vec::new();
    if let Ok(Ok(iter)) = timeout(RESOLVE_TIMEOUT, lookup_host((clean, 0))).await {
        for addr in iter {
            if let IpAddr::V4(v4) = addr.ip() {
                if !result.contains(&v4) {
                    result.push(v4);
                }
            }
        }
    }
    result
}

async fn resolve_remote(host: &str, server: IpAddr) -> Result<IpAddr> {
    let v4_fut = query_remote(host, server, 1u16);
    let v6_fut = query_remote(host, server, 28u16);
    tokio::pin!(v4_fut);
    tokio::pin!(v6_fut);

    tokio::select! {
        v4_res = &mut v4_fut => {
            if let Ok(Some(ip)) = v4_res {
                return Ok(ip);
            }
            let v6_res = v6_fut.await;
            if let Ok(Some(ip)) = v6_res {
                return Ok(ip);
            }
            match (v4_res, v6_res) {
                (Err(e), _) | (_, Err(e)) => Err(e),
                _ => Err(anyhow!("remote DNS returned no A/AAAA answer for {host}")),
            }
        }
        v6_res = &mut v6_fut => {
            if let Ok(Some(ip6)) = v6_res {
                if let Ok(Ok(Some(ip4))) = timeout(Duration::from_millis(25), v4_fut).await {
                    return Ok(ip4);
                }
                return Ok(ip6);
            }
            let v4_res = v4_fut.await;
            if let Ok(Some(ip4)) = v4_res {
                return Ok(ip4);
            }
            match (v4_res, v6_res) {
                (Err(e), _) | (_, Err(e)) => Err(e),
                _ => Err(anyhow!("remote DNS returned no A/AAAA answer for {host}")),
            }
        }
    }
}

async fn query_remote(host: &str, server: IpAddr, qtype: u16) -> Result<Option<IpAddr>> {
    let request = build_query(host, qtype)?;
    let expected_id = u16::from_be_bytes([request[0], request[1]]);
    let server_addr = SocketAddr::new(server, DNS_PORT);
    let socket = UdpSocket::bind(if server.is_ipv4() {
        "0.0.0.0:0"
    } else {
        "[::]:0"
    })
    .await?;
    socket.send_to(&request, server_addr).await?;
    let mut buf = vec![0u8; MAX_PACKET];
    let size = recv_from_expected(&socket, &mut buf, server_addr).await?;
    buf.truncate(size);
    validate_transaction_id(&buf, expected_id)?;
    let (ip, truncated) = parse_response(&buf, qtype, host)?;
    if !truncated {
        return Ok(ip);
    }
    let response = dns_tcp_query(server_addr, &request).await?;
    validate_transaction_id(&response, expected_id)?;
    Ok(parse_response(&response, qtype, host)?.0)
}

async fn query_remote_all_ipv4(host: &str, server: IpAddr) -> Result<Vec<Ipv4Addr>> {
    let request = build_query(host, 1)?;
    let expected_id = u16::from_be_bytes([request[0], request[1]]);
    let server_addr = SocketAddr::new(server, DNS_PORT);
    let socket = UdpSocket::bind(if server.is_ipv4() {
        "0.0.0.0:0"
    } else {
        "[::]:0"
    })
    .await?;
    socket.send_to(&request, server_addr).await?;
    let mut buf = vec![0u8; MAX_PACKET];
    let size = recv_from_expected(&socket, &mut buf, server_addr).await?;
    buf.truncate(size);
    validate_transaction_id(&buf, expected_id)?;
    let (ips, truncated) = parse_all_ipv4_from_response(&buf, host)?;
    if !truncated && !ips.is_empty() {
        return Ok(ips);
    }
    let response = dns_tcp_query(server_addr, &request).await?;
    validate_transaction_id(&response, expected_id)?;
    Ok(parse_all_ipv4_from_response(&response, host)?.0)
}

/// Receive one datagram, discarding (with a DEBUG log) any datagram whose
/// source does not exactly match the resolver address the query was sent to,
/// until the resolver timeout expires. Basic anti-spoofing: never accept a
/// DNS reply that did not come from the queried server.
async fn recv_from_expected(
    socket: &UdpSocket,
    buf: &mut [u8],
    server_addr: SocketAddr,
) -> Result<usize> {
    let (size, _) = timeout(
        RESOLVE_TIMEOUT,
        async {
            loop {
                let (size, peer) = socket.recv_from(buf).await?;
                if peer != server_addr {
                    tracing::debug!(peer=%peer, expected=%server_addr, "discarded DNS reply from unexpected source");
                    continue;
                }
                return Ok::<(usize, SocketAddr), std::io::Error>((size, peer));
            }
        },
    )
    .await
    .map_err(|_| anyhow!("remote DNS UDP timeout"))??;
    Ok(size)
}

async fn dns_tcp_query(server: SocketAddr, request: &[u8]) -> Result<Vec<u8>> {
    let mut stream = timeout(RESOLVE_TIMEOUT, TcpStream::connect(server)).await??;
    let len = u16::try_from(request.len()).map_err(|_| anyhow!("DNS request too large"))?;
    stream.write_all(&len.to_be_bytes()).await?;
    stream.write_all(request).await?;
    let mut header = [0u8; 2];
    timeout(RESOLVE_TIMEOUT, stream.read_exact(&mut header)).await??;
    let size = u16::from_be_bytes(header) as usize;
    if size == 0 || size > MAX_PACKET {
        bail!("invalid DNS TCP response length: {size}");
    }
    let mut response = vec![0u8; size];
    timeout(RESOLVE_TIMEOUT, stream.read_exact(&mut response)).await??;
    Ok(response)
}

fn validate_transaction_id(buf: &[u8], expected: u16) -> Result<()> {
    if buf.len() < 2 {
        bail!("DNS response too short for transaction ID");
    }
    let actual = u16::from_be_bytes([buf[0], buf[1]]);
    if actual != expected {
        bail!("DNS transaction ID mismatch: expected {expected:#06x}, got {actual:#06x}");
    }
    Ok(())
}

fn build_query(host: &str, qtype: u16) -> Result<Vec<u8>> {
    let labels: Vec<&str> = host.trim_end_matches('.').split('.').collect();
    if labels.is_empty()
        || labels
            .iter()
            .any(|label| label.is_empty() || label.len() > 63)
    {
        bail!("invalid DNS hostname: {host}");
    }
    let mut id_bytes = [0u8; 2];
    rand::thread_rng().fill_bytes(&mut id_bytes);
    let mut out = Vec::with_capacity(64);
    out.extend_from_slice(&id_bytes);
    out.extend_from_slice(&0x0100u16.to_be_bytes());
    out.extend_from_slice(&1u16.to_be_bytes());
    out.extend_from_slice(&0u16.to_be_bytes());
    out.extend_from_slice(&0u16.to_be_bytes());
    out.extend_from_slice(&0u16.to_be_bytes());
    for label in labels {
        out.push(label.len() as u8);
        out.extend_from_slice(label.as_bytes());
    }
    out.push(0);
    out.extend_from_slice(&qtype.to_be_bytes());
    out.extend_from_slice(&1u16.to_be_bytes());
    Ok(out)
}

fn parse_response(buf: &[u8], qtype: u16, host: &str) -> Result<(Option<IpAddr>, bool)> {
    if buf.len() < 12 {
        bail!("DNS response too short");
    }
    let flags = u16::from_be_bytes([buf[2], buf[3]]);
    let truncated = flags & 0x0200 != 0;
    let rcode = flags & 0x000f;
    if rcode != 0 {
        bail!("DNS response error code {rcode}");
    }
    let qd = u16::from_be_bytes([buf[4], buf[5]]) as usize;
    let an = u16::from_be_bytes([buf[6], buf[7]]) as usize;
    let mut offset = 12usize;
    validate_question_section(buf, &mut offset, qd, host, qtype)?;
    for _ in 0..an {
        skip_name(buf, &mut offset)?;
        if offset + 10 > buf.len() {
            bail!("truncated DNS answer header");
        }
        let rr_type = u16::from_be_bytes([buf[offset], buf[offset + 1]]);
        let class = u16::from_be_bytes([buf[offset + 2], buf[offset + 3]]);
        let rd_len = u16::from_be_bytes([buf[offset + 8], buf[offset + 9]]) as usize;
        offset += 10;
        if offset + rd_len > buf.len() {
            bail!("truncated DNS answer data");
        }
        if class == 1 && rr_type == qtype {
            match qtype {
                1 if rd_len == 4 => {
                    return Ok((
                        Some(IpAddr::V4(Ipv4Addr::new(
                            buf[offset],
                            buf[offset + 1],
                            buf[offset + 2],
                            buf[offset + 3],
                        ))),
                        truncated,
                    ))
                }
                28 if rd_len == 16 => {
                    let mut octets = [0u8; 16];
                    octets.copy_from_slice(&buf[offset..offset + 16]);
                    return Ok((Some(IpAddr::V6(Ipv6Addr::from(octets))), truncated));
                }
                _ => {}
            }
        }
        offset += rd_len;
    }
    Ok((None, truncated))
}

fn parse_all_ipv4_from_response(buf: &[u8], host: &str) -> Result<(Vec<Ipv4Addr>, bool)> {
    if buf.len() < 12 {
        bail!("DNS response too short");
    }
    let flags = u16::from_be_bytes([buf[2], buf[3]]);
    let truncated = flags & 0x0200 != 0;
    let rcode = flags & 0x000f;
    if rcode != 0 {
        bail!("DNS response error code {rcode}");
    }
    let qd = u16::from_be_bytes([buf[4], buf[5]]) as usize;
    let an = u16::from_be_bytes([buf[6], buf[7]]) as usize;
    let mut offset = 12usize;
    validate_question_section(buf, &mut offset, qd, host, 1)?;
    let mut ips = Vec::new();
    for _ in 0..an {
        skip_name(buf, &mut offset)?;
        if offset + 10 > buf.len() {
            bail!("truncated DNS answer header");
        }
        let rr_type = u16::from_be_bytes([buf[offset], buf[offset + 1]]);
        let class = u16::from_be_bytes([buf[offset + 2], buf[offset + 3]]);
        let rd_len = u16::from_be_bytes([buf[offset + 8], buf[offset + 9]]) as usize;
        offset += 10;
        if offset + rd_len > buf.len() {
            bail!("truncated DNS answer data");
        }
        if class == 1 && rr_type == 1 && rd_len == 4 {
            let ip = Ipv4Addr::new(
                buf[offset],
                buf[offset + 1],
                buf[offset + 2],
                buf[offset + 3],
            );
            if !ips.contains(&ip) {
                ips.push(ip);
            }
        }
        offset += rd_len;
    }
    Ok((ips, truncated))
}

fn skip_name(buf: &[u8], offset: &mut usize) -> Result<()> {
    let mut pos = *offset;
    let mut labels = 0usize;
    loop {
        if pos >= buf.len() {
            bail!("truncated DNS name");
        }
        let len = buf[pos];
        if len & 0xc0 == 0xc0 {
            if pos + 1 >= buf.len() {
                bail!("truncated DNS name pointer");
            }
            pos += 2;
            break;
        }
        if len == 0 {
            pos += 1;
            break;
        }
        if len & 0xc0 != 0 || len as usize > 63 {
            bail!("invalid DNS label");
        }
        pos += 1 + len as usize;
        if pos > buf.len() {
            bail!("truncated DNS label");
        }
        labels += 1;
        if labels > 128 {
            bail!("DNS name too deep");
        }
    }
    *offset = pos;
    Ok(())
}

/// Decode a DNS name starting at `start` into its textual form (labels joined
/// by '.'), following compression pointers. Only backward references are
/// accepted and the jump count is bounded, so a crafted name cannot loop.
/// Returns the name and the offset just past the name on the wire.
fn decode_name(buf: &[u8], start: usize) -> Result<(String, usize)> {
    let mut out = String::new();
    let mut pos = start;
    let mut end = start;
    let mut jumps = 0usize;
    let mut labels = 0usize;
    loop {
        if pos >= buf.len() {
            bail!("truncated DNS name");
        }
        let len = buf[pos];
        if len & 0xc0 == 0xc0 {
            if pos + 1 >= buf.len() {
                bail!("truncated DNS name pointer");
            }
            if jumps == 0 {
                end = pos + 2;
            }
            jumps += 1;
            if jumps > 64 {
                bail!("too many DNS name pointers");
            }
            let target = (((len & 0x3f) as usize) << 8) | buf[pos + 1] as usize;
            if target >= pos {
                bail!("invalid DNS name pointer");
            }
            pos = target;
            continue;
        }
        if len == 0 {
            if jumps == 0 {
                end = pos + 1;
            }
            break;
        }
        if len & 0xc0 != 0 || len > 63 {
            bail!("invalid DNS label");
        }
        if labels >= 128 {
            bail!("DNS name too deep");
        }
        let l = len as usize;
        if pos + 1 + l > buf.len() {
            bail!("truncated DNS label");
        }
        if !out.is_empty() {
            out.push('.');
        }
        out.push_str(&String::from_utf8_lossy(&buf[pos + 1..pos + 1 + l]));
        labels += 1;
        pos += 1 + l;
    }
    Ok((out, end))
}

/// Validate that the response echoes the question we asked: same name
/// (case-insensitive), same qtype and the IN qclass. A response whose question
/// section does not match the pending query is discarded (error) so it can
/// never be used to answer it. On success `offset` is positioned after the
/// question section.
fn validate_question_section(
    buf: &[u8],
    offset: &mut usize,
    count: usize,
    host: &str,
    qtype: u16,
) -> Result<()> {
    let mut seen = false;
    for _ in 0..count {
        let (name, end) = decode_name(buf, *offset)?;
        if end + 4 > buf.len() {
            bail!("truncated DNS question");
        }
        let rq_type = u16::from_be_bytes([buf[end], buf[end + 1]]);
        let rq_class = u16::from_be_bytes([buf[end + 2], buf[end + 3]]);
        if !name.eq_ignore_ascii_case(host) || rq_type != qtype || rq_class != 1 {
            bail!("DNS question section mismatch");
        }
        seen = true;
        *offset = end + 4;
    }
    if !seen {
        bail!("DNS response without question section");
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn builds_query_with_question() {
        let q = build_query("example.com", 1).unwrap();
        assert!(q.len() > 16);
        assert_eq!(u16::from_be_bytes([q[4], q[5]]), 1);
    }
    #[test]
    fn transaction_id_mismatch_is_rejected() {
        let response = [0x12, 0x35, 0x81, 0x80, 0, 1, 0, 0, 0, 0, 0, 0];
        assert!(validate_transaction_id(&response, 0x1234).is_err());
        assert!(validate_transaction_id(&response, 0x1235).is_ok());
    }
    #[test]
    fn parses_ipv4_answer() {
        let response = vec![
            0x12, 0x34, 0x81, 0x80, 0x00, 0x01, 0x00, 0x01, 0x00, 0x00, 0x00, 0x00, 0x07, b'e',
            b'x', b'a', b'm', b'p', b'l', b'e', 0x03, b'c', b'o', b'm', 0x00, 0x00, 0x01, 0x00,
            0x01, 0xc0, 0x0c, 0x00, 0x01, 0x00, 0x01, 0x00, 0x00, 0x00, 0x3c, 0x00, 0x04, 1, 2, 3,
            4,
        ];
        let (ip, truncated) = parse_response(&response, 1, "example.com").unwrap();
        assert!(!truncated);
        assert_eq!(ip, Some(IpAddr::V4(Ipv4Addr::new(1, 2, 3, 4))));
    }
    #[test]
    fn question_name_mismatch_is_rejected() {
        let response = vec![
            0x12, 0x34, 0x81, 0x80, 0x00, 0x01, 0x00, 0x01, 0x00, 0x00, 0x00, 0x00, 0x07, b'e',
            b'x', b'a', b'm', b'p', b'l', b'e', 0x03, b'o', b'r', b'g', 0x00, 0x00, 0x01, 0x00,
            0x01, 0xc0, 0x0c, 0x00, 0x01, 0x00, 0x01, 0x00, 0x00, 0x00, 0x3c, 0x00, 0x04, 1, 2, 3,
            4,
        ];
        // Different question name: the response must be discarded.
        assert!(parse_response(&response, 1, "example.com").is_err());
        // Same name in a different case must still match (case-insensitive).
        assert!(parse_response(&response, 1, "EXAMPLE.ORG").is_ok());
    }
    #[test]
    fn parses_multiple_ipv4_answers() {
        let response = vec![
            0x12, 0x34, 0x81, 0x80, 0x00, 0x01, 0x00, 0x02, 0x00, 0x00, 0x00, 0x00, 0x07, b'e',
            b'x', b'a', b'm', b'p', b'l', b'e', 0x03, b'c', b'o', b'm', 0x00, 0x00, 0x01, 0x00,
            0x01, 0xc0, 0x0c, 0x00, 0x01, 0x00, 0x01, 0x00, 0x00, 0x00, 0x3c, 0x00, 0x04, 104, 21,
            50, 1, 0xc0, 0x0c, 0x00, 0x01, 0x00, 0x01, 0x00, 0x00, 0x00, 0x3c, 0x00, 0x04, 172, 67,
            180, 2,
        ];
        let (ips, truncated) = parse_all_ipv4_from_response(&response, "example.com").unwrap();
        assert!(!truncated);
        assert_eq!(
            ips,
            vec![
                Ipv4Addr::new(104, 21, 50, 1),
                Ipv4Addr::new(172, 67, 180, 2)
            ]
        );
    }

    #[tokio::test]
    async fn parses_bracketed_ipv6_without_dns_query() {
        let ip = resolve_host("[::1]").await.unwrap();
        assert_eq!(ip, "::1".parse::<IpAddr>().unwrap());

        let ip2 = resolve_host("[2001:db8::1]").await.unwrap();
        assert_eq!(ip2, "2001:db8::1".parse::<IpAddr>().unwrap());

        let v4_list = resolve_all_ipv4("[::1]").await.unwrap();
        assert!(v4_list.is_empty());
    }
}
} // mod dns

pub mod flow {
//! Per-stream send credit gate (W3 flow control).
//!
//! A gate starts **unbounded** (v1 behavior) so a session never blocks
//! before negotiation completes. Once the peer's VERSION frame is known,
//! [`CreditGate::enable`] flips it to bounded mode with the advertised
//! window; from then on [`CreditGate::acquire`] throttles the sender and
//! incoming WINDOW frames replenish credit via [`CreditGate::release`].
//!
//! [`CreditGate::close`] wakes every waiter and passes it through: the
//! stream/session is going away, so remaining sends must fail at the
//! socket instead of dead-locking on credit that will never arrive.

use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use tokio::sync::Notify;

const UNBOUNDED_TAG: u32 = 0;
const CLOSED_TAG: u32 = u32::MAX;
const MAX_WINDOW: i64 = (u32::MAX - 1) as i64;

#[inline(always)]
const fn pack_state(window: u32, available: u32) -> u64 {
    ((window as u64) << 32) | (available as u64)
}

#[inline(always)]
const fn unpack_state(raw: u64) -> (u32, u32) {
    ((raw >> 32) as u32, raw as u32)
}

#[derive(Debug)]
pub(crate) struct CreditGate {
    /// Packed `(window: u32, available: u32)`:
    /// - `window == 0`: Unbounded (initial fast path)
    /// - `window == u32::MAX`: Closed (pass-through)
    /// - `1..=u32::MAX-1`: Bounded with `available <= window`
    state: AtomicU64,
    notify: Notify,
}

impl CreditGate {
    pub(crate) fn new() -> Arc<Self> {
        Arc::new(Self {
            state: AtomicU64::new(pack_state(UNBOUNDED_TAG, 0)),
            notify: Notify::new(),
        })
    }

    /// Enters bounded mode with `window` bytes of credit. Idempotent: the
    /// first VERSION wins, later ones cannot shrink/grow a live window.
    pub(crate) fn enable(&self, window: i64) {
        let w = window.clamp(1, MAX_WINDOW) as u32;
        if self
            .state
            .compare_exchange(
                pack_state(UNBOUNDED_TAG, 0),
                pack_state(w, w),
                Ordering::AcqRel,
                Ordering::Relaxed,
            )
            .is_ok()
        {
            self.notify.notify_waiters();
        }
    }

    /// Credits `n` consumed bytes back to the window (capped at the
    /// advertised maximum so a buggy peer cannot inflate it).
    #[inline]
    pub(crate) fn release(&self, n: i64) {
        if n <= 0 {
            return;
        }
        let add = n.min(MAX_WINDOW) as u64;
        let mut cur = self.state.load(Ordering::Relaxed);
        loop {
            let (win, avail) = unpack_state(cur);
            if win == UNBOUNDED_TAG || win == CLOSED_TAG {
                return;
            }
            let new_avail = ((avail as u64) + add).min(win as u64) as u32;
            if new_avail == avail {
                return;
            }
            match self.state.compare_exchange_weak(
                cur,
                pack_state(win, new_avail),
                Ordering::Release,
                Ordering::Relaxed,
            ) {
                Ok(_) => {
                    self.notify.notify_one();
                    return;
                }
                Err(actual) => cur = actual,
            }
        }
    }

    /// Unblocks all waiters and switches to pass-through mode.
    pub(crate) fn close(&self) {
        self.state
            .store(pack_state(CLOSED_TAG, 0), Ordering::Release);
        self.notify.notify_waiters();
    }

    /// Waits until `n` bytes of credit are available. Unbounded/Closed
    /// states return immediately. `n` is clamped to the window so an
    /// oversized frame can never wait for credit that can never arrive.
    #[inline]
    pub(crate) async fn acquire(&self, n: usize) {
        // Fast path: try to acquire without constructing or registering a Notified waiter.
        let mut cur = self.state.load(Ordering::Acquire);
        loop {
            let (win, avail) = unpack_state(cur);
            if win == UNBOUNDED_TAG || win == CLOSED_TAG {
                return;
            }
            let need = n.min(win as usize) as u32;
            if avail < need {
                break;
            }
            match self.state.compare_exchange_weak(
                cur,
                pack_state(win, avail - need),
                Ordering::AcqRel,
                Ordering::Acquire,
            ) {
                Ok(_) => return,
                Err(actual) => cur = actual,
            }
        }

        // Slow path: register waiter BEFORE re-checking state so neither
        // `release()` (`notify_one`) nor `close()`/`enable()` (`notify_waiters`)
        // can fire in the gap between the state check and `.await`.
        loop {
            let notified = self.notify.notified();
            tokio::pin!(notified);
            notified.as_mut().enable();

            let mut cur = self.state.load(Ordering::Acquire);
            loop {
                let (win, avail) = unpack_state(cur);
                if win == UNBOUNDED_TAG || win == CLOSED_TAG {
                    return;
                }
                let need = n.min(win as usize) as u32;
                if avail < need {
                    break;
                }
                match self.state.compare_exchange_weak(
                    cur,
                    pack_state(win, avail - need),
                    Ordering::AcqRel,
                    Ordering::Acquire,
                ) {
                    Ok(_) => return,
                    Err(actual) => cur = actual,
                }
            }

            notified.await;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn unbounded_gate_passes_immediately() {
        let gate = CreditGate::new();
        gate.acquire(usize::MAX).await;
    }

    #[tokio::test]
    async fn bounded_gate_throttles_until_release() {
        let gate = CreditGate::new();
        gate.enable(100);
        gate.acquire(60).await;

        let mut waiting = tokio::spawn({
            let gate = gate.clone();
            async move {
                gate.acquire(60).await;
                1
            }
        });
        // The waiter needs 60 but only 40 remain: it must not finish yet.
        let poll =
            tokio::time::timeout(std::time::Duration::from_millis(50), &mut waiting).await;
        assert!(poll.is_err(), "acquire passed without credit");

        gate.release(60);
        let done = tokio::time::timeout(std::time::Duration::from_secs(2), waiting)
            .await
            .expect("release did not wake waiter")
            .unwrap();
        assert_eq!(done, 1);
    }

    #[tokio::test]
    async fn release_is_capped_at_window() {
        let gate = CreditGate::new();
        gate.enable(100);
        gate.acquire(100).await;
        gate.release(1_000_000);
        gate.acquire(100).await;
        // Capped: another full window is NOT available.
        let pending = tokio::time::timeout(
            std::time::Duration::from_millis(50),
            gate.acquire(1),
        )
        .await;
        assert!(pending.is_err(), "window inflated beyond advertised max");
        gate.close();
    }

    #[tokio::test]
    async fn close_unblocks_waiters_and_passes_through() {
        let gate = CreditGate::new();
        gate.enable(1);
        gate.acquire(1).await;

        let waiting = tokio::spawn({
            let gate = gate.clone();
            async move {
                gate.acquire(64 * 1024).await;
            }
        });
        tokio::time::sleep(std::time::Duration::from_millis(20)).await;
        gate.close();
        tokio::time::timeout(std::time::Duration::from_secs(2), waiting)
            .await
            .expect("close did not wake waiter")
            .unwrap();
        // Closed stays pass-through.
        gate.acquire(usize::MAX).await;
    }

    #[tokio::test]
    async fn oversized_acquire_clamps_to_window() {
        let gate = CreditGate::new();
        gate.enable(64);
        // Request larger than the window: served from a full window
        // instead of waiting forever.
        gate.acquire(64 * 1024).await;
        gate.close();
    }

    #[tokio::test]
    async fn enable_is_idempotent() {
        let gate = CreditGate::new();
        gate.enable(100);
        gate.acquire(100).await;
        // A second VERSION must not refill the window.
        gate.enable(100);
        let pending =
            tokio::time::timeout(std::time::Duration::from_millis(50), gate.acquire(1)).await;
        assert!(pending.is_err(), "re-enable refilled a live window");
        gate.close();
    }
}
} // mod flow

pub mod mux_pool {
//! Client-side physical MUX session pool for plain WebSocket (`ws://`).
//!
//! Secure WebSocket (`wss://`) connections, including Cloudflare FakeHost and Edge
//! fallback, are handled authoritatively by [`crate::wss_client::WssSessionPool`].

use crate::common::{
    configured_cipher, parse_ws_url_with_port, send_mux_parts, send_mux_parts_reuse,
    try_cloudflare_edges, HTTP_200_CONNECTION_ESTABLISHED,
};
use crate::crypto::XorCipher;
use crate::dns;
use crate::flow::CreditGate;
use crate::mux_writer::{
    recycle_encode_buf, recycle_encode_bufs, write_owned_frames_vectored, MuxFrameWriter,
};
use crate::protocol::{
    decode_version_payload, decode_window_payload, encode_version_payload, encode_window_payload,
    MuxCommand, MuxFrame, OwnedMuxFrame, SynPayload, U32HashMap, MUX_INITIAL_WINDOW_KIB,
    MUX_WINDOW_REFRESH,
};
use crate::proxy::{
    parse_authority_with_default, read_client_proxy_request, socks5_success_response, SocksCommand,
    TargetAddr,
};
use crate::runtime::{
    apply_listener_options, apply_socket_options, drain_join_set, enforce_target_policy,
    recycle_buf, relay_buf, wait_shutdown, RuntimeConfig,
};
use crate::udp_relay::handle_local_udp_proxy;
use crate::ws::{
    build_client_handshake_request, encode_ws_frame, read_frame, read_frame_with_cipher,
    validate_client_handshake_response,
};
use anyhow::{anyhow, bail, Result};
use std::sync::{
    atomic::{AtomicBool, AtomicU32, AtomicUsize, Ordering},
    Arc,
};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{
    tcp::{OwnedReadHalf, OwnedWriteHalf},
    TcpListener, TcpStream,
};
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

struct ClientStreamEntry {
    tx: mpsc::Sender<OwnedMuxFrame>,
    gate: Arc<CreditGate>,
}

struct SessionState {
    writer: Arc<MuxFrameWriter>,
    cipher: XorCipher,
    obfs: bool,
    // Read-mostly under concurrency (one shared lookup per DATA or WINDOW frame),
    // so a single RwLock<U32HashMap> holds both the downstream channel and upload gate.
    streams: Arc<std::sync::RwLock<U32HashMap<ClientStreamEntry>>>,
    /// Peer-advertised receive window in KiB once its VERSION arrived
    /// (0 => un-negotiated: v1 unbounded sends, no WINDOW refunds).
    peer_window_kib: AtomicU32,
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
        let (mut rd, mut wr) = socket.into_split();
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
        let hs_timeout = Duration::from_secs(cfg.connection_timeout.max(1));
        let response = crate::ws::read_http_headers_timeout(&mut rd, hs_timeout).await?;
        validate_client_handshake_response(&response, &key)?;
        let (writer, _writer_task) = MuxFrameWriter::spawn(wr);
        let cipher = configured_cipher(&cfg.key);
        let mut hello = b"MUX\n".to_vec();
        cipher.apply(&mut hello);
        writer
            .send(encode_ws_frame(&hello, 2, true).map_err(|e| anyhow!(e.to_string()))?)
            .await?;
        let mut frame_buf = crate::mux_writer::acquire_encode_buf();
        let read_res = timeout(
            hs_timeout,
            read_frame(
                &mut rd,
                Option::<&mut OwnedWriteHalf>::None,
                &mut frame_buf,
            ),
        )
        .await
        .map_err(|_| anyhow!("upstream MUX handshake OK timed out"))?;
        recycle_encode_buf(frame_buf);
        let Some((opcode, mut ok)) = read_res? else {
            bail!("upstream closed during MUX handshake")
        };
        if opcode != 2 {
            recycle_encode_buf(ok);
            bail!("invalid MUX handshake response opcode")
        };
        cipher.apply(&mut ok);
        let is_ok = ok == b"OK\n";
        recycle_encode_buf(ok);
        if !is_ok {
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
            streams: Arc::new(std::sync::RwLock::new(U32HashMap::default())),
            peer_window_kib: AtomicU32::new(0),
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
            // Unblock upload tasks parked on credit: no WINDOW will arrive.
            let mut streams = reader_state
                .streams
                .write()
                .unwrap_or_else(|e| e.into_inner());
            for entry in streams.values() {
                entry.gate.close();
            }
            streams.clear();
            drop(streams);
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
        initial_data: &[u8],
    ) -> Result<(u32, mpsc::Receiver<OwnedMuxFrame>, Arc<CreditGate>)> {
        if self.closed.load(Ordering::Acquire) {
            bail!("MUX session is closed")
        }

        let port_digits = if target.port >= 10000 {
            5
        } else if target.port >= 1000 {
            4
        } else if target.port >= 100 {
            3
        } else if target.port >= 10 {
            2
        } else {
            1
        };
        let target_len = target.host.len() + 1 + port_digits;
        let max_syn_initial = (u16::MAX as usize).saturating_sub(2 + target_len);
        let (syn_initial, remaining_initial) =
            initial_data.split_at(initial_data.len().min(max_syn_initial));

        let syn_payload = SynPayload::encode_parts(&target.host, target.port, syn_initial)
            .map_err(|e| anyhow!(e.to_string()))?;

        // Pre-allocate channel and credit gate outside the RwLock write guard.
        let (tx, rx) = mpsc::channel(crate::protocol::MUX_STREAM_QUEUE_CAP);
        let gate = CreditGate::new();

        let id = {
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
            let kib = self.peer_window_kib.load(Ordering::Acquire);
            if kib > 0 {
                gate.enable(i64::from(kib) * 1024);
            }
            streams.insert(
                id,
                ClientStreamEntry {
                    tx,
                    gate: gate.clone(),
                },
            );
            id
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
            if let Some(entry) = self
                .streams
                .write()
                .unwrap_or_else(|e| e.into_inner())
                .remove(&id)
            {
                entry.gate.close();
                self.active.fetch_sub(1, Ordering::AcqRel);
            }
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
                if let Some(entry) = self
                    .streams
                    .write()
                    .unwrap_or_else(|e| e.into_inner())
                    .remove(&id)
                {
                    entry.gate.close();
                    self.active.fetch_sub(1, Ordering::AcqRel);
                }
                self.closed.store(true, Ordering::Release);
                self.capacity_notify.notify_waiters();
                return Err(e);
            }
        }

        Ok((id, rx, gate))
    }
    async fn close_stream(&self, id: u32) {
        if let Some(entry) = self
            .streams
            .write()
            .unwrap_or_else(|e| e.into_inner())
            .remove(&id)
        {
            entry.gate.close();
            self.active.fetch_sub(1, Ordering::AcqRel);
            self.capacity_notify.notify_waiters();
        }
    }
}
async fn client_reader_loop(mut rd: OwnedReadHalf, state: Arc<SessionState>) -> Result<()> {
    let mut frame_buf = crate::mux_writer::acquire_encode_buf();
    let res = async {
        loop {
            let Some((opcode, payload)) = read_frame_with_cipher(
                &mut rd,
                Option::<&mut OwnedWriteHalf>::None,
                &mut frame_buf,
                Some(&state.cipher),
            )
            .await?
            else {
                break;
            };
            if opcode != 2 {
                recycle_encode_buf(payload);
                continue;
            }
            let frame = match MuxFrame::decode_owned(payload) {
                Ok(v) => v,
                Err(_) => continue,
            };
            // W3 control frames are session/stream-level: handle them here
            // instead of routing into per-stream channels (VERSION uses
            // stream id 0 and would otherwise be dropped silently).
            match frame.command {
                MuxCommand::Version => {
                    let decoded = decode_version_payload(frame.payload());
                    recycle_encode_buf(frame.into_storage());
                    if let Some((version, kib)) = decoded {
                        if version >= 1 {
                            let prev = state.peer_window_kib.swap(kib as u32, Ordering::AcqRel);
                            if prev == 0 {
                                let streams = state
                                    .streams
                                    .read()
                                    .unwrap_or_else(|e| e.into_inner());
                                let window = i64::from(kib) * 1024;
                                for entry in streams.values() {
                                    entry.gate.enable(window);
                                }
                                drop(streams);
                                tracing::debug!(kib, "peer VERSION received; send window enabled");
                            }
                        }
                    }
                    continue;
                }
                MuxCommand::Window => {
                    let stream_id = frame.stream_id;
                    let decoded = decode_window_payload(frame.payload());
                    recycle_encode_buf(frame.into_storage());
                    if let Some(credit) = decoded {
                        if let Some(entry) = state
                            .streams
                            .read()
                            .unwrap_or_else(|e| e.into_inner())
                            .get(&stream_id)
                        {
                            entry.gate.release(i64::from(credit));
                        }
                    }
                    continue;
                }
                _ => {}
            }
            let id = frame.stream_id;
            let send_res = {
                let streams = state.streams.read().unwrap_or_else(|e| e.into_inner());
                streams.get(&id).map(|entry| entry.tx.try_send(frame))
            };
            match send_res {
                Some(Ok(())) => {}
                Some(Err(mpsc::error::TrySendError::Full(f))) => {
                    recycle_encode_buf(f.into_storage());
                    tracing::warn!(
                        stream_id = id,
                        "mux RST: stream queue full; resetting stalled stream"
                    );
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
                Some(Err(mpsc::error::TrySendError::Closed(f))) => {
                    recycle_encode_buf(f.into_storage());
                    state.close_stream(id).await;
                }
                None => {}
            }
        }
        Ok::<(), anyhow::Error>(())
    }
    .await;
    recycle_encode_buf(frame_buf);
    res
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
        let missing = {
            let mut sessions = self.sessions.lock().await;
            sessions.retain(|s| !s.closed.load(Ordering::Acquire));
            target.saturating_sub(sessions.len())
        };
        if missing == 0 {
            self.consecutive_failures.store(0, Ordering::Release);
            return false;
        }
        let mut set = tokio::task::JoinSet::new();
        for _ in 0..missing {
            let cfg = self.cfg.clone();
            let notify = self.capacity_notify.clone();
            set.spawn(async move {
                SessionState::connect(&cfg, notify).await
            });
        }
        let mut added = 0usize;
        while let Some(res) = set.join_next().await {
            if let Ok(Ok(s)) = res {
                self.sessions.lock().await.push(s);
                added += 1;
                self.capacity_notify.notify_waiters();
            }
        }
        if added > 0 {
            self.consecutive_failures.store(0, Ordering::Release);
            false
        } else {
            let failures = self.consecutive_failures.fetch_add(1, Ordering::AcqRel) + 1;
            tracing::warn!(failures, "MUX physical session creation failed; will retry with backoff");
            false
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
            let notified = self.capacity_notify.notified();
            tokio::pin!(notified);
            notified.as_mut().enable();

            let (chosen, need_new) = {
                let mut sessions = self.sessions.lock().await;
                sessions.retain(|s| !s.closed.load(Ordering::Acquire));
                let chosen = sessions
                    .iter()
                    .filter(|s| s.available())
                    .min_by_key(|s| s.active.load(Ordering::Acquire))
                    .cloned();
                let need_new = sessions.len() < n;
                (chosen, need_new)
            };
            if let Some(s) = chosen {
                match s.open_stream(target, &initial_data).await {
                    Ok(r) => {
                        recycle_encode_buf(initial_data);
                        if need_new {
                            let pool = self.clone();
                            tokio::spawn(async move {
                                pool.replenish().await;
                            });
                        }
                        return Ok((s, r.0, r.1, r.2));
                    }
                    Err(_) if s.closed.load(Ordering::Acquire) || !s.available() => continue,
                    Err(e) => {
                        recycle_encode_buf(initial_data);
                        return Err(e);
                    }
                }
            }
            if !need_new {
                let _ = timeout(Duration::from_millis(500), &mut notified).await;
                continue;
            }

            let Ok(_creation_guard) = self.session_creation.try_lock() else {
                let _ = timeout(Duration::from_millis(200), &mut notified).await;
                continue;
            };
            let mut sessions = self.sessions.lock().await;
            sessions.retain(|s| !s.closed.load(Ordering::Acquire));
            if sessions.iter().any(|s| s.available()) || sessions.len() >= n {
                continue;
            }
            drop(sessions);

            let s = SessionState::connect(&self.cfg, self.capacity_notify.clone()).await?;
            let r = s.open_stream(target, &initial_data).await?;
            recycle_encode_buf(initial_data);
            self.sessions.lock().await.push(s.clone());
            self.capacity_notify.notify_waiters();
            return Ok((s, r.0, r.1, r.2));
        }
    }
}

async fn handle_tcp_proxy(
    mut local: TcpStream,
    cfg: RuntimeConfig,
    pool: Arc<MuxSessionPool>,
) -> Result<()> {
    let req = timeout(
        Duration::from_secs(cfg.connection_timeout.max(1)),
        read_client_proxy_request(&mut local),
    )
    .await
    .map_err(|_| anyhow!("client proxy request timed out"))??;
    if req.command == SocksCommand::UdpAssociate {
        return handle_local_udp_proxy(local, cfg, req.target).await;
    }
    if req.command != SocksCommand::Connect {
        bail!("MUX client supports CONNECT or UDP ASSOCIATE only")
    }
    enforce_target_policy(&cfg, &req.target)?;
    if req.is_socks5 {
        local.write_all(&socks5_success_response()).await?;
    } else if req.is_connect {
        local.write_all(HTTP_200_CONNECTION_ESTABLISHED).await?;
    }
    let mut initial_data = req.initial_payload.unwrap_or_default();
    if initial_data.is_empty() {
        let mut first_pkt = crate::mux_writer::acquire_encode_buf();
        first_pkt.resize(16384, 0);
        match timeout(Duration::from_millis(2), local.read(&mut first_pkt)).await {
            Ok(Ok(n)) if n > 0 => {
                crate::stats::add_bytes(n as i64, 0);
                first_pkt.truncate(n);
                initial_data = first_pkt;
            }
            _ => {
                recycle_encode_buf(first_pkt);
            }
        }
    }
    let (session, id, mut rx, gate) = pool.acquire(&req.target, initial_data).await?;
    let (mut local_rd, mut local_wr) = local.into_split();
    let writer = session.writer.clone();
    let cipher = session.cipher.clone();
    let obfs = session.obfs;
    let upload = tokio::spawn(async move {
        let mut buf = relay_buf(cfg.buffer_size).await;
        // Local first-frame scratch; after each `mem::take` the next
        // buffer comes from the writer-side encode pool (capacity reused).
        let mut frame_scratch = crate::mux_writer::acquire_encode_buf();
        let res = async {
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
            Result::<()>::Ok(())
        }
        .await;
        recycle_encode_buf(frame_scratch);
        recycle_buf(buf).await;
        res
    });
    let mut result = Result::<()>::Ok(());
    let mut remote_fin = false;
    // W3 receive-side refund accumulator (client -> server WINDOW).
    let mut refund_pending: u32 = 0;
    let mut data_batch: Vec<OwnedMuxFrame> = Vec::with_capacity(16);
    let mut deferred_ctrl: Option<OwnedMuxFrame> = None;
    loop {
        let frame = if let Some(ctrl) = deferred_ctrl.take() {
            ctrl
        } else {
            match rx.recv().await {
                Some(f) => f,
                None => break,
            }
        };
        match frame.command {
            MuxCommand::Data => {
                data_batch.push(frame);
                while data_batch.len() < 16 {
                    match rx.try_recv() {
                        Ok(next) if next.command == MuxCommand::Data => data_batch.push(next),
                        Ok(other) => {
                            deferred_ctrl = Some(other);
                            break;
                        }
                        Err(_) => break,
                    }
                }
                let write_res = write_owned_frames_vectored(&mut local_wr, &data_batch).await;
                recycle_encode_bufs(data_batch.drain(..).map(OwnedMuxFrame::into_storage));
                let written = match write_res {
                    Ok(n) => n,
                    Err(e) => {
                        if let Some(ctrl) = deferred_ctrl.take() {
                            recycle_encode_buf(ctrl.into_storage());
                        }
                        result = Err(e.into());
                        break;
                    }
                };
                if written > 0 {
                    crate::stats::add_bytes(0, written as i64);
                    let negotiated = session.peer_window_kib.load(Ordering::Relaxed) > 0;
                    if negotiated {
                        refund_pending = refund_pending.saturating_add(written as u32);
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
                                if let Some(ctrl) = deferred_ctrl.take() {
                                    recycle_encode_buf(ctrl.into_storage());
                                }
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
                recycle_encode_buf(frame.into_storage());
                let _ = local_wr.shutdown().await;
                remote_fin = true;
                break;
            }
            MuxCommand::Rst => {
                recycle_encode_buf(frame.into_storage());
                result = Err(anyhow!("upstream reset after CONNECT established"));
                break;
            }
            MuxCommand::Syn | MuxCommand::Version | MuxCommand::Window => {
                recycle_encode_buf(frame.into_storage());
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
    } else if result.is_err() {
        let _ = send_mux_parts(
            &session.writer,
            &session.cipher,
            id,
            MuxCommand::Rst,
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
} // mod mux_pool

pub mod mux_writer {
//! Dedicated per-session WebSocket frame writer.
//!
//! GoWay `muxOutboundWriter` parity: instead of every relay stream locking a
//! shared `Mutex<WriteHalf>` per frame (which collapses under many-stream
//! concurrency — measured c32 persistently below c8), each MUX session owns
//! one writer task fed by a bounded channel. Callers pre-encode complete WS
//! frames (see [`crate::ws::encode_ws_frame`], masking included) so the hot
//! loop is pure I/O, and consecutive queued frames are coalesced into
//! vectored writes.

use crate::crypto::XorCipher;
use crate::protocol::{write_frame_parts, MuxCommand, OwnedMuxFrame, U32HashMap};
use crate::ws::{next_mask, ws_header_into};
use anyhow::{anyhow, Result};
use std::collections::VecDeque;
use std::future::poll_fn;
use std::io::IoSlice;
use std::pin::Pin;
use std::sync::{
    atomic::{AtomicBool, AtomicUsize, Ordering},
    Arc, Mutex as StdMutex, OnceLock,
};
use std::time::Duration;
use tokio::io::{AsyncWrite, AsyncWriteExt};
use tokio::sync::mpsc;
use tokio::task::JoinHandle;
use tokio::time::timeout;

const WRITER_QUEUE: usize = 256;
const FEED_CAP: usize = 256;
const BATCH_MAX_FRAMES: usize = 32;
const BATCH_MAX_BYTES: usize = 1024 * 1024;
/// Wall-clock bound for one batch write. A peer that silently stops reading
/// (dead NAT/CDN path, black-holed connection) would otherwise park the
/// writer task forever and stall every stream multiplexed on the session.
/// Past this bound the write surfaces as a regular io error, so the session
/// tears down through the normal write-failure path.
const WRITE_BATCH_TIMEOUT: Duration = Duration::from_secs(30);
/// Encode-buffer pool: outbound frames are pre-encoded into an owned `Vec`
/// that the writer task only reads, then drops. Recycling those buffers
/// after a successful write removes the per-frame malloc/free that showed
/// up as `realloc`/`grow_amortized` on the encode hot path.
///
/// Kept deliberately small: one batch is at most [`BATCH_MAX_FRAMES`], and
/// a larger pool showed up as a clear setup-mode RSS regression (8:0) in
/// the first A/B — 32 × ~80 KiB ≈ 2.5 MiB retained ceiling.
const ENCODE_POOL_MAX_COUNT: usize = 32;
/// Frames top out near 67 KiB + WS header + obfs pad; pool only up to
/// ~80 KiB so a rare huge frame cannot pin memory.
const ENCODE_POOL_MAX_CAP: usize = 80 * 1024;
/// Shards for the encode-buffer pool.
///
/// One global mutex made every relay's encode/recycle pair serialise against
/// every other relay in the process, which caps packets-per-second scaling
/// once frames get small. Sharding spreads that across [`ENCODE_POOL_SHARDS`]
/// locks while keeping the *total* retained ceiling at
/// [`ENCODE_POOL_MAX_COUNT`] buffers: the per-shard cap is `MAX / SHARDS`,
/// because the first A/B measured a larger pool as a setup-mode RSS
/// regression and sharding deliberately must not grow it.
const ENCODE_POOL_SHARDS: usize = 4;
const ENCODE_POOL_SHARD_MAX_COUNT: usize = ENCODE_POOL_MAX_COUNT / ENCODE_POOL_SHARDS;
/// Per-stream byte credit added each deficit round (GoWay parity).
/// Set to 128KB so that even maximum-size MUX frames (~67KB) can be
/// dispatched in a single round without deficit underflow stall.
const DRR_QUANTUM: usize = 128 * 1024;
/// Cap accumulated credit so a long-idle stream cannot hog the link.
const DRR_MAX_DEFICIT: usize = 512 * 1024;

#[repr(align(64))]
struct PoolShard {
    len: AtomicUsize,
    bufs: StdMutex<Vec<Vec<u8>>>,
}

static ENCODE_POOL: OnceLock<[PoolShard; ENCODE_POOL_SHARDS]> = OnceLock::new();
static ENCODE_POOL_NEXT_SHARD: AtomicUsize = AtomicUsize::new(0);

thread_local! {
    /// Shard index for this thread, assigned on first use.
    ///
    /// A per-thread index (rather than a shared round-robin counter) keeps the
    /// frame hot path free of any contended cache line after init, and
    /// guarantees a buffer acquired and recycled on the same thread lands in
    /// the same shard — which is what `encode_pool_reuses_capacity` relies on.
    static ENCODE_POOL_SHARD: usize =
        ENCODE_POOL_NEXT_SHARD.fetch_add(1, Ordering::Relaxed) % ENCODE_POOL_SHARDS;
}

#[inline(always)]
fn pool_shard() -> usize {
    ENCODE_POOL_SHARD.with(|s| *s)
}

#[inline(always)]
fn encode_pool() -> &'static [PoolShard; ENCODE_POOL_SHARDS] {
    ENCODE_POOL.get_or_init(|| {
        std::array::from_fn(|_| PoolShard {
            len: AtomicUsize::new(0),
            bufs: StdMutex::new(Vec::with_capacity(ENCODE_POOL_SHARD_MAX_COUNT)),
        })
    })
}

/// Takes a pooled encode buffer (cleared, capacity retained) or a fresh
/// empty `Vec` when every shard is empty. The writer returns written frames
/// via [`recycle_encode_bufs`].
///
/// Starts at this thread's own shard and only scans outwards on a miss,
/// using a relaxed atomic length check to skip empty shards without locking.
pub(crate) fn acquire_encode_buf() -> Vec<u8> {
    let shards = encode_pool();
    let start = pool_shard();
    for offset in 0..ENCODE_POOL_SHARDS {
        let shard = &shards[(start + offset) % ENCODE_POOL_SHARDS];
        if shard.len.load(Ordering::Relaxed) == 0 {
            continue;
        }
        let mut pool = shard.bufs.lock().unwrap_or_else(|e| e.into_inner());
        if let Some(mut buf) = pool.pop() {
            shard.len.store(pool.len(), Ordering::Relaxed);
            drop(pool);
            buf.clear();
            return buf;
        }
        shard.len.store(0, Ordering::Relaxed);
    }
    Vec::new()
}

/// Returns a post-write frame buffer to the pool (best-effort: zero-cap or
/// oversized buffers are dropped, as are buffers when every shard is full).
pub(crate) fn recycle_encode_buf(mut buf: Vec<u8>) {
    if buf.capacity() == 0 || buf.capacity() > ENCODE_POOL_MAX_CAP {
        return;
    }
    buf.clear();
    let shards = encode_pool();
    let start = pool_shard();
    for offset in 0..ENCODE_POOL_SHARDS {
        let shard = &shards[(start + offset) % ENCODE_POOL_SHARDS];
        if shard.len.load(Ordering::Relaxed) >= ENCODE_POOL_SHARD_MAX_COUNT {
            continue;
        }
        let mut pool = shard.bufs.lock().unwrap_or_else(|e| e.into_inner());
        if pool.len() < ENCODE_POOL_SHARD_MAX_COUNT {
            pool.push(buf);
            shard.len.store(pool.len(), Ordering::Relaxed);
            return;
        }
    }
}

/// Returns a batch of post-write frame buffers to the pool, taking at most
/// one lock per non-full shard (zero allocation).
pub(crate) fn recycle_encode_bufs<I>(bufs: I)
where
    I: IntoIterator<Item = Vec<u8>>,
{
    let mut iter = bufs.into_iter().peekable();
    if iter.peek().is_none() {
        return;
    }
    let shards = encode_pool();
    let start = pool_shard();
    for offset in 0..ENCODE_POOL_SHARDS {
        if iter.peek().is_none() {
            return;
        }
        let shard = &shards[(start + offset) % ENCODE_POOL_SHARDS];
        if shard.len.load(Ordering::Relaxed) >= ENCODE_POOL_SHARD_MAX_COUNT {
            continue;
        }
        let mut pool = shard.bufs.lock().unwrap_or_else(|e| e.into_inner());
        while pool.len() < ENCODE_POOL_SHARD_MAX_COUNT {
            match iter.next() {
                Some(mut buf) => {
                    if buf.capacity() == 0 || buf.capacity() > ENCODE_POOL_MAX_CAP {
                        continue;
                    }
                    buf.clear();
                    pool.push(buf);
                }
                None => {
                    shard.len.store(pool.len(), Ordering::Relaxed);
                    return;
                }
            }
        }
        shard.len.store(pool.len(), Ordering::Relaxed);
    }
}

/// One pre-encoded outbound frame with scheduling identity.
///
/// `priority` frames (WS handshake hellos/pings via [`MuxFrameWriter::send`]
/// and non-DATA MUX commands via [`MuxFrameWriter::send_mux`]) drain from a
/// dedicated lane ahead of bulk DATA; DATA frames are deficit-round-robin
/// scheduled per `stream_id` (GoWay `muxOutboundWriter` parity).
///
/// `stream_id` is `Some` for MUX control frames (SYN/FIN/RST) so the
/// scheduler can keep a stream-closing control behind its own queued DATA.
/// `yield_to_data` is set ONLY for FIN/RST: a SYN must always lead (it
/// creates the server-side stream — yielding it behind its own DATA makes
/// the peer drop that DATA as unknown-stream, a live-caught burst bug).
/// System frames (hello/ping) carry `None` and always jump the queue.
pub(crate) struct OutboundFrame {
    stream_id: Option<u32>,
    priority: bool,
    yield_to_data: bool,
    bytes: Vec<u8>,
}

pub(crate) struct MuxFrameWriter {
    tx: mpsc::Sender<OutboundFrame>,
    closed: Arc<AtomicBool>,
}

impl MuxFrameWriter {
    /// Spawns the writer task owning `w` and returns the shared handle plus
    /// the task. Dropping the last handle closes the channel; the task then
    /// flushes leftovers best-effort, shuts the write half down, and exits.
    ///
    /// NB: the task holds only the `closed` flag, never a full `Arc<Self>`
    /// (which would keep a sender alive and prevent channel close forever).
    pub fn spawn<W>(w: W) -> (Arc<Self>, JoinHandle<()>)
    where
        W: AsyncWrite + Unpin + Send + 'static,
    {
        let (tx, rx) = mpsc::channel(FEED_CAP);
        let closed = Arc::new(AtomicBool::new(false));
        let this = Arc::new(Self {
            tx,
            closed: closed.clone(),
        });
        let handle = tokio::spawn(async move {
            writer_loop(w, rx).await;
            closed.store(true, Ordering::Release);
        });
        (this, handle)
    }

    /// Queues one pre-encoded system frame (WS handshake hello, heartbeat
    /// ping). System frames take the priority lane, ahead of MUX DATA.
    /// Fails fast once the transport died; otherwise back-pressures when
    /// the queue is full.
    pub async fn send(&self, frame: Vec<u8>) -> Result<()> {
        self.send_frame(OutboundFrame {
            stream_id: None,
            priority: true,
            yield_to_data: false,
            bytes: frame,
        })
        .await
    }

    /// Queues one pre-encoded MUX frame. Non-DATA commands take the priority
    /// lane; DATA frames are DRR-scheduled per stream. Same failure and
    /// backpressure contract as [`MuxFrameWriter::send`].
    pub async fn send_mux(
        &self,
        stream_id: u32,
        command: MuxCommand,
        frame: Vec<u8>,
    ) -> Result<()> {
        // Only stream-CLOSING controls yield to their own queued DATA.
        // SYN must lead (it creates the peer-side stream); DATA never
        // takes the priority lane at all.
        let yield_to_data = command == MuxCommand::Fin || command == MuxCommand::Rst;
        self.send_frame(OutboundFrame {
            stream_id: Some(stream_id),
            priority: command != MuxCommand::Data,
            yield_to_data,
            bytes: frame,
        })
        .await
    }

    async fn send_frame(&self, frame: OutboundFrame) -> Result<()> {
        if self.closed.load(Ordering::Acquire) {
            return Err(anyhow!("mux writer closed"));
        }
        self.tx.send(frame).await.map_err(|_| {
            self.closed.store(true, Ordering::Release);
            anyhow!("mux writer closed")
        })
    }

    pub fn is_closed(&self) -> bool {
        self.closed.load(Ordering::Acquire)
    }
}

struct StreamQueue {
    frames: VecDeque<Vec<u8>>,
    deficit: usize,
}

/// Deficit-round-robin scheduler over one MUX session's outbound frames.
///
/// Mirrors GoWay's `muxOutboundWriter`: a priority lane (system frames plus
/// SYN/FIN/RST) drains first; DATA frames rotate strictly across streams
/// with a 64KB quantum each. Per-stream FIFOs preserve in-order delivery;
/// queue depth is bounded at [`WRITER_QUEUE`] frames total (priority + all
/// streams), preserving the old backpressure contract.
struct Scheduler {
    priority: VecDeque<(Option<u32>, bool, Vec<u8>)>,
    streams: U32HashMap<StreamQueue>,
    rotation: Vec<u32>,
    pos: usize,
    total: usize,
}

impl Scheduler {
    fn new() -> Self {
        Self {
            priority: VecDeque::new(),
            streams: U32HashMap::default(),
            rotation: Vec::new(),
            pos: 0,
            total: 0,
        }
    }

    fn len(&self) -> usize {
        self.total
    }

    fn is_empty(&self) -> bool {
        self.total == 0
    }

    #[cfg(test)]
    fn live_stream_count(&self) -> usize {
        self.streams.len()
    }

    fn push(&mut self, frame: OutboundFrame) {
        // Termination guard: the deficit caps at DRR_MAX_DEFICIT, so an
        // oversized frame could never afford emission and would wedge the
        // stream. MUX frames top out near 67KB — far below the cap.
        debug_assert!(frame.bytes.len() <= DRR_MAX_DEFICIT);
        if frame.priority {
            self.priority
                .push_back((frame.stream_id, frame.yield_to_data, frame.bytes));
            self.total += 1;
            return;
        }
        let Some(stream_id) = frame.stream_id else {
            // Unreachable: only send_mux (always Some) reaches here with
            // priority == false. Guard anyway to never strand a frame.
            self.priority.push_back((None, false, frame.bytes));
            self.total += 1;
            return;
        };
        let q = self.streams.entry(stream_id).or_insert_with(|| {
            self.rotation.push(stream_id);
            StreamQueue {
                frames: VecDeque::new(),
                deficit: 0,
            }
        });
        q.frames.push_back(frame.bytes);
        self.total += 1;
    }

    /// Pops the next frame to emit: priority lane first, then one DRR step.
    ///
    /// Intra-stream order wins over cross-stream priority, but ONLY for
    /// stream-closing controls (FIN/RST): those yield while their own
    /// stream still has queued DATA (it stays queued), otherwise the peer
    /// would close early and drop the tail. SYN never yields — it creates
    /// the peer-side stream, so yielding it makes the peer drop its own
    /// DATA as unknown-stream. System frames (`None`) and controls for
    /// streams with empty queues jump immediately.
    fn next(&mut self) -> Option<Vec<u8>> {
        if !self.priority.is_empty() {
            let ready_idx =
                self.priority
                    .iter()
                    .enumerate()
                    .find_map(|(idx, &(sid, yield_to_data, _))| {
                        let blocked_by_data = yield_to_data
                            && sid.is_some_and(|id| {
                                self.streams.get(&id).is_some_and(|q| !q.frames.is_empty())
                            });
                        if blocked_by_data {
                            return None;
                        }
                        if let Some(id) = sid {
                            let blocked_by_earlier_ctrl = self
                                .priority
                                .iter()
                                .take(idx)
                                .any(|&(earlier_sid, _, _)| earlier_sid == Some(id));
                            if blocked_by_earlier_ctrl {
                                return None;
                            }
                        }
                        Some(idx)
                    });
            if let Some(idx) = ready_idx {
                let (sid_opt, yield_flag, frame) = self.priority.remove(idx).unwrap();
                self.total -= 1;
                if yield_flag {
                    if let Some(id) = sid_opt {
                        self.remove_stream(&id);
                    }
                }
                return Some(frame);
            }
        }
        if self.total == 0 {
            if !self.streams.is_empty() {
                self.streams.clear();
                self.rotation.clear();
                self.pos = 0;
            }
            return None;
        }
        if self.rotation.is_empty() {
            return None;
        }
        // Strict rotation: every stream gets +quantum per round and the
        // first affordable head-of-queue wins. Deficits persist across
        // calls, so no stream starves; unaffordable rounds simply emit
        // nothing until credit accumulates.
        let mut checked = 0;
        while checked < self.rotation.len() {
            if self.pos >= self.rotation.len() {
                self.pos = 0;
            }
            let sid = self.rotation[self.pos];
            self.pos += 1;
            checked += 1;
            if let Some(q) = self.streams.get_mut(&sid) {
                if !q.frames.is_empty() {
                    q.deficit = (q.deficit + DRR_QUANTUM).min(DRR_MAX_DEFICIT);
                    let affordable = q.frames.front().map(|f| f.len()).unwrap_or(usize::MAX);
                    if affordable <= q.deficit {
                        q.deficit -= affordable;
                        let frame = q.frames.pop_front();
                        self.total -= 1;
                        if frame.is_some() {
                            return frame;
                        }
                    }
                    continue;
                }
            }
            // Drop streams whose queue drained eagerly. Streams closed
            // without a closing control (RST / abort paths that only
            // clear the connection table) would otherwise linger in
            // `rotation` forever, forcing every `next()` to scan them.
            // `remove_stream` repairs `pos`; the priority-lane yield
            // check above treats a missing entry as unblocked, so a
            // pending FIN/RST still fires after its DATA.
            self.remove_stream(&sid);
        }
        None
    }

    fn remove_stream(&mut self, sid: &u32) {
        self.streams.remove(sid);
        if let Some(idx) = self.rotation.iter().position(|s| s == sid) {
            self.rotation.remove(idx);
            if self.pos > idx {
                self.pos -= 1;
            }
            if self.pos >= self.rotation.len() && !self.rotation.is_empty() {
                self.pos = 0;
            }
        }
    }
}

/// Max random padding appended to MUX DATA frames when obfuscation is on
/// (GoWay `-obfs` parity: same 1400-byte cap).
pub(crate) const OBFS_PAD_MAX: usize = 1400;

/// Encodes one MUX frame already wrapped in its WebSocket frame, in a
/// single buffer with a single allocation: `[WS header][mask?][MUX header |
/// payload][pad?]`.
///
/// Previously this took two allocations and a full extra copy (MUX `Vec`
/// then WS `Vec`). Layout: the WS header describes the MUX frame length
/// (`7 + payload.len()`, plus obfs pad when enabled), and the
/// cipher + WS mask (client side) cover the whole region including pad.
///
/// When `obfs` is set, DATA frames carry `[0, OBFS_PAD_MAX]` random pad
/// bytes inside the WS payload past the declared MUX length. Receivers
/// (old and new, Go and Rust) slice by the declared length, so padding is
/// transparently ignored — unilateral deployment is safe.
pub(crate) fn encode_mux_ws_frame(
    out: &mut Vec<u8>,
    stream_id: u32,
    command: MuxCommand,
    payload: &[u8],
    cipher: &XorCipher,
    masked: bool,
    obfs: bool,
) -> Result<()> {
    let mux_len = 7 + payload.len();
    // Unilateral obfuscation: random pad past the declared MUX length.
    // Computed up-front so the WS header covers it — otherwise the peer
    // would leave pad bytes in the stream and desync the next frame.
    // Receivers slice by the declared MUX length, so the tail is ignored
    // by old and new peers alike. DATA only — control frames stay exact.
    let pad_len = if obfs && command == MuxCommand::Data {
        (u32::from_ne_bytes(next_mask()) as usize) % (OBFS_PAD_MAX + 1)
    } else {
        0
    };
    let total = mux_len + pad_len;
    let mut ws_header = [0u8; 14];
    let ws_header_len = ws_header_into(&mut ws_header, total, 0x2, masked);
    let mask_len = if masked { 4 } else { 0 };
    out.clear();
    let needed = ws_header_len + mask_len + total;
    if needed > out.capacity() {
        if needed > 4096 && needed <= ENCODE_POOL_MAX_CAP {
            out.reserve_exact(ENCODE_POOL_MAX_CAP);
        } else {
            out.reserve(needed);
        }
    }
    out.extend_from_slice(&ws_header[..ws_header_len]);
    let mask_key = if masked {
        let key = next_mask();
        out.extend_from_slice(&key);
        Some(key)
    } else {
        None
    };
    let mux_start = out.len();
    write_frame_parts(out, stream_id, command, payload).map_err(|e| {
        // Keep `out` reusable on error: only the WS header (and key) were
        // written before the MUX payload, so truncate back to empty.
        out.clear();
        anyhow!(e.to_string())
    })?;
    debug_assert_eq!(out.len() - mux_start, mux_len);
    // Pad lands inside the WS payload and gets masked/ciphered with the
    // rest; the receiver slices by the declared MUX length and ignores it.
    if pad_len > 0 {
        let start = out.len();
        out.resize(start + pad_len, 0);
        for chunk in out[start..].chunks_mut(4) {
            let m = next_mask();
            chunk.copy_from_slice(&m[..chunk.len()]);
        }
    }
    // Single pass over the payload: cipher keystream XOR WS mask.
    // Offsets are region-relative (key byte `i` maps to
    // `keystream[i % XOR_KEY_SIZE]`), exactly matching two sequential
    // `TransformInPlace` + mask passes. Mask period 4 divides the 8-byte
    // word, so bulk stays word-at-a-time.
    let region = &mut out[mux_start..];
    match mask_key {
        None => cipher.apply(region),
        Some(key) => {
            let ks = cipher.keystream();
            if ks.is_empty() {
                crate::ws::apply_ws_mask(region, key);
            } else {
                crate::crypto::apply_fused_xor(region, ks, key);
            }
        }
    }
    Ok(())
}

async fn writer_loop<W>(mut w: W, mut rx: mpsc::Receiver<OutboundFrame>)
where
    W: AsyncWrite + Unpin + Send + 'static,
{
    let mut sched = Scheduler::new();
    let mut batch: Vec<Vec<u8>> = Vec::with_capacity(BATCH_MAX_FRAMES);
    loop {
        // Admit: block for the first frame when the scheduler is empty
        // (channel close here means all owners gone); otherwise drain the
        // feed opportunistically while under the total queue cap. A full
        // scheduler/feed makes senders block, preserving backpressure.
        if sched.is_empty() {
            match rx.recv().await {
                Some(frame) => sched.push(frame),
                None => break,
            }
        }
        while sched.len() < WRITER_QUEUE {
            match rx.try_recv() {
                Ok(frame) => sched.push(frame),
                Err(_) => break,
            }
        }
        // Emit one DRR-ordered batch (priority lane first, then one frame
        // per affordable stream); vectored write preserved.
        //
        // A `None` here only means "nothing affordable THIS round" — each
        // next() call adds a quantum to every stream, so retrying while the
        // scheduler is non-empty always reaches an emission within a few
        // rounds (frames are far below the deficit cap). Breaking at the
        // first None instead would strand the tail whenever no further
        // arrivals come to wake us (caught live: 4MiB tail stall).
        // `batch` is drained (not cleared) after each write so frame buffers
        // return to the encode pool instead of being freed.
        debug_assert!(batch.is_empty());
        let mut bytes = 0usize;
        while batch.len() < BATCH_MAX_FRAMES && bytes < BATCH_MAX_BYTES {
            match sched.next() {
                Some(frame) => {
                    bytes += frame.len();
                    batch.push(frame);
                }
                None if sched.is_empty() => break,
                None => continue,
            }
        }
        if batch.is_empty() {
            // Scheduler non-empty but nothing affordable (DRR credit still
            // accumulating) and feed drained: park until a new frame
            // arrives or the channel closes. recv() also returns frames
            // already sitting in the feed, so this cannot deadlock.
            // Deficits persist across rounds — no stream starves.
            match rx.recv().await {
                Some(frame) => {
                    sched.push(frame);
                    continue;
                }
                // Feed closed: drain everything still scheduled before
                // exiting — abandoning queued frames here would silently
                // drop tails (caught live: 56 stranded frames at close).
                // Terminates: every next() either emits (total--) or
                // accrues deficit, and frames are far below the deficit
                // cap, so emission is always reached within a few calls.
                // The iteration bound is pure paranoia against a hang at
                // teardown (a hang there is worse than a drop).
                None => {
                    let mut spins = 0usize;
                    let max_spins = sched.len() * 8 + 32;
                    while !sched.is_empty() && spins < max_spins {
                        spins += 1;
                        if let Some(frame) = sched.next() {
                            batch.push(frame);
                            if batch.len() >= BATCH_MAX_FRAMES {
                                let _ = write_batch(&mut w, &batch).await;
                                recycle_encode_bufs(batch.drain(..));
                            }
                        }
                    }
                    if !batch.is_empty() {
                        let _ = write_batch(&mut w, &batch).await;
                        recycle_encode_bufs(batch.drain(..));
                    }
                    break;
                }
            }
        }
        if write_batch(&mut w, &batch).await.is_err() {
            recycle_encode_bufs(batch.drain(..));
            break;
        }
        recycle_encode_bufs(batch.drain(..));
    }
    // Shutdown drain path already wrote leftovers above; recycle anything
    // still sitting in `batch` from a partial fill that never wrote.
    recycle_encode_bufs(batch.drain(..));
    let _ = w.shutdown().await;
}

async fn write_batch<W>(w: &mut W, batch: &[Vec<u8>]) -> std::io::Result<()>
where
    W: AsyncWrite + Unpin,
{
    match timeout(WRITE_BATCH_TIMEOUT, write_batch_inner(w, batch)).await {
        Ok(res) => res,
        Err(_) => Err(std::io::Error::new(
            std::io::ErrorKind::TimedOut,
            "mux writer batch write timed out",
        )),
    }
}

async fn write_batch_inner<W>(w: &mut W, batch: &[Vec<u8>]) -> std::io::Result<()>
where
    W: AsyncWrite + Unpin,
{
    if batch.len() == 1 {
        w.write_all(&batch[0]).await?;
        return Ok(());
    }
    let mut idx = 0usize;
    let mut off = 0usize;
    while idx < batch.len() {
        let n = poll_fn(|cx| {
            let count = (batch.len() - idx).min(BATCH_MAX_FRAMES);
            let mut slices: [IoSlice<'_>; BATCH_MAX_FRAMES] =
                [IoSlice::new(&[]); BATCH_MAX_FRAMES];
            slices[0] = IoSlice::new(&batch[idx][off..]);
            for (i, buf) in batch[idx + 1..idx + count].iter().enumerate() {
                slices[i + 1] = IoSlice::new(buf);
            }
            Pin::new(&mut *w).poll_write_vectored(cx, &slices[..count])
        })
        .await?;
        if n == 0 {
            return Err(std::io::Error::new(
                std::io::ErrorKind::WriteZero,
                "vectored write returned zero",
            ));
        }
        let mut remaining = n;
        while remaining > 0 {
            let available = batch[idx].len() - off;
            if remaining < available {
                off += remaining;
                remaining = 0;
            } else {
                remaining -= available;
                idx += 1;
                off = 0;
            }
        }
    }
    Ok(())
}

/// Writes a slice of [`OwnedMuxFrame`] payloads to `w` using vectored I/O
/// (`writev` / `WSASend`), returning the total payload bytes written.
pub(crate) async fn write_owned_frames_vectored<W>(
    w: &mut W,
    batch: &[OwnedMuxFrame],
) -> std::io::Result<usize>
where
    W: AsyncWrite + Unpin,
{
    let mut total_len = 0usize;
    for f in batch {
        total_len += f.payload().len();
    }
    if total_len == 0 {
        return Ok(0);
    }
    if batch.len() == 1 {
        w.write_all(batch[0].payload()).await?;
        return Ok(total_len);
    }
    let mut idx = 0usize;
    let mut off = 0usize;
    while idx < batch.len() {
        if batch[idx].payload().len() == off {
            idx += 1;
            off = 0;
            continue;
        }
        let n = poll_fn(|cx| {
            let count = (batch.len() - idx).min(BATCH_MAX_FRAMES);
            let mut slices: [IoSlice<'_>; BATCH_MAX_FRAMES] =
                [IoSlice::new(&[]); BATCH_MAX_FRAMES];
            slices[0] = IoSlice::new(&batch[idx].payload()[off..]);
            let mut slice_count = 1;
            for f in &batch[idx + 1..idx + count] {
                let p = f.payload();
                if !p.is_empty() {
                    slices[slice_count] = IoSlice::new(p);
                    slice_count += 1;
                }
            }
            Pin::new(&mut *w).poll_write_vectored(cx, &slices[..slice_count])
        })
        .await?;
        if n == 0 {
            return Err(std::io::Error::new(
                std::io::ErrorKind::WriteZero,
                "vectored write returned zero",
            ));
        }
        let mut remaining = n;
        while remaining > 0 {
            let available = batch[idx].payload().len() - off;
            if remaining < available {
                off += remaining;
                remaining = 0;
            } else {
                remaining -= available;
                idx += 1;
                off = 0;
            }
        }
    }
    Ok(total_len)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::protocol::MuxFrame;
    use crate::ws::{encode_ws_frame, read_frame};
    use tokio::io::{duplex, AsyncWriteExt};

    #[tokio::test]
    async fn fused_encoding_matches_two_step_path() {
        let cipher = XorCipher::new("fuse-key");
        for (command, payload_len) in [
            (MuxCommand::Syn, 0),
            (MuxCommand::Data, 7),
            (MuxCommand::Data, 60000),
            (MuxCommand::Fin, 0),
        ] {
            let payload = vec![0xABu8; payload_len];
            for masked in [false, true] {
                // Fused single-buffer path (obfs off: exact lengths).
                let mut fused = Vec::new();
                encode_mux_ws_frame(
                    &mut fused, 0x01020304, command, &payload, &cipher, masked, false,
                )
                .unwrap();
                // Lengths must match the legacy path exactly (mask keys
                // are random per frame, so bytes themselves differ).
                let mut mux = Vec::new();
                write_frame_parts(&mut mux, 0x01020304, command, &payload).unwrap();
                cipher.apply(&mut mux);
                let legacy = encode_ws_frame(&mux, 2, masked).unwrap();
                assert_eq!(
                    fused.len(),
                    legacy.len(),
                    "length mismatch for {command:?}/{masked}"
                );
                // Round-trip: WS decode, XOR off, MUX decode.
                let (mut a, mut b) = duplex(128 * 1024);
                a.write_all(&fused).await.unwrap();
                drop(a);
                let mut buf = Vec::new();
                let (opcode, mut got) = read_frame(
                    &mut b,
                    Option::<&mut tokio::io::DuplexStream>::None,
                    &mut buf,
                )
                .await
                .unwrap()
                .unwrap();
                assert_eq!(opcode, 2);
                cipher.apply(&mut got);
                let frame = MuxFrame::decode(&got).unwrap();
                assert_eq!(frame.stream_id, 0x01020304);
                assert_eq!(frame.command, command);
                assert_eq!(frame.payload, payload);
            }
        }
    }

    #[tokio::test]
    async fn obfs_pads_data_only_within_cap() {
        let cipher = XorCipher::new("obfs-key");
        let payload = vec![0xCDu8; 100];
        // Control frames are never padded.
        for command in [MuxCommand::Syn, MuxCommand::Fin, MuxCommand::Rst] {
            let mut out = Vec::new();
            encode_mux_ws_frame(&mut out, 1, command, &payload, &cipher, true, true).unwrap();
            assert_eq!(out.len(), mux_ws_len(100, true), "control must stay exact");
        }
        // DATA frames gain [0, OBFS_PAD_MAX] random tail past the declared
        // length; the declared prefix always decodes.
        let mut seen_pad = false;
        for _ in 0..16 {
            let mut out = Vec::new();
            encode_mux_ws_frame(&mut out, 9, MuxCommand::Data, &payload, &cipher, true, true)
                .unwrap();
            let min_len = mux_ws_len(100, true);
            let max_len = mux_ws_len(100 + OBFS_PAD_MAX, true);
            assert!((min_len..=max_len).contains(&out.len()));
            if out.len() > min_len {
                seen_pad = true;
            }
            // Strip the WS layer using the header's own length field
            // (pad is inside the WS payload now): unmask, decrypt, then
            // verify the MUX header still declares exactly 100 bytes.
            assert_eq!(out[0] & 0x0F, 0x2);
            let (ws_len, header_len) = match out[1] & 0x7F {
                126 => (
                    u16::from_be_bytes(out[2..4].try_into().unwrap()) as usize,
                    4,
                ),
                127 => (
                    u64::from_be_bytes(out[2..10].try_into().unwrap()) as usize,
                    10,
                ),
                n => (n as usize, 2),
            };
            assert!((107..=107 + OBFS_PAD_MAX).contains(&ws_len));
            let mask: [u8; 4] = out[header_len..header_len + 4].try_into().unwrap();
            let mut mux_frame = out[header_len + 4..header_len + 4 + ws_len].to_vec();
            for (i, byte) in mux_frame.iter_mut().enumerate() {
                *byte ^= mask[i & 3];
            }
            cipher.apply(&mut mux_frame);
            let frame = MuxFrame::decode(&mux_frame).unwrap();
            assert_eq!(frame.stream_id, 9);
            assert_eq!(frame.payload, payload);
        }
        assert!(seen_pad, "16 padded frames all came out unpadded (p ~= 0)");
    }

    /// Wire length of an unpadded MUX+WS frame (mirrors the encoder).
    fn mux_ws_len(payload_len: usize, masked: bool) -> usize {
        let mux_len = 7 + payload_len;
        let ws_header_len = if mux_len <= 125 {
            2
        } else if mux_len <= u16::MAX as usize {
            4
        } else {
            10
        };
        ws_header_len + if masked { 4 } else { 0 } + mux_len
    }

    #[test]
    fn encode_pool_reuses_capacity() {
        let mut a = acquire_encode_buf();
        a.reserve(64 * 1024);
        a.extend_from_slice(&[0xAB; 64 * 1024]);
        let cap = a.capacity();
        recycle_encode_buf(a);
        let b = acquire_encode_buf();
        assert_eq!(b.len(), 0, "pooled buf must be cleared");
        assert!(
            b.capacity() >= cap,
            "pooled buf must retain capacity (got {} want >= {})",
            b.capacity(),
            cap
        );
        // Oversized buffers are not retained.
        let mut huge = Vec::with_capacity(ENCODE_POOL_MAX_CAP + 1);
        huge.extend_from_slice(&[1u8; ENCODE_POOL_MAX_CAP + 1]);
        recycle_encode_buf(huge);
    }

    #[tokio::test]
    async fn writer_task_exits_when_handles_dropped() {
        let (a, _b) = duplex(1024 * 1024);
        let (writer, handle) = MuxFrameWriter::spawn(a);
        drop(writer);
        tokio::time::timeout(std::time::Duration::from_secs(5), handle)
            .await
            .expect("writer task must exit after last handle dropped")
            .unwrap();
    }

    #[tokio::test]
    async fn preserves_order_across_coalesced_batch() {
        let (a, mut b) = duplex(1024 * 1024);
        let (writer, handle) = MuxFrameWriter::spawn(a);
        // Flood the queue so the loop is forced to coalesce.
        for i in 0u8..64 {
            let payload = vec![i; 1024];
            writer
                .send(encode_ws_frame(&payload, 2, false).unwrap())
                .await
                .unwrap();
        }
        drop(writer);
        let mut buf = Vec::new();
        for i in 0u8..64 {
            let (opcode, payload) = read_frame(
                &mut b,
                Option::<&mut tokio::io::DuplexStream>::None,
                &mut buf,
            )
            .await
            .unwrap()
            .unwrap();
            assert_eq!(opcode, 2);
            assert_eq!(payload, vec![i; 1024]);
        }
        handle.await.unwrap();
    }

    #[tokio::test]
    async fn send_fails_after_transport_death() {
        let (a, b) = duplex(64);
        let (writer, handle) = MuxFrameWriter::spawn(a);
        drop(b);
        // Small queue + dead peer: sends must eventually fail, never hang.
        let mut failed = false;
        for _ in 0..(WRITER_QUEUE + 8) {
            let payload = vec![9u8; 1024];
            if writer
                .send(encode_ws_frame(&payload, 2, false).unwrap())
                .await
                .is_err()
            {
                failed = true;
                break;
            }
        }
        assert!(failed, "writer must report transport death");
        assert!(writer.is_closed());
        handle.await.unwrap();
    }

    fn drr_frame(stream_id: u32, priority: bool, len: usize, tag: u8) -> OutboundFrame {
        drr_frame_yield(stream_id, priority, priority, len, tag)
    }

    /// priority + yield_to_data set independently (mirrors send_mux: only
    /// FIN/RST yield; SYN never does even though it is priority).
    fn drr_frame_yield(
        stream_id: u32,
        priority: bool,
        yield_to_data: bool,
        len: usize,
        tag: u8,
    ) -> OutboundFrame {
        let mut bytes = vec![tag; len];
        if !bytes.is_empty() {
            bytes[0] = tag;
        }
        OutboundFrame {
            stream_id: Some(stream_id),
            priority,
            yield_to_data,
            bytes,
        }
    }

    fn drr_system(len: usize, tag: u8) -> OutboundFrame {
        OutboundFrame {
            stream_id: None,
            priority: true,
            yield_to_data: false,
            bytes: vec![tag; len],
        }
    }

    #[test]
    fn drr_interactive_not_starved_by_bulk() {
        // 4 bulk streams queue 8 x 64KB DATA each BEFORE the interactive
        // stream arrives. Without fairness the interactive frames would sit
        // at positions 32..35; DRR must interleave them from round one.
        let mut sched = Scheduler::new();
        for sid in 1..=4u32 {
            for _ in 0..8 {
                sched.push(drr_frame(sid, false, 65536, sid as u8));
            }
        }
        for _ in 0..4 {
            sched.push(drr_frame(9, false, 100, 9));
        }
        assert_eq!(sched.len(), 36);
        // Drain-until-empty: a `None` only means "nothing affordable this
        // round" (credit still accumulating), never "done".
        let mut order = Vec::new();
        while !sched.is_empty() {
            if let Some(frame) = sched.next() {
                order.push(frame[0]);
            }
        }
        assert_eq!(order.len(), 36);
        // Strict rotation over insertion order 1,2,3,4,9: each stream sends
        // one frame per round, so stream 9 first appears at index 4.
        let first_interactive = order.iter().position(|&t| t == 9).unwrap();
        assert_eq!(first_interactive, 4);
        // Per-stream FIFO: bulk tags appear in stream rounds, interactive
        // frames spread across rounds (never clumped at the tail).
        let last_interactive = order.iter().rposition(|&t| t == 9).unwrap();
        assert!(
            last_interactive < 20,
            "interactive tail at {last_interactive}"
        );
        for sid in 1..=4u32 {
            assert_eq!(order.iter().filter(|&&t| t == sid as u8).count(), 8);
        }
    }

    #[test]
    fn drr_control_never_overtakes_own_data() {
        // Regression: a FIN must not jump ahead of its own stream's queued
        // DATA (peer would close early and drop the tail), but it still
        // jumps ahead of OTHER streams' bulk. System frames always go first
        // among ready controls.
        let mut sched = Scheduler::new();
        sched.push(drr_system(2, 3)); // ping, queued first
        sched.push(drr_frame(5, false, 65536, 5)); // other stream's bulk
        sched.push(drr_frame(1, false, 100, 1)); // own DATA
        sched.push(drr_frame(1, true, 7, 2)); // own FIN
        let mut order = Vec::new();
        while !sched.is_empty() {
            if let Some(frame) = sched.next() {
                order.push(frame[0]);
            }
        }
        // Ping first; the FIN yields to DRR while its own DATA is queued
        // (stream 5's bulk wins that round), but it still precedes nothing
        // of its own stream: D1 always comes before FIN. Intra-stream order
        // is what correctness needs; cross-stream order stays fair.
        assert_eq!(order, vec![3, 5, 1, 2]);
    }

    #[test]
    fn drr_drained_streams_are_dropped_eagerly() {
        // Regression (v0.0.36): a stream whose queue drained must leave
        // `rotation` even while other streams still hold frames. Streams
        // closed via RST/abort paths never send a closing control through
        // the scheduler; without eager removal their dead entries would
        // make every `next()` scan them forever on long-lived sessions.
        let mut sched = Scheduler::new();
        sched.push(drr_frame(1, false, 100, 1)); // small: drains in 1 round
        sched.push(drr_frame(2, false, 400_000, 2)); // big: needs 4 DRR rounds
        assert_eq!(sched.live_stream_count(), 2);

        // Round 1: stream 1 emits and drains; stream 2 accrues deficit.
        let f = sched.next().unwrap();
        assert_eq!(f[0], 1);
        // Round 2: the scan reaches drained stream 1 again -> it must be
        // dropped eagerly, not left lingering in `rotation`.
        assert!(sched.next().is_none());
        assert_eq!(sched.live_stream_count(), 1);

        // Stream 2 keeps accruing deficit across rounds, then emits.
        let mut got = false;
        for _ in 0..8 {
            if let Some(f) = sched.next() {
                assert_eq!(f[0], 2);
                got = true;
                break;
            }
        }
        assert!(got, "stream 2 must eventually afford its frame");
        assert!(sched.is_empty());
    }

    #[test]
    fn drr_syn_never_yields_to_own_data() {
        // SYN creates the peer-side stream: it must lead even when its own
        // DATA is already queued. Yielding it (like FIN/RST) makes the peer
        // drop that DATA as unknown-stream — live-caught with full 64KB
        // flows vanishing under burst while zero probes fired.
        let mut sched = Scheduler::new();
        sched.push(drr_frame(1, false, 100, 1));
        sched.push(drr_frame_yield(1, true, false, 7, 2));
        sched.push(drr_frame(1, false, 100, 1));
        let mut order = Vec::new();
        while !sched.is_empty() {
            if let Some(frame) = sched.next() {
                order.push(frame[0]);
            }
        }
        assert_eq!(order, vec![2, 1, 1]);
    }

    /// Regression: 8 bulk streams racing one writer must deliver every
    /// frame, including the close-time drain (a prior revision stranded
    /// queued frames when the feed closed, and an earlier one wedged the
    /// tail when no further arrivals came).
    #[tokio::test]
    async fn concurrent_bulk_all_frames_delivered() {
        use crate::crypto::XorCipher;
        use crate::ws::read_frame;
        use tokio::io::duplex;
        let (a, mut b) = duplex(256 * 1024 * 1024);
        let (writer, handle) = MuxFrameWriter::spawn(a);
        let cipher = XorCipher::new("repro");
        let mut tasks = Vec::new();
        for sid in 1..=8u32 {
            let w = writer.clone();
            let c = cipher.clone();
            tasks.push(tokio::spawn(async move {
                for _ in 0..70 {
                    let mut enc = Vec::new();
                    encode_mux_ws_frame(
                        &mut enc,
                        sid,
                        MuxCommand::Data,
                        &[sid as u8; 65535],
                        &c,
                        true,
                        false,
                    )
                    .unwrap();
                    w.send_mux(sid, MuxCommand::Data, enc).await.unwrap();
                }
                let mut enc = Vec::new();
                encode_mux_ws_frame(&mut enc, sid, MuxCommand::Fin, &[], &c, true, false).unwrap();
                w.send_mux(sid, MuxCommand::Fin, enc).await.unwrap();
            }));
        }
        drop(writer);
        let reader = tokio::spawn(async move {
            let mut buf = Vec::new();
            let mut count = 0usize;
            while count < 8 * 71 {
                let (op, mut payload) = read_frame(
                    &mut b,
                    Option::<&mut tokio::io::DuplexStream>::None,
                    &mut buf,
                )
                .await
                .unwrap()
                .unwrap();
                assert_eq!(op, 2);
                cipher.apply(&mut payload);
                let _ = MuxFrame::decode(&payload).unwrap();
                count += 1;
            }
            count
        });
        let (_senders, received) = tokio::join!(
            async {
                for t in tasks {
                    t.await.unwrap();
                }
            },
            tokio::time::timeout(std::time::Duration::from_secs(30), reader),
        );
        let n = received.expect("writer stalled: reader timed out").unwrap();
        assert_eq!(n, 8 * 71);
        handle.await.unwrap();
    }

    #[tokio::test]
    async fn send_mux_flows_through_scheduler_live_task() {
        use crate::crypto::XorCipher;
        use crate::ws::read_frame;
        use tokio::io::duplex;
        let (a, mut b) = duplex(1024 * 1024);
        let (writer, handle) = MuxFrameWriter::spawn(a);
        let cipher = XorCipher::new("probe");
        let mut enc = Vec::new();
        encode_mux_ws_frame(
            &mut enc,
            5,
            MuxCommand::Data,
            &[7u8; 100],
            &cipher,
            true,
            false,
        )
        .unwrap();
        writer.send_mux(5, MuxCommand::Data, enc).await.unwrap();
        drop(writer);
        let mut buf = Vec::new();
        let res = tokio::time::timeout(
            std::time::Duration::from_secs(5),
            read_frame(
                &mut b,
                Option::<&mut tokio::io::DuplexStream>::None,
                &mut buf,
            ),
        )
        .await
        .expect("timed out waiting for send_mux frame - TASK STUCK");
        let (opcode, mut payload) = res.unwrap().unwrap();
        assert_eq!(opcode, 2);
        cipher.apply(&mut payload);
        let frame = MuxFrame::decode(&payload).unwrap();
        assert_eq!(frame.stream_id, 5);
        assert_eq!(frame.payload, vec![7u8; 100]);
        handle.await.unwrap();
    }

    #[test]
    fn drr_deficit_accumulates_across_rounds() {
        // A 200KB frame needs 4 quanta (64KB x 3 = 192KB < 200KB): streams
        // 2 and 3 emit first while stream 1 accrues 64/128/192KB over
        // "empty" rounds, then 256KB (capped) lets it through.
        let mut sched = Scheduler::new();
        sched.push(drr_frame(1, false, 200 * 1024, 1));
        sched.push(drr_frame(2, false, 65536, 2));
        sched.push(drr_frame(3, false, 65536, 3));
        let mut order = Vec::new();
        while !sched.is_empty() {
            if let Some(frame) = sched.next() {
                order.push(frame[0]);
            }
        }
        assert_eq!(order, vec![2, 3, 1]);
    }
}
} // mod mux_writer

pub mod nonmux {
//! Plain WebSocket 1:1 relay compatibility path.

use crate::common::{
    configured_cipher, relay_non_mux_ws, split_ws_url, try_cloudflare_edges, NonMuxOkRejection,
    NonMuxPool, NonMuxRelaySpec, NON_MUX_POOL_SIZE,
};
use crate::dns::resolve_socket;
use crate::proxy::{
    parse_authority_with_default, parse_target_authority, read_client_proxy_request,
    ClientProxyRequest, SocksCommand,
};
use crate::runtime::{
    apply_socket_options, drain_join_set, enforce_target_policy, recycle_buf, relay_buf,
    resolve_target, wait_shutdown, RuntimeConfig,
};
use crate::udp_relay::handle_local_udp_proxy;
use crate::ws::{
    build_client_handshake_request, build_server_handshake_response, read_frame,
    read_frame_with_cipher, read_http_headers, validate_client_handshake_response,
    validate_server_handshake, write_frame, write_frame_borrowed,
};
use anyhow::{anyhow, bail, Result};
use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{
    tcp::{OwnedReadHalf, OwnedWriteHalf},
    TcpListener, TcpStream,
};
use tokio::sync::Semaphore;
use tokio::time::{timeout, Duration};

fn parse_ws_url(input: &str) -> Result<(String, String)> {
    let (authority, path) = split_ws_url(input, "non-MUX plain client requires ws:// upstream")?;
    let authority = if authority.starts_with('[') || authority.matches(':').count() == 1 {
        authority
    } else {
        format!("{}:80", authority)
    };
    Ok((authority, path))
}
/// Plain-WS Cloudflare edge fallback. Mirrors goway.go `dialFallback`:
/// IP upstream + `-fakehost` and primary unreachable -> try fakehost edges.
async fn connect_ws_with_fallback(
    cfg: &RuntimeConfig,
    target_host: &str,
    target_port: u16,
) -> Result<TcpStream> {
    let primary = resolve_socket(target_host, target_port).await?;
    let conn_timeout = Duration::from_secs(cfg.connection_timeout.max(1));
    let primary_res = timeout(conn_timeout, TcpStream::connect(primary)).await;
    let failed = match primary_res {
        Ok(Ok(socket)) => return Ok(socket),
        Ok(Err(e)) => anyhow::anyhow!("TCP connect failed: {e}"),
        Err(_) => anyhow::anyhow!("upstream connection timeout to {primary}"),
    };
    if target_host.parse::<std::net::IpAddr>().is_ok() && cfg.fakehost.is_some() {
        let sni = cfg
            .fakehost
            .as_deref()
            .map(|s| s.split(':').next().unwrap_or(s))
            .unwrap();
        tracing::warn!(%primary, error=%failed, "[WS] Primary upstream unreachable; trying Cloudflare fallback edges via fakehost");
        if let Some(socket) =
            try_cloudflare_edges(sni, primary.ip(), target_port, conn_timeout).await?
        {
            return Ok(socket);
        }
        bail!("primary {primary} unreachable and all Cloudflare fallback edges failed for fakehost: {sni}");
    }
    Err(failed)
}

async fn open_upstream(cfg: &RuntimeConfig) -> Result<(OwnedReadHalf, OwnedWriteHalf)> {
    let upstream = cfg
        .upstream
        .as_deref()
        .ok_or_else(|| anyhow!("client mode requires upstream"))?;
    let (addr, path) = parse_ws_url(upstream)?;
    let target = parse_authority_with_default(&addr, 80).map_err(|e| anyhow!(e.to_string()))?;
    let header_host = cfg.fakehost.as_deref().unwrap_or(&target.host).to_string();
    let sni_base = cfg
        .fakehost
        .as_deref()
        .map(|s| s.split(':').next().unwrap_or(s))
        .unwrap_or(&target.host);
    let tcp = connect_ws_with_fallback(cfg, &target.host, target.port).await?;
    apply_socket_options(&tcp, cfg);
    let (mut rd, mut wr) = tcp.into_split();
    let header_base = header_host.split(':').next().unwrap_or(&header_host);
    let origin = format!("http://{}", sni_base);
    let sec_fetch_site = if sni_base.eq_ignore_ascii_case(header_base) {
        "same-origin"
    } else {
        "cross-site"
    };
    let (request, key) =
        build_client_handshake_request(&header_host, &path, Some(&origin), Some(sec_fetch_site));
    wr.write_all(&request).await?;
    wr.flush().await?;
    let response = read_http_headers(&mut rd).await?;
    validate_client_handshake_response(&response, &key)?;
    Ok((rd, wr))
}

type PlainNonMuxPool = NonMuxPool<RuntimeConfig, OwnedReadHalf, OwnedWriteHalf>;

/// Boxed adapter so the shared [`NonMuxPool`] can dial the plain-WS opener.
fn dial_pooled_plain(
    cfg: &RuntimeConfig,
) -> Pin<Box<dyn Future<Output = Result<(OwnedReadHalf, OwnedWriteHalf)>> + Send + '_>> {
    Box::pin(open_upstream(cfg))
}

const PLAIN_NON_MUX_RELAY: NonMuxRelaySpec = NonMuxRelaySpec {
    closed_error: "upstream closed before non-MUX OK",
    bad_opcode_error: "invalid non-MUX handshake opcode",
    ok_rejection: NonMuxOkRejection::Propagate("upstream rejected non-MUX target"),
};

async fn relay_client(
    local: TcpStream,
    cfg: RuntimeConfig,
    req: ClientProxyRequest,
    pool: Arc<PlainNonMuxPool>,
) -> Result<()> {
    enforce_target_policy(&cfg, &req.target)?;
    let (rd, wr) = pool.get_or_dial().await?;
    relay_non_mux_ws(
        local,
        &req.target,
        req.is_socks5,
        req.is_connect,
        req.initial_payload,
        &cfg.key,
        cfg.buffer_size,
        rd,
        wr,
        PLAIN_NON_MUX_RELAY,
    )
    .await
}
async fn handle_client_connection(
    mut local: TcpStream,
    cfg: RuntimeConfig,
    pool: Arc<PlainNonMuxPool>,
) -> Result<()> {
    let req = timeout(
        Duration::from_secs(cfg.connection_timeout.max(1)),
        read_client_proxy_request(&mut local),
    )
    .await
    .map_err(|_| anyhow!("non-MUX client proxy request timed out"))??;
    if req.command == SocksCommand::UdpAssociate {
        return handle_local_udp_proxy(local, cfg, req.target).await;
    }
    if req.command != SocksCommand::Connect {
        bail!("non-MUX client supports CONNECT or UDP ASSOCIATE only")
    }
    relay_client(local, cfg, req, pool).await
}
pub async fn run_client(cfg: RuntimeConfig) -> Result<()> {
    let pool = NonMuxPool::new(cfg.clone(), Box::new(dial_pooled_plain), "[non-MUX]");
    let maintainer = pool.clone();
    tokio::spawn(async move {
        maintainer.maintain().await;
    });
    let listener = TcpListener::bind(format!("{}:{}", cfg.proxy_host, cfg.proxy_port)).await?;
    crate::runtime::apply_listener_options(&listener);
    let semaphore = Arc::new(Semaphore::new(cfg.max_connections.max(1)));
    tracing::info!(
        "RushWay non-MUX client proxy listening on {}:{} ({} pre-warmed upstream connections)",
        cfg.proxy_host,
        cfg.proxy_port,
        NON_MUX_POOL_SIZE,
    );
    let mut set = tokio::task::JoinSet::new();
    loop {
        tokio::select! {
            _ = wait_shutdown() => {
                tracing::info!("shutdown requested, draining non-MUX client connections");
                break;
            }
            res = listener.accept() => {
                // A transient accept error (EMFILE, ENFILE, ENOBUFS) must not
                // kill the whole proxy: log, back off briefly, and retry.
                let (stream, peer) = match res {
                    Ok(v) => v,
                    Err(e) => {
                        tracing::warn!(error=%e, "non-MUX client listener accept failed; retrying in 100ms");
                        tokio::time::sleep(Duration::from_millis(100)).await;
                        continue;
                    }
                };
                let permit = match semaphore.clone().try_acquire_owned() {
                    Ok(v) => v,
                    Err(_) => {
                        tracing::debug!(%peer,"maximum client connections reached");
                        continue;
                    }
                };
                let cfg2 = cfg.clone();
                apply_socket_options(&stream, &cfg2);
                let pool2 = pool.clone();
                set.spawn(async move {
                    let _permit = permit;
                    let _conn = crate::stats::ConnGuard::new();
                    if let Err(error) = handle_client_connection(stream, cfg2, pool2).await {
                        tracing::debug!(%peer,%error,"non-MUX client connection closed")
                    }
                });
            }
        }
    }
    drain_join_set(&mut set).await;
    Ok(())
}
pub async fn run_server(cfg: RuntimeConfig) -> Result<()> {
    let listener = TcpListener::bind(format!("{}:{}", cfg.proxy_host, cfg.proxy_port)).await?;
    crate::runtime::apply_listener_options(&listener);
    let semaphore = Arc::new(Semaphore::new(cfg.max_connections.max(1)));
    tracing::info!(
        "RushWay non-MUX server listening on {}:{}",
        cfg.proxy_host,
        cfg.proxy_port
    );
    let mut set = tokio::task::JoinSet::new();
    loop {
        tokio::select! {
            _ = wait_shutdown() => {
                tracing::info!("shutdown requested, draining non-MUX server connections");
                break;
            }
            res = listener.accept() => {
                // A transient accept error (EMFILE, ENFILE, ENOBUFS) must not
                // kill the whole proxy: log, back off briefly, and retry.
                let (stream, peer) = match res {
                    Ok(v) => v,
                    Err(e) => {
                        tracing::warn!(error=%e, "non-MUX server listener accept failed; retrying in 100ms");
                        tokio::time::sleep(Duration::from_millis(100)).await;
                        continue;
                    }
                };
                let permit = match semaphore.clone().try_acquire_owned() {
                    Ok(v) => v,
                    Err(_) => {
                        tracing::debug!(%peer,"maximum server connections reached");
                        continue;
                    }
                };
                let cfg2 = cfg.clone();
                apply_socket_options(&stream, &cfg2);
                set.spawn(async move {
                    let _permit = permit;
                    let _conn = crate::stats::ConnGuard::new();
                    if let Err(error) = handle_server(stream, cfg2).await {
                        tracing::debug!(%peer,%error,"non-MUX transport closed")
                    }
                });
            }
        }
    }
    drain_join_set(&mut set).await;
    Ok(())
}
async fn handle_server(stream: TcpStream, cfg: RuntimeConfig) -> Result<()> {
    let (mut rd, mut wr) = stream.into_split();
    let request = read_http_headers(&mut rd).await?;
    let key = validate_server_handshake(&request)?;
    wr.write_all(&build_server_handshake_response(&key)).await?;
    wr.flush().await?;
    let c = configured_cipher(&cfg.key);
    let mut frame_buf = crate::mux_writer::acquire_encode_buf();
    let Some((opcode, target_frame)) = read_frame_with_cipher(
        &mut rd,
        Option::<&mut OwnedWriteHalf>::None,
        &mut frame_buf,
        Some(&c),
    )
    .await?
    else {
        crate::mux_writer::recycle_encode_buf(frame_buf);
        bail!("missing non-MUX target")
    };
    if opcode != 2 {
        crate::mux_writer::recycle_encode_buf(target_frame);
        crate::mux_writer::recycle_encode_buf(frame_buf);
        bail!("invalid non-MUX target opcode")
    };
    let target_addr = match std::str::from_utf8(&target_frame) {
        Ok(text) => match parse_target_authority(text.trim()) {
            Ok(addr) => addr,
            Err(e) => {
                crate::mux_writer::recycle_encode_buf(target_frame);
                crate::mux_writer::recycle_encode_buf(frame_buf);
                return Err(anyhow!(e.to_string()));
            }
        },
        Err(_) => {
            crate::mux_writer::recycle_encode_buf(target_frame);
            crate::mux_writer::recycle_encode_buf(frame_buf);
            bail!("invalid non-MUX target UTF-8")
        }
    };
    crate::mux_writer::recycle_encode_buf(target_frame);
    // Server-side non-MUX had no target policy at all: `-block-local`
    // defaulted to true yet any authenticated peer could make this process
    // dial loopback / RFC1918 / link-local addresses.
    if let Err(e) = enforce_target_policy(&cfg, &target_addr) {
        crate::mux_writer::recycle_encode_buf(frame_buf);
        return Err(e);
    }
    let resolved = match resolve_target(cfg.block_local, &target_addr.host, target_addr.port).await
    {
        Ok(v) => v,
        Err(e) => {
            crate::mux_writer::recycle_encode_buf(frame_buf);
            return Err(e);
        }
    };
    let target_stream = match timeout(
        Duration::from_secs(cfg.connection_timeout.max(1)),
        TcpStream::connect(resolved),
    )
    .await
    {
        Ok(Ok(s)) => s,
        Ok(Err(e)) => {
            crate::mux_writer::recycle_encode_buf(frame_buf);
            return Err(e.into());
        }
        Err(e) => {
            crate::mux_writer::recycle_encode_buf(frame_buf);
            return Err(e.into());
        }
    };
    apply_socket_options(&target_stream, &cfg);
    let mut ok = b"OK\n".to_vec();
    c.apply(&mut ok);
    if let Err(e) = write_frame(&mut wr, &ok, 2, false).await {
        crate::mux_writer::recycle_encode_buf(frame_buf);
        return Err(e.into());
    }
    let (mut target_rd, mut target_wr) = target_stream.into_split();
    let buffer_size = cfg.buffer_size;
    let mut download = tokio::spawn(async move {
        let mut buf = relay_buf(buffer_size).await;
        loop {
            let n = target_rd.read(&mut buf).await?;
            if n == 0 {
                break;
            }
            crate::stats::add_bytes(0, n as i64);
            // Non-MUX data frames are plaintext per GoWay server.
            write_frame_borrowed(&mut wr, &mut buf[..n], 2, false).await?
        }
        recycle_buf(buf).await;
        Result::<()>::Ok(())
    });
    let result = async {
        loop {
            tokio::select! {
                _ = &mut download => {
                    break;
                }
                res = read_frame(
                    &mut rd,
                    Option::<&mut OwnedWriteHalf>::None,
                    &mut frame_buf,
                ) => {
                    let Some((opcode, payload)) = res? else {
                        break;
                    };
                    if opcode == 8 {
                        crate::mux_writer::recycle_encode_buf(payload);
                        break;
                    }
                    if opcode != 2 {
                        crate::mux_writer::recycle_encode_buf(payload);
                        continue;
                    }
                    // Non-MUX data frames are plaintext per GoWay server.
                    let len = payload.len();
                    let write_res = target_wr.write_all(&payload).await;
                    crate::mux_writer::recycle_encode_buf(payload);
                    if write_res.is_err() {
                        break;
                    }
                    crate::stats::add_bytes(len as i64, 0);
                }
            }
        }
        Ok::<(), anyhow::Error>(())
    }
    .await;
    crate::mux_writer::recycle_encode_buf(frame_buf);
    let _ = target_wr.shutdown().await;
    download.abort();
    result
}
} // mod nonmux

pub mod profile {
//! `-cpuprofile` / `-cpuprofile-duration` support (GoWay `runtime/pprof`
//! parity via the `pprof` crate on Unix signal sampling).
//!
//! Windows builds accept the flags but cannot sample other threads' stacks
//! without DbgHelp suspension, so they warn and skip — documented in SPEC.

use std::path::PathBuf;

/// Starts CPU profiling. On Unix, samples at ~99 Hz into a `pprof` protobuf.
/// `duration_secs = Some(n)` stops and writes after `n` seconds; `None`/`0`
/// keeps sampling until [`finish`] is called at shutdown.
pub fn start(path: PathBuf, duration_secs: Option<u64>) -> anyhow::Result<()> {
    start_imp(path, duration_secs)
}

/// Writes any in-flight profile and stops the sampler. Safe to call twice.
pub fn finish() {
    finish_imp()
}

#[cfg(unix)]
mod imp {
    use anyhow::{anyhow, Result};
    use pprof::protos::Message as _;
    use std::path::PathBuf;
    use std::sync::Mutex;
    use std::time::Duration;

    type Guard = pprof::ProfilerGuard<'static>;

    static STATE: Mutex<Option<(Guard, PathBuf)>> = Mutex::new(None);

    pub(crate) fn start(path: PathBuf, duration_secs: Option<u64>) -> Result<()> {
        let guard = pprof::ProfilerGuardBuilder::default()
            .frequency(99)
            .build()
            .map_err(|e| anyhow!("failed to start CPU profiler: {e}"))?;
        {
            let mut slot = STATE.lock().unwrap_or_else(|e| e.into_inner());
            if slot.is_some() {
                return Err(anyhow!("CPU profiler already running"));
            }
            *slot = Some((guard, path.clone()));
        }
        tracing::info!(path = %path.display(), "CPU profiler started (99 Hz)");
        if let Some(secs) = duration_secs.filter(|s| *s > 0) {
            std::thread::spawn(move || {
                std::thread::sleep(Duration::from_secs(secs));
                finish();
            });
        }
        Ok(())
    }

    pub(crate) fn finish() {
        let taken = STATE.lock().unwrap_or_else(|e| e.into_inner()).take();
        let Some((guard, path)) = taken else { return };
        match guard.report().build() {
            Ok(report) => match report.pprof() {
                Ok(profile) => {
                    let mut content = Vec::new();
                    if let Err(error) = profile.encode(&mut content) {
                        tracing::warn!(%error, "failed to encode CPU profile");
                        return;
                    }
                    let mut file = match std::fs::File::create(&path) {
                        Ok(f) => f,
                        Err(error) => {
                            tracing::warn!(path = %path.display(), %error, "failed to create CPU profile file");
                            return;
                        }
                    };
                    use std::io::Write as _;
                    if let Err(error) = file.write_all(&content) {
                        tracing::warn!(path = %path.display(), %error, "failed to write CPU profile");
                        return;
                    }
                    tracing::info!(path = %path.display(), "CPU profile saved");
                }
                Err(error) => {
                    tracing::warn!(%error, "failed to encode CPU profile");
                }
            },
            Err(error) => {
                tracing::warn!(%error, "failed to build CPU profile report");
            }
        }
    }
}

#[cfg(not(unix))]
mod imp {
    use anyhow::Result;
    use std::path::PathBuf;

    pub(crate) fn start(path: PathBuf, _duration_secs: Option<u64>) -> Result<()> {
        tracing::warn!(
            path = %path.display(),
            "CPU profiling requires a Unix build (pprof signal sampler); flag accepted, no profile will be written on this platform"
        );
        Ok(())
    }

    pub(crate) fn finish() {}
}

use imp::{finish as finish_imp, start as start_imp};
} // mod profile

pub mod protocol {
//! GoWay v1.8.4 wire primitives.
//!
//! The MUX header is exactly 7 bytes:
//!   uint32 stream id (big endian)
//!   uint8 command
//!   uint16 payload length (big endian)
//!
//! Commands are SYN=0x01, DATA=0x02, FIN=0x03, RST=0x04 plus the W3
//! flow-control extensions VERSION=0x05 and WINDOW=0x06. VERSION/WINDOW
//! are probe-negotiated: peers that predate them skip the unknown command
//! (decode error -> whole frame skipped, one mux frame per WS frame), so
//! old endpoints keep v1 behavior automatically.

use std::collections::HashMap;
use std::fmt;
use std::hash::{BuildHasherDefault, Hasher};

/// Fast single-multiply Fibonacci hasher for internal `u32` MUX stream IDs.
/// Eliminates SipHash-1-3 overhead on per-frame stream table lookups.
#[derive(Default, Clone, Copy)]
pub(crate) struct U32Hasher(u64);

impl Hasher for U32Hasher {
    #[inline(always)]
    fn finish(&self) -> u64 {
        self.0
    }

    #[inline(always)]
    fn write(&mut self, bytes: &[u8]) {
        let mut x = self.0;
        for &b in bytes {
            x = (x ^ u64::from(b)).wrapping_mul(0x517cc1b727220a95);
        }
        self.0 = x;
    }

    #[inline(always)]
    fn write_u32(&mut self, i: u32) {
        self.0 = u64::from(i).wrapping_mul(0x517cc1b727220a95);
    }
}

pub(crate) type U32BuildHasher = BuildHasherDefault<U32Hasher>;
pub(crate) type U32HashMap<V> = HashMap<u32, V, U32BuildHasher>;

pub const MUX_HEADER_LEN: usize = 7;
pub const MUX_SYN: u8 = 0x01;
pub const MUX_DATA: u8 = 0x02;
pub const MUX_FIN: u8 = 0x03;
pub const MUX_RST: u8 = 0x04;
pub const MUX_VERSION: u8 = 0x05;
pub const MUX_WINDOW: u8 = 0x06;
pub const MAX_MUX_PAYLOAD: usize = u16::MAX as usize;

/// Wire version advertised in the VERSION frame payload.
#[allow(dead_code)]
pub const MUX_PROTO_VERSION: u8 = 1;
/// Initial per-stream receive window advertised in VERSION (KiB).
#[allow(dead_code)]
pub const MUX_INITIAL_WINDOW_KIB: u16 = 8192;
/// Send a WINDOW refund once this many consumed bytes have accumulated.
#[allow(dead_code)]
pub const MUX_WINDOW_REFRESH: usize = 1024 * 1024;
/// Floor applied to a peer-advertised window so a broken/zero VERSION
/// can never starve the sender (frames are at most 64 KiB).
#[allow(dead_code)]
pub const MUX_WINDOW_MIN_KIB: u16 = 64;
/// Capacity for per-stream MUX data channel (GoWay parity).
/// Sized to 768 frames so that an entire 8 MiB window burst
/// (up to ~512-768 frames of 10-16 KiB each) can be absorbed in-flight
/// without hitting `TrySendError::Full` and triggering premature stream resets.
#[allow(dead_code)]
pub const MUX_STREAM_QUEUE_CAP: usize = 768;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MuxCommand {
    Syn,
    Data,
    Fin,
    Rst,
    Version,
    Window,
}

impl MuxCommand {
    pub fn as_u8(self) -> u8 {
        match self {
            Self::Syn => MUX_SYN,
            Self::Data => MUX_DATA,
            Self::Fin => MUX_FIN,
            Self::Rst => MUX_RST,
            Self::Version => MUX_VERSION,
            Self::Window => MUX_WINDOW,
        }
    }
}

impl TryFrom<u8> for MuxCommand {
    type Error = ProtocolError;

    fn try_from(value: u8) -> Result<Self, Self::Error> {
        match value {
            MUX_SYN => Ok(Self::Syn),
            MUX_DATA => Ok(Self::Data),
            MUX_FIN => Ok(Self::Fin),
            MUX_RST => Ok(Self::Rst),
            MUX_VERSION => Ok(Self::Version),
            MUX_WINDOW => Ok(Self::Window),
            other => Err(ProtocolError::UnknownCommand(other)),
        }
    }
}

/// VERSION payload: `[u8 version][u16 window_kib BE]` (3 bytes).
#[allow(dead_code)]
pub fn encode_version_payload(window_kib: u16) -> [u8; 3] {
    [
        MUX_PROTO_VERSION,
        (window_kib >> 8) as u8,
        window_kib as u8,
    ]
}

/// Decodes a VERSION payload; rejects truncation and out-of-range window
/// (returns the floored window in KiB).
#[allow(dead_code)]
pub fn decode_version_payload(payload: &[u8]) -> Option<(u8, u16)> {
    if payload.len() != 3 {
        return None;
    }
    let kib = u16::from_be_bytes([payload[1], payload[2]]).max(MUX_WINDOW_MIN_KIB);
    Some((payload[0], kib))
}

/// WINDOW payload: `[u32 credit_bytes BE]` (4 bytes).
#[allow(dead_code)]
pub fn encode_window_payload(credit: u32) -> [u8; 4] {
    credit.to_be_bytes()
}

/// Decodes a WINDOW payload; rejects truncation, zero and absurd credits
/// (larger than the max possible window: u16 KiB => 64 MiB).
#[allow(dead_code)]
pub fn decode_window_payload(payload: &[u8]) -> Option<u32> {
    if payload.len() != 4 {
        return None;
    }
    let credit = u32::from_be_bytes(payload[..4].try_into().unwrap());
    if credit == 0 || credit > (u16::MAX as u32) * 1024 {
        return None;
    }
    Some(credit)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct MuxHeader {
    pub stream_id: u32,
    pub command: MuxCommand,
    pub payload_len: usize,
}

impl MuxHeader {
    pub fn parse(buf: &[u8]) -> Result<Self, ProtocolError> {
        if buf.len() < MUX_HEADER_LEN {
            return Err(ProtocolError::TruncatedHeader(buf.len()));
        }
        let stream_id = u32::from_be_bytes(buf[0..4].try_into().unwrap());
        let command = MuxCommand::try_from(buf[4])?;
        let payload_len = u16::from_be_bytes(buf[5..7].try_into().unwrap()) as usize;
        // Trailing bytes past the declared payload are obfuscation padding
        // (GoWay `-obfs`): slice by the declared length and ignore the tail.
        // Only a short buffer is an error.
        if buf.len() < MUX_HEADER_LEN + payload_len {
            return Err(ProtocolError::LengthMismatch {
                declared: payload_len,
                available: buf.len().saturating_sub(MUX_HEADER_LEN),
            });
        }
        Ok(Self {
            stream_id,
            command,
            payload_len,
        })
    }

    #[allow(dead_code)]
    pub fn payload<'a>(&self, buf: &'a [u8]) -> &'a [u8] {
        &buf[MUX_HEADER_LEN..MUX_HEADER_LEN + self.payload_len]
    }
}

pub fn encode_header(
    out: &mut Vec<u8>,
    stream_id: u32,
    command: MuxCommand,
    payload_len: usize,
) -> Result<(), ProtocolError> {
    if payload_len > MAX_MUX_PAYLOAD {
        return Err(ProtocolError::PayloadTooLarge(payload_len));
    }
    out.reserve(MUX_HEADER_LEN);
    out.extend_from_slice(&stream_id.to_be_bytes());
    out.push(command.as_u8());
    out.extend_from_slice(&(payload_len as u16).to_be_bytes());
    Ok(())
}

pub fn write_frame_parts(
    out: &mut Vec<u8>,
    stream_id: u32,
    command: MuxCommand,
    payload: &[u8],
) -> Result<(), ProtocolError> {
    encode_header(out, stream_id, command, payload.len())?;
    out.extend_from_slice(payload);
    Ok(())
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MuxFrame {
    pub stream_id: u32,
    pub command: MuxCommand,
    pub payload: Vec<u8>,
}

#[derive(Debug)]
#[allow(dead_code)]
pub struct OwnedMuxFrame {
    pub stream_id: u32,
    pub command: MuxCommand,
    storage: Vec<u8>,
    payload_len: usize,
}

impl OwnedMuxFrame {
    /// Constructs an `OwnedMuxFrame` directly from parts with a single allocation,
    /// eliminating redundant encode + decode_owned passes.
    pub fn from_parts(
        stream_id: u32,
        command: MuxCommand,
        payload: &[u8],
    ) -> Result<Self, ProtocolError> {
        let payload_len = payload.len();
        if payload_len > MAX_MUX_PAYLOAD {
            return Err(ProtocolError::PayloadTooLarge(payload_len));
        }
        let mut storage = crate::mux_writer::acquire_encode_buf();
        storage.reserve(MUX_HEADER_LEN + payload_len);
        write_frame_parts(&mut storage, stream_id, command, payload)?;
        Ok(Self {
            stream_id,
            command,
            storage,
            payload_len,
        })
    }

    /// Payload bounded by the declared MUX length. Storage may hold extra
    /// trailing bytes (GoWay `-obfs` padding); those must never leak into
    /// the relayed stream.
    pub fn payload(&self) -> &[u8] {
        &self.storage[MUX_HEADER_LEN..MUX_HEADER_LEN + self.payload_len]
    }

    #[allow(dead_code)]
    pub fn into_storage(self) -> Vec<u8> {
        self.storage
    }
}

#[allow(dead_code)]
impl MuxFrame {
    pub fn new(
        stream_id: u32,
        command: MuxCommand,
        payload: Vec<u8>,
    ) -> Result<Self, ProtocolError> {
        if payload.len() > MAX_MUX_PAYLOAD {
            return Err(ProtocolError::PayloadTooLarge(payload.len()));
        }
        Ok(Self {
            stream_id,
            command,
            payload,
        })
    }

    pub fn encode(&self, out: &mut Vec<u8>) -> Result<(), ProtocolError> {
        write_frame_parts(out, self.stream_id, self.command, &self.payload)
    }

    #[allow(dead_code)]
    pub fn decode(buf: &[u8]) -> Result<Self, ProtocolError> {
        let header = MuxHeader::parse(buf)?;
        Ok(Self {
            stream_id: header.stream_id,
            command: header.command,
            payload: header.payload(buf).to_vec(),
        })
    }

    pub fn decode_owned(buf: Vec<u8>) -> Result<OwnedMuxFrame, ProtocolError> {
        let header = MuxHeader::parse(&buf)?;
        Ok(OwnedMuxFrame {
            stream_id: header.stream_id,
            command: header.command,
            payload_len: header.payload_len,
            storage: buf,
        })
    }
}

/// SYN payload: uint16 target length, target bytes, optional initial data.
#[derive(Debug, Clone, PartialEq, Eq)]
#[allow(dead_code)]
pub struct SynPayload {
    pub target: Vec<u8>,
    pub initial_data: Vec<u8>,
}

#[allow(dead_code)]
impl SynPayload {
    pub fn encode(&self) -> Result<Vec<u8>, ProtocolError> {
        if self.target.len() > u16::MAX as usize {
            return Err(ProtocolError::TargetTooLarge(self.target.len()));
        }
        let mut out = Vec::with_capacity(2 + self.target.len() + self.initial_data.len());
        out.extend_from_slice(&(self.target.len() as u16).to_be_bytes());
        out.extend_from_slice(&self.target);
        out.extend_from_slice(&self.initial_data);
        Ok(out)
    }

    /// Encodes `host:port` + `initial_data` directly into a single `Vec<u8>`
    /// without intermediate `format!` or struct allocations.
    pub fn encode_parts(
        host: &str,
        port: u16,
        initial_data: &[u8],
    ) -> Result<Vec<u8>, ProtocolError> {
        use std::io::Write as _;
        // Max port string is 5 digits + 1 colon = 6 bytes.
        let est_target_len = host.len() + 6;
        let mut out = Vec::with_capacity(2 + est_target_len + initial_data.len());
        out.extend_from_slice(&[0, 0]);
        let _ = write!(&mut out, "{host}:{port}");
        let target_len = out.len() - 2;
        if target_len > u16::MAX as usize {
            return Err(ProtocolError::TargetTooLarge(target_len));
        }
        let len_bytes = (target_len as u16).to_be_bytes();
        out[0] = len_bytes[0];
        out[1] = len_bytes[1];
        out.extend_from_slice(initial_data);
        Ok(out)
    }

    /// Decodes a SYN payload into borrowed `(target, initial_data)` slices
    /// without heap allocation.
    pub fn decode_slices(payload: &[u8]) -> Result<(&[u8], &[u8]), ProtocolError> {
        if payload.len() < 2 {
            return Err(ProtocolError::TruncatedSyn);
        }
        let target_len = u16::from_be_bytes(payload[0..2].try_into().unwrap()) as usize;
        if payload.len() < 2 + target_len {
            return Err(ProtocolError::TruncatedSyn);
        }
        Ok((&payload[2..2 + target_len], &payload[2 + target_len..]))
    }

    pub fn decode(payload: &[u8]) -> Result<Self, ProtocolError> {
        let (target, initial_data) = Self::decode_slices(payload)?;
        Ok(Self {
            target: target.to_vec(),
            initial_data: initial_data.to_vec(),
        })
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ProtocolError {
    TruncatedHeader(usize),
    #[allow(dead_code)]
    TruncatedSyn,
    UnknownCommand(u8),
    PayloadTooLarge(usize),
    #[allow(dead_code)]
    TargetTooLarge(usize),
    LengthMismatch {
        declared: usize,
        available: usize,
    },
}

impl fmt::Display for ProtocolError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::TruncatedHeader(n) => write!(f, "truncated MUX header: {n} bytes"),
            Self::TruncatedSyn => write!(f, "truncated SYN payload"),
            Self::UnknownCommand(c) => write!(f, "unknown MUX command: 0x{c:02x}"),
            Self::PayloadTooLarge(n) => write!(f, "MUX payload exceeds uint16: {n} bytes"),
            Self::TargetTooLarge(n) => write!(f, "SYN target exceeds uint16: {n} bytes"),
            Self::LengthMismatch {
                declared,
                available,
            } => {
                write!(
                    f,
                    "MUX payload length mismatch: declared {declared}, available {available}"
                )
            }
        }
    }
}

impl std::error::Error for ProtocolError {}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn mux_header_is_exactly_seven_bytes() {
        let frame = MuxFrame::new(0x01020304, MuxCommand::Data, vec![1, 2, 3]).unwrap();
        let mut encoded = Vec::new();
        frame.encode(&mut encoded).unwrap();
        assert_eq!(&encoded[..7], &[1, 2, 3, 4, MUX_DATA, 0, 3]);
        assert_eq!(encoded.len(), MUX_HEADER_LEN + 3);
        assert_eq!(MuxFrame::decode(&encoded).unwrap(), frame);
    }

    #[test]
    fn zero_copy_header_parse_round_trip() {
        let mut encoded = Vec::new();
        write_frame_parts(&mut encoded, 0x01020304, MuxCommand::Data, &[1, 2, 3, 4]).unwrap();
        let header = MuxHeader::parse(&encoded).unwrap();
        assert_eq!(header.stream_id, 0x01020304);
        assert_eq!(header.command, MuxCommand::Data);
        assert_eq!(header.payload_len, 4);
        assert_eq!(header.payload(&encoded), &[1, 2, 3, 4]);
    }

    #[test]
    fn owned_decode_keeps_original_payload_storage() {
        let mut encoded = Vec::with_capacity(MUX_HEADER_LEN + 4);
        write_frame_parts(&mut encoded, 7, MuxCommand::Data, &[1, 2, 3, 4]).unwrap();
        let ptr = encoded.as_ptr();
        let frame = MuxFrame::decode_owned(encoded).unwrap();
        assert_eq!(frame.payload(), &[1, 2, 3, 4]);
        assert_eq!(frame.payload().as_ptr(), unsafe { ptr.add(MUX_HEADER_LEN) });
    }

    #[test]
    fn all_commands_round_trip() {
        for command in [
            MuxCommand::Syn,
            MuxCommand::Data,
            MuxCommand::Fin,
            MuxCommand::Rst,
            MuxCommand::Version,
            MuxCommand::Window,
        ] {
            let frame = MuxFrame::new(7, command, vec![]).unwrap();
            let mut bytes = Vec::new();
            frame.encode(&mut bytes).unwrap();
            assert_eq!(MuxFrame::decode(&bytes).unwrap(), frame);
        }
    }

    #[test]
    fn version_and_window_wire_values() {
        assert_eq!(MuxCommand::Version.as_u8(), 0x05);
        assert_eq!(MuxCommand::Window.as_u8(), 0x06);
        assert_eq!(MuxCommand::try_from(0x05).unwrap(), MuxCommand::Version);
        assert_eq!(MuxCommand::try_from(0x06).unwrap(), MuxCommand::Window);
        assert!(matches!(
            MuxCommand::try_from(0x07),
            Err(ProtocolError::UnknownCommand(0x07))
        ));
    }

    #[test]
    fn version_payload_round_trip_and_validation() {
        let payload = encode_version_payload(1024);
        assert_eq!(payload, [MUX_PROTO_VERSION, 0x04, 0x00]);
        assert_eq!(decode_version_payload(&payload), Some((1, 1024)));
        // Truncation / overlong payloads rejected.
        assert_eq!(decode_version_payload(&payload[..2]), None);
        assert_eq!(decode_version_payload(&[0, 0, 0, 0]), None);
        // Zero/undersized window floored to the 64 KiB minimum.
        assert_eq!(decode_version_payload(&[1, 0, 0]), Some((1, 64)));
        assert_eq!(decode_version_payload(&[1, 0, 32]), Some((1, 64)));
    }

    #[test]
    fn window_payload_round_trip_and_validation() {
        assert_eq!(decode_window_payload(&encode_window_payload(65536)), Some(65536));
        assert_eq!(decode_window_payload(&encode_window_payload(1)), Some(1));
        assert_eq!(decode_window_payload(&[0, 0, 0]), None);
        assert_eq!(decode_window_payload(&[0, 0, 0, 0, 0]), None);
        assert_eq!(decode_window_payload(&encode_window_payload(0)), None);
        // Credits above the largest possible window are rejected.
        let too_big = ((u16::MAX as u32) * 1024) + 1;
        assert_eq!(decode_window_payload(&encode_window_payload(too_big)), None);
    }

    #[test]
    fn version_window_frames_decode_owned() {
        // Stream 0 control frames must survive decode_owned (the client
        // reader routes non-DATA commands by stream id).
        let mut encoded = Vec::new();
        write_frame_parts(&mut encoded, 0, MuxCommand::Version, &encode_version_payload(1024))
            .unwrap();
        let frame = MuxFrame::decode_owned(encoded).unwrap();
        assert_eq!(frame.stream_id, 0);
        assert_eq!(frame.command, MuxCommand::Version);
        assert_eq!(
            decode_version_payload(frame.payload()),
            Some((MUX_PROTO_VERSION, 1024))
        );
    }

    #[test]
    fn syn_payload_round_trip_with_initial_data() {
        let payload = SynPayload {
            target: b"example.com:443".to_vec(),
            initial_data: b"hello".to_vec(),
        };
        let encoded = payload.encode().unwrap();
        assert_eq!(&encoded[..2], &[0, 15]);
        assert_eq!(SynPayload::decode(&encoded).unwrap(), payload);
    }

    #[test]
    fn malformed_frames_are_rejected() {
        assert!(matches!(
            MuxFrame::decode(&[0; 6]),
            Err(ProtocolError::TruncatedHeader(6))
        ));
        assert!(matches!(
            MuxFrame::decode(&[0, 0, 0, 1, 0xff, 0, 0]),
            Err(ProtocolError::UnknownCommand(0xff))
        ));
        assert!(matches!(
            MuxFrame::decode(&[0, 0, 0, 1, MUX_DATA, 0, 2, 1]),
            Err(ProtocolError::LengthMismatch { .. })
        ));
    }

    #[test]
    fn syn_payload_rejects_truncated() {
        // Empty, 1-byte, and over-declared targets must all Err, never panic.
        assert!(SynPayload::decode(&[]).is_err());
        assert!(SynPayload::decode(&[0]).is_err());
        assert!(SynPayload::decode(&[0, 100, b'a', b'b', b'c']).is_err());
        // Exact fit decodes with empty initial data.
        let payload = SynPayload::decode(&[0, 3, b'a', b'b', b'c']).unwrap();
        assert_eq!(payload.target, b"abc");
        assert!(payload.initial_data.is_empty());
        // Sweep every truncation: cuts below the declared target end
        // (2 + 5) must Err; longer prefixes decode (remainder is
        // initial_data, possibly empty).
        let mut full = vec![0, 5];
        full.extend_from_slice(b"h:1+i");
        full.extend_from_slice(b"PAYLOAD");
        for cut in 0..=full.len() {
            let r = SynPayload::decode(&full[..cut]);
            if cut < 2 + 5 {
                assert!(r.is_err(), "truncation at {cut} decoded: {r:?}");
            } else {
                let p = r.unwrap();
                assert_eq!(p.target, b"h:1+i");
                assert_eq!(p.initial_data, &full[7..cut]);
            }
        }
    }

    #[test]
    fn trailing_obfs_padding_is_ignored() {
        // Declared payload is 2 bytes ("hi"); 1400 random pad bytes follow
        // (GoWay `-obfs` unilateral padding must not break decoding).
        let mut encoded = vec![0, 0, 0, 1, MUX_DATA, 0, 2, b'h', b'i'];
        encoded.extend_from_slice(&[0x5Au8; 1400]);
        let frame = MuxFrame::decode(&encoded).unwrap();
        assert_eq!(frame.stream_id, 1);
        assert_eq!(frame.command, MuxCommand::Data);
        assert_eq!(frame.payload, b"hi");
        // The zero-copy owned path must bound by the declared length too —
        // this was the live data-leak (pad bytes forwarded into streams).
        let owned = MuxFrame::decode_owned(encoded).unwrap();
        assert_eq!(owned.payload(), b"hi");
    }

    #[test]
    fn owned_mux_frame_from_parts() {
        let frame = OwnedMuxFrame::from_parts(42, MuxCommand::Data, b"hello world").unwrap();
        assert_eq!(frame.stream_id, 42);
        assert_eq!(frame.command, MuxCommand::Data);
        assert_eq!(frame.payload(), b"hello world");
    }
}
} // mod protocol

pub mod proxy {
//! Proxy front-end protocol primitives.
//!
//! These parsers intentionally stop at protocol decoding. Network forwarding,
//! MUX routing, DNS policy and connection lifecycle are implemented separately.
//! The shapes follow the GoWay v1.8.4 client-facing protocols observed in the
//! compatibility baseline.

use std::fmt;
use tokio::io::{AsyncReadExt, AsyncWriteExt};

pub const SOCKS5_VERSION: u8 = 0x05;
#[allow(dead_code)]
pub const SOCKS5_NO_AUTH: u8 = 0x00;
pub const SOCKS5_CONNECT: u8 = 0x01;
pub const SOCKS5_UDP_ASSOCIATE: u8 = 0x03;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TargetAddr {
    pub host: String,
    pub port: u16,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SocksCommand {
    Connect,
    UdpAssociate,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SocksRequest {
    pub command: SocksCommand,
    pub target: TargetAddr,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ClientProxyRequest {
    pub command: SocksCommand,
    pub target: TargetAddr,
    pub is_socks5: bool,
    pub is_connect: bool,
    pub initial_payload: Option<Vec<u8>>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ProxyParseError {
    Truncated,
    InvalidVersion(u8),
    #[allow(dead_code)]
    NoAcceptableMethod,
    UnsupportedCommand(u8),
    UnsupportedAddressType(u8),
    InvalidUtf8,
    InvalidPort,
    InvalidHttpRequest,
    HeaderTooLarge,
}

impl fmt::Display for ProxyParseError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Truncated => write!(f, "truncated proxy request"),
            Self::InvalidVersion(v) => write!(f, "invalid SOCKS version: {v}"),
            Self::NoAcceptableMethod => write!(f, "no acceptable SOCKS5 authentication method"),
            Self::UnsupportedCommand(c) => write!(f, "unsupported SOCKS5 command: 0x{c:02x}"),
            Self::UnsupportedAddressType(a) => {
                write!(f, "unsupported SOCKS5 address type: 0x{a:02x}")
            }
            Self::InvalidUtf8 => write!(f, "invalid SOCKS5 domain name"),
            Self::InvalidPort => write!(f, "invalid target port"),
            Self::InvalidHttpRequest => write!(f, "invalid HTTP CONNECT request"),
            Self::HeaderTooLarge => write!(f, "proxy header too large"),
        }
    }
}
impl std::error::Error for ProxyParseError {}

#[allow(dead_code)]
pub fn parse_socks5_greeting(buf: &[u8]) -> Result<(), ProxyParseError> {
    if buf.len() < 2 {
        return Err(ProxyParseError::Truncated);
    }
    if buf[0] != SOCKS5_VERSION {
        return Err(ProxyParseError::InvalidVersion(buf[0]));
    }
    let n = buf[1] as usize;
    if buf.len() < 2 + n {
        return Err(ProxyParseError::Truncated);
    }
    if buf[2..2 + n].contains(&SOCKS5_NO_AUTH) {
        Ok(())
    } else {
        Err(ProxyParseError::NoAcceptableMethod)
    }
}

pub fn parse_socks5_request(buf: &[u8]) -> Result<SocksRequest, ProxyParseError> {
    if buf.len() < 4 {
        return Err(ProxyParseError::Truncated);
    }
    if buf[0] != SOCKS5_VERSION {
        return Err(ProxyParseError::InvalidVersion(buf[0]));
    }
    let command = match buf[1] {
        SOCKS5_CONNECT => SocksCommand::Connect,
        SOCKS5_UDP_ASSOCIATE => SocksCommand::UdpAssociate,
        other => return Err(ProxyParseError::UnsupportedCommand(other)),
    };
    if buf[2] != 0 {
        return Err(ProxyParseError::InvalidHttpRequest);
    }
    let (host, consumed) = match buf[3] {
        0x01 => {
            if buf.len() < 10 {
                return Err(ProxyParseError::Truncated);
            }
            let h = format!("{}.{}.{}.{}", buf[4], buf[5], buf[6], buf[7]);
            (h, 8)
        }
        0x03 => {
            if buf.len() < 5 {
                return Err(ProxyParseError::Truncated);
            }
            let n = buf[4] as usize;
            if buf.len() < 5 + n {
                return Err(ProxyParseError::Truncated);
            }
            let h = std::str::from_utf8(&buf[5..5 + n])
                .map_err(|_| ProxyParseError::InvalidUtf8)?
                .to_owned();
            (h, 5 + n)
        }
        0x04 => {
            if buf.len() < 22 {
                return Err(ProxyParseError::Truncated);
            }
            let mut octets = [0u8; 16];
            octets.copy_from_slice(&buf[4..20]);
            (std::net::Ipv6Addr::from(octets).to_string(), 20)
        }
        other => return Err(ProxyParseError::UnsupportedAddressType(other)),
    };
    if buf.len() < consumed + 2 {
        return Err(ProxyParseError::Truncated);
    }
    let port = u16::from_be_bytes([buf[consumed], buf[consumed + 1]]);
    if port == 0 && matches!(command, SocksCommand::Connect) {
        return Err(ProxyParseError::InvalidPort);
    }
    Ok(SocksRequest {
        command,
        target: TargetAddr { host, port },
    })
}

pub fn socks5_success_response() -> [u8; 10] {
    [0x05, 0x00, 0x00, 0x01, 0, 0, 0, 0, 0, 0]
}

/// RFC 1928 general SOCKS5 failure, with an IPv4 zero bind address.
pub fn socks5_failure_response() -> [u8; 10] {
    [0x05, 0x05, 0x00, 0x01, 0, 0, 0, 0, 0, 0]
}

pub fn parse_socks5_udp_datagram(buf: &[u8]) -> Result<(TargetAddr, &[u8]), ProxyParseError> {
    if buf.len() < 4 {
        return Err(ProxyParseError::Truncated);
    }
    if buf[0] != 0 || buf[1] != 0 {
        return Err(ProxyParseError::InvalidHttpRequest);
    }
    if buf[2] != 0 {
        return Err(ProxyParseError::InvalidHttpRequest);
    }
    let (host, consumed) = match buf[3] {
        0x01 => {
            if buf.len() < 10 {
                return Err(ProxyParseError::Truncated);
            }
            (format!("{}.{}.{}.{}", buf[4], buf[5], buf[6], buf[7]), 8)
        }
        0x03 => {
            if buf.len() < 5 {
                return Err(ProxyParseError::Truncated);
            }
            let n = buf[4] as usize;
            if buf.len() < 5 + n {
                return Err(ProxyParseError::Truncated);
            }
            (
                std::str::from_utf8(&buf[5..5 + n])
                    .map_err(|_| ProxyParseError::InvalidUtf8)?
                    .to_owned(),
                5 + n,
            )
        }
        0x04 => {
            if buf.len() < 22 {
                return Err(ProxyParseError::Truncated);
            }
            let mut octets = [0u8; 16];
            octets.copy_from_slice(&buf[4..20]);
            (std::net::Ipv6Addr::from(octets).to_string(), 20)
        }
        other => return Err(ProxyParseError::UnsupportedAddressType(other)),
    };
    if buf.len() < consumed + 2 {
        return Err(ProxyParseError::Truncated);
    }
    let port = u16::from_be_bytes([buf[consumed], buf[consumed + 1]]);
    if port == 0 {
        return Err(ProxyParseError::InvalidPort);
    }
    Ok((TargetAddr { host, port }, &buf[consumed + 2..]))
}

pub fn parse_target_authority(authority: &str) -> Result<TargetAddr, ProxyParseError> {
    let authority = authority.trim();
    if authority.is_empty() {
        return Err(ProxyParseError::InvalidHttpRequest);
    }
    if !authority.starts_with('[') && authority.matches(':').count() > 1 {
        return Err(ProxyParseError::InvalidHttpRequest);
    }
    if authority.starts_with('[') {
        let close = authority
            .find(']')
            .ok_or(ProxyParseError::InvalidHttpRequest)?;
        let host = &authority[1..close];
        if host.is_empty() {
            return Err(ProxyParseError::InvalidHttpRequest);
        }
        let port_text = authority
            .get(close + 1..)
            .unwrap_or("")
            .strip_prefix(':')
            .ok_or(ProxyParseError::InvalidHttpRequest)?;
        let port = port_text
            .parse::<u16>()
            .map_err(|_| ProxyParseError::InvalidPort)?;
        if port == 0 {
            return Err(ProxyParseError::InvalidPort);
        }
        return Ok(TargetAddr {
            host: host.to_owned(),
            port,
        });
    }
    let (host, port_text) = authority
        .rsplit_once(':')
        .ok_or(ProxyParseError::InvalidHttpRequest)?;
    if host.is_empty() {
        return Err(ProxyParseError::InvalidHttpRequest);
    }
    let port = port_text
        .parse::<u16>()
        .map_err(|_| ProxyParseError::InvalidPort)?;
    if port == 0 {
        return Err(ProxyParseError::InvalidPort);
    }
    Ok(TargetAddr {
        host: host.to_owned(),
        port,
    })
}

pub fn parse_authority_with_default(
    authority: &str,
    default_port: u16,
) -> Result<TargetAddr, ProxyParseError> {
    let authority = authority.trim();
    if authority.is_empty() {
        return Err(ProxyParseError::InvalidHttpRequest);
    }
    if !authority.starts_with('[') && authority.matches(':').count() > 1 {
        return Err(ProxyParseError::InvalidHttpRequest);
    }
    if authority.starts_with('[') {
        let close = authority
            .find(']')
            .ok_or(ProxyParseError::InvalidHttpRequest)?;
        let host = &authority[1..close];
        if host.is_empty() {
            return Err(ProxyParseError::InvalidHttpRequest);
        }
        let port = match authority.get(close + 1..).and_then(|t| t.strip_prefix(':')) {
            Some(p) => {
                let val = p.parse::<u16>().map_err(|_| ProxyParseError::InvalidPort)?;
                if val == 0 {
                    return Err(ProxyParseError::InvalidPort);
                }
                val
            }
            None => default_port,
        };
        return Ok(TargetAddr {
            host: host.to_owned(),
            port,
        });
    }
    if let Some((host, port_text)) = authority.rsplit_once(':') {
        if host.is_empty() {
            return Err(ProxyParseError::InvalidHttpRequest);
        }
        let port = port_text
            .parse::<u16>()
            .map_err(|_| ProxyParseError::InvalidPort)?;
        if port == 0 {
            return Err(ProxyParseError::InvalidPort);
        }
        Ok(TargetAddr {
            host: host.to_owned(),
            port,
        })
    } else {
        Ok(TargetAddr {
            host: authority.to_owned(),
            port: default_port,
        })
    }
}

pub fn parse_http_connect(buf: &[u8]) -> Result<TargetAddr, ProxyParseError> {
    let end = buf
        .windows(4)
        .position(|w| w == b"\r\n\r\n")
        .map(|n| n + 4)
        .or_else(|| buf.windows(2).position(|w| w == b"\n\n").map(|n| n + 2))
        .ok_or(ProxyParseError::Truncated)?;
    if end > 8192 {
        return Err(ProxyParseError::HeaderTooLarge);
    }
    let first_line_end = buf[..end]
        .iter()
        .position(|&b| b == b'\n')
        .ok_or(ProxyParseError::InvalidHttpRequest)?;
    let line = std::str::from_utf8(&buf[..first_line_end])
        .map_err(|_| ProxyParseError::InvalidHttpRequest)?
        .trim_end_matches('\r');
    let mut parts = line.split_whitespace();
    if !parts
        .next()
        .is_some_and(|m| m.eq_ignore_ascii_case("CONNECT"))
    {
        return Err(ProxyParseError::InvalidHttpRequest);
    }
    let authority = parts.next().ok_or(ProxyParseError::InvalidHttpRequest)?;
    if !parts.next().is_some_and(|v| v.starts_with("HTTP/")) {
        return Err(ProxyParseError::InvalidHttpRequest);
    }
    parse_target_authority(authority)
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HttpProxyRequest {
    pub target: TargetAddr,
    pub is_connect: bool,
    pub initial_payload: Option<Vec<u8>>,
}

pub fn parse_http_proxy_request(buf: &[u8]) -> Result<HttpProxyRequest, ProxyParseError> {
    let end = buf
        .windows(4)
        .position(|w| w == b"\r\n\r\n")
        .map(|n| n + 4)
        .or_else(|| buf.windows(2).position(|w| w == b"\n\n").map(|n| n + 2))
        .ok_or(ProxyParseError::Truncated)?;
    if end > 8192 {
        return Err(ProxyParseError::HeaderTooLarge);
    }
    let first_line_end = buf[..end]
        .iter()
        .position(|&b| b == b'\n')
        .ok_or(ProxyParseError::InvalidHttpRequest)?;
    let line = std::str::from_utf8(&buf[..first_line_end])
        .map_err(|_| ProxyParseError::InvalidHttpRequest)?
        .trim_end_matches('\r');
    let mut parts = line.split_whitespace();
    let method = parts.next().ok_or(ProxyParseError::InvalidHttpRequest)?;
    let uri = parts.next().ok_or(ProxyParseError::InvalidHttpRequest)?;
    let proto = parts.next().ok_or(ProxyParseError::InvalidHttpRequest)?;
    if !proto.starts_with("HTTP/") {
        return Err(ProxyParseError::InvalidHttpRequest);
    }

    if method.eq_ignore_ascii_case("CONNECT") {
        let target = parse_http_connect(buf)?;
        return Ok(HttpProxyRequest {
            target,
            is_connect: true,
            initial_payload: None,
        });
    }

    let mut target_host = String::new();
    let mut target_port = 80u16;

    if let Some(rest) = uri
        .strip_prefix("http://")
        .or_else(|| uri.strip_prefix("https://"))
    {
        let is_https = uri.starts_with("https://");
        let default_port = if is_https { 443 } else { 80 };
        let authority = rest.split('/').next().unwrap_or(rest);
        if let Ok(target) = parse_authority_with_default(authority, default_port) {
            target_host = target.host;
            target_port = target.port;
        }
    }

    if target_host.is_empty() {
        let headers_str = std::str::from_utf8(&buf[first_line_end + 1..end])
            .map_err(|_| ProxyParseError::InvalidHttpRequest)?;
        for line in headers_str.lines() {
            let line = line.trim();
            if let Some(val) = line
                .strip_prefix("Host:")
                .or_else(|| line.strip_prefix("host:"))
            {
                if let Ok(target) = parse_authority_with_default(val.trim(), target_port) {
                    target_host = target.host;
                    target_port = target.port;
                }
                break;
            }
        }
    }

    if target_host.is_empty() {
        return Err(ProxyParseError::InvalidHttpRequest);
    }

    Ok(HttpProxyRequest {
        target: TargetAddr {
            host: target_host,
            port: target_port,
        },
        is_connect: false,
        initial_payload: Some(buf.to_vec()),
    })
}

pub async fn read_client_proxy_request<R>(stream: &mut R) -> anyhow::Result<ClientProxyRequest>
where
    R: AsyncReadExt + AsyncWriteExt + Unpin,
{
    let first = stream.read_u8().await?;
    if first == SOCKS5_VERSION {
        let n = stream.read_u8().await? as usize;
        let mut methods = [0u8; 255];
        stream.read_exact(&mut methods[..n]).await?;
        if !methods[..n].contains(&0) {
            stream.write_all(&[5, 0xff]).await?;
            anyhow::bail!("SOCKS5 no-auth unavailable")
        }
        stream.write_all(&[5, 0]).await?;
        // Max SOCKS5 request is 4 (head) + 1 (domain len) + 255 (domain) + 2 (port) = 262 bytes.
        let mut req = [0u8; 262];
        stream.read_exact(&mut req[..4]).await?;
        if req[1] != SOCKS5_CONNECT && req[1] != SOCKS5_UDP_ASSOCIATE {
            stream.write_all(&[5, 7, 0, 1, 0, 0, 0, 0, 0, 0]).await?;
            anyhow::bail!("unsupported SOCKS5 command")
        }
        let req_len = match req[3] {
            1 => {
                stream.read_exact(&mut req[4..10]).await?;
                10
            }
            3 => {
                let dlen = stream.read_u8().await? as usize;
                req[4] = dlen as u8;
                let end = 5 + dlen + 2;
                stream.read_exact(&mut req[5..end]).await?;
                end
            }
            4 => {
                stream.read_exact(&mut req[4..22]).await?;
                22
            }
            _ => {
                stream.write_all(&[5, 8, 0, 1, 0, 0, 0, 0, 0, 0]).await?;
                anyhow::bail!("unsupported SOCKS5 address type")
            }
        };
        let parsed =
            parse_socks5_request(&req[..req_len]).map_err(|e| anyhow::anyhow!(e.to_string()))?;
        return Ok(ClientProxyRequest {
            command: parsed.command,
            target: parsed.target,
            is_socks5: true,
            is_connect: false,
            initial_payload: None,
        });
    }

    let mut buf = Vec::with_capacity(512);
    buf.push(first);
    let mut temp = [0u8; 1024];
    while buf.len() < 8192 {
        let prev_len = buf.len();
        let max_to_read = (8192 - prev_len).min(temp.len());
        let n = stream.read(&mut temp[..max_to_read]).await?;
        if n == 0 {
            break;
        }
        buf.extend_from_slice(&temp[..n]);
        let scan_start = prev_len.saturating_sub(3);
        let tail = &buf[scan_start..];
        if tail.windows(4).any(|w| w == b"\r\n\r\n") || tail.windows(2).any(|w| w == b"\n\n") {
            break;
        }
    }
    let req = parse_http_proxy_request(&buf).map_err(|e| anyhow::anyhow!(e.to_string()))?;
    Ok(ClientProxyRequest {
        command: SocksCommand::Connect,
        target: req.target,
        is_socks5: false,
        is_connect: req.is_connect,
        initial_payload: req.initial_payload,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn socks_greeting_accepts_no_auth() {
        assert!(parse_socks5_greeting(&[5, 1, 0]).is_ok());
        assert!(matches!(
            parse_socks5_greeting(&[5, 1, 2]),
            Err(ProxyParseError::NoAcceptableMethod)
        ));
    }

    #[test]
    fn socks_connect_ipv4() {
        let req = [5, 1, 0, 1, 127, 0, 0, 1, 0x01, 0xbb];
        assert_eq!(
            parse_socks5_request(&req).unwrap().target,
            TargetAddr {
                host: "127.0.0.1".into(),
                port: 443
            }
        );
    }

    #[test]
    fn socks_udp_associate_allows_zero_port() {
        let req = [5, 3, 0, 1, 0, 0, 0, 0, 0, 0];
        let parsed = parse_socks5_request(&req).unwrap();
        assert_eq!(parsed.command, SocksCommand::UdpAssociate);
        assert_eq!(parsed.target.port, 0);
    }

    #[test]
    fn socks_connect_domain() {
        let mut req = vec![5, 1, 0, 3, 11];
        req.extend_from_slice(b"example.com");
        req.extend_from_slice(&443u16.to_be_bytes());
        assert_eq!(
            parse_socks5_request(&req).unwrap().target.host,
            "example.com"
        );
    }

    #[test]
    fn socks_udp_domain_and_payload() {
        let mut pkt = vec![0, 0, 0, 3, 11];
        pkt.extend_from_slice(b"example.com");
        pkt.extend_from_slice(&53u16.to_be_bytes());
        pkt.extend_from_slice(b"dns");
        let (target, data) = parse_socks5_udp_datagram(&pkt).unwrap();
        assert_eq!(target.port, 53);
        assert_eq!(data, b"dns");
    }

    #[test]
    fn socks_failure_response_is_general_failure() {
        assert_eq!(socks5_failure_response(), [5, 5, 0, 1, 0, 0, 0, 0, 0, 0]);
    }

    #[test]
    fn http_connect_ipv4_and_ipv6() {
        let req = b"CONNECT 127.0.0.1:443 HTTP/1.1\r\nHost: 127.0.0.1:443\r\n\r\n";
        assert_eq!(parse_http_connect(req).unwrap().port, 443);
        let req6 = b"CONNECT [::1]:8443 HTTP/1.1\r\n\r\n";
        assert_eq!(parse_http_connect(req6).unwrap().host, "::1");
    }

    #[test]
    fn http_header_limit_is_enforced() {
        let mut req = b"CONNECT example.com:443 HTTP/1.1\r\nX: ".to_vec();
        req.resize(8193, b'x');
        req.extend_from_slice(b"\r\n\r\n");
        assert!(matches!(
            parse_http_connect(&req),
            Err(ProxyParseError::HeaderTooLarge)
        ));
    }

    #[test]
    fn http_proxy_request_plain_get_and_post() {
        let get_req = b"GET http://example.com/index.html HTTP/1.1\r\nHost: example.com\r\n\r\n";
        let parsed = parse_http_proxy_request(get_req).unwrap();
        assert_eq!(parsed.target.host, "example.com");
        assert_eq!(parsed.target.port, 80);
        assert!(!parsed.is_connect);
        assert_eq!(parsed.initial_payload.unwrap(), get_req);

        let post_req = b"POST /api/test HTTP/1.1\r\nHost: api.example.com:8080\r\n\r\n";
        let parsed_post = parse_http_proxy_request(post_req).unwrap();
        assert_eq!(parsed_post.target.host, "api.example.com");
        assert_eq!(parsed_post.target.port, 8080);
        assert!(!parsed_post.is_connect);
    }

    #[tokio::test]
    async fn test_read_client_proxy_request_socks5_and_http() {
        let (mut client, mut server) = tokio::io::duplex(1024);
        tokio::spawn(async move {
            client.write_all(&[5, 1, 0]).await.unwrap();
            let mut auth_resp = [0u8; 2];
            client.read_exact(&mut auth_resp).await.unwrap();
            assert_eq!(auth_resp, [5, 0]);
            client
                .write_all(&[5, 1, 0, 1, 127, 0, 0, 1, 0x01, 0xbb])
                .await
                .unwrap();
        });
        let req = read_client_proxy_request(&mut server).await.unwrap();
        assert_eq!(req.command, SocksCommand::Connect);
        assert!(req.is_socks5);
        assert_eq!(req.target.port, 443);

        let (mut client, mut server) = tokio::io::duplex(1024);
        tokio::spawn(async move {
            client
                .write_all(b"GET http://example.com/ HTTP/1.1\r\nHost: example.com\r\n\r\n")
                .await
                .unwrap();
        });
        let req2 = read_client_proxy_request(&mut server).await.unwrap();
        assert_eq!(req2.command, SocksCommand::Connect);
        assert!(!req2.is_socks5);
        assert!(!req2.is_connect);
        assert_eq!(req2.target.host, "example.com");
        assert_eq!(req2.target.port, 80);

        let (mut client, mut server) = tokio::io::duplex(1024);
        tokio::spawn(async move {
            client.write_all(&[5, 1, 0]).await.unwrap();
            let mut auth_resp = [0u8; 2];
            client.read_exact(&mut auth_resp).await.unwrap();
            assert_eq!(auth_resp, [5, 0]);
            client
                .write_all(&[5, 3, 0, 1, 127, 0, 0, 1, 0x00, 0x00])
                .await
                .unwrap();
        });
        let req3 = read_client_proxy_request(&mut server).await.unwrap();
        assert_eq!(req3.command, SocksCommand::UdpAssociate);
        assert!(req3.is_socks5);

        let (mut client, mut server) = tokio::io::duplex(1024);
        tokio::spawn(async move {
            client
                .write_all(b"POST http://example.com/api HTTP/1.1\r\nHost: example.com\r\nContent-Length: 5\r\n\r\nhello")
                .await
                .unwrap();
        });
        let req4 = read_client_proxy_request(&mut server).await.unwrap();
        assert_eq!(req4.command, SocksCommand::Connect);
        assert!(!req4.is_socks5);
        assert!(!req4.is_connect);
        assert_eq!(req4.target.host, "example.com");
        let initial = req4.initial_payload.expect("must have initial payload");
        assert!(initial.ends_with(b"hello"));
    }

    #[test]
    fn test_parse_target_authority_and_default() {
        let t1 = parse_target_authority("127.0.0.1:8080").unwrap();
        assert_eq!(t1.host, "127.0.0.1");
        assert_eq!(t1.port, 8080);

        let t2 = parse_target_authority("example.com:443").unwrap();
        assert_eq!(t2.host, "example.com");
        assert_eq!(t2.port, 443);

        let t3 = parse_target_authority("[2001:db8::1]:8443").unwrap();
        assert_eq!(t3.host, "2001:db8::1");
        assert_eq!(t3.port, 8443);

        let t4 = parse_authority_with_default("[2001:db8::1]", 80).unwrap();
        assert_eq!(t4.host, "2001:db8::1");
        assert_eq!(t4.port, 80);

        let t5 = parse_authority_with_default("example.com", 8080).unwrap();
        assert_eq!(t5.host, "example.com");
        assert_eq!(t5.port, 8080);

        assert!(parse_target_authority("example.com").is_err());
        assert!(parse_target_authority("example.com:0").is_err());
        assert!(parse_target_authority("2001:db8::1").is_err());
        assert!(parse_authority_with_default("2001:db8::1", 80).is_err());
    }
}
} // mod proxy

pub mod quic {
//! GoWay v1.8.4-compatible QUIC transport.

use crate::common::{
    socks5_udp_associate_reply, HTTP_200_CONNECTION_ESTABLISHED, HTTP_502_BAD_GATEWAY,
};
use crate::dns;
use crate::proxy::{
    parse_authority_with_default, parse_socks5_udp_datagram, parse_target_authority,
    read_client_proxy_request, socks5_failure_response, socks5_success_response,
    ClientProxyRequest, SocksCommand, TargetAddr,
};
use crate::runtime::{
    apply_listener_options, apply_socket_options, drain_join_set, enforce_target_policy,
    recycle_buf, relay_buf, resolve_target, wait_shutdown, RuntimeConfig,
};
use crate::udp_batch::{UdpBatchReader, UdpBatchWriter};
use anyhow::{anyhow, bail, Context, Result};
use quinn::crypto::rustls::{QuicClientConfig, QuicServerConfig};
use quinn::{
    ClientConfig, Connection, Endpoint, RecvStream, SendStream, ServerConfig, TransportConfig,
    VarInt,
};
use rcgen::generate_simple_self_signed;
use rustls::client::danger::{HandshakeSignatureValid, ServerCertVerified, ServerCertVerifier};
use rustls::pki_types::{CertificateDer, PrivateKeyDer, PrivatePkcs8KeyDer, ServerName, UnixTime};
use rustls::{ClientConfig as RustlsClientConfig, RootCertStore};
use std::collections::HashSet;
use std::sync::{Arc, Mutex as StdMutex};
use std::time::Duration;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream, UdpSocket};
use tokio::sync::{Mutex, Semaphore};
use tokio::time::timeout;
use zeroize::Zeroizing;

const ALPN: &[&[u8]] = &[b"goway-quic", b"h3"];
const MAX_QUIC_UDP_PACKET: usize = u16::MAX as usize;
/// Default UDP socket buffer for QUIC endpoints when `--socket-buffer` is
/// unset. OS defaults (tens of KB) starve bulk transfer; quinn itself
/// documents enlarged buffers as required for throughput.
const DEFAULT_QUIC_SOCKET_BUFFER: usize = 8 * 1024 * 1024;

fn quic_socket_buffer_bytes(cfg: &RuntimeConfig) -> usize {
    if cfg.socket_buffer > 0 {
        cfg.socket_buffer.saturating_mul(1024)
    } else {
        DEFAULT_QUIC_SOCKET_BUFFER
    }
}

/// Builds a bound UDP socket with enlarged send/receive buffers for a QUIC
/// endpoint. Best-effort: the OS may clamp silently, which only reduces the
/// gain instead of failing the bind.
fn bound_udp_socket(bind: std::net::SocketAddr, buf_bytes: usize) -> Result<std::net::UdpSocket> {
    let sock = socket2::Socket::new(
        socket2::Domain::for_address(bind),
        socket2::Type::DGRAM,
        Some(socket2::Protocol::UDP),
    )
    .context("QUIC UDP socket creation failed")?;
    sock.set_nonblocking(true)
        .context("QUIC UDP nonblocking failed")?;
    // Dual-stack is mandatory for wildcard IPv6 binds: without it,
    // IPv4-mapped destinations (::ffff:127.0.0.1) fail with
    // AddrNotAvailable. Quinn enables this internally; doing it manually
    // is the price of custom socket buffers.
    if bind.is_ipv6() {
        sock.set_only_v6(false)
            .context("QUIC dual-stack setup failed")?;
    }
    if sock.set_send_buffer_size(buf_bytes).is_err() {
        tracing::debug!(bytes = buf_bytes, "QUIC UDP send buffer request denied");
    }
    if sock.set_recv_buffer_size(buf_bytes).is_err() {
        tracing::debug!(bytes = buf_bytes, "QUIC UDP receive buffer request denied");
    }
    sock.bind(&bind.into()).context("QUIC UDP bind failed")?;
    Ok(sock.into())
}

fn transport_config() -> Arc<TransportConfig> {
    let mut cfg = TransportConfig::default();
    // Trimmed (GoWay #5 parity): idle 60s -> 30s so dead mobile conns are
    // reclaimed faster; explicit per-conn bidi stream cap bounds a
    // malicious peer's stream table; DATAGRAM receive disabled — no
    // send/read_datagram call exists anywhere, so negotiating it only
    // costs handshake bytes. KeepAlive stays 15s (longer risks NAT-binding
    // loss on strict networks); uni streams stay at 0 (unused, stricter
    // than GoWay's 128); windows stay 8/16 MiB (bulk-tuned, GoWay parity).
    cfg.max_idle_timeout(Some(Duration::from_secs(30).try_into().expect("30s fits")));
    cfg.keep_alive_interval(Some(Duration::from_secs(15)));
    cfg.max_concurrent_bidi_streams(VarInt::from_u32(512));
    cfg.datagram_receive_buffer_size(None);
    cfg.stream_receive_window(VarInt::from_u64(8 * 1024 * 1024).expect("8MiB fits QUIC VarInt"));
    cfg.receive_window(VarInt::from_u64(16 * 1024 * 1024).expect("16MiB fits QUIC VarInt"));
    cfg.max_concurrent_uni_streams(0u32.into());
    // Without path MTU discovery every datagram stays near 1200 bytes;
    // discovering larger MTUs cuts per-packet crypto/scheduling overhead.
    cfg.mtu_discovery_config(Some(quinn::MtuDiscoveryConfig::default()));
    // NOTE (2026-09-17): BbrConfig was tried here and REVERTED — on
    // loopback it halved throughput and tripled variance vs Cubic
    // (c8 92→~55). Keep Cubic until VPS-line evidence says otherwise.
    Arc::new(cfg)
}
fn insecure_client_crypto() -> Result<RustlsClientConfig> {
    let mut cfg = RustlsClientConfig::builder()
        .dangerous()
        .with_custom_certificate_verifier(Arc::new(QuicNoCertificateVerification))
        .with_no_client_auth();
    cfg.alpn_protocols = ALPN.iter().map(|v| v.to_vec()).collect();
    Ok(cfg)
}
fn verified_client_crypto() -> Result<RustlsClientConfig> {
    let mut roots = RootCertStore::empty();
    roots.extend(webpki_roots::TLS_SERVER_ROOTS.iter().cloned());
    let mut cfg = RustlsClientConfig::builder()
        .with_root_certificates(roots)
        .with_no_client_auth();
    cfg.alpn_protocols = ALPN.iter().map(|v| v.to_vec()).collect();
    Ok(cfg)
}
fn client_config(verify_ssl: bool) -> Result<ClientConfig> {
    let rustls = if verify_ssl {
        verified_client_crypto()?
    } else {
        insecure_client_crypto()?
    };
    let crypto = QuicClientConfig::try_from(rustls).context("build QUIC client crypto")?;
    let mut cfg = ClientConfig::new(Arc::new(crypto));
    cfg.transport_config(transport_config());
    Ok(cfg)
}
fn server_config() -> Result<ServerConfig> {
    // `--cert`/`--key` identity when configured, else the historical
    // self-signed fallback (which only clients running `--no-verify-ssl`
    // will accept).
    let (certs, key) = match crate::tls::server_identity() {
        Some(identity) => identity,
        None => {
            let cert = generate_simple_self_signed(vec!["localhost".into()])
                .context("generate QUIC certificate")?;
            let key =
                PrivateKeyDer::Pkcs8(PrivatePkcs8KeyDer::from(cert.key_pair.serialize_der()));
            let cert_der: CertificateDer<'static> = cert.cert.der().clone();
            (vec![cert_der], key)
        }
    };
    let mut rustls = rustls::ServerConfig::builder()
        .with_no_client_auth()
        .with_single_cert(certs, key)
        .context("build QUIC server TLS")?;
    rustls.alpn_protocols = ALPN.iter().map(|v| v.to_vec()).collect();
    let crypto = QuicServerConfig::try_from(rustls).context("build QUIC server crypto")?;
    let mut cfg = ServerConfig::with_crypto(Arc::new(crypto));
    cfg.transport_config(transport_config());
    Ok(cfg)
}
#[derive(Debug)]
struct QuicNoCertificateVerification;
impl ServerCertVerifier for QuicNoCertificateVerification {
    fn verify_server_cert(
        &self,
        _: &CertificateDer<'_>,
        _: &[CertificateDer<'_>],
        _: &ServerName<'_>,
        _: &[u8],
        _: UnixTime,
    ) -> std::result::Result<ServerCertVerified, rustls::Error> {
        Ok(ServerCertVerified::assertion())
    }
    fn verify_tls12_signature(
        &self,
        _: &[u8],
        _: &CertificateDer<'_>,
        _: &rustls::DigitallySignedStruct,
    ) -> std::result::Result<HandshakeSignatureValid, rustls::Error> {
        Ok(HandshakeSignatureValid::assertion())
    }
    fn verify_tls13_signature(
        &self,
        _: &[u8],
        _: &CertificateDer<'_>,
        _: &rustls::DigitallySignedStruct,
    ) -> std::result::Result<HandshakeSignatureValid, rustls::Error> {
        Ok(HandshakeSignatureValid::assertion())
    }
    fn supported_verify_schemes(&self) -> Vec<rustls::SignatureScheme> {
        vec![
            rustls::SignatureScheme::ECDSA_NISTP256_SHA256,
            rustls::SignatureScheme::ECDSA_NISTP384_SHA384,
            rustls::SignatureScheme::ECDSA_NISTP521_SHA512,
            rustls::SignatureScheme::ED25519,
            rustls::SignatureScheme::RSA_PSS_SHA256,
            rustls::SignatureScheme::RSA_PSS_SHA384,
            rustls::SignatureScheme::RSA_PSS_SHA512,
            rustls::SignatureScheme::RSA_PKCS1_SHA256,
            rustls::SignatureScheme::RSA_PKCS1_SHA384,
            rustls::SignatureScheme::RSA_PKCS1_SHA512,
        ]
    }
}
fn parse_upstream(input: &str) -> Result<String> {
    let rest = input
        .strip_prefix("quic://")
        .or_else(|| input.strip_prefix("quic+tls://"))
        .ok_or_else(|| anyhow!("QUIC client requires quic:// or quic+tls:// upstream"))?;
    let authority = match rest.split_once('/') {
        Some((a, _)) => a,
        None => rest,
    };
    if authority.is_empty() {
        bail!("empty QUIC upstream authority")
    }
    Ok(
        if authority.starts_with('[') || authority.matches(':').count() == 1 {
            authority.to_string()
        } else {
            format!("{}:443", authority)
        },
    )
}
async fn resolve_upstream(input: &str) -> Result<(std::net::SocketAddr, String)> {
    let authority = parse_upstream(input)?;
    let target =
        parse_authority_with_default(&authority, 443).map_err(|e| anyhow!(e.to_string()))?;
    let addr = dns::resolve_socket(&target.host, target.port).await?;
    Ok((addr, target.host))
}
async fn read_quic_line(recv: &mut RecvStream, limit: usize) -> Result<String> {
    let mut buf = Vec::with_capacity(128);
    while buf.len() < limit {
        let mut one = [0u8; 1];
        let n = recv.read(&mut one).await?;
        let Some(n) = n else {
            bail!("QUIC stream closed while reading header")
        };
        if n == 0 {
            bail!("QUIC stream returned empty header read")
        };
        buf.push(one[0]);
        if one[0] == b'\n' {
            return Ok(String::from_utf8(buf)?.trim().to_string());
        }
    }
    bail!("QUIC header too large")
}
/// SHA-256 of `key` as lowercase hex: the only key form a client puts on the
/// wire, so a plaintext key never traverses an untrusted hop.
fn key_digest_hex(key: &str) -> String {
    use std::fmt::Write as _;
    let digest = ring::digest::digest(&ring::digest::SHA256, key.as_bytes());
    let mut hex = String::with_capacity(2 * digest.as_ref().len());
    for byte in digest.as_ref() {
        let _ = write!(hex, "{:02x}", byte);
    }
    hex
}
/// Byte-wise XOR-fold equality for the auth line (std-only; the fold avoids
/// memcmp's early-exit on key material).
fn line_eq(a: &str, b: &str) -> bool {
    a.len() == b.len() && a.bytes().zip(b.bytes()).fold(0u8, |acc, (x, y)| acc | (x ^ y)) == 0
}
/// First-line QUIC auth: the SHA-256 hex digest of the configured key is the
/// primary form; the legacy inline plaintext key stays accepted for
/// plaintext-only peers (GoWay interop).
fn quic_auth_matches(presented: &str, configured: &Zeroizing<String>) -> bool {
    if line_eq(presented, &key_digest_hex(configured)) {
        return true;
    }
    if line_eq(presented, configured.as_str()) {
        tracing::warn!("QUIC peer authenticated with a legacy inline plaintext key");
        true
    } else {
        false
    }
}
/// True when a first-line response reads as an auth rejection ("...AUTH...")
/// rather than a downstream failure (dial, target policy): only this shape
/// justifies re-sending the key in legacy plaintext form.
fn is_auth_rejection(response: &str) -> bool {
    response.to_ascii_uppercase().contains("AUTH")
}

/// Writes the failure response to the local proxy client and closes it.
async fn quic_reject_local(local: &mut TcpStream, is_socks5: bool) -> Result<()> {
    if is_socks5 {
        local.write_all(&socks5_failure_response()).await?
    } else {
        local.write_all(HTTP_502_BAD_GATEWAY).await?
    }
    local.shutdown().await.ok();
    Ok(())
}
/// Reads one length-prefixed datagram into `buf` and returns its length.
///
/// Returns `Ok(None)` on a clean end of stream. Callers slice `&buf[..n]`
/// afterwards — the previous `Ok(Some(buf.clone()))` cost a heap allocation
/// plus a full copy on every inbound datagram, which is pure overhead on the
/// UDP hot path since `buf` is reused for the next read anyway.
async fn read_len_prefixed_udp(
    recv: &mut RecvStream,
    buf: &mut Vec<u8>,
) -> Result<Option<usize>> {
    let mut len_buf = [0u8; 2];
    match recv.read_exact(&mut len_buf).await {
        Ok(()) => {}
        Err(_) => return Ok(None),
    }
    let len = u16::from_be_bytes(len_buf) as usize;
    if len > buf.capacity() {
        // `Vec::reserve` takes *additional* capacity beyond `len`, not a total.
        buf.reserve(len.saturating_sub(buf.len()))
    }
    buf.resize(len, 0);
    recv.read_exact(buf).await?;
    Ok(Some(len))
}
async fn write_len_prefixed_udp(send: &mut SendStream, payload: &[u8]) -> Result<()> {
    if payload.len() > MAX_QUIC_UDP_PACKET {
        bail!("QUIC UDP payload too large")
    }
    send.write_all(&(payload.len() as u16).to_be_bytes())
        .await?;
    send.write_all(payload).await?;
    Ok(())
}
async fn write_len_prefixed_udp_reuse(
    send: &mut SendStream,
    payload: &[u8],
    scratch: &mut Vec<u8>,
) -> Result<()> {
    if payload.len() > MAX_QUIC_UDP_PACKET {
        bail!("QUIC UDP payload too large")
    }
    scratch.clear();
    scratch.reserve(2 + payload.len());
    scratch.extend_from_slice(&(payload.len() as u16).to_be_bytes());
    scratch.extend_from_slice(payload);
    send.write_all(scratch).await?;
    Ok(())
}
#[derive(Clone)]
struct QuicClientPool {
    endpoint: Endpoint,
    server_addr: std::net::SocketAddr,
    server_name: String,
    connection: Arc<Mutex<Option<Connection>>>,
    timeout_secs: u64,
    /// Latched once an upstream explicitly rejects the key digest with an
    /// auth error: later connections start on the legacy inline key so the
    /// failed digest handshake is paid once per process, not per connection.
    needs_plaintext: Arc<StdMutex<bool>>,
}
impl QuicClientPool {
    fn new(
        endpoint: Endpoint,
        server_addr: std::net::SocketAddr,
        server_name: String,
        timeout_secs: u64,
    ) -> Arc<Self> {
        Arc::new(Self {
            endpoint,
            server_addr,
            server_name,
            connection: Arc::new(Mutex::new(None)),
            timeout_secs,
            needs_plaintext: Arc::new(StdMutex::new(false)),
        })
    }
    async fn open_bi(&self) -> Result<(SendStream, RecvStream)> {
        for _ in 0..2 {
            let connection = {
                let mut guard = self.connection.lock().await;
                if let Some(existing) = guard
                    .as_ref()
                    .filter(|c| c.close_reason().is_none())
                    .cloned()
                {
                    existing
                } else {
                    let connecting = self.endpoint.connect(self.server_addr, &self.server_name)?;
                    let conn = timeout(Duration::from_secs(self.timeout_secs.max(1)), connecting)
                        .await??;
                    *guard = Some(conn.clone());
                    conn
                }
            };
            match connection.open_bi().await {
                Ok(pair) => return Ok(pair),
                Err(_) => {
                    let mut guard = self.connection.lock().await;
                    if guard
                        .as_ref()
                        .is_some_and(|c| c.stable_id() == connection.stable_id())
                    {
                        guard.take();
                    }
                }
            }
        }
        bail!("QUIC pooled connection unavailable")
    }
    /// Whether an earlier connection learned (via an explicit auth rejection
    /// of the key digest) that this upstream is plaintext-only.
    fn plaintext_needed(&self) -> bool {
        *self
            .needs_plaintext
            .lock()
            .unwrap_or_else(|e| e.into_inner())
    }
    /// Latches the plaintext-only marker (idempotent).
    fn mark_plaintext_needed(&self) {
        *self
            .needs_plaintext
            .lock()
            .unwrap_or_else(|e| e.into_inner()) = true;
    }
}
fn format_target(target: &TargetAddr) -> String {
    format!("{}:{}", target.host, target.port)
}
async fn relay_quic(
    mut local: TcpStream,
    cfg: RuntimeConfig,
    pool: Arc<QuicClientPool>,
    req: ClientProxyRequest,
) -> Result<()> {
    enforce_target_policy(&cfg, &req.target)?;
    let (mut send, mut recv) = match pool.open_bi().await {
        Ok(v) => v,
        Err(error) => {
            quic_reject_local(&mut local, req.is_socks5).await?;
            return Err(error);
        }
    };
    // The key crosses the wire as SHA-256(key) hex so a plaintext key never
    // traverses an untrusted (--no-verify-ssl) hop. Plaintext-only upstreams
    // (GoWay) reject the digest line: fall back to the legacy inline key
    // exactly once, on a fresh stream — but only on an auth-shaped rejection
    // or a stream closed before any response. A downstream failure after an
    // accepted digest (dial, target policy) must never re-send the key.
    let key = cfg.key.as_ref().map(|k| Zeroizing::new(k.clone()));
    let target = format_target(&req.target);
    let mut response = String::new();
    let mut retried = pool.plaintext_needed();
    loop {
        let header = match key.as_deref() {
            Some(k) if retried => format!("{} {}\n", k, target),
            Some(k) => format!("{} {}\n", key_digest_hex(k), target),
            None => format!("{}\n", target),
        };
        send.write_all(header.as_bytes()).await?;
        let mut no_response = false;
        match read_quic_line(&mut recv, 8192).await {
            Ok(line) => response = line,
            Err(error) => {
                if key.is_none() || retried {
                    return Err(error);
                }
                no_response = true;
            }
        }
        if response == "OK" || key.is_none() || retried {
            break;
        }
        if !no_response && !is_auth_rejection(&response) {
            break;
        }
        tracing::warn!("QUIC upstream rejected the key digest; retrying once with the legacy inline key");
        retried = true;
        if !no_response {
            // An explicit auth rejection is deterministic evidence of a
            // plaintext-only upstream; a closed stream is not, so it stays
            // per-connection and cannot latch the pool on a transient error.
            pool.mark_plaintext_needed();
        }
        (send, recv) = match pool.open_bi().await {
            Ok(v) => v,
            Err(error) => {
                quic_reject_local(&mut local, req.is_socks5).await?;
                return Err(error);
            }
        };
    }
    if response != "OK" {
        quic_reject_local(&mut local, req.is_socks5).await?;
        bail!("QUIC upstream rejected target: {}", response)
    }
    if req.is_socks5 {
        local.write_all(&socks5_success_response()).await?
    } else if req.is_connect {
        local.write_all(HTTP_200_CONNECTION_ESTABLISHED).await?
    }
    if let Some(initial) = req.initial_payload {
        send.write_all(&initial).await?;
    }
    let (lr, lw) = local.into_split();
    let buffer_size = cfg.buffer_size;
    let upload = tokio::spawn(async move {
        let mut r = lr;
        let mut buf = relay_buf(buffer_size).await;
        loop {
            let n = r.read(&mut buf).await?;
            if n == 0 {
                let _ = send.finish();
                break;
            }
            crate::stats::add_bytes(n as i64, 0);
            send.write_all(&buf[..n]).await?
        }
        recycle_buf(buf).await;
        Result::<()>::Ok(())
    });
    let mut lw2 = lw;
    let mut buf = relay_buf(buffer_size).await;
    loop {
        match recv.read(&mut buf).await? {
            Some(0) | None => break,
            Some(n) => {
                crate::stats::add_bytes(0, n as i64);
                lw2.write_all(&buf[..n]).await?
            }
        }
    }
    let _ = lw2.shutdown().await;
    upload.abort();
    recycle_buf(buf).await;
    Ok(())
}
async fn relay_quic_udp(
    mut control: TcpStream,
    cfg: RuntimeConfig,
    pool: Arc<QuicClientPool>,
) -> Result<()> {
    let udp = Arc::new(UdpSocket::bind("0.0.0.0:0").await?);
    let bound = udp.local_addr()?;
    let resp = socks5_udp_associate_reply(bound);
    control.write_all(&resp).await?;
    let (mut send, mut recv) = pool.open_bi().await?;
    // Same digest-first handshake as the TCP relay: the plaintext key only
    // goes on the wire when a plaintext-only upstream rejects the digest —
    // and only on an auth-shaped rejection or a stream closed before any
    // response, never after the digest was accepted and a downstream error
    // came back.
    let key = cfg.key.as_ref().map(|k| Zeroizing::new(k.clone()));
    let mut ok = String::new();
    let mut retried = pool.plaintext_needed();
    loop {
        let header = match key.as_deref() {
            Some(k) if retried => format!("{} UDP\n", k),
            Some(k) => format!("{} UDP\n", key_digest_hex(k)),
            None => "UDP\n".to_string(),
        };
        send.write_all(header.as_bytes()).await?;
        let mut no_response = false;
        match read_quic_line(&mut recv, 8192).await {
            Ok(line) => ok = line,
            Err(error) => {
                if key.is_none() || retried {
                    return Err(error);
                }
                no_response = true;
            }
        }
        if ok == "OK" || key.is_none() || retried {
            break;
        }
        if !no_response && !is_auth_rejection(&ok) {
            break;
        }
        tracing::warn!("QUIC UDP upstream rejected the key digest; retrying once with the legacy inline key");
        retried = true;
        if !no_response {
            // Same latch rule as the TCP relay: only an explicit auth
            // rejection marks the upstream plaintext-only.
            pool.mark_plaintext_needed();
        }
        (send, recv) = pool.open_bi().await?;
    }
    if ok != "OK" {
        bail!("QUIC UDP upstream rejected: {}", ok)
    }
    let latest = Arc::new(StdMutex::new(None::<std::net::SocketAddr>));
    let udp_send = udp.clone();
    let latest_send = latest.clone();
    let mut send_task = send;
    let upload = tokio::spawn(async move {
        let mut batch = UdpBatchReader::new(udp_send);
        let mut scratch = Vec::with_capacity(2048);
        loop {
            let count = match batch.recv().await {
                Ok(n) => n,
                Err(_) => break,
            };
            for i in 0..count {
                let (pkt, peer) = batch.packet(i);
                *latest_send.lock().unwrap_or_else(|e| e.into_inner()) = Some(peer);
                crate::stats::add_bytes(pkt.len() as i64, 0);
                if write_len_prefixed_udp_reuse(&mut send_task, pkt, &mut scratch)
                    .await
                    .is_err()
                {
                    return Ok::<(), anyhow::Error>(());
                }
            }
        }
        Ok::<(), anyhow::Error>(())
    });
    let mut frame_buf = Vec::with_capacity(64 * 1024);
    let mut dummy = [0u8; 1];
    let mut batch_writer = UdpBatchWriter::new(udp.clone());
    tokio::select! {
        _ = control.read(&mut dummy) => {}
        _ = async {
            loop {
                let Some(packet_len) =
                    read_len_prefixed_udp(&mut recv, &mut frame_buf).await?
                else {
                    break;
                };
                let packet = &frame_buf[..packet_len];
                let (target, _payload) =
                    parse_socks5_udp_datagram(packet).map_err(|e| anyhow!(e.to_string()))?;
                if enforce_target_policy(&cfg, &target).is_err() {
                    continue;
                }
                let peer_opt = *latest.lock().unwrap_or_else(|e| e.into_inner());
                if let Some(peer) = peer_opt {
                    let _ = batch_writer.send(packet, peer).await;
                    crate::stats::add_bytes(0, packet_len as i64);
                }
            }
            let _ = batch_writer.flush().await;
            Ok::<(), anyhow::Error>(())
        } => {}
    }
    upload.abort();
    let _ = control.shutdown().await;
    Ok(())
}

pub async fn run_client(cfg: RuntimeConfig, verify_ssl: bool) -> Result<()> {
    let upstream = cfg
        .upstream
        .clone()
        .ok_or_else(|| anyhow!("QUIC client requires upstream"))?;
    let (server_addr, server_name) = resolve_upstream(&upstream).await?;
    let socket_buf = quic_socket_buffer_bytes(&cfg);
    let std_socket = bound_udp_socket("[::]:0".parse().unwrap(), socket_buf)?;
    let mut endpoint = Endpoint::new(
        Default::default(),
        None,
        std_socket,
        Arc::new(quinn::TokioRuntime),
    )?;
    endpoint.set_default_client_config(client_config(verify_ssl)?);
    tracing::debug!(
        bytes = socket_buf,
        "QUIC client UDP socket buffers requested"
    );
    let pool = QuicClientPool::new(endpoint, server_addr, server_name, cfg.connection_timeout);
    let listener = TcpListener::bind(format!("{}:{}", cfg.proxy_host, cfg.proxy_port)).await?;
    apply_listener_options(&listener);
    let semaphore = Arc::new(Semaphore::new(cfg.max_connections.max(1)));
    tracing::info!(
        "RushWay QUIC client proxy listening on {}:{}",
        cfg.proxy_host,
        cfg.proxy_port
    );
    let mut set = tokio::task::JoinSet::new();
    loop {
        tokio::select! {
            _ = wait_shutdown() => {
                tracing::info!("shutdown requested, draining QUIC client connections");
                break;
            }
            res = listener.accept() => {
                // A transient accept error (EMFILE, ENFILE, ENOBUFS) must not
                // kill the whole proxy: log, back off briefly, and retry.
                let (mut stream, peer) = match res {
                    Ok(v) => v,
                    Err(e) => {
                        tracing::warn!(error=%e, "QUIC client listener accept failed; retrying in 100ms");
                        tokio::time::sleep(Duration::from_millis(100)).await;
                        continue;
                    }
                };
                apply_socket_options(&stream, &cfg);
                let permit = match semaphore.clone().try_acquire_owned() {
                    Ok(v) => v,
                    Err(_) => {
                        tracing::debug!(%peer,"maximum QUIC client connections reached");
                        continue;
                    }
                };
                let cfg2 = cfg.clone();
                let pool2 = pool.clone();
                set.spawn(async move {
                    let _permit = permit;
                    let _conn = crate::stats::ConnGuard::new();
                    // The SOCKS/HTTP request must be read inside the spawned
                    // task. Doing it on the accept loop meant a client that
                    // connected and then sent nothing stalled `listener.accept()`
                    // forever, freezing every other QUIC connection while it
                    // held a semaphore permit.
                    let req = match timeout(
                        Duration::from_secs(cfg2.connection_timeout.max(1)),
                        read_client_proxy_request(&mut stream),
                    )
                    .await
                    {
                        Ok(Ok(v)) => v,
                        Ok(Err(e)) => {
                            tracing::debug!(%peer, error = %e, "QUIC proxy request rejected");
                            return;
                        }
                        Err(_) => {
                            tracing::debug!(%peer, "QUIC proxy request timed out");
                            return;
                        }
                    };
                    match req.command {
                        SocksCommand::UdpAssociate => {
                            if let Err(e) = relay_quic_udp(stream, cfg2, pool2).await {
                                tracing::debug!(%peer,error=%e,"QUIC UDP proxy closed")
                            }
                        }
                        SocksCommand::Connect => {
                            if let Err(e) = relay_quic(stream, cfg2, pool2, req).await {
                                tracing::debug!(%peer,error=%e,"QUIC proxy connection closed")
                            }
                        }
                    }
                });
            }
        }
    }
    drain_join_set(&mut set).await;
    Ok(())
}

pub async fn run_server(cfg: RuntimeConfig) -> Result<()> {
    let bind: std::net::SocketAddr = format!("{}:{}", cfg.proxy_host, cfg.proxy_port).parse()?;
    let socket_buf = quic_socket_buffer_bytes(&cfg);
    let std_socket = bound_udp_socket(bind, socket_buf)?;
    let endpoint = Endpoint::new(
        Default::default(),
        Some(server_config()?),
        std_socket,
        Arc::new(quinn::TokioRuntime),
    )?;
    tracing::debug!(
        bytes = socket_buf,
        "QUIC server UDP socket buffers requested"
    );
    let semaphore = Arc::new(Semaphore::new(cfg.max_connections.max(1)));
    tracing::info!("RushWay QUIC server listening on {}", bind);
    let mut set = tokio::task::JoinSet::new();
    loop {
        tokio::select! {
            _ = wait_shutdown() => {
                tracing::info!("shutdown requested, draining QUIC server connections");
                break;
            }
            incoming = endpoint.accept() => {
                let Some(incoming) = incoming else {
                    break;
                };
                let cfg2 = cfg.clone();
                let sem = semaphore.clone();
                set.spawn(async move {
                    let permit = match sem.clone().try_acquire_owned() {
                        Ok(v) => v,
                        Err(_) => return,
                    };
                    let _conn = crate::stats::ConnGuard::new();
                    match incoming.await {
                        Ok(connection) => {
                            let _permit = permit;
                            while let Ok((send, recv)) = connection.accept_bi().await {
                                let cfg3 = cfg2.clone();
                                tokio::spawn(async move {
                                    if let Err(error) = handle_server_stream(send, recv, cfg3).await {
                                        tracing::debug!(%error,"QUIC stream closed")
                                    }
                                });
                            }
                        }
                        Err(error) => tracing::debug!(%error,"QUIC connection handshake failed"),
                    }
                });
            }
        }
    }
    endpoint.close(0u32.into(), b"shutdown");
    drain_join_set(&mut set).await;
    Ok(())
}

async fn handle_server_stream(
    mut send: SendStream,
    mut recv: RecvStream,
    cfg: RuntimeConfig,
) -> Result<()> {
    let line = read_quic_line(&mut recv, 8192).await?;
    let target_str = if let Some(key) = cfg.key.as_ref().map(|k| Zeroizing::new(k.clone())) {
        let mut parts = line.splitn(2, ' ');
        let k = parts.next().unwrap_or("");
        let rest = parts.next().unwrap_or("");
        if !quic_auth_matches(k, &key) {
            send.write_all(b"ERR: AUTH_FAILED\n").await?;
            let _ = send.finish();
            bail!("QUIC authentication failed");
        }
        rest
    } else {
        line.as_str()
    };
    if target_str.eq_ignore_ascii_case("UDP") || target_str.to_ascii_uppercase().starts_with("UDP")
    {
        return handle_server_udp_stream(send, recv, cfg).await;
    }
    let target_addr = match parse_target_authority(target_str) {
        Ok(t) => t,
        Err(_) => {
            send.write_all(b"ERR: INVALID_TARGET\n").await?;
            let _ = send.finish();
            bail!("invalid QUIC target");
        }
    };
    if enforce_target_policy(&cfg, &target_addr).is_err() {
        send.write_all(b"ERR: DIAL_FAILED\n").await?;
        let _ = send.finish();
        return Ok(());
    }
    let target_socket = resolve_target(cfg.block_local, &target_addr.host, target_addr.port).await?;
    let target_stream = match timeout(
        Duration::from_secs(cfg.connection_timeout.max(1)),
        TcpStream::connect(target_socket),
    )
    .await
    {
        Ok(Ok(s)) => s,
        _ => {
            send.write_all(b"ERR: DIAL_FAILED\n").await?;
            let _ = send.finish();
            return Ok(());
        }
    };
    apply_socket_options(&target_stream, &cfg);
    let (mut target_rd, mut target_wr) = target_stream.into_split();
    send.write_all(b"OK\n").await?;
    let buffer_size = cfg.buffer_size;
    let mut send_task = tokio::spawn(async move {
        let mut buf = relay_buf(buffer_size).await;
        loop {
            match target_rd.read(&mut buf).await {
                Ok(0) | Err(_) => break,
                Ok(n) => {
                    crate::stats::add_bytes(0, n as i64);
                    if send.write_all(&buf[..n]).await.is_err() {
                        break;
                    }
                }
            }
        }
        let _ = send.finish();
        recycle_buf(buf).await;
        Result::<()>::Ok(())
    });
    let mut buf = relay_buf(buffer_size).await;
    loop {
        tokio::select! {
            _ = &mut send_task => {
                break;
            }
            res = recv.read(&mut buf) => {
                match res? {
                    Some(0) | None => break,
                    Some(n) => {
                        target_wr.write_all(&buf[..n]).await?;
                        crate::stats::add_bytes(n as i64, 0);
                    }
                }
            }
        }
    }
    let _ = target_wr.shutdown().await;
    send_task.abort();
    recycle_buf(buf).await;
    Ok(())
}

async fn handle_server_udp_stream(
    mut send: SendStream,
    mut recv: RecvStream,
    cfg: RuntimeConfig,
) -> Result<()> {
    send.write_all(b"OK\n").await?;
    let udp = Arc::new(UdpSocket::bind("0.0.0.0:0").await?);
    // Same gate as the WS UDP relay: the socket is unconnected, so only
    // datagrams whose source is an address this session actually dialed may
    // reach the client's downstream; anything else is dropped as untrusted.
    let known_targets: Arc<StdMutex<HashSet<std::net::SocketAddr>>> =
        Arc::new(StdMutex::new(HashSet::new()));
    let udp_send = udp.clone();
    let known_send = known_targets.clone();
    let mut sender = tokio::spawn(async move {
        let mut batch = UdpBatchReader::new(udp_send);
        let mut packet = Vec::with_capacity(2048);
        loop {
            let count = match batch.recv().await {
                Ok(n) => n,
                Err(_) => break,
            };
            for i in 0..count {
                let (pkt, src) = batch.packet(i);
                if !known_send
                    .lock()
                    .unwrap_or_else(|e| e.into_inner())
                    .contains(&src)
                {
                    tracing::debug!(%src, "QUIC UDP relay dropped a datagram from an undialed source");
                    continue;
                }
                let body_len = match src.ip() {
                    std::net::IpAddr::V4(_) => 10 + pkt.len(),
                    std::net::IpAddr::V6(_) => 22 + pkt.len(),
                };
                if body_len > MAX_QUIC_UDP_PACKET {
                    continue;
                }
                crate::stats::add_bytes(0, pkt.len() as i64);
                packet.clear();
                packet.reserve(2 + body_len);
                packet.extend_from_slice(&(body_len as u16).to_be_bytes());
                packet.extend_from_slice(&[0, 0, 0]);
                match src.ip() {
                    std::net::IpAddr::V4(ip) => {
                        packet.push(1);
                        packet.extend_from_slice(&ip.octets())
                    }
                    std::net::IpAddr::V6(ip) => {
                        packet.push(4);
                        packet.extend_from_slice(&ip.octets())
                    }
                }
                packet.extend_from_slice(&src.port().to_be_bytes());
                packet.extend_from_slice(pkt);
                if send.write_all(&packet).await.is_err() {
                    return Ok::<(), anyhow::Error>(());
                }
            }
        }
        Ok::<(), anyhow::Error>(())
    });
    let mut frame_buf = Vec::with_capacity(64 * 1024);
    let mut batch_writer = UdpBatchWriter::new(udp.clone());
    tokio::select! {
        _ = &mut sender => {}
        _ = async {
            while let Some(packet_len) =
                read_len_prefixed_udp(&mut recv, &mut frame_buf).await?
            {
                let packet = &frame_buf[..packet_len];
                let (target, payload) =
                    parse_socks5_udp_datagram(packet).map_err(|e| anyhow!(e.to_string()))?;
                if enforce_target_policy(&cfg, &target).is_err() {
                    continue;
                }
                let addr = resolve_target(cfg.block_local, &target.host, target.port).await?;
                // Recorded before the first payload leaves, so a reply can
                // never arrive ahead of its target joining the known set.
                known_targets
                    .lock()
                    .unwrap_or_else(|e| e.into_inner())
                    .insert(addr);
                crate::stats::add_bytes(payload.len() as i64, 0);
                let _ = batch_writer.send(payload, addr).await;
            }
            let _ = batch_writer.flush().await;
            Ok::<(), anyhow::Error>(())
        } => {}
    }
    sender.abort();
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    const TEST_KEY: &str = "rushway-quic-test-key";

    /// An in-process QUIC connection with one open bidirectional stream per
    /// side, built from the production endpoint and config builders
    /// (self-signed server identity, non-verifying client). The endpoints and
    /// connections are parked in the struct so the streams outlive setup.
    struct QuicPair {
        _server_ep: Endpoint,
        _client_ep: Endpoint,
        _client_conn: Connection,
        server_conn: Connection,
        client: (SendStream, RecvStream),
    }

    impl QuicPair {
        async fn accept_server(&self) -> Result<(SendStream, RecvStream)> {
            self.server_conn
                .accept_bi()
                .await
                .context("accept server bidirectional stream")
        }
    }

    async fn open_quic_pair() -> Result<QuicPair> {
        let server_sock = bound_udp_socket("127.0.0.1:0".parse().unwrap(), 1 << 20)?;
        let server_addr = server_sock.local_addr()?;
        let server_ep = Endpoint::new(
            Default::default(),
            Some(server_config()?),
            server_sock,
            Arc::new(quinn::TokioRuntime),
        )?;
        let client_sock = bound_udp_socket("127.0.0.1:0".parse().unwrap(), 1 << 20)?;
        let mut client_ep = Endpoint::new(
            Default::default(),
            None,
            client_sock,
            Arc::new(quinn::TokioRuntime),
        )?;
        client_ep.set_default_client_config(client_config(false)?);
        let handshake: Result<Result<(Connection, (SendStream, RecvStream), Connection), anyhow::Error>, _> =
            timeout(Duration::from_secs(5), async {
                let connect_fut = async {
                    let conn = client_ep.connect(server_addr, "localhost")?.await?;
                    let client = conn.open_bi().await?;
                    Ok::<_, anyhow::Error>((conn, client))
                };
                let accept_fut = async {
                    let incoming = server_ep
                        .accept()
                        .await
                        .ok_or_else(|| anyhow!("test QUIC server endpoint closed"))?;
                    let server_conn = incoming.await?;
                    Ok::<_, anyhow::Error>(server_conn)
                };
                let ((client_conn, client), server_conn) = tokio::try_join!(connect_fut, accept_fut)?;
                Ok((client_conn, client, server_conn))
            })
            .await;
        let (client_conn, client, server_conn) = handshake
            .expect("QUIC test handshake timed out")?;
        Ok(QuicPair {
            _server_ep: server_ep,
            _client_ep: client_ep,
            _client_conn: client_conn,
            server_conn,
            client,
        })
    }

    #[test]
    fn parse_upstream_appends_default_port() {
        assert_eq!(parse_upstream("quic://example.com").unwrap(), "example.com:443");
        assert_eq!(parse_upstream("quic://example.com/path").unwrap(), "example.com:443");
    }

    #[test]
    fn parse_upstream_keeps_explicit_port() {
        assert_eq!(parse_upstream("quic://example.com:8443").unwrap(), "example.com:8443");
        assert_eq!(
            parse_upstream("quic://example.com:8443/abc").unwrap(),
            "example.com:8443"
        );
    }

    #[test]
    fn parse_upstream_accepts_quic_tls_scheme() {
        assert_eq!(parse_upstream("quic+tls://example.com").unwrap(), "example.com:443");
        assert_eq!(parse_upstream("quic+tls://example.com:8443").unwrap(), "example.com:8443");
    }

    #[test]
    fn parse_upstream_keeps_bracketed_ipv6() {
        assert_eq!(parse_upstream("quic://[2001:db8::1]").unwrap(), "[2001:db8::1]");
        assert_eq!(parse_upstream("quic://[2001:db8::1]:443").unwrap(), "[2001:db8::1]:443");
    }

    #[test]
    fn parse_upstream_rejects_non_quic_and_empty_authority() {
        for bad in [
            "",
            "example.com:443",
            "http://example.com",
            "QUIC://example.com",
            "quic://",
            "quic:///path",
            "quic+tls://",
        ] {
            assert!(parse_upstream(bad).is_err(), "expected rejection for {bad:?}");
        }
    }

    #[test]
    fn key_digest_hex_matches_sha256_vectors() {
        // Standard SHA-256 vectors: the empty string and "abc".
        assert_eq!(
            key_digest_hex(""),
            "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855"
        );
        assert_eq!(
            key_digest_hex("abc"),
            "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
        );
        for key in ["", "abc", TEST_KEY] {
            let digest = key_digest_hex(key);
            assert_eq!(digest.len(), 64);
            assert!(digest.bytes().all(|b| b.is_ascii_hexdigit()));
            assert!(!digest.bytes().any(|b| b.is_ascii_uppercase()));
        }
    }

    #[test]
    fn quic_auth_matches_digest_form() {
        let key = Zeroizing::new(TEST_KEY.to_string());
        assert!(quic_auth_matches(&key_digest_hex(TEST_KEY), &key));
        assert!(!quic_auth_matches(&key_digest_hex("other-key"), &key));
        // Hex is compared byte-wise: an uppercased digest is neither the
        // configured digest nor the plaintext key.
        let upper = key_digest_hex(TEST_KEY).to_ascii_uppercase();
        assert!(!quic_auth_matches(&upper, &key));
    }

    #[test]
    fn quic_auth_matches_legacy_plaintext() {
        // The legacy inline key stays accepted for plaintext-only peers
        // (GoWay interop); near-misses on either side must not authenticate.
        let key = Zeroizing::new("legacy-inline-key".to_string());
        assert!(quic_auth_matches("legacy-inline-key", &key));
        assert!(!quic_auth_matches("legacy-inline-ke", &key));
        assert!(!quic_auth_matches("legacy-inline-keyx", &key));
    }

    #[test]
    fn quic_auth_matches_empty_key_boundaries() {
        // Empty configured key: the digest form is the primary match, and the
        // legacy path also accepts an empty presented line.
        let empty = Zeroizing::new(String::new());
        assert!(quic_auth_matches(&key_digest_hex(""), &empty));
        assert!(quic_auth_matches("", &empty));
        // An empty presented line against a non-empty key must not pass.
        let key = Zeroizing::new(TEST_KEY.to_string());
        assert!(!quic_auth_matches("", &key));
    }

    #[test]
    fn line_eq_is_length_gated_and_exact() {
        assert!(line_eq("", ""));
        assert!(line_eq("OK", "OK"));
        assert!(!line_eq("OK", "ok"));
        assert!(!line_eq("ab", "abc"));
        assert!(!line_eq("abc", "ab"));
    }

    #[test]
    fn is_auth_rejection_matches_auth_shaped_lines() {
        assert!(is_auth_rejection("ERR: AUTH_FAILED"));
        assert!(is_auth_rejection("err: auth_failed"));
        assert!(!is_auth_rejection("ERR: DIAL_FAILED"));
        assert!(!is_auth_rejection("OK"));
        assert!(!is_auth_rejection(""));
    }

    #[tokio::test]
    async fn len_prefixed_round_trip_preserves_payload_and_framing() {
        let mut pair = open_quic_pair().await.unwrap();
        let payload: Vec<u8> = (0..=255u8).cycle().take(5000).collect();
        write_len_prefixed_udp(&mut pair.client.0, &payload).await.unwrap();
        let mut server = pair.accept_server().await.unwrap();
        let mut buf = Vec::with_capacity(8192);
        let n = read_len_prefixed_udp(&mut server.1, &mut buf).await.unwrap().unwrap();
        assert_eq!(n, 5000);
        assert_eq!(&buf[..n], &payload[..]);
        // A second frame right behind the first: the length prefix must not
        // drift, and stale bytes beyond `n` must stay out of the slice.
        write_len_prefixed_udp(&mut pair.client.0, b"tail").await.unwrap();
        let n = read_len_prefixed_udp(&mut server.1, &mut buf).await.unwrap().unwrap();
        assert_eq!(n, 4);
        assert_eq!(&buf[..n], &b"tail"[..]);
    }

    #[tokio::test]
    async fn len_prefixed_zero_length_frame_then_eof_reads_none() {
        let mut pair = open_quic_pair().await.unwrap();
        // A zero-length datagram is a legal frame: the prefix says 0 and the
        // payload read is a no-op.
        write_len_prefixed_udp(&mut pair.client.0, b"").await.unwrap();
        let _ = pair.client.0.finish();
        let mut server = pair.accept_server().await.unwrap();
        let mut buf = Vec::with_capacity(1024);
        let n = read_len_prefixed_udp(&mut server.1, &mut buf).await.unwrap().unwrap();
        assert_eq!(n, 0);
        assert!(buf.is_empty());
        // After the last frame, a clean end of stream reads as Ok(None).
        let eof = read_len_prefixed_udp(&mut server.1, &mut buf).await.unwrap();
        assert!(eof.is_none());
    }

    #[tokio::test]
    async fn len_prefixed_max_payload_round_trips() {
        let mut pair = open_quic_pair().await.unwrap();
        // Exactly MAX_QUIC_UDP_PACKET bytes: the largest value the u16 prefix
        // can express and the write-side guard still accepts.
        let payload = vec![0xA5u8; MAX_QUIC_UDP_PACKET];
        write_len_prefixed_udp(&mut pair.client.0, &payload).await.unwrap();
        let mut server = pair.accept_server().await.unwrap();
        let mut buf = Vec::with_capacity(MAX_QUIC_UDP_PACKET);
        let n = read_len_prefixed_udp(&mut server.1, &mut buf).await.unwrap().unwrap();
        assert_eq!(n, MAX_QUIC_UDP_PACKET);
        assert!(buf[..n].iter().all(|&b| b == 0xA5));
    }

    #[tokio::test]
    async fn len_prefixed_oversize_payload_is_rejected_without_writes() {
        let mut pair = open_quic_pair().await.unwrap();
        let too_big = vec![0u8; MAX_QUIC_UDP_PACKET + 1];
        assert!(write_len_prefixed_udp(&mut pair.client.0, &too_big).await.is_err());
        // The size guard fires before any byte is written, so the stream is
        // still usable for a well-sized frame afterwards.
        write_len_prefixed_udp(&mut pair.client.0, b"ok").await.unwrap();
        let mut server = pair.accept_server().await.unwrap();
        let mut buf = Vec::with_capacity(1024);
        let n = read_len_prefixed_udp(&mut server.1, &mut buf).await.unwrap().unwrap();
        assert_eq!(n, 2);
        assert_eq!(&buf[..n], &b"ok"[..]);
    }
}
} // mod quic

pub mod runtime {
//! Runtime forwarding paths for the GoWay-compatible transport slice.

use crate::common::configured_cipher;
use crate::crypto::XorCipher;
use crate::dns::resolve_socket;
use crate::flow::CreditGate;
use crate::mux_writer::MuxFrameWriter;
use crate::protocol::{
    decode_version_payload, decode_window_payload, encode_version_payload, encode_window_payload,
    MuxCommand, MuxFrame, OwnedMuxFrame, SynPayload, U32HashMap, MUX_INITIAL_WINDOW_KIB,
    MUX_WINDOW_REFRESH,
};
use crate::proxy::{parse_socks5_udp_datagram, parse_target_authority, TargetAddr};
use crate::udp_batch::{UdpBatchReader, UdpBatchWriter};
use crate::ws::{
    build_server_handshake_response, encode_ws_frame, read_frame, read_frame_with_cipher,
    read_http_headers, validate_server_handshake, write_frame, write_frame_borrowed,
};
use anyhow::{anyhow, bail, Context, Result};
use socket2::SockRef;
use std::collections::HashSet;
use std::net::{IpAddr, SocketAddr};
use std::sync::atomic::{AtomicU32, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex as StdMutex, OnceLock};
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};
use tokio::net::tcp::OwnedReadHalf;
use tokio::net::{TcpListener, TcpStream, UdpSocket};
use tokio::sync::{mpsc, watch, Semaphore};
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
            connection_timeout: 10,
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
/// Blocks a target from its *literal* hostname, before DNS runs.
///
/// This is only a fast pre-filter: it cannot see what the name resolves to,
/// so `resolve_target` re-checks the actual address afterwards.
pub(crate) fn is_blocked_local_host(host: &str) -> bool {
    // `getaddrinfo("localhost.")` resolves to loopback and the DNS cache
    // normalizes trailing dots, so the dotted root form (and RFC 6761's
    // `*.localhost` zone) has to be blocked too. An exact `== "localhost"`
    // comparison let both through.
    let clean = host.trim().trim_matches(|c| c == '[' || c == ']').trim_end_matches('.');
    let is_localhost = match clean.rsplit_once('.') {
        Some((_, tld)) => tld.eq_ignore_ascii_case("localhost"),
        None => clean.eq_ignore_ascii_case("localhost"),
    };
    if is_localhost {
        return true;
    }
    match clean.parse::<IpAddr>() {
        Ok(ip) => is_blocked_local_ip(ip),
        Err(_) => false,
    }
}

/// Blocks a resolved address: IPv4 loopback / RFC1918 / link-local /
/// unspecified, plus IPv6 loopback / unspecified / ULA / link-local /
/// site-local multicast and anything IPv4-mapped onto those.
///
/// Deliberately mirrors the address set GoWay blocks. RFC6598 CGNAT
/// (100.64/10) and 198.18/15 are *not* included: some deployments run
/// targets inside carrier-grade NAT, and widening the range would break them.
pub(crate) fn is_blocked_local_ip(ip: IpAddr) -> bool {
    match ip {
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
    }
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

fn recycle_buf_sync(buf: Vec<u8>) {
    if buf.len() > POOLED_BUF_MAX_SIZE {
        return;
    }
    let mut pool = relay_bufs().lock().unwrap_or_else(|e| e.into_inner());
    if pool.len() < POOLED_BUF_MAX_COUNT {
        pool.push(buf);
    }
}

/// RAII guard for pooled relay buffers.
///
/// Automatically returns the allocated buffer to `RELAY_BUFS` on drop,
/// ensuring 100% recycling even when relay tasks are aborted or exit early.
pub(crate) struct RelayBufGuard(pub(crate) Option<Vec<u8>>);

impl std::ops::Deref for RelayBufGuard {
    type Target = [u8];
    #[inline]
    fn deref(&self) -> &Self::Target {
        self.0.as_deref().unwrap_or(&[])
    }
}

impl std::ops::DerefMut for RelayBufGuard {
    #[inline]
    fn deref_mut(&mut self) -> &mut Self::Target {
        self.0.as_deref_mut().unwrap_or(&mut [])
    }
}

impl AsRef<[u8]> for RelayBufGuard {
    #[inline]
    fn as_ref(&self) -> &[u8] {
        self.0.as_deref().unwrap_or(&[])
    }
}

impl AsMut<[u8]> for RelayBufGuard {
    #[inline]
    fn as_mut(&mut self) -> &mut [u8] {
        self.0.as_deref_mut().unwrap_or(&mut [])
    }
}

impl Drop for RelayBufGuard {
    fn drop(&mut self) {
        if let Some(buf) = self.0.take() {
            recycle_buf_sync(buf);
        }
    }
}

/// Gets a zeroed relay buffer of exactly `relay_buffer_size(requested)`
/// bytes wrapped in a `RelayBufGuard`, reusing a pooled one when available.
pub(crate) async fn relay_buf(requested: usize) -> RelayBufGuard {
    let size = relay_buffer_size(requested);
    if size <= POOLED_BUF_MAX_SIZE {
        let mut pool = relay_bufs().lock().unwrap_or_else(|e| e.into_inner());
        if let Some(pos) = pool.iter().position(|b| b.len() == size) {
            return RelayBufGuard(Some(pool.swap_remove(pos)));
        }
    }
    RelayBufGuard(Some(vec![0u8; size]))
}

/// Returns a relay buffer to the pool (best-effort: over-capacity or
/// oversized buffers are simply dropped).
pub(crate) async fn recycle_buf(_buf: RelayBufGuard) {}

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
/// Target policy shared by every dial path.
///
/// Previously gated on `cfg.upstream.is_some()`, which meant the check was
/// silently skipped in **server** mode even though `-block-local` defaults to
/// `true` and the banner/TUI report it as enabled. Server mode is exactly
/// where it matters: an authenticated (or `--allow-open`) peer could make the
/// server dial `127.0.0.1`, RFC1918 hosts or the cloud metadata address
/// `169.254.169.254`. Operators who legitimately need to proxy to LAN hosts
/// from a server pass `-no-block-local`.
pub(crate) fn enforce_target_policy(cfg: &RuntimeConfig, target: &TargetAddr) -> Result<()> {
    check_target_policy(cfg.block_local, target)
}

/// Shared target-policy check: rejects loopback/RFC1918/link-local hostnames
/// and zero ports before any dial is attempted.
///
/// This is only the *pre*-DNS filter — see `resolve_target`, which re-checks
/// the address that is actually dialed.
pub(crate) fn check_target_policy(block_local: bool, target: &TargetAddr) -> Result<()> {
    if block_local && is_blocked_local_host(&target.host) {
        bail!("local/LAN target blocked by target policy")
    }
    if target.port == 0 {
        bail!("target port must be non-zero")
    }
    Ok(())
}

/// Resolves a *target* hostname and re-checks the address that will actually
/// be dialed.
///
/// The literal-hostname check runs before DNS and therefore cannot see what
/// the name resolves to. Three concrete bypasses existed:
///   * DNS rebinding — `evil.attacker.com -> 127.0.0.1`, amplified by the
///     5-minute positive cache so one successful poison lasts;
///   * decimal/octal short forms — `2130706433`, `0x7f000001`, `127.1` are
///     rejected by Rust's strict `IpAddr::parse` but accepted by
///     `getaddrinfo`, which resolves them to loopback;
///   * any hostname whose resolved record is private.
///
/// Checking here — on the exact `SocketAddr` handed to `TcpStream::connect`
/// — is the only place the decision can be made correctly. Upstream/edge
/// resolution deliberately does *not* go through this function.
pub(crate) async fn resolve_target(
    block_local: bool,
    host: &str,
    port: u16,
) -> Result<SocketAddr> {
    let addr = resolve_socket(host, port).await?;
    if block_local && is_blocked_local_ip(addr.ip()) {
        bail!("local/LAN address blocked by target policy: {}", addr.ip());
    }
    Ok(addr)
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

        // 5. SO_BUSY_POLL: Low-latency socket polling in microseconds (Linux 3.11+).
        // Polls the device driver receive queue for incoming packets for 50µs before
        // sleeping, drastically cutting tail latency and context switch overhead on busy servers.
        let busy_poll_us: libc::c_int = 50;
        unsafe {
            let _ = libc::setsockopt(
                fd,
                libc::SOL_SOCKET,
                libc::SO_BUSY_POLL,
                &busy_poll_us as *const _ as *const libc::c_void,
                std::mem::size_of_val(&busy_poll_us) as libc::socklen_t,
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
/// - TCP_DEFER_ACCEPT: Defers wakeup of accept() until the first data packet arrives
///   (up to 3 seconds), eliminating empty connection wakeups and reducing context switches.
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

        // Removed TCP_DEFER_ACCEPT for instant local proxy accept wakeup
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
        let addr = resolve_target(cfg.block_local, &target.host, target.port)
            .await
            .map_err(|e| std::io::Error::other(e.to_string()))?;
        TcpStream::connect(addr).await
    })
    .await
    .context("target connection timeout")??;
    apply_socket_options(&stream, cfg);
    Ok(stream)
}
async fn send_mux_parts_encrypted_reuse(
    writer: &Arc<MuxFrameWriter>,
    cipher: &XorCipher,
    stream_id: u32,
    command: MuxCommand,
    payload: &[u8],
    scratch: &mut Vec<u8>,
    obfs: bool,
) -> Result<()> {
    if scratch.capacity() == 0 {
        *scratch = crate::mux_writer::acquire_encode_buf();
    }
    crate::mux_writer::encode_mux_ws_frame(
        scratch, stream_id, command, payload, cipher, false, obfs,
    )
    .map_err(|e| anyhow!(e.to_string()))?;
    writer.send_mux(stream_id, command, std::mem::take(scratch)).await
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
    send_mux_parts_encrypted_reuse(
        writer, cipher, stream_id, command, payload, &mut bytes, obfs,
    )
    .await
}
async fn send_reset_encrypted(
    writer: &Arc<MuxFrameWriter>,
    cipher: &XorCipher,
    id: u32,
    obfs: bool,
) -> Result<()> {
    send_mux_parts_encrypted(writer, cipher, id, MuxCommand::Rst, &[], obfs).await
}
async fn target_to_mux(
    id: u32,
    mut target: OwnedReadHalf,
    writer: Arc<MuxFrameWriter>,
    buffer_size: usize,
    cipher: XorCipher,
    obfs: bool,
    gate: Arc<CreditGate>,
) {
    let mut buf = relay_buf(buffer_size).await;
    let mut frame_scratch = crate::mux_writer::acquire_encode_buf();
    loop {
        match target.read(&mut buf).await {
            Ok(0) => {
                let _ = send_mux_parts_encrypted_reuse(
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
            Ok(n) => {
                crate::stats::add_bytes(0, n as i64);
                let mut off = 0;
                while off < n {
                    let end = (off + u16::MAX as usize).min(n);
                    gate.acquire(end - off).await;
                    if send_mux_parts_encrypted_reuse(
                        &writer,
                        &cipher,
                        id,
                        MuxCommand::Data,
                        &buf[off..end],
                        &mut frame_scratch,
                        obfs,
                    )
                    .await
                    .is_err()
                    {
                        crate::mux_writer::recycle_encode_buf(frame_scratch);
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
    crate::mux_writer::recycle_encode_buf(frame_scratch);
    recycle_buf(buf).await;
}
/// Accumulates consumed bytes per stream and emits a WINDOW refund once
/// the refresh threshold is reached. Refund accounting only runs after the
/// peer's VERSION arrived (un-negotiated peers never send/receive WINDOW).
async fn maybe_send_window(
    writer: &Arc<MuxFrameWriter>,
    cipher: &XorCipher,
    stream_id: u32,
    obfs: bool,
    peer_window: &AtomicU32,
    refund_pending: &mut u32,
    consumed: usize,
) {
    let negotiated = peer_window.load(Ordering::Relaxed) > 0;
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

/// Session-wide accounting for bytes parked in pre-dial `pending` queues.
///
/// Each stream task holds one of these for as long as its frames sit in
/// `pending`: bytes are reserved when a frame is pulled out of the stream
/// channel and returned once it has been handed to the target socket.
/// `Drop` returns whatever is still held, so no early `return` on any error
/// path can leak budget.
///
/// Per stream the ceiling stays at `MAX_PENDING_BYTES` (8 MiB, goway's
/// `muxServerStreamBufferLimit`); summed over a session's 2048 streams that
/// is a ~16 GiB theoretical peak, which under `panic = "abort"` means a
/// failed allocation takes the whole process down. The session-wide ceiling
/// bounds that peak instead, at the cost of resetting an individual stream
/// when it is hit.
struct PendingBytes {
    total: Arc<AtomicUsize>,
    held: usize,
}

impl PendingBytes {
    fn new(total: Arc<AtomicUsize>) -> Self {
        Self { total, held: 0 }
    }

    /// Bytes this task currently holds against the session budget; doubles
    /// as the per-stream `pending` byte count.
    fn held_bytes(&self) -> usize {
        self.held
    }

    /// Reserve `len` bytes against the session budget, or leave it untouched
    /// and return `false` when the reservation would exceed `max`.
    fn try_reserve(&mut self, len: usize, max: usize) -> bool {
        // Optimistic add-then-check: a racing task can push the counter past
        // `max` for the width of this call, but the overshoot is bounded by a
        // single frame and is rolled back below.
        let prev = self.total.fetch_add(len, Ordering::Relaxed);
        if prev.saturating_add(len) > max {
            self.total.fetch_sub(len, Ordering::Relaxed);
            return false;
        }
        self.held = self.held.saturating_add(len);
        true
    }

    /// Return `len` bytes (clamped to what this task actually holds).
    fn release(&mut self, len: usize) {
        let len = len.min(self.held);
        if len == 0 {
            return;
        }
        self.total.fetch_sub(len, Ordering::Relaxed);
        self.held -= len;
    }
}

impl Drop for PendingBytes {
    fn drop(&mut self) {
        let held = self.held;
        self.release(held);
    }
}

async fn handle_mux_parts<R, W>(
    mut rd: R,
    wr: W,
    cfg: RuntimeConfig,
    cipher: XorCipher,
) -> Result<()>
where
    R: AsyncRead + Unpin + Send + 'static,
    W: AsyncWrite + Unpin + Send + 'static,
{
    // Hand the write half to the dedicated writer task; all MUX frames on
    // this session go through its serialized, coalescing queue.
    let (writer, _writer_task) = MuxFrameWriter::spawn(wr);
    let mut ok = b"OK\n".to_vec();
    cipher.apply(&mut ok);
    writer
        .send(encode_ws_frame(&ok, 2, false).map_err(|e| anyhow!(e.to_string()))?)
        .await?;

    // W3 version negotiation: advertise our protocol version + initial
    // receive window right after the session handshake. Old clients skip
    // the unknown command; new clients answer with their own VERSION.
    let peer_window: Arc<AtomicU32> = Arc::new(AtomicU32::new(0));
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
    let streams: Arc<std::sync::RwLock<U32HashMap<StreamEntry>>> =
        Arc::new(std::sync::RwLock::new(U32HashMap::default()));
    // Session-wide ceiling on bytes parked in pre-dial `pending` queues
    // across every stream multiplexed here (see `PendingBytes`).
    const MAX_SESSION_PENDING_BYTES: usize = 512 * 1024 * 1024;
    let pending_bytes_total = Arc::new(AtomicUsize::new(0));
    // JoinSet, not Vec<JoinHandle>: a long-lived session used to push one
    // handle per stream it ever opened and only released them at session end.
    let mut stream_tasks = tokio::task::JoinSet::new();
    let mut frame_buf = crate::mux_writer::acquire_encode_buf();

    loop {
        let Some((opcode, payload)) = read_frame_with_cipher(
            &mut rd,
            Option::<&mut W>::None,
            &mut frame_buf,
            Some(&cipher),
        )
        .await?
        else {
            break;
        };

        if opcode != 2 {
            crate::mux_writer::recycle_encode_buf(payload);
            continue;
        }

        let frame = match MuxFrame::decode_owned(payload) {
            Ok(f) => f,
            Err(_) => continue,
        };

        match frame.command {
            MuxCommand::Syn => {
                let stream_id = frame.stream_id;

                let (target_bytes, initial_data) = match SynPayload::decode_slices(frame.payload())
                {
                    Ok(v) => v,
                    Err(_) => {
                        crate::mux_writer::recycle_encode_buf(frame.into_storage());
                        tracing::warn!(stream_id, "mux RST: bad syn payload");
                        let _ = send_reset_encrypted(&writer, &cipher, stream_id, cfg.obfs).await;
                        continue;
                    }
                };

                let target_text = match std::str::from_utf8(target_bytes) {
                    Ok(v) => v,
                    Err(_) => {
                        crate::mux_writer::recycle_encode_buf(frame.into_storage());
                        tracing::warn!(stream_id, "mux RST: target not utf-8");
                        let _ = send_reset_encrypted(&writer, &cipher, stream_id, cfg.obfs).await;
                        continue;
                    }
                };

                let target = match parse_target_authority(target_text) {
                    Ok(v) => v,
                    Err(_) => {
                        crate::mux_writer::recycle_encode_buf(frame.into_storage());
                        tracing::warn!(stream_id, "mux RST: bad target authority");
                        let _ = send_reset_encrypted(&writer, &cipher, stream_id, cfg.obfs).await;
                        continue;
                    }
                };

                if let Err(e) = enforce_target_policy(&cfg, &target) {
                    crate::mux_writer::recycle_encode_buf(frame.into_storage());
                    tracing::warn!(stream_id, error=%e, "mux RST: target policy");
                    let _ = send_reset_encrypted(&writer, &cipher, stream_id, cfg.obfs).await;
                    continue;
                }

                let (tx, mut rx) = mpsc::channel(crate::protocol::MUX_STREAM_QUEUE_CAP);
                let (cancel, cancelled) = watch::channel(false);
                let gate = CreditGate::new();
                let kib = peer_window.load(Ordering::Acquire);
                if kib > 0 {
                    gate.enable(i64::from(kib) * 1024);
                }

                // Atomically admit and register the logical stream. This removes
                // the check-then-insert race during large concurrent SYN bursts.
                let (admitted, old_cancel) = {
                    let mut guard = streams.write().unwrap_or_else(|e| e.into_inner());
                    if guard.len() >= 2048 {
                        (false, None)
                    } else if let Some(old) = guard.remove(&stream_id) {
                        (false, Some(old.cancel))
                    } else {
                        guard.insert(
                            stream_id,
                            StreamEntry {
                                tx: tx.clone(),
                                cancel: cancel.clone(),
                                gate: gate.clone(),
                            },
                        );
                        (true, None)
                    }
                };
                if let Some(old_cancel) = old_cancel {
                    let _ = old_cancel.send(true);
                }
                if !admitted {
                    crate::mux_writer::recycle_encode_buf(frame.into_storage());
                    tracing::warn!(stream_id, "mux RST: stream admission rejected (table full or duplicate id)");
                    let _ = send_reset_encrypted(&writer, &cipher, stream_id, cfg.obfs).await;
                    continue;
                }

                if !initial_data.is_empty() {
                    let owned = OwnedMuxFrame::from_parts(
                        stream_id,
                        MuxCommand::Data,
                        initial_data,
                    )
                    .map_err(|e| anyhow!(e.to_string()))?;
                    crate::mux_writer::recycle_encode_buf(frame.into_storage());

                    tx.send(StreamCommand::Data(owned))
                        .await
                        .map_err(|_| anyhow!("stream task exited before initial data"))?;
                } else {
                    crate::mux_writer::recycle_encode_buf(frame.into_storage());
                }

                let streams_task = streams.clone();
                let writer_task = writer.clone();
                let cipher_task = cipher.clone();
                let cfg_task = cfg.clone();
                let gate_task = gate;
                let peer_window_task = peer_window.clone();
                let pending_total_task = pending_bytes_total.clone();

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
                    streams: &Arc<std::sync::RwLock<U32HashMap<StreamEntry>>>,
                    stream_id: u32,
                    obfs: bool,
                ) {
                    tracing::warn!(stream_id, "mux RST: pre-dial pending overflow");
                    let _ = send_reset_encrypted(writer, cipher, stream_id, obfs).await;
                    streams.write().unwrap_or_else(|e| e.into_inner()).remove(&stream_id);
                }
                stream_tasks.spawn(async move {
                    let mut cancelled = cancelled;
                    let mut pending = Vec::new();
                    let mut pending_budget = PendingBytes::new(pending_total_task);
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
                                        streams_task.write().unwrap_or_else(|e| e.into_inner()).remove(&stream_id);
                                        return;
                                    }
                                }
                            }

                            changed = cancelled.changed() => {
                                match changed {
                                    Ok(()) if *cancelled.borrow() => {
                                        streams_task.write().unwrap_or_else(|e| e.into_inner()).remove(&stream_id);
                                        return;
                                    }
                                    Ok(()) => {}
                                    Err(_) => {
                                        streams_task.write().unwrap_or_else(|e| e.into_inner()).remove(&stream_id);
                                        return;
                                    }
                                }
                            }

                            command = rx.recv() => {
                                match command {
                                    Some(StreamCommand::Data(frame)) => {
                                        let len = frame.payload().len();
                                        // Per-stream ceiling first so no
                                        // reservation is made when it trips,
                                        // then the session-wide budget.
                                        if pending.len() >= MAX_PENDING_FRAMES
                                            || pending_budget.held_bytes().saturating_add(len)
                                                > MAX_PENDING_BYTES
                                            || !pending_budget
                                                .try_reserve(len, MAX_SESSION_PENDING_BYTES)
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
                                        streams_task.write().unwrap_or_else(|e| e.into_inner()).remove(&stream_id);
                                        return;
                                    }
                                }
                            }
                        }
                    };

                    if *cancelled.borrow() {
                        streams_task.write().unwrap_or_else(|e| e.into_inner()).remove(&stream_id);
                        return;
                    }

                    while let Ok(command) = rx.try_recv() {
                        match command {
                            StreamCommand::Data(frame) => {
                                let len = frame.payload().len();
                                if pending.len() >= MAX_PENDING_FRAMES
                                    || pending_budget.held_bytes().saturating_add(len)
                                        > MAX_PENDING_BYTES
                                    || !pending_budget
                                        .try_reserve(len, MAX_SESSION_PENDING_BYTES)
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
                                streams_task.write().unwrap_or_else(|e| e.into_inner()).remove(&stream_id);
                                return;
                            }
                        }
                    }
                    let (rd_target, mut wr_target) = target_stream.into_split();

                    // W3 receive-side refunds: credit the client's upload
                    // window once we have consumed >= 64 KiB toward the
                    // target. Only accumulated once the client negotiated.
                    let mut refund_pending: u32 = 0;

                    if !pending.is_empty() {
                        let write_res =
                            crate::mux_writer::write_owned_frames_vectored(&mut wr_target, &pending)
                                .await;
                        let held = pending_budget.held_bytes();
                        crate::mux_writer::recycle_encode_bufs(
                            pending.into_iter().map(OwnedMuxFrame::into_storage),
                        );
                        pending_budget.release(held);
                        match write_res {
                            Ok(written) => {
                                if written > 0 {
                                    crate::stats::add_bytes(written as i64, 0);
                                    maybe_send_window(
                                        &writer_task,
                                        &cipher_task,
                                        stream_id,
                                        cfg_task.obfs,
                                        &peer_window_task,
                                        &mut refund_pending,
                                        written,
                                    )
                                    .await;
                                }
                            }
                            Err(_) => {
                                streams_task
                                    .write()
                                    .unwrap_or_else(|e| e.into_inner())
                                    .remove(&stream_id);
                                return;
                            }
                        }
                    }

                    let mut reader = tokio::spawn(target_to_mux(
                        stream_id,
                        rd_target,
                        writer_task.clone(),
                        cfg_task.buffer_size,
                        cipher_task.clone(),
                        cfg_task.obfs,
                        gate_task.clone(),
                    ));

                    if client_fin {
                        let _ = wr_target.shutdown().await;
                    } else {
                        let mut data_batch: Vec<OwnedMuxFrame> = Vec::new();
                        let mut deferred: Option<StreamCommand> = None;
                        loop {
                            let command = match deferred.take() {
                                Some(c) => c,
                                None => match rx.recv().await {
                                    Some(c) => c,
                                    None => break,
                                },
                            };
                            match command {
                                StreamCommand::Data(frame) => {
                                    match rx.try_recv() {
                                        Ok(StreamCommand::Data(second)) => {
                                            if data_batch.capacity() == 0 {
                                                data_batch.reserve(16);
                                            }
                                            data_batch.push(frame);
                                            data_batch.push(second);
                                            while data_batch.len() < 16 {
                                                match rx.try_recv() {
                                                    Ok(StreamCommand::Data(next)) => {
                                                        data_batch.push(next);
                                                    }
                                                    Ok(other) => {
                                                        deferred = Some(other);
                                                        break;
                                                    }
                                                    Err(_) => break,
                                                }
                                            }
                                            let write_res =
                                                crate::mux_writer::write_owned_frames_vectored(
                                                    &mut wr_target,
                                                    &data_batch,
                                                )
                                                .await;
                                            crate::mux_writer::recycle_encode_bufs(
                                                data_batch
                                                    .drain(..)
                                                    .map(OwnedMuxFrame::into_storage),
                                            );
                                            match write_res {
                                                Ok(written) => {
                                                    if written > 0 {
                                                        crate::stats::add_bytes(written as i64, 0);
                                                        maybe_send_window(
                                                            &writer_task,
                                                            &cipher_task,
                                                            stream_id,
                                                            cfg_task.obfs,
                                                            &peer_window_task,
                                                            &mut refund_pending,
                                                            written,
                                                        )
                                                        .await;
                                                    }
                                                }
                                                Err(_) => break,
                                            }
                                        }
                                        Ok(other) => {
                                            deferred = Some(other);
                                            let len = frame.payload().len();
                                            let write_res =
                                                wr_target.write_all(frame.payload()).await;
                                            crate::mux_writer::recycle_encode_buf(
                                                frame.into_storage(),
                                            );
                                            if write_res.is_err() {
                                                break;
                                            }
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
                                        Err(_) => {
                                            let len = frame.payload().len();
                                            let write_res =
                                                wr_target.write_all(frame.payload()).await;
                                            crate::mux_writer::recycle_encode_buf(
                                                frame.into_storage(),
                                            );
                                            if write_res.is_err() {
                                                break;
                                            }
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
                                    }
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

                    streams_task.write().unwrap_or_else(|e| e.into_inner()).remove(&stream_id);
                });

                // Reclaim streams that already finished instead of holding
                // their handle around until the session ends.
                while stream_tasks.try_join_next().is_some() {}
            }

            MuxCommand::Data => {
                let id = frame.stream_id;
                let tx = streams
                    .read()
                    .unwrap_or_else(|e| e.into_inner())
                    .get(&id)
                    .map(|s| s.tx.clone());
                if let Some(tx) = tx {
                    // Never park the session read loop on one stream's
                    // backpressure. `send().await` here meant a single target
                    // that stopped reading (with 64 frames already queued)
                    // froze uploads for *every* stream multiplexed on this
                    // session — no error, no timeout, no recovery until that
                    // target drained. A full queue means this frame cannot be
                    // delivered, so reset that stream and keep the session
                    // moving; the blast radius stays on the one stalled flow.
                    match tx.try_send(StreamCommand::Data(frame)) {
                        Ok(()) => {}
                        Err(mpsc::error::TrySendError::Full(StreamCommand::Data(f))) => {
                            crate::mux_writer::recycle_encode_buf(f.into_storage());
                            tracing::warn!(stream_id = id, "mux RST: stream queue full; resetting stalled stream");
                            let _ = send_reset_encrypted(&writer, &cipher, id, cfg.obfs).await;
                            streams.write().unwrap_or_else(|e| e.into_inner()).remove(&id);
                        }
                        Err(mpsc::error::TrySendError::Closed(StreamCommand::Data(f))) => {
                            crate::mux_writer::recycle_encode_buf(f.into_storage());
                            streams.write().unwrap_or_else(|e| e.into_inner()).remove(&id);
                        }
                        Err(_) => {}
                    }
                } else {
                    crate::mux_writer::recycle_encode_buf(frame.into_storage());
                }
            }

            MuxCommand::Fin => {
                let id = frame.stream_id;
                crate::mux_writer::recycle_encode_buf(frame.into_storage());
                let tx = streams
                    .read()
                    .unwrap_or_else(|e| e.into_inner())
                    .get(&id)
                    .map(|s| s.tx.clone());
                if let Some(tx) = tx {
                    // Same non-blocking rule as DATA: a FIN addressed to an
                    // already-backed-up stream must not stall the read loop.
                    if let Err(mpsc::error::TrySendError::Full(_)) = tx.try_send(StreamCommand::Fin)
                    {
                        tracing::warn!(stream_id = id, "mux RST: queue full at FIN; resetting stalled stream");
                        let _ = send_reset_encrypted(&writer, &cipher, id, cfg.obfs).await;
                        streams.write().unwrap_or_else(|e| e.into_inner()).remove(&id);
                    }
                }
            }

            MuxCommand::Rst => {
                let stream_id = frame.stream_id;
                crate::mux_writer::recycle_encode_buf(frame.into_storage());
                let target = streams
                    .read()
                    .unwrap_or_else(|e| e.into_inner())
                    .get(&stream_id)
                    .map(|s| (s.tx.clone(), s.cancel.clone()));
                if let Some((tx, cancel)) = target {
                    // Best-effort: the data queue may already be full, and
                    // `cancel` is an out-of-band watch channel, so the stream
                    // task is signalled either way without blocking the read
                    // loop on a stalled consumer.
                    let _ = tx.try_send(StreamCommand::Reset);
                    let _ = cancel.send(true);
                }
            }

            MuxCommand::Version => {
                // W3 negotiation: first VERSION wins; enable send credit on
                // every registered stream (streams created later enable at
                // admission time).
                let decoded = decode_version_payload(frame.payload());
                crate::mux_writer::recycle_encode_buf(frame.into_storage());
                if let Some((version, kib)) = decoded {
                    if version >= 1 {
                        let prev = peer_window.swap(kib as u32, Ordering::AcqRel);
                        if prev == 0 {
                            let window = i64::from(kib) * 1024;
                            for entry in streams.read().unwrap_or_else(|e| e.into_inner()).values()
                            {
                                entry.gate.enable(window);
                            }
                            tracing::debug!(kib, "peer VERSION received; send window enabled");
                        }
                    }
                }
            }

            MuxCommand::Window => {
                let stream_id = frame.stream_id;
                let decoded = decode_window_payload(frame.payload());
                crate::mux_writer::recycle_encode_buf(frame.into_storage());
                if let Some(credit) = decoded {
                    if let Some(entry) = streams
                        .read()
                        .unwrap_or_else(|e| e.into_inner())
                        .get(&stream_id)
                    {
                        entry.gate.release(i64::from(credit));
                    }
                }
            }
        }
    }
    crate::mux_writer::recycle_encode_buf(frame_buf);

    // Unblock any target_to_mux sender parked on credit: the session is
    // over, so remaining sends must fail at the socket, not on WINDOWs
    // that will never arrive.
    for entry in streams.read().unwrap_or_else(|e| e.into_inner()).values() {
        entry.gate.close();
    }

    stream_tasks.abort_all();

    streams.write().unwrap_or_else(|e| e.into_inner()).clear();

    Ok(())
}
/// Plain non-MUX TCP relay, mirroring goway.go `handleServer`:
/// only the target hello (`host:port\n`) and `OK\n` are XOR-encrypted,
/// all subsequent data frames are plaintext.
async fn handle_server_tcp_parts<R, W>(
    mut rd: R,
    mut wr: W,
    cfg: RuntimeConfig,
    cipher: XorCipher,
    target_frame: Vec<u8>,
) -> Result<()>
where
    R: AsyncRead + Unpin + Send + 'static,
    W: AsyncWrite + Unpin + Send + 'static,
{
    let target_text =
        std::str::from_utf8(&target_frame).map_err(|_| anyhow!("invalid non-MUX target UTF-8"))?;
    let target = parse_target_authority(target_text.trim()).map_err(|e| anyhow!(e.to_string()))?;
    crate::mux_writer::recycle_encode_buf(target_frame);
    enforce_target_policy(&cfg, &target)?;
    let resolved = resolve_target(cfg.block_local, &target.host, target.port).await?;
    let target_stream = timeout(
        Duration::from_secs(cfg.connection_timeout.max(1)),
        TcpStream::connect(resolved),
    )
    .await
    .context("target connection timeout")??;
    apply_socket_options(&target_stream, &cfg);
    let mut ok = b"OK\n".to_vec();
    cipher.apply(&mut ok);
    write_frame(&mut wr, &ok, 2, false).await?;
    let (mut target_rd, mut target_wr) = target_stream.into_split();
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
            write_frame_borrowed(&mut wr, &mut buf[..n], 2, false).await?;
        }
        recycle_buf(buf).await;
        Result::<()>::Ok(())
    });
    let mut frame_buf = crate::mux_writer::acquire_encode_buf();
    // Run the upload loop as its own future: the old loop carried `?` on
    // `read_frame`, so a read error returned straight out of this function
    // and skipped `download.abort()`, leaving the download task detached
    // and holding the target socket for the rest of the process.
    let result = async {
        loop {
            tokio::select! {
                _ = &mut download => break,
                res = read_frame(
                    &mut rd,
                    Option::<&mut W>::None,
                    &mut frame_buf,
                ) => {
                    let Some((opcode, payload)) = res? else {
                        break;
                    };
                    if opcode == 8 {
                        crate::mux_writer::recycle_encode_buf(payload);
                        break;
                    }
                    if opcode != 2 {
                        crate::mux_writer::recycle_encode_buf(payload);
                        continue;
                    }
                    if target_wr.write_all(&payload).await.is_err() {
                        crate::mux_writer::recycle_encode_buf(payload);
                        break;
                    }
                    crate::stats::add_bytes(payload.len() as i64, 0);
                    crate::mux_writer::recycle_encode_buf(payload);
                }
            }
        }
        Ok::<(), anyhow::Error>(())
    }
    .await;
    crate::mux_writer::recycle_encode_buf(frame_buf);
    let _ = target_wr.shutdown().await;
    download.abort();
    result
}
async fn resolve_udp_target(block_local: bool, target: &TargetAddr) -> Result<SocketAddr> {
    resolve_target(block_local, &target.host, target.port).await
}
fn udp_envelope(source: SocketAddr, payload: &[u8]) -> Vec<u8> {
    let mut out = crate::mux_writer::acquire_encode_buf();
    let needed = 22 + payload.len();
    if out.capacity() < needed {
        // `Vec::reserve` takes *additional* capacity beyond `len`, not a total:
        // `needed - capacity` would under-reserve (often a complete no-op).
        out.reserve(needed.saturating_sub(out.len()));
    }
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
async fn handle_server_udp_parts<R, W>(
    mut rd: R,
    mut wr: W,
    cfg: RuntimeConfig,
    cipher: XorCipher,
) -> Result<()>
where
    R: AsyncRead + Unpin + Send + 'static,
    W: AsyncWrite + Unpin + Send + 'static,
{
    let mut ok = b"OK\n".to_vec();
    cipher.apply(&mut ok);
    write_frame(&mut wr, &ok, 2, false).await?;
    let udp = Arc::new(UdpSocket::bind("0.0.0.0:0").await?);
    // The relay socket is unconnected, so only datagrams whose source is an
    // address this session actually dialed may reach the client's downstream:
    // anyone who merely learns the port must not be able to inject packets
    // (or a forged source address) into the client's UDP leg. Target replies
    // always originate from the dialed address; everything else is dropped.
    let known_targets: Arc<StdMutex<HashSet<SocketAddr>>> = Arc::new(StdMutex::new(HashSet::new()));
    let udp_send = udp.clone();
    let known_send = known_targets.clone();
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
                if !known_send
                    .lock()
                    .unwrap_or_else(|e| e.into_inner())
                    .contains(&source)
                {
                    tracing::debug!(%source, "UDP relay dropped a datagram from an undialed source");
                    continue;
                }
                crate::stats::add_bytes(0, pkt.len() as i64);
                let mut packet = udp_envelope(source, pkt);
                cipher_send.apply(&mut packet);
                // Borrowed write: the envelope goes out via a vectored write
                // straight from this buffer, with no intermediate frame copy.
                let res = write_frame_borrowed(&mut wr, &mut packet, 2, false).await;
                crate::mux_writer::recycle_encode_buf(packet);
                if res.is_err() {
                    return Ok::<(), anyhow::Error>(());
                }
            }
        }
        Ok::<(), anyhow::Error>(())
    });
    let mut frame_buf = crate::mux_writer::acquire_encode_buf();
    let mut batch_writer = UdpBatchWriter::new(udp.clone());
    // Same guard as handle_server_tcp_parts above: the loop carried both a
    // `?` on `read_frame` and an early `return` on a bad datagram, and either
    // one skipped `send_task.abort()`, detaching a task that owns the UDP
    // socket and the session write half for the rest of the process.
    let result = async {
        loop {
            let Some((opcode, packet)) = read_frame_with_cipher(
                &mut rd,
                Option::<&mut W>::None,
                &mut frame_buf,
                Some(&cipher),
            )
            .await?
            else {
                break;
            };
            if opcode != 2 {
                crate::mux_writer::recycle_encode_buf(packet);
                continue;
            }
            let (target, payload) = match parse_socks5_udp_datagram(&packet) {
                Ok(res) => res,
                Err(e) => {
                    crate::mux_writer::recycle_encode_buf(packet);
                    return Err(anyhow!(e.to_string()));
                }
            };
            if cfg.block_local && is_blocked_local_host(&target.host) {
                crate::mux_writer::recycle_encode_buf(packet);
                continue;
            }
            let addr = match resolve_udp_target(cfg.block_local, &target).await {
                Ok(v) => v,
                Err(_) => {
                    crate::mux_writer::recycle_encode_buf(packet);
                    continue;
                }
            };
            // Recorded before the first payload leaves, so a reply can never
            // arrive ahead of its target joining the known set. Bounded to
            // 4096 entries to prevent unbounded growth under scanning attacks.
            {
                let mut guard = known_targets.lock().unwrap_or_else(|e| e.into_inner());
                if guard.len() >= 4096 {
                    guard.clear();
                }
                guard.insert(addr);
            }
            crate::stats::add_bytes(payload.len() as i64, 0);
            let _ = batch_writer.send(payload, addr).await;
            crate::mux_writer::recycle_encode_buf(packet);
        }
        Ok::<(), anyhow::Error>(())
    }
    .await;
    crate::mux_writer::recycle_encode_buf(frame_buf);
    let _ = batch_writer.flush().await;
    send_task.abort();
    result
}

/// Completes the server-side WebSocket HTTP upgrade and dispatches the
/// connection directly into the MUX, UDP, or non-MUX TCP handler without
/// any intermediate loopback hop.
pub(crate) async fn dispatch_server_ws_parts<R, W>(
    mut rd: R,
    mut wr: W,
    cfg: RuntimeConfig,
) -> Result<()>
where
    R: AsyncRead + Unpin + Send + 'static,
    W: AsyncWrite + Unpin + Send + 'static,
{
    let request = read_http_headers(&mut rd).await?;
    let key = validate_server_handshake(&request)?;
    wr.write_all(&build_server_handshake_response(&key)).await?;
    wr.flush().await?;
    // Built once per connection and fused into the first frame read so
    // the hello is unmasked and decrypted in a single pass with zero clones.
    let cipher = configured_cipher(&cfg.key);
    let mut buf = crate::mux_writer::acquire_encode_buf();
    let first_res = read_frame_with_cipher(&mut rd, Option::<&mut W>::None, &mut buf, Some(&cipher)).await;
    crate::mux_writer::recycle_encode_buf(buf);
    let Some((opcode, first)) = first_res? else {
        bail!("missing transport handshake")
    };
    if opcode != 2 {
        crate::mux_writer::recycle_encode_buf(first);
        bail!("invalid transport handshake opcode")
    }
    if first == b"UDP\n" {
        crate::mux_writer::recycle_encode_buf(first);
        return handle_server_udp_parts(rd, wr, cfg, cipher).await;
    }
    if first == b"MUX\n" {
        crate::mux_writer::recycle_encode_buf(first);
        return handle_mux_parts(rd, wr, cfg, cipher).await;
    }
    // Anything else is a plain non-MUX target ("host:port\n"),
    // exactly like goway.go handleServer's fallthrough branch.
    handle_server_tcp_parts(rd, wr, cfg, cipher, first).await
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
                // A transient accept error (EMFILE, ENFILE, ENOBUFS) must not
                // kill the whole proxy: log, back off briefly, and retry.
                let (stream, peer) = match res {
                    Ok(v) => v,
                    Err(e) => {
                        tracing::warn!(error=%e, "server listener accept failed; retrying in 100ms");
                        tokio::time::sleep(Duration::from_millis(100)).await;
                        continue;
                    }
                };
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
                    let (rd, wr) = stream.into_split();
                    if let Err(e) = dispatch_server_ws_parts(rd, wr, cfg2).await {
                        tracing::debug!(%peer, error=%e, "transport connection closed")
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

    /// The session-wide pre-dial budget must survive a holder that returns
    /// early: `PendingBytes::drop` is what puts the bytes back on every error
    /// path in the stream task.
    #[test]
    fn pending_budget_reserves_releases_and_returns_on_drop() {
        let total = Arc::new(AtomicUsize::new(0));
        let mut a = PendingBytes::new(total.clone());
        assert!(a.try_reserve(400, 1000));
        assert_eq!(a.held_bytes(), 400);
        assert_eq!(total.load(Ordering::Relaxed), 400);

        // A second holder fits exactly; a third byte does not, and the failed
        // reservation must leave the counter untouched.
        let mut b = PendingBytes::new(total.clone());
        assert!(b.try_reserve(600, 1000));
        assert!(!a.try_reserve(1, 1000));
        assert_eq!(total.load(Ordering::Relaxed), 1000);

        // Releasing on drain makes room again.
        a.release(400);
        assert_eq!(total.load(Ordering::Relaxed), 600);
        assert!(a.try_reserve(1, 1000));

        // Dropping a holder returns exactly what it still holds.
        drop(b);
        assert_eq!(total.load(Ordering::Relaxed), 1);
        drop(a);
        assert_eq!(total.load(Ordering::Relaxed), 0);

        // Over-releasing past what this holder owns is clamped.
        let mut c = PendingBytes::new(total.clone());
        assert!(c.try_reserve(10, 1000));
        c.release(1_000_000);
        assert_eq!(c.held_bytes(), 0);
        assert_eq!(total.load(Ordering::Relaxed), 0);
    }

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

    #[test]
    fn localhost_root_and_subdomains_are_blocked() {
        // The old check was an exact `eq_ignore_ascii_case("localhost")`.
        // `getaddrinfo` accepts `localhost.` (trailing root) and RFC 6761
        // reserves `*.localhost` — both resolve to loopback, neither matched.
        for host in [
            "localhost",
            "localhost.",
            "LOCALHOST.",
            "foo.localhost",
            "api.localhost.",
            "  localhost  ",
        ] {
            assert!(is_blocked_local_host(host), "{host:?} should be blocked");
        }
        // `localhost.com` is a real public domain and must stay reachable.
        for host in ["localhostx", "mylocalhost", "localhost.com", "example.org"] {
            assert!(
                !is_blocked_local_host(host),
                "{host:?} should NOT be blocked"
            );
        }
    }

    #[test]
    fn target_policy_applies_in_server_mode() {
        // `-block-local` defaults to true but used to be gated on
        // `upstream.is_some()`, so server mode silently skipped it while the
        // banner and TUI reported it as enabled.
        let mut cfg = RuntimeConfig::default();
        assert!(cfg.upstream.is_none(), "default config is server mode");
        assert!(cfg.block_local);

        let loopback = TargetAddr {
            host: "127.0.0.1".into(),
            port: 8080,
        };
        assert!(enforce_target_policy(&cfg, &loopback).is_err());

        let metadata = TargetAddr {
            host: "169.254.169.254".into(),
            port: 80,
        };
        assert!(enforce_target_policy(&cfg, &metadata).is_err());

        cfg.block_local = false;
        assert!(enforce_target_policy(&cfg, &loopback).is_ok());

        // Port 0 is rejected regardless of the block_local setting.
        let zero_port = TargetAddr {
            host: "example.com".into(),
            port: 0,
        };
        assert!(enforce_target_policy(&cfg, &zero_port).is_err());
    }

    #[tokio::test]
    async fn resolve_target_rechecks_after_resolution() {
        // IP literals short-circuit inside `resolve_host`, so these run
        // without touching the network.
        assert!(resolve_target(true, "127.0.0.1", 8080).await.is_err());
        assert!(resolve_target(true, "10.0.0.5", 8080).await.is_err());
        assert!(resolve_target(true, "169.254.169.254", 80).await.is_err());
        // With the policy off the same address dials normally.
        assert!(resolve_target(false, "127.0.0.1", 8080).await.is_ok());
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
} // mod runtime

pub mod stats {
//! Process-wide forwarding statistics (GoWay `Statistics` parity).
//!
//! Hot counters are plain relaxed atomics so relay tasks never contend on a
//! mutex; speed floats are written once per monitor tick and read by the TUI
//! or the `[STATS]` line.

use std::sync::atomic::{AtomicI64, AtomicU64, Ordering};

#[repr(align(64))]
struct HotCounters {
    active_conns: AtomicI64,
    bytes_up: AtomicI64,
    bytes_down: AtomicI64,
}

#[repr(align(64))]
struct SpeedWords {
    up_bits: AtomicU64,
    down_bits: AtomicU64,
}

static HOT: HotCounters = HotCounters {
    active_conns: AtomicI64::new(0),
    bytes_up: AtomicI64::new(0),
    bytes_down: AtomicI64::new(0),
};

static SPEEDS: SpeedWords = SpeedWords {
    up_bits: AtomicU64::new(0),
    down_bits: AtomicU64::new(0),
};

/// RAII guard: increments the active-connection gauge for the lifetime of a
/// spawned proxy/transport task. Mirrors GoWay `tryAcquireConn`/`releaseConn`
/// bookkeeping (the actual capacity limit remains the accept-loop semaphore).
pub struct ConnGuard;

impl ConnGuard {
    #[must_use]
    pub fn new() -> Self {
        HOT.active_conns.fetch_add(1, Ordering::Relaxed);
        Self
    }
}

impl Default for ConnGuard {
    fn default() -> Self {
        Self::new()
    }
}

impl Drop for ConnGuard {
    fn drop(&mut self) {
        let _ = HOT
            .active_conns
            .fetch_update(Ordering::Relaxed, Ordering::Relaxed, |v| {
                if v > 0 {
                    Some(v - 1)
                } else {
                    Some(0)
                }
            });
    }
}

/// Records relayed bytes: `up` is client→remote, `down` is remote→client.
#[inline]
pub fn add_bytes(up: i64, down: i64) {
    if up > 0 {
        HOT.bytes_up.fetch_add(up, Ordering::Relaxed);
    }
    if down > 0 {
        HOT.bytes_down.fetch_add(down, Ordering::Relaxed);
    }
}

#[inline]
pub fn active_conns() -> i64 {
    HOT.active_conns.load(Ordering::Relaxed)
}

#[inline]
pub fn totals() -> (i64, i64) {
    (
        HOT.bytes_up.load(Ordering::Relaxed),
        HOT.bytes_down.load(Ordering::Relaxed),
    )
}

fn set_speeds(up_mbps: f64, down_mbps: f64) {
    SPEEDS.up_bits.store(up_mbps.to_bits(), Ordering::Relaxed);
    SPEEDS
        .down_bits
        .store(down_mbps.to_bits(), Ordering::Relaxed);
}

pub fn speeds() -> (f64, f64) {
    (
        f64::from_bits(SPEEDS.up_bits.load(Ordering::Relaxed)),
        f64::from_bits(SPEEDS.down_bits.load(Ordering::Relaxed)),
    )
}

/// Spawns the GoWay `monitorStats` equivalent: recompute speeds every tick,
/// publish them atomically, and either drive the TUI refresh or print the
/// single-line `[STATS]` status (suppressed while the TUI owns the screen).
pub fn spawn_monitor(tui_enabled: bool, print_stats_line: bool) {
    tokio::spawn(async move {
        let interval_secs: u64 = if tui_enabled { 1 } else { 3 };
        let mut ticker = tokio::time::interval(std::time::Duration::from_secs(interval_secs));
        ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
        // Consume the immediate first tick so the initial window is a full interval.
        ticker.tick().await;
        let mut last_up = 0i64;
        let mut last_down = 0i64;
        loop {
            ticker.tick().await;
            let (curr_up, curr_down) = totals();
            let active = active_conns();
            let secs = interval_secs as f64;
            let up_speed = (curr_up.saturating_sub(last_up)) as f64 / secs / 1024.0 / 1024.0;
            let down_speed = (curr_down.saturating_sub(last_down)) as f64 / secs / 1024.0 / 1024.0;
            last_up = curr_up;
            last_down = curr_down;
            set_speeds(up_speed, down_speed);
            if tui_enabled {
                crate::tui::trigger_refresh();
            } else if print_stats_line {
                use std::io::Write as _;
                print!(
                    "\r\x1b[90m[STATS] Conns: {active} | Up: {up_speed:.2} MB/s | Down: {down_speed:.2} MB/s\x1b[0m"
                );
                let _ = std::io::stdout().flush();
            }
        }
    });
}
} // mod stats

pub mod tls {
//! TLS primitives for GoWay-compatible WSS.

use crate::ws::{pick_browser_profile_index, BROWSER_PROFILES};
use anyhow::{anyhow, bail, Context, Result};
use base64::{engine::general_purpose::STANDARD, Engine as _};
use rcgen::generate_simple_self_signed;
use rustls::crypto::CryptoProvider;
use rustls::pki_types::{
    CertificateDer, PrivateKeyDer, PrivatePkcs1KeyDer, PrivatePkcs8KeyDer, PrivateSec1KeyDer,
    ServerName,
};
use rustls::version::{TLS12, TLS13};
use rustls::{CipherSuite, ClientConfig, NamedGroup, RootCertStore, ServerConfig};
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, OnceLock};

type ProfileConfigCache = Mutex<HashMap<(usize, bool), Arc<ClientConfig>>>;
use tokio::net::TcpStream;
use tokio_rustls::{client::TlsStream, server::TlsAcceptor, TlsConnector};

pub type RushTlsStream = TlsStream<TcpStream>;

/// GoWay `tlsCiphersChrome136` order, restricted to suites rustls/ring
/// implements (RSA key exchange and CBC suites are unavailable and skipped).
const CHROMIUM_SUITE_ORDER: &[CipherSuite] = &[
    CipherSuite::TLS13_AES_128_GCM_SHA256,
    CipherSuite::TLS13_AES_256_GCM_SHA384,
    CipherSuite::TLS13_CHACHA20_POLY1305_SHA256,
    CipherSuite::TLS_ECDHE_ECDSA_WITH_AES_128_GCM_SHA256,
    CipherSuite::TLS_ECDHE_RSA_WITH_AES_128_GCM_SHA256,
    CipherSuite::TLS_ECDHE_ECDSA_WITH_AES_256_GCM_SHA384,
    CipherSuite::TLS_ECDHE_RSA_WITH_AES_256_GCM_SHA384,
    CipherSuite::TLS_ECDHE_ECDSA_WITH_CHACHA20_POLY1305_SHA256,
    CipherSuite::TLS_ECDHE_RSA_WITH_CHACHA20_POLY1305_SHA256,
];

/// GoWay `tlsCiphersFirefox138` order (same implementable subset).
const FIREFOX_SUITE_ORDER: &[CipherSuite] = &[
    CipherSuite::TLS13_AES_128_GCM_SHA256,
    CipherSuite::TLS13_CHACHA20_POLY1305_SHA256,
    CipherSuite::TLS13_AES_256_GCM_SHA384,
    CipherSuite::TLS_ECDHE_ECDSA_WITH_AES_128_GCM_SHA256,
    CipherSuite::TLS_ECDHE_RSA_WITH_AES_128_GCM_SHA256,
    CipherSuite::TLS_ECDHE_ECDSA_WITH_CHACHA20_POLY1305_SHA256,
    CipherSuite::TLS_ECDHE_RSA_WITH_CHACHA20_POLY1305_SHA256,
    CipherSuite::TLS_ECDHE_ECDSA_WITH_AES_256_GCM_SHA384,
    CipherSuite::TLS_ECDHE_RSA_WITH_AES_256_GCM_SHA384,
];

/// GoWay `curvePrefsChrome`/`curvePrefsFirefox` restricted to ring's key
/// exchange groups (ring has no P-521, so Firefox emits the shared subset).
const KX_GROUP_ORDER: &[NamedGroup] = &[
    NamedGroup::X25519,
    NamedGroup::secp256r1,
    NamedGroup::secp384r1,
];

fn roots() -> RootCertStore {
    static ROOTS: OnceLock<RootCertStore> = OnceLock::new();
    ROOTS
        .get_or_init(|| {
            let mut store = RootCertStore::empty();
            store.extend(webpki_roots::TLS_SERVER_ROOTS.iter().cloned());
            store
        })
        .clone()
}

/// Reorders the ring provider's cipher suites / key-exchange groups to a
/// browser profile's preference, mirroring GoWay's per-profile
/// `tls.Config` (`CipherSuites` + `CurvePreferences`, ALPN `http/1.1`).
/// Suites the provider lacks keep their relative order at the end.
fn provider_for_profile(profile_index: usize) -> CryptoProvider {
    let profile = &BROWSER_PROFILES[profile_index % BROWSER_PROFILES.len()];
    let order = if profile.tls_chromium_order {
        CHROMIUM_SUITE_ORDER
    } else {
        FIREFOX_SUITE_ORDER
    };
    let mut provider = rustls::crypto::ring::default_provider();
    provider.cipher_suites.sort_by_key(|suite| {
        order
            .iter()
            .position(|want| *want == suite.suite())
            .unwrap_or(usize::MAX)
    });
    provider.kx_groups.sort_by_key(|group| {
        KX_GROUP_ORDER
            .iter()
            .position(|want| *want == group.name())
            .unwrap_or(usize::MAX)
    });
    provider
}

fn build_profile_config(profile_index: usize, verify_ssl: bool) -> Arc<ClientConfig> {
    let provider = provider_for_profile(profile_index);
    let builder = ClientConfig::builder_with_provider(provider.into())
        .with_protocol_versions(&[&TLS13, &TLS12])
        .expect("ring provider supports TLS 1.2/1.3");
    let mut config = if verify_ssl {
        builder
            .with_root_certificates(roots())
            .with_no_client_auth()
    } else {
        builder
            .dangerous()
            .with_custom_certificate_verifier(Arc::new(NoCertificateVerification))
            .with_no_client_auth()
    };
    config.alpn_protocols = vec![b"http/1.1".to_vec()];
    Arc::new(config)
}

fn profile_config(profile_index: usize, verify_ssl: bool) -> Arc<ClientConfig> {
    static CACHE: OnceLock<ProfileConfigCache> = OnceLock::new();
    let cache = CACHE.get_or_init(|| Mutex::new(HashMap::new()));
    if let Some(config) = cache.lock().unwrap_or_else(|e| e.into_inner()).get(&(profile_index, verify_ssl)) {
        return config.clone();
    }
    let config = build_profile_config(profile_index, verify_ssl);
    cache
        .lock().unwrap_or_else(|e| e.into_inner())
        .insert((profile_index, verify_ssl), config.clone());
    config
}

/// Desired cipher-suite preference order for a profile (test hook).
#[cfg(test)]
pub fn suite_order_for_profile(profile_index: usize) -> &'static [CipherSuite] {
    if BROWSER_PROFILES[profile_index % BROWSER_PROFILES.len()].tls_chromium_order {
        CHROMIUM_SUITE_ORDER
    } else {
        FIREFOX_SUITE_ORDER
    }
}

/// Connect to an upstream TLS endpoint with a random browser-profile TLS
/// fingerprint (GoWay draws a random profile per session).
pub async fn connect(stream: TcpStream, host: &str, verify_ssl: bool) -> Result<RushTlsStream> {
    connect_with_profile(stream, host, verify_ssl, pick_browser_profile_index()).await
}

/// Connect with an explicit profile so callers can correlate the TLS
/// fingerprint with the HTTP handshake identity.
pub async fn connect_with_profile(
    stream: TcpStream,
    host: &str,
    verify_ssl: bool,
    profile_index: usize,
) -> Result<RushTlsStream> {
    let config = profile_config(profile_index, verify_ssl);
    let server_name = ServerName::try_from(host.to_owned())
        .map_err(|_| anyhow!("invalid TLS server name: {host}"))?;
    let connector = TlsConnector::from(config);
    connector
        .connect(server_name, stream)
        .await
        .context("TLS handshake failed")
}

/// Server identity supplied with `--cert` / `--key`.
///
/// Server modes otherwise generate a self-signed certificate at startup, which
/// no verifying client will accept. Holding it behind one `OnceLock` lets the
/// WSS acceptor and the QUIC server config share it without threading a new
/// parameter through every call site.
static SERVER_IDENTITY: OnceLock<Option<ServerIdentity>> = OnceLock::new();

struct ServerIdentity {
    certs: Vec<CertificateDer<'static>>,
    key: PrivateKeyDer<'static>,
}

/// Load the PEM certificate chain and private key used by server modes.
///
/// Both paths must be supplied together: a half-configured identity is a
/// startup error rather than a silent fall back to self-signed, because a
/// silent fall back would leave the operator believing they had pinned a
/// certificate when they had not.
pub fn configure_server_identity(
    cert_path: Option<PathBuf>,
    key_path: Option<PathBuf>,
) -> Result<()> {
    let (cert_path, key_path) = match (cert_path, key_path) {
        (None, None) => return Ok(()),
        (Some(cert), Some(key)) => (cert, key),
        _ => bail!("--cert and --key must be given together"),
    };
    let identity = ServerIdentity {
        certs: load_certs(&cert_path)?,
        key: load_key(&key_path)?,
    };
    SERVER_IDENTITY
        .set(Some(identity))
        .map_err(|_| anyhow!("server certificate configured twice"))?;
    tracing::info!(
        cert = %cert_path.display(),
        key = %key_path.display(),
        "loaded server certificate (self-signed generation disabled)"
    );
    Ok(())
}

/// The configured server identity, or `None` when `--cert`/`--key` were not
/// given and server mode should generate a self-signed certificate.
///
/// Returns freshly cloned material because both the WSS acceptor and the QUIC
/// server config consume it independently.
pub fn server_identity() -> Option<(Vec<CertificateDer<'static>>, PrivateKeyDer<'static>)> {
    let identity = SERVER_IDENTITY.get_or_init(|| None).as_ref()?;
    Some((identity.certs.clone(), identity.key.clone_key()))
}

/// Minimal RFC 7468 reader: split on `-----BEGIN <label>-----` /
/// `-----END <label>-----` and base64-decode the body.
///
/// Hand-rolled rather than pulled in as a dependency for two files read once
/// at startup; the crate already depends on `base64`, and the body lines are
/// concatenated back together (PEM wraps at a multiple of four characters, so
/// the concatenation stays valid) with missing padding restored.
fn pem_blocks(pem: &str) -> Result<Vec<(String, Vec<u8>)>> {
    let mut blocks = Vec::new();
    let mut label: Option<String> = None;
    let mut body = String::new();
    for line in pem.lines() {
        let line = line.trim();
        if let Some(rest) = line.strip_prefix("-----BEGIN ") {
            if let Some(name) = rest.strip_suffix("-----") {
                label = Some(name.to_string());
                body.clear();
            }
        } else if line.starts_with("-----END ") {
            if let Some(name) = label.take() {
                let mut encoded = body.replace(' ', "");
                while encoded.len() % 4 != 0 {
                    encoded.push('=');
                }
                let der = STANDARD
                    .decode(encoded)
                    .with_context(|| format!("decode base64 body of the {name:?} PEM block"))?;
                if der.is_empty() {
                    bail!("the {name:?} PEM block is empty");
                }
                blocks.push((name, der));
            }
            body.clear();
        } else if label.is_some() {
            body.push_str(line);
        }
    }
    Ok(blocks)
}

const CERT_LABELS: &[&str] = &["CERTIFICATE", "X509 CERTIFICATE", "TRUSTED CERTIFICATE"];

fn load_certs(path: &Path) -> Result<Vec<CertificateDer<'static>>> {
    let text = std::fs::read_to_string(path)
        .with_context(|| format!("read certificate file {}", path.display()))?;
    let certs: Vec<CertificateDer<'static>> = pem_blocks(&text)?
        .into_iter()
        .filter(|(label, _)| CERT_LABELS.contains(&label.as_str()))
        .map(|(_, der)| CertificateDer::from(der))
        .collect();
    if certs.is_empty() {
        bail!("{} contains no CERTIFICATE PEM block", path.display());
    }
    Ok(certs)
}

fn load_key(path: &Path) -> Result<PrivateKeyDer<'static>> {
    let text = std::fs::read_to_string(path)
        .with_context(|| format!("read private key file {}", path.display()))?;
    for (label, der) in pem_blocks(&text)? {
        match label.as_str() {
            "PRIVATE KEY" => return Ok(PrivateKeyDer::Pkcs8(PrivatePkcs8KeyDer::from(der))),
            "EC PRIVATE KEY" => return Ok(PrivateKeyDer::Sec1(PrivateSec1KeyDer::from(der))),
            "RSA PRIVATE KEY" => return Ok(PrivateKeyDer::Pkcs1(PrivatePkcs1KeyDer::from(der))),
            _ if CERT_LABELS.contains(&label.as_str()) => {}
            other => bail!(
                "{}: unsupported private key PEM label {other:?}; expected PRIVATE KEY, \
                 EC PRIVATE KEY or RSA PRIVATE KEY (re-encode with \
                 `openssl pkcs8 -topk8 -nocrypt`)",
                path.display()
            ),
        }
    }
    bail!(
        "{} contains no private key PEM block (expected PRIVATE KEY, EC PRIVATE KEY or RSA PRIVATE KEY)",
        path.display()
    )
}

/// Build the HTTP/1.1 TLS acceptor for standalone WSS server mode.
///
/// Uses the `--cert`/`--key` identity when one was configured, otherwise falls
/// back to a freshly generated self-signed certificate — which only works if
/// the client runs with `--no-verify-ssl`.
pub fn standalone_server_acceptor() -> Result<TlsAcceptor> {
    let (certs, key) = match server_identity() {
        Some(identity) => identity,
        None => {
            let cert = generate_simple_self_signed(vec!["localhost".into()])
                .context("generate WSS server certificate")?;
            let cert_der: CertificateDer<'static> = cert.cert.der().clone();
            let key =
                PrivateKeyDer::Pkcs8(PrivatePkcs8KeyDer::from(cert.key_pair.serialize_der()));
            (vec![cert_der], key)
        }
    };
    let mut config = ServerConfig::builder()
        .with_no_client_auth()
        .with_single_cert(certs, key)
        .context("build WSS server TLS config")?;
    config.alpn_protocols = vec![b"http/1.1".to_vec()];
    Ok(TlsAcceptor::from(Arc::new(config)))
}

#[derive(Debug)]
struct NoCertificateVerification;

impl rustls::client::danger::ServerCertVerifier for NoCertificateVerification {
    fn verify_server_cert(
        &self,
        _end_entity: &rustls::pki_types::CertificateDer<'_>,
        _intermediates: &[rustls::pki_types::CertificateDer<'_>],
        _server_name: &ServerName<'_>,
        _ocsp_response: &[u8],
        _now: rustls::pki_types::UnixTime,
    ) -> std::result::Result<rustls::client::danger::ServerCertVerified, rustls::Error> {
        Ok(rustls::client::danger::ServerCertVerified::assertion())
    }

    fn verify_tls12_signature(
        &self,
        _message: &[u8],
        _cert: &rustls::pki_types::CertificateDer<'_>,
        _dss: &rustls::DigitallySignedStruct,
    ) -> std::result::Result<rustls::client::danger::HandshakeSignatureValid, rustls::Error> {
        Ok(rustls::client::danger::HandshakeSignatureValid::assertion())
    }

    fn verify_tls13_signature(
        &self,
        _message: &[u8],
        _cert: &rustls::pki_types::CertificateDer<'_>,
        _dss: &rustls::DigitallySignedStruct,
    ) -> std::result::Result<rustls::client::danger::HandshakeSignatureValid, rustls::Error> {
        Ok(rustls::client::danger::HandshakeSignatureValid::assertion())
    }

    fn supported_verify_schemes(&self) -> Vec<rustls::SignatureScheme> {
        vec![
            rustls::SignatureScheme::ECDSA_NISTP256_SHA256,
            rustls::SignatureScheme::ECDSA_NISTP384_SHA384,
            rustls::SignatureScheme::ECDSA_NISTP521_SHA512,
            rustls::SignatureScheme::ED25519,
            rustls::SignatureScheme::RSA_PSS_SHA256,
            rustls::SignatureScheme::RSA_PSS_SHA384,
            rustls::SignatureScheme::RSA_PSS_SHA512,
            rustls::SignatureScheme::RSA_PKCS1_SHA256,
            rustls::SignatureScheme::RSA_PKCS1_SHA384,
            rustls::SignatureScheme::RSA_PKCS1_SHA512,
        ]
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn root_store_is_constructible() {
        let _ = roots();
    }

    #[test]
    fn pem_blocks_decodes_wrapped_and_unpadded_bodies() {
        // Windows line endings plus a body wrapped across two lines: the
        // reader has to concatenate the base64 back into one stream.
        let wrapped = concat!(
            "-----BEGIN CERTIFICATE-----\r\n",
            "YWJj\r\n",
            "ZA==\r\n",
            "-----END CERTIFICATE-----\r\n",
            "-----BEGIN PRIVATE KEY-----\n",
            "c2VjcmV0\n",
            "-----END PRIVATE KEY-----\n",
        );
        let blocks = pem_blocks(wrapped).unwrap();
        assert_eq!(blocks.len(), 2);
        assert_eq!(blocks[0].0, "CERTIFICATE");
        assert_eq!(&blocks[0].1[..], &b"abcd"[..]);
        assert_eq!(blocks[1].0, "PRIVATE KEY");
        assert_eq!(&blocks[1].1[..], &b"secret"[..]);

        // Six base64 characters: the missing "==" padding is restored.
        let unpadded = "-----BEGIN CERTIFICATE-----\nYWJjZA\n-----END CERTIFICATE-----\n";
        let blocks = pem_blocks(unpadded).unwrap();
        assert_eq!(blocks.len(), 1);
        assert_eq!(&blocks[0].1[..], &b"abcd"[..]);
    }

    #[test]
    fn loads_cert_and_key_from_disk_and_reports_bad_input() {
        let dir = tempfile::tempdir().unwrap();
        let cert_path = dir.path().join("cert.pem");
        let key_path = dir.path().join("key.pem");
        let bad_path = dir.path().join("bad.pem");

        std::fs::write(
            &cert_path,
            "-----BEGIN CERTIFICATE-----\nYWJjZA==\n-----END CERTIFICATE-----\n",
        )
        .unwrap();
        std::fs::write(
            &key_path,
            "-----BEGIN PRIVATE KEY-----\nc2VjcmV0\n-----END PRIVATE KEY-----\n",
        )
        .unwrap();
        std::fs::write(&bad_path, "this is not a PEM file").unwrap();

        let certs = load_certs(&cert_path).unwrap();
        assert_eq!(certs.len(), 1);
        assert_eq!(certs[0].as_ref(), &b"abcd"[..]);
        assert!(matches!(load_key(&key_path), Ok(PrivateKeyDer::Pkcs8(_))));

        let err = load_certs(&bad_path).unwrap_err().to_string();
        assert!(err.contains("no CERTIFICATE PEM block"), "{err}");
    }

    #[test]
    fn cached_configs_are_reused() {
        assert!(Arc::ptr_eq(
            &profile_config(0, true),
            &profile_config(0, true)
        ));
        assert!(Arc::ptr_eq(
            &profile_config(0, false),
            &profile_config(0, false)
        ));
        assert!(!Arc::ptr_eq(
            &profile_config(0, true),
            &profile_config(0, false)
        ));
    }

    #[test]
    fn profile_suite_orders_match_goway() {
        // Chromium puts AES-256-GCM second; Firefox puts ChaCha20 second.
        assert_eq!(
            suite_order_for_profile(0)[1],
            CipherSuite::TLS13_AES_256_GCM_SHA384
        );
        let firefox = BROWSER_PROFILES
            .iter()
            .position(|p| !p.is_chromium)
            .unwrap();
        assert_eq!(
            suite_order_for_profile(firefox)[1],
            CipherSuite::TLS13_CHACHA20_POLY1305_SHA256
        );
    }

    #[test]
    fn provider_starts_with_profile_suite() {
        for idx in 0..BROWSER_PROFILES.len() {
            let provider = provider_for_profile(idx);
            let want = suite_order_for_profile(idx)[0];
            assert_eq!(provider.cipher_suites[0].suite(), want);
            // Key-exchange preference leads with X25519 (GoWay parity).
            assert_eq!(provider.kx_groups[0].name(), NamedGroup::X25519);
        }
    }

    #[test]
    fn standalone_server_acceptor_is_constructible() {
        assert!(standalone_server_acceptor().is_ok());
    }

    /// Minimal repro probe for the WSS second-session stall: two sequential
    /// bare TLS handshakes against one acceptor, second while the first is
    /// still open. Times out loudly instead of hanging the suite.
    #[tokio::test]
    async fn two_sequential_tls_handshakes() {
        let acceptor = standalone_server_acceptor().unwrap();
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();
        let srv = tokio::spawn(async move {
            for _ in 0..2 {
                let (stream, _) = listener.accept().await.unwrap();
                tokio::time::timeout(std::time::Duration::from_secs(10), acceptor.accept(stream))
                    .await
                    .expect("server accept timeout")
                    .expect("server accept failed");
            }
        });
        let mut first = None;
        for _ in 0..2 {
            let tcp = tokio::net::TcpStream::connect(("127.0.0.1", port))
                .await
                .unwrap();
            let tls = tokio::time::timeout(
                std::time::Duration::from_secs(10),
                connect(tcp, "localhost", false),
            )
            .await
            .expect("client handshake timeout")
            .expect("client handshake failed");
            first = Some(tls);
        }
        drop(first);
        srv.await.unwrap();
    }
}
} // mod tls

pub mod tui {
//! GoWay-parity UX surfaces: TUI dashboard, `-log-file` ring, and the
//! tracing bridge that feeds both.
//!
//! Layout and cadence mirror `goway.go` `drawTUI` / `tuiRefreshLoop` /
//! `addLogFileEntry` so operators can switch tools without relearning the
//! dashboard.

use crate::runtime::RuntimeConfig;
use crate::stats;
use std::collections::VecDeque;
use std::fmt::Write as _;
use std::io::IsTerminal;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Mutex, OnceLock};
use tracing::{Event, Subscriber};
use tracing_subscriber::layer::{Context, Layer};
use tracing_subscriber::registry::LookupSpan;

const TUI_RING_CAP: usize = 100;
const LOG_FILE_RING_CAP: usize = 10;

const ANSI_RESET: &str = "\x1b[0m";
const ANSI_CYAN: &str = "\x1b[36m";
const ANSI_GREEN: &str = "\x1b[32m";
const ANSI_YELLOW: &str = "\x1b[33m";
const ANSI_RED: &str = "\x1b[31m";
const ANSI_MAGENTA: &str = "\x1b[35m";
const ANSI_GREY: &str = "\x1b[90m";

// ---------------------------------------------------------------------------
// TUI ring + dirty flag
// ---------------------------------------------------------------------------

static ENABLED: AtomicBool = AtomicBool::new(false);

struct TuiState {
    dirty: AtomicBool,
    ring: Mutex<VecDeque<String>>,
}

fn tui_state() -> &'static TuiState {
    static STATE: OnceLock<TuiState> = OnceLock::new();
    STATE.get_or_init(|| TuiState {
        dirty: AtomicBool::new(false),
        ring: Mutex::new(VecDeque::with_capacity(TUI_RING_CAP)),
    })
}

/// Enables TUI bookkeeping. Must be called before the refresh loop starts.
pub fn enable_tui() {
    ENABLED.store(true, Ordering::Relaxed);
}

pub fn tui_enabled() -> bool {
    ENABLED.load(Ordering::Relaxed)
}

pub fn trigger_refresh() {
    if tui_enabled() {
        tui_state().dirty.store(true, Ordering::Relaxed);
    }
}

fn add_tui_log(line: &str) {
    if !tui_enabled() {
        return;
    }
    let line = line.trim();
    if line.is_empty() {
        return;
    }
    let mut ring = tui_state().ring.lock().unwrap_or_else(|e| e.into_inner());
    if ring.len() == TUI_RING_CAP {
        ring.pop_front();
    }
    ring.push_back(line.to_string());
    tui_state().dirty.store(true, Ordering::Relaxed);
}

fn tui_log_slice(n: usize) -> Vec<String> {
    let ring = tui_state().ring.lock().unwrap_or_else(|e| e.into_inner());
    let take = n.min(ring.len());
    ring.iter().rev().take(take).rev().cloned().collect()
}

// ---------------------------------------------------------------------------
// `-log-file` ring (last 10 WARN/ERROR lines, rewritten on each append)
// ---------------------------------------------------------------------------

struct LogFileState {
    path: Mutex<Option<PathBuf>>,
    ring: Mutex<VecDeque<String>>,
}

fn log_file_state() -> &'static LogFileState {
    static STATE: OnceLock<LogFileState> = OnceLock::new();
    STATE.get_or_init(|| LogFileState {
        path: Mutex::new(None),
        ring: Mutex::new(VecDeque::with_capacity(LOG_FILE_RING_CAP)),
    })
}

/// Configures the `-log-file` destination (call once during startup).
pub fn configure_log_file(path: PathBuf) {
    let state = log_file_state();
    *state.path.lock().unwrap_or_else(|e| e.into_inner()) = Some(path);
}

fn log_file_add(entry: &str) {
    let state = log_file_state();
    let path = state.path.lock().unwrap_or_else(|e| e.into_inner()).clone();
    let Some(path) = path else { return };
    let stamp = stamp_ymd_hms();
    let mut ring = state.ring.lock().unwrap_or_else(|e| e.into_inner());
    if ring.len() == LOG_FILE_RING_CAP {
        ring.pop_front();
    }
    ring.push_back(format!("{stamp} {entry}"));
    let content = render_log_file(&ring);
    drop(ring);
    let _ = std::fs::write(&path, content);
}

fn render_log_file(ring: &VecDeque<String>) -> String {
    let mut out = String::new();
    for line in ring {
        let _ = writeln!(out, "{line}");
    }
    out
}

/// Final flush on shutdown (GoWay `saveLogFile` parity).
pub fn save_log_file() {
    let state = log_file_state();
    let path = state.path.lock().unwrap_or_else(|e| e.into_inner()).clone();
    let Some(path) = path else { return };
    let ring = state.ring.lock().unwrap_or_else(|e| e.into_inner());
    if ring.is_empty() {
        return;
    }
    let content = render_log_file(&ring);
    let count = ring.len();
    drop(ring);
    match std::fs::write(&path, content) {
        Ok(()) => tracing::info!(path = %path.display(), entries = count, "log file saved"),
        Err(error) => tracing::warn!(path = %path.display(), %error, "failed to save log file"),
    }
}

// ---------------------------------------------------------------------------
// Timestamps
// ---------------------------------------------------------------------------

fn clock_hms() -> String {
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default();
    let secs = now.as_secs() % 86_400;
    format!(
        "{:02}:{:02}:{:02}",
        secs / 3600,
        (secs % 3600) / 60,
        secs % 60
    )
}

fn stamp_ymd_hms() -> String {
    // Civil date from Unix seconds (Howard Hinnant's algorithm) — avoids a
    // formatting dependency and matches GoWay's local-looking wall stamp shape.
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs() as i64;
    let days = now.div_euclid(86_400);
    let secs_of_day = now.rem_euclid(86_400);
    let (y, m, d) = civil_from_days(days);
    format!(
        "{y:04}-{m:02}-{d:02} {:02}:{:02}:{:02}",
        secs_of_day / 3600,
        (secs_of_day % 3600) / 60,
        secs_of_day % 60
    )
}

fn civil_from_days(z: i64) -> (i64, u32, u32) {
    let z = z + 719_468;
    let era = if z >= 0 { z } else { z - 146_096 } / 146_097;
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    (if m <= 2 { y + 1 } else { y }, m as u32, d as u32)
}

// ---------------------------------------------------------------------------
// ANSI helpers
// ---------------------------------------------------------------------------

fn visible_len(s: &str) -> usize {
    let mut len = 0;
    let mut bytes = s.bytes();
    while let Some(b) = bytes.next() {
        if b == 0x1b {
            for b in bytes.by_ref() {
                if b == b'm' {
                    break;
                }
            }
            continue;
        }
        len += 1;
    }
    len
}

fn pad_visible(s: &str, width: usize) -> String {
    let vl = visible_len(s);
    if vl >= width {
        return s.to_string();
    }
    format!("{s}{}", " ".repeat(width - vl))
}

fn truncate_visible(s: &str, max_len: usize) -> String {
    if visible_len(s) <= max_len {
        return s.to_string();
    }
    let limit = max_len.saturating_sub(3);
    let mut out = String::new();
    let mut count = 0;
    let mut in_esc = false;
    for ch in s.chars() {
        if ch == '\x1b' {
            in_esc = true;
            out.push(ch);
            continue;
        }
        if in_esc {
            out.push(ch);
            if ch == 'm' {
                in_esc = false;
            }
            continue;
        }
        if count < limit {
            out.push(ch);
            count += 1;
        } else {
            break;
        }
    }
    out.push_str("...");
    out.push_str(ANSI_RESET);
    out
}

fn format_bytes(bytes: i64) -> String {
    if bytes == 0 {
        return "0 B".to_string();
    }
    const UNITS: [f64; 5] = [
        1024.0,
        1_048_576.0,
        1_073_741_824.0,
        1_099_511_627_776.0,
        1_125_899_906_842_624.0,
    ];
    const LABELS: [&str; 5] = ["KB", "MB", "GB", "TB", "PB"];
    let abs = bytes.unsigned_abs() as f64;
    let neg = bytes < 0;
    if abs < 1024.0 {
        return format!("{bytes} B");
    }
    for (i, div) in UNITS.iter().enumerate() {
        if abs < *div * 1024.0 || i == UNITS.len() - 1 {
            let v = abs / div;
            return format!("{}{v:.2} {}", if neg { "-" } else { "" }, LABELS[i]);
        }
    }
    format!("{bytes} B")
}

fn two_col(
    left_label: &str,
    left_val: &str,
    right_label: &str,
    right_val: &str,
    col: usize,
) -> String {
    let left = pad_visible(&format!("{left_label}{left_val}"), col);
    format!("{left}{right_label}{right_val}")
}

// ---------------------------------------------------------------------------
// Dashboard render + refresh loop
// ---------------------------------------------------------------------------

/// Immutable snapshot of everything the dashboard shows (built once in main).
#[derive(Clone)]
pub struct TuiConfig {
    pub version: String,
    pub mode: String,
    pub listen: String,
    pub upstream: String,
    pub mux: String,
    pub obfs: String,
    pub dns: String,
    pub auth: String,
    pub buffer: String,
    pub max_conns: usize,
    pub block_local: bool,
    pub verify_ssl: bool,
    pub log_level: String,
    pub fakehost: Option<String>,
    pub tcp_nodelay: bool,
    pub tcp_keepalive: bool,
    pub connection_timeout: u64,
}

impl TuiConfig {
    pub fn from_runtime(
        cfg: &RuntimeConfig,
        mux_sessions: usize,
        dns_display: &str,
        verify_ssl: bool,
        log_level: &str,
    ) -> Self {
        let mode = match cfg.upstream.as_deref() {
            None => "Server".to_string(),
            Some(u) if u.starts_with("wss://") => "Client (WSS + SOCKS5/HTTP)".to_string(),
            Some(u) if u.starts_with("quic") => "Client (QUIC + SOCKS5/HTTP)".to_string(),
            Some(_) => "Client (WS + SOCKS5/HTTP)".to_string(),
        };
        let mux = if cfg.mux {
            format!("Enabled (sessions: {mux_sessions})")
        } else {
            "Disabled (1:1)".to_string()
        };
        let auth = if cfg.key.is_some() {
            "Enabled (XOR)".to_string()
        } else if cfg.upstream.is_none() {
            "DISABLED (Open Proxy!)".to_string()
        } else {
            "Disabled".to_string()
        };
        Self {
            version: env!("CARGO_PKG_VERSION").to_string(),
            mode,
            listen: format!("{}:{}", cfg.proxy_host, cfg.proxy_port),
            upstream: cfg.upstream.clone().unwrap_or_else(|| "N/A".to_string()),
            mux,
            obfs: if cfg.obfs { "Enabled" } else { "Disabled" }.to_string(),
            dns: dns_display.to_string(),
            auth,
            buffer: {
                let mut s = format!("{} KB", cfg.buffer_size / 1024);
                if cfg.socket_buffer > 0 {
                    let _ = write!(s, " (Socket: {} KB)", cfg.socket_buffer);
                }
                s
            },
            max_conns: cfg.max_connections,
            block_local: cfg.block_local,
            verify_ssl,
            log_level: log_level.trim().to_ascii_uppercase(),
            fakehost: cfg.fakehost.clone(),
            tcp_nodelay: cfg.tcp_nodelay,
            tcp_keepalive: cfg.tcp_keepalive,
            connection_timeout: cfg.connection_timeout,
        }
    }
}

fn draw_tui(cfg: &TuiConfig) {
    let (term_w, term_h) = terminal_size();
    let mut box_w = term_w.saturating_sub(2);
    box_w = box_w.clamp(50, 110);
    let inner = box_w.saturating_sub(4);
    let col = inner / 2;

    let h = "─".repeat(box_w.saturating_sub(2));
    let top = format!("┌{h}┐\n");
    let mid = format!("├{h}┤\n");
    let bot = format!("└{h}┘\n");

    let mut buf = String::with_capacity(box_w * (term_h + 4));
    buf.push_str("\x1b[H\x1b[J");
    buf.push_str(&top);

    let title = format!("RUSHWAY Proxy Dashboard (v{})", cfg.version);
    let pad = (box_w.saturating_sub(2).saturating_sub(title.len())) / 2;
    let rpad = box_w
        .saturating_sub(2)
        .saturating_sub(pad)
        .saturating_sub(title.len());
    let _ = writeln!(
        buf,
        "│{}{ANSI_CYAN}{title}{ANSI_RESET}{}│",
        " ".repeat(pad),
        " ".repeat(rpad)
    );
    buf.push_str(&mid);

    let mode_col = if cfg.upstream == "N/A" {
        format!("{ANSI_YELLOW}{}{ANSI_RESET}", cfg.mode)
    } else {
        format!("{ANSI_GREEN}{}{ANSI_RESET}", cfg.mode)
    };
    row(
        &mut buf,
        &two_col(
            "Mode:      ",
            &mode_col,
            "Max Conns:   ",
            &cfg.max_conns.to_string(),
            col,
        ),
        box_w,
    );
    row(
        &mut buf,
        &two_col(
            "Listen:    ",
            &format!("{ANSI_YELLOW}{}{ANSI_RESET}", cfg.listen),
            "Timeout:     ",
            &format!("{ANSI_YELLOW}{}s{ANSI_RESET}", cfg.connection_timeout),
            col,
        ),
        box_w,
    );
    let block_col = if cfg.block_local {
        format!("{ANSI_GREEN}Enabled{ANSI_RESET}")
    } else {
        format!("{ANSI_RED}Disabled{ANSI_RESET}")
    };
    row(
        &mut buf,
        &two_col(
            "Upstream:  ",
            &format!("{ANSI_MAGENTA}{}{ANSI_RESET}", cfg.upstream),
            "Block Local: ",
            &block_col,
            col,
        ),
        box_w,
    );
    let ssl_col = if cfg.upstream == "N/A" {
        format!("{ANSI_GREY}N/A{ANSI_RESET}")
    } else if cfg.verify_ssl {
        format!("{ANSI_GREEN}Enabled{ANSI_RESET}")
    } else {
        format!("{ANSI_RED}Disabled (Insecure){ANSI_RESET}")
    };
    let auth_col = if cfg.auth.starts_with("Enabled") {
        format!("{ANSI_GREEN}{}{ANSI_RESET}", cfg.auth)
    } else if cfg.auth.contains("DISABLED") {
        format!("{ANSI_RED}{}{ANSI_RESET}", cfg.auth)
    } else {
        format!("{ANSI_YELLOW}{}{ANSI_RESET}", cfg.auth)
    };
    row(
        &mut buf,
        &two_col("Auth:      ", &auth_col, "SSL Verify:  ", &ssl_col, col),
        box_w,
    );
    let dns_col = if cfg.dns == "System default" {
        format!("{ANSI_YELLOW}{}{ANSI_RESET}", cfg.dns)
    } else {
        format!("{ANSI_GREEN}Remote: {}{ANSI_RESET}", cfg.dns)
    };
    let level_col = match cfg.log_level.as_str() {
        "DEBUG" => format!("{ANSI_CYAN}DEBUG{ANSI_RESET}"),
        "WARN" | "WARNING" => format!("{ANSI_YELLOW}WARN{ANSI_RESET}"),
        "ERROR" => format!("{ANSI_RED}ERROR{ANSI_RESET}"),
        "OFF" => format!("{ANSI_GREY}OFF{ANSI_RESET}"),
        _ => format!("{ANSI_GREEN}INFO{ANSI_RESET}"),
    };
    row(
        &mut buf,
        &two_col("DNS:       ", &dns_col, "Log Level:   ", &level_col, col),
        box_w,
    );
    let nd = if cfg.tcp_nodelay {
        format!("{ANSI_GREEN}On{ANSI_RESET}")
    } else {
        format!("{ANSI_RED}Off{ANSI_RESET}")
    };
    let ka = if cfg.tcp_keepalive {
        format!("{ANSI_GREEN}On{ANSI_RESET}")
    } else {
        format!("{ANSI_RED}Off{ANSI_RESET}")
    };
    row(
        &mut buf,
        &two_col(
            "Buffer:    ",
            &cfg.buffer,
            "TCP Settings:",
            &format!("NoDelay:{nd} KeepAlive:{ka}"),
            col,
        ),
        box_w,
    );
    let mut config_lines = 6u32;
    if let Some(fh) = cfg.fakehost.as_deref().filter(|s| !s.is_empty()) {
        row(
            &mut buf,
            &two_col(
                "Fake Host: ",
                &format!("{ANSI_MAGENTA}{fh}{ANSI_RESET}"),
                "",
                "",
                col,
            ),
            box_w,
        );
        config_lines = 7;
    }
    row(
        &mut buf,
        &two_col("Mux:       ", &cfg.mux, "Obfs:        ", &cfg.obfs, col),
        box_w,
    );
    config_lines += 1;

    buf.push_str(&mid);

    let (active, (up_total, down_total)) = (stats::active_conns(), stats::totals());
    let (speed_up, speed_down) = stats::speeds();
    row(
        &mut buf,
        &two_col(
            "Conns:     ",
            &format!("{active} / {}", cfg.max_conns),
            "",
            "",
            col,
        ),
        box_w,
    );
    row(
        &mut buf,
        &two_col(
            "Download:  ",
            &format!("{speed_down:.2} MB/s"),
            "Total Down:  ",
            &format_bytes(down_total),
            col,
        ),
        box_w,
    );
    row(
        &mut buf,
        &two_col(
            "Upload:    ",
            &format!("{speed_up:.2} MB/s"),
            "Total Up:    ",
            &format_bytes(up_total),
            col,
        ),
        box_w,
    );

    buf.push_str(&mid);

    // 3 borders + title lines + config rows + mid + 3 stats + mid + logs + bot
    let used = 3 + 2 + config_lines + 1 + 3 + 1 + 1 + 1;
    let mut log_lines = (term_h as i32 - used as i32).max(5) as usize;
    if log_lines > 40 {
        log_lines = 40;
    }
    row(
        &mut buf,
        &format!("{ANSI_CYAN}Recent Logs:{ANSI_RESET}"),
        box_w,
    );
    let logs = tui_log_slice(log_lines);
    for line in &logs {
        row(&mut buf, &truncate_visible(line, inner), box_w);
    }
    for _ in logs.len()..log_lines {
        row(&mut buf, "", box_w);
    }

    buf.push_str(&bot);
    use std::io::Write as _;
    let mut out = std::io::stdout();
    let _ = out.write_all(buf.as_bytes());
    let _ = out.flush();
}

fn row(buf: &mut String, content: &str, width: usize) {
    buf.push_str("│ ");
    buf.push_str(content);
    let pad = width.saturating_sub(4).saturating_sub(visible_len(content));
    if pad > 0 {
        buf.push_str(&" ".repeat(pad));
    }
    buf.push_str(" │\n");
}

fn terminal_size() -> (usize, usize) {
    let mut w = std::env::var("COLUMNS").ok().and_then(|v| v.parse().ok());
    let mut h = std::env::var("LINES").ok().and_then(|v| v.parse().ok());
    if w.is_none() || h.is_none() {
        // GoWay's single-file build hardcodes 80×24; keep that as the floor and
        // allow COLUMNS/LINES overrides for better UX on real terminals.
        if w.is_none() {
            w = Some(80);
        }
        if h.is_none() {
            h = Some(24);
        }
    }
    (w.unwrap_or(80), h.unwrap_or(24))
}

/// GoWay `tuiRefreshLoop`: initial draw, then redraw at 10 Hz only when dirty.
pub fn spawn_refresh_loop(cfg: TuiConfig) {
    tokio::spawn(async move {
        draw_tui(&cfg);
        let mut ticker = tokio::time::interval(std::time::Duration::from_millis(100));
        ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
        loop {
            ticker.tick().await;
            if tui_state().dirty.swap(false, Ordering::Relaxed) {
                draw_tui(&cfg);
            }
        }
    });
}

// ---------------------------------------------------------------------------
// tracing bridge
// ---------------------------------------------------------------------------

#[derive(Clone, Copy)]
pub struct UxLayer {
    tui: bool,
    log_file: bool,
}

impl UxLayer {
    pub fn new(tui: bool, log_file: bool) -> Self {
        Self { tui, log_file }
    }
}

#[derive(Default)]
struct EventVisitor {
    message: String,
    extras: Vec<String>,
}

impl tracing::field::Visit for EventVisitor {
    fn record_debug(&mut self, field: &tracing::field::Field, value: &dyn std::fmt::Debug) {
        if field.name() == "message" {
            // `format_args!` Debug forwards to Display → no surrounding quotes.
            self.message = format!("{value:?}");
        } else {
            self.extras.push(format!("{}={:?}", field.name(), value));
        }
    }

    fn record_str(&mut self, field: &tracing::field::Field, value: &str) {
        if field.name() == "message" {
            self.message = value.to_string();
        } else {
            self.extras.push(format!("{}={}", field.name(), value));
        }
    }

    fn record_i64(&mut self, field: &tracing::field::Field, value: i64) {
        if field.name() != "message" {
            self.extras.push(format!("{}={}", field.name(), value));
        }
    }

    fn record_u64(&mut self, field: &tracing::field::Field, value: u64) {
        if field.name() != "message" {
            self.extras.push(format!("{}={}", field.name(), value));
        }
    }

    fn record_bool(&mut self, field: &tracing::field::Field, value: bool) {
        if field.name() != "message" {
            self.extras.push(format!("{}={}", field.name(), value));
        }
    }

    fn record_f64(&mut self, field: &tracing::field::Field, value: f64) {
        if field.name() != "message" {
            self.extras.push(format!("{}={}", field.name(), value));
        }
    }
}

impl<S> Layer<S> for UxLayer
where
    S: Subscriber + for<'lookup> LookupSpan<'lookup>,
{
    fn on_event(&self, event: &Event<'_>, _ctx: Context<'_, S>) {
        let meta = event.metadata();
        let level = *meta.level();
        let mut visitor = EventVisitor::default();
        event.record(&mut visitor);
        let mut text = visitor.message;
        if !visitor.extras.is_empty() {
            if !text.is_empty() {
                text.push(' ');
            }
            text.push_str(&visitor.extras.join(" "));
        }
        if text.is_empty() {
            return;
        }

        if self.tui {
            let (color, tag) = match level {
                tracing::Level::TRACE => (ANSI_GREY, "TRACE"),
                tracing::Level::DEBUG => (ANSI_CYAN, "DEBUG"),
                tracing::Level::INFO => (ANSI_GREEN, "INFO "),
                tracing::Level::WARN => (ANSI_YELLOW, "WARN"),
                tracing::Level::ERROR => (ANSI_RED, "ERROR"),
            };
            add_tui_log(&format!(
                "{} {color}[{tag}]{ANSI_RESET} {text}",
                clock_hms()
            ));
        }

        if self.log_file && (level == tracing::Level::WARN || level == tracing::Level::ERROR) {
            log_file_add(&format!("[{level}] {text}"));
        }
    }
}

/// Returns true when stdout is a character device (enables terminal control
/// sequences without pulling in a TTY crate).
pub fn stdout_is_tty() -> bool {
    std::io::stdout().is_terminal()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn visible_len_skips_ansi() {
        assert_eq!(visible_len("\x1b[32mHi\x1b[0m"), 2);
    }

    #[test]
    fn civil_from_days_known_epochs() {
        // 1970-01-01 is day 0.
        assert_eq!(civil_from_days(0), (1970, 1, 1));
        // 2000-03-01 (leap year check): days = 11017
        assert_eq!(civil_from_days(11017), (2000, 3, 1));
    }

    #[test]
    fn format_bytes_rounds() {
        assert_eq!(format_bytes(0), "0 B");
        assert_eq!(format_bytes(2048), "2.00 KB");
    }

    #[test]
    fn log_file_ring_keeps_last_entries() {
        let dir = std::env::temp_dir().join(format!("rushway-logfile-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("out.log");
        configure_log_file(path.clone());
        for i in 0..15 {
            log_file_add(&format!("[ERROR] entry-{i}"));
        }
        let content = std::fs::read_to_string(&path).unwrap();
        let lines: Vec<_> = content.lines().collect();
        assert_eq!(lines.len(), LOG_FILE_RING_CAP);
        assert!(lines[0].contains("entry-5"));
        assert!(lines[9].contains("entry-14"));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn stats_conn_guard_tracks_active() {
        let before = crate::stats::active_conns();
        {
            let _g = crate::stats::ConnGuard::new();
            assert_eq!(crate::stats::active_conns(), before + 1);
        }
        assert_eq!(crate::stats::active_conns(), before);
    }

    #[test]
    fn stats_add_bytes_accumulates() {
        let (u0, d0) = crate::stats::totals();
        crate::stats::add_bytes(100, 200);
        let (u1, d1) = crate::stats::totals();
        assert_eq!(u1 - u0, 100);
        assert_eq!(d1 - d0, 200);
    }
}
} // mod tui

pub mod udp_batch {
//! Batched UDP reads shared by all UDP relay upload paths.
//!
//! GoWay parity (`#2 UDP batching`): DNS bursts and game traffic arrive as
//! packet trains — one syscall per datagram wastes the pps budget. This
//! drains up to [`UDP_BATCH`] datagrams per syscall via `recvmmsg` on Linux
//! and falls back to a single `recv_from` elsewhere (e.g. Windows, whose
//! kernel has no equivalent). Behavior is identical on all platforms; only
//! Linux goes faster.
//!
//! Design notes:
//! - The socket stays a shared `Arc<tokio::net::UdpSocket>` (senders keep
//!   using `send_to` on their clones); readiness comes from
//!   `readable()` and the batch is pulled with one `recvmmsg`, exactly the
//!   pattern Quinn uses. No lock, no extra task, no new dependency.
//! - Buffers stay owned full-length (`len == cap == 65535`); only the valid
//!   prefix (tracked in `lens`) is ever read, so there is no `set_len`
//!   unsoundness. 64 KiB covers the maximum UDP payload, matching the
//!   single-recv loops this replaces.

use std::io;
use std::net::SocketAddr;
use std::sync::Arc;

/// Datagrams per `recvmmsg` syscall. 8 amortizes the syscall without adding
/// latency: a partial batch returns immediately, `recvmmsg` never waits for
/// a full one.
pub(crate) const UDP_BATCH: usize = 8;
/// Max UDP payload size: a full-size datagram is never truncated.
pub(crate) const UDP_BUF_SIZE: usize = 65535;

pub(crate) struct UdpBatchReader {
    sock: Arc<tokio::net::UdpSocket>,
    bufs: Vec<Vec<u8>>,
    lens: Vec<usize>,
    addrs: Vec<SocketAddr>,
}

impl UdpBatchReader {
    pub(crate) fn new(sock: Arc<tokio::net::UdpSocket>) -> Self {
        Self {
            sock,
            bufs: (0..UDP_BATCH).map(|_| vec![0u8; UDP_BUF_SIZE]).collect(),
            lens: vec![0; UDP_BATCH],
            addrs: vec![SocketAddr::from(([0, 0, 0, 0], 0)); UDP_BATCH],
        }
    }

    /// Receives up to [`UDP_BATCH`] datagrams, returning the count. Access
    /// each via [`UdpBatchReader::packet`].
    pub(crate) async fn recv(&mut self) -> io::Result<usize> {
        loop {
            self.sock.readable().await?;
            match self.try_recv_batch() {
                Err(e) if e.kind() == io::ErrorKind::WouldBlock => continue,
                res => return res,
            }
        }
    }

    #[cfg(target_os = "linux")]
    fn try_recv_batch(&mut self) -> io::Result<usize> {
        recv_batch_linux(&self.sock, &mut self.bufs, &mut self.lens, &mut self.addrs)
    }

    #[cfg(not(target_os = "linux"))]
    fn try_recv_batch(&mut self) -> io::Result<usize> {
        match self.sock.try_recv_from(&mut self.bufs[0]) {
            Ok((n, addr)) => {
                self.lens[0] = n;
                self.addrs[0] = addr;
                Ok(1)
            }
            Err(e) => Err(e),
        }
    }

    /// Payload + source of datagram `i` (valid for `i` below the last
    /// [`UdpBatchReader::recv`] count).
    pub(crate) fn packet(&self, i: usize) -> (&[u8], SocketAddr) {
        (&self.bufs[i][..self.lens[i]], self.addrs[i])
    }
}

/// Batched UDP outbound writer.
///
/// Uses `sendmmsg` on Linux to emit up to [`UDP_BATCH`] datagrams per syscall,
/// amortizing kernel context switch overhead during DNS bursts and high-PPS traffic.
/// Falls back to single `try_send_to` calls on non-Linux platforms.
pub(crate) struct UdpBatchWriter {
    sock: Arc<tokio::net::UdpSocket>,
    queue: Vec<(Vec<u8>, SocketAddr)>,
}

impl UdpBatchWriter {
    pub(crate) fn new(sock: Arc<tokio::net::UdpSocket>) -> Self {
        Self {
            sock,
            queue: Vec::with_capacity(UDP_BATCH),
        }
    }

    /// Appends a datagram to the batch. Flushes automatically when reaching [`UDP_BATCH`].
    pub(crate) async fn push(&mut self, payload: &[u8], addr: SocketAddr) -> io::Result<()> {
        let mut buf = crate::mux_writer::acquire_encode_buf();
        buf.extend_from_slice(payload);
        self.queue.push((buf, addr));
        if self.queue.len() >= UDP_BATCH {
            self.flush().await?;
        }
        Ok(())
    }

    /// Flushes all pending datagrams in the batch.
    pub(crate) async fn flush(&mut self) -> io::Result<usize> {
        if self.queue.is_empty() {
            return Ok(0);
        }
        let mut total = 0;
        let dummy_addr = SocketAddr::from(([0, 0, 0, 0], 0));
        while !self.queue.is_empty() {
            self.sock.writable().await?;
            let take = self.queue.len().min(UDP_BATCH);
            let mut slice: [(&[u8], SocketAddr); UDP_BATCH] = [(&[], dummy_addr); UDP_BATCH];
            for i in 0..take {
                slice[i] = (self.queue[i].0.as_slice(), self.queue[i].1);
            }

            #[cfg(target_os = "linux")]
            match send_batch_linux(&self.sock, &slice[..take]) {
                Ok(n) if n > 0 => {
                    for (buf, _) in self.queue.drain(..n) {
                        crate::mux_writer::recycle_encode_buf(buf);
                    }
                    total += n;
                }
                Ok(_) => break,
                Err(ref e) if e.kind() == io::ErrorKind::WouldBlock => continue,
                Err(e) => return Err(e),
            }

            #[cfg(not(target_os = "linux"))]
            match send_batch_fallback(&self.sock, &slice[..take]) {
                Ok(n) if n > 0 => {
                    for (buf, _) in self.queue.drain(..n) {
                        crate::mux_writer::recycle_encode_buf(buf);
                    }
                    total += n;
                }
                Ok(_) => break,
                Err(ref e) if e.kind() == io::ErrorKind::WouldBlock => continue,
                Err(e) => return Err(e),
            }
        }
        Ok(total)
    }

    /// Immediately sends a datagram, flushing any previously queued datagrams in the same syscall.
    pub(crate) async fn send(&mut self, payload: &[u8], addr: SocketAddr) -> io::Result<()> {
        if self.queue.is_empty() {
            match self.sock.try_send_to(payload, addr) {
                Ok(_) => return Ok(()),
                Err(ref e) if e.kind() == io::ErrorKind::WouldBlock => {}
                Err(e) => return Err(e),
            }
        }
        self.push(payload, addr).await?;
        self.flush().await?;
        Ok(())
    }
}

impl Drop for UdpBatchWriter {
    fn drop(&mut self) {
        for (buf, _) in self.queue.drain(..) {
            crate::mux_writer::recycle_encode_buf(buf);
        }
    }
}

#[cfg(target_os = "linux")]
fn recv_batch_linux(
    sock: &tokio::net::UdpSocket,
    bufs: &mut [Vec<u8>],
    lens: &mut [usize],
    addrs: &mut [SocketAddr],
) -> io::Result<usize> {
    use std::os::fd::AsRawFd;

    let fd = sock.as_raw_fd();
    debug_assert_eq!(bufs.len(), UDP_BATCH);

    let mut iovs = [libc::iovec {
        iov_base: std::ptr::null_mut(),
        iov_len: 0,
    }; UDP_BATCH];
    // SAFETY: all-zero is a valid initializer for these structs; every
    // field the kernel reads is set below before the syscall.
    let mut msgs: [libc::mmsghdr; UDP_BATCH] = unsafe { std::mem::zeroed() };
    let mut names: [libc::sockaddr_storage; UDP_BATCH] = unsafe { std::mem::zeroed() };

    for i in 0..UDP_BATCH {
        iovs[i].iov_base = bufs[i].as_mut_ptr() as *mut libc::c_void;
        iovs[i].iov_len = UDP_BUF_SIZE;
        msgs[i].msg_hdr.msg_name = &mut names[i] as *mut _ as *mut libc::c_void;
        msgs[i].msg_hdr.msg_namelen =
            std::mem::size_of::<libc::sockaddr_storage>() as libc::socklen_t;
        msgs[i].msg_hdr.msg_iov = &mut iovs[i];
        msgs[i].msg_hdr.msg_iovlen = 1;
    }

    // SAFETY: fd is a live non-blocking UDP socket owned by `sock`
    // (outlives this call); all pointers target live stack/heap memory
    // with correct lengths; flags/timeout unused.
    let ret = unsafe {
        libc::recvmmsg(
            fd,
            msgs.as_mut_ptr(),
            UDP_BATCH as _,
            0,
            std::ptr::null_mut(),
        )
    };
    if ret < 0 {
        return Err(io::Error::last_os_error());
    }
    for i in 0..ret as usize {
        lens[i] = msgs[i].msg_len as usize;
        addrs[i] = sockaddr_to_std(&names[i], msgs[i].msg_hdr.msg_namelen)?;
    }
    Ok(ret as usize)
}

#[cfg(target_os = "linux")]
fn sockaddr_to_std(ss: &libc::sockaddr_storage, len: libc::socklen_t) -> io::Result<SocketAddr> {
    let len = len as usize;
    match ss.ss_family as i32 {
        libc::AF_INET if len >= std::mem::size_of::<libc::sockaddr_in>() => {
            // SAFETY: sockaddr_storage is suitably aligned/sized for any
            // address family; length was validated above.
            let a: &libc::sockaddr_in = unsafe { &*(ss as *const _ as *const _) };
            let raw = a.sin_addr.s_addr.to_ne_bytes();
            let ip = std::net::Ipv4Addr::new(raw[0], raw[1], raw[2], raw[3]);
            Ok(SocketAddr::new(ip.into(), u16::from_be(a.sin_port)))
        }
        libc::AF_INET6 if len >= std::mem::size_of::<libc::sockaddr_in6>() => {
            let a: &libc::sockaddr_in6 = unsafe { &*(ss as *const _ as *const _) };
            let ip = std::net::Ipv6Addr::from(a.sin6_addr.s6_addr);
            Ok(SocketAddr::new(ip.into(), u16::from_be(a.sin6_port)))
        }
        _ => Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "recvmmsg returned unsupported address family",
        )),
    }
}

#[cfg(target_os = "linux")]
fn std_to_sockaddr(addr: SocketAddr) -> (libc::sockaddr_storage, libc::socklen_t) {
    let mut ss: libc::sockaddr_storage = unsafe { std::mem::zeroed() };
    match addr {
        SocketAddr::V4(v4) => {
            let sin = libc::sockaddr_in {
                sin_family: libc::AF_INET as libc::sa_family_t,
                sin_port: v4.port().to_be(),
                sin_addr: libc::in_addr {
                    s_addr: u32::from_ne_bytes(v4.ip().octets()),
                },
                sin_zero: [0; 8],
            };
            unsafe {
                std::ptr::copy_nonoverlapping(
                    &sin as *const _ as *const u8,
                    &mut ss as *mut _ as *mut u8,
                    std::mem::size_of::<libc::sockaddr_in>(),
                );
            }
            (ss, std::mem::size_of::<libc::sockaddr_in>() as libc::socklen_t)
        }
        SocketAddr::V6(v6) => {
            let sin6 = libc::sockaddr_in6 {
                sin6_family: libc::AF_INET6 as libc::sa_family_t,
                sin6_port: v6.port().to_be(),
                sin6_flowinfo: v6.flowinfo(),
                sin6_addr: libc::in6_addr {
                    s6_addr: v6.ip().octets(),
                },
                sin6_scope_id: v6.scope_id(),
            };
            unsafe {
                std::ptr::copy_nonoverlapping(
                    &sin6 as *const _ as *const u8,
                    &mut ss as *mut _ as *mut u8,
                    std::mem::size_of::<libc::sockaddr_in6>(),
                );
            }
            (ss, std::mem::size_of::<libc::sockaddr_in6>() as libc::socklen_t)
        }
    }
}

#[cfg(target_os = "linux")]
pub(crate) fn send_batch_linux(
    sock: &tokio::net::UdpSocket,
    pkts: &[(&[u8], SocketAddr)],
) -> io::Result<usize> {
    use std::os::fd::AsRawFd;

    if pkts.is_empty() {
        return Ok(0);
    }
    let count = pkts.len().min(UDP_BATCH);
    let fd = sock.as_raw_fd();

    let mut iovs = [libc::iovec {
        iov_base: std::ptr::null_mut(),
        iov_len: 0,
    }; UDP_BATCH];
    let mut msgs: [libc::mmsghdr; UDP_BATCH] = unsafe { std::mem::zeroed() };
    let mut addrs: [libc::sockaddr_storage; UDP_BATCH] = unsafe { std::mem::zeroed() };

    for i in 0..count {
        let (payload, dest) = pkts[i];
        let (ss, ss_len) = std_to_sockaddr(dest);
        addrs[i] = ss;
        iovs[i].iov_base = payload.as_ptr() as *mut libc::c_void;
        iovs[i].iov_len = payload.len();

        msgs[i].msg_hdr.msg_name = &mut addrs[i] as *mut _ as *mut libc::c_void;
        msgs[i].msg_hdr.msg_namelen = ss_len;
        msgs[i].msg_hdr.msg_iov = &mut iovs[i];
        msgs[i].msg_hdr.msg_iovlen = 1;
    }

    let ret = unsafe {
        libc::sendmmsg(
            fd,
            msgs.as_mut_ptr(),
            count as _,
            0,
        )
    };
    if ret < 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(ret as usize)
}

#[cfg(not(target_os = "linux"))]
pub(crate) fn send_batch_fallback(
    sock: &tokio::net::UdpSocket,
    pkts: &[(&[u8], SocketAddr)],
) -> io::Result<usize> {
    if pkts.is_empty() {
        return Ok(0);
    }
    let (payload, dest) = pkts[0];
    sock.try_send_to(payload, dest).map(|_| 1)
}

#[cfg(test)]
mod tests {
    use super::*;
    use tokio::net::UdpSocket;

    #[tokio::test]
    async fn batch_reader_delivers_in_order() {
        let rx = Arc::new(UdpSocket::bind("127.0.0.1:0").await.unwrap());
        let tx = UdpSocket::bind("127.0.0.1:0").await.unwrap();
        let tx_addr = tx.local_addr().unwrap();
        tx.connect(rx.local_addr().unwrap()).await.unwrap();
        const K: usize = 64;
        for i in 0..K {
            let msg = [((i >> 8) & 0xff) as u8, (i & 0xff) as u8, 0xAB, 0xCD];
            tx.send(&msg).await.unwrap();
        }
        let mut reader = UdpBatchReader::new(rx);
        let mut got = 0usize;
        while got < K {
            let n = reader.recv().await.unwrap();
            assert!((1..=UDP_BATCH).contains(&n), "batch size {n} out of range");
            // Verify in arrival order across batch boundaries.
            for i in 0..n {
                let (pkt, addr) = reader.packet(i);
                assert_eq!(addr, tx_addr);
                let want = got as u16;
                assert_eq!(pkt.len(), 4);
                assert_eq!(pkt[0], ((want >> 8) & 0xff) as u8);
                assert_eq!(pkt[1], (want & 0xff) as u8);
                got += 1;
            }
        }
        assert_eq!(got, K);
    }

    #[tokio::test]
    async fn batch_writer_delivers_in_order() {
        let rx = Arc::new(UdpSocket::bind("127.0.0.1:0").await.unwrap());
        let rx_addr = rx.local_addr().unwrap();
        let tx = Arc::new(UdpSocket::bind("127.0.0.1:0").await.unwrap());
        let mut writer = UdpBatchWriter::new(tx);
        const K: usize = 32;
        for i in 0..K {
            let msg = [((i >> 8) & 0xff) as u8, (i & 0xff) as u8, 0xEF, 0x12];
            writer.push(&msg, rx_addr).await.unwrap();
        }
        writer.flush().await.unwrap();

        let mut buf = [0u8; 128];
        for i in 0..K {
            let (n, _) = rx.recv_from(&mut buf).await.unwrap();
            assert_eq!(n, 4);
            let want = i as u16;
            assert_eq!(buf[0], ((want >> 8) & 0xff) as u8);
            assert_eq!(buf[1], (want & 0xff) as u8);
            assert_eq!(buf[2], 0xEF);
            assert_eq!(buf[3], 0x12);
        }
    }
}
} // mod udp_batch

pub mod udp_relay {
use crate::common::{
    configured_cipher, parse_ws_url_with_port, socks5_udp_associate_reply, try_cloudflare_edges,
};
use crate::dns;
use crate::proxy::{parse_authority_with_default, parse_socks5_udp_datagram, TargetAddr};
use crate::runtime::{
    apply_socket_options, check_target_policy, enforce_target_policy, RuntimeConfig,
};
use crate::udp_batch::{UdpBatchReader, UdpBatchWriter};
use crate::ws::{
    build_client_handshake_request, read_frame, read_frame_with_cipher, read_http_headers,
    validate_client_handshake_response, write_frame,
};
use anyhow::{anyhow, bail, Result};
use std::net::SocketAddr;
use std::sync::{Arc, Mutex as StdMutex};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{tcp::OwnedWriteHalf, TcpStream, UdpSocket};
use tokio::time::{timeout, Duration};

pub async fn handle_local_udp_proxy(
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
    let (authority, path) =
        parse_ws_url_with_port(&upstream, "UDP relay requires ws:// upstream")?;
    let actual_host = cfg.fakehost.clone().unwrap_or_else(|| authority.clone());
    let target =
        parse_authority_with_default(&authority, 80).map_err(|e| anyhow!(e.to_string()))?;
    let conn_timeout = Duration::from_secs(cfg.connection_timeout.max(1));
    let primary = dns::resolve_socket(&target.host, target.port).await?;
    let socket = match timeout(conn_timeout, TcpStream::connect(primary)).await {
        Ok(Ok(s)) => s,
        res => {
            if target.host.parse::<std::net::IpAddr>().is_ok() && cfg.fakehost.is_some() {
                let sni = cfg
                    .fakehost
                    .as_deref()
                    .map(|s| s.split(':').next().unwrap_or(s))
                    .unwrap();
                tracing::warn!("[WS] Primary upstream unreachable; trying Cloudflare fallback edges via fakehost: {res:?}");
                match try_cloudflare_edges(sni, primary.ip(), target.port, conn_timeout).await? {
                    Some(s) => s,
                    None => {
                        return Err(anyhow!(
                            "primary {primary} unreachable and all fallback edges failed"
                        ))
                    }
                }
            } else {
                return Err(anyhow!("upstream connection failed to {primary}"));
            }
        }
    };
    apply_socket_options(&socket, &cfg);
    let (mut rd, mut wr) = socket.into_split();
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
    let c = configured_cipher(&cfg.key);
    let mut hello = b"UDP\n".to_vec();
    c.apply(&mut hello);
    write_frame(&mut wr, &hello, 2, true).await?;
    let mut frame_buf = crate::mux_writer::acquire_encode_buf();
    let Some((opcode, mut ok)) = read_frame(
        &mut rd,
        Option::<&mut OwnedWriteHalf>::None,
        &mut frame_buf,
    )
    .await?
    else {
        crate::mux_writer::recycle_encode_buf(frame_buf);
        bail!("upstream closed during UDP handshake")
    };
    if opcode != 2 {
        crate::mux_writer::recycle_encode_buf(ok);
        crate::mux_writer::recycle_encode_buf(frame_buf);
        bail!("invalid UDP handshake response opcode")
    }
    c.apply(&mut ok);
    if ok != b"OK\n" {
        crate::mux_writer::recycle_encode_buf(ok);
        crate::mux_writer::recycle_encode_buf(frame_buf);
        bail!("upstream rejected UDP handshake")
    }
    crate::mux_writer::recycle_encode_buf(ok);
    let latest = Arc::new(StdMutex::new(None::<SocketAddr>));
    let block_local = cfg.block_local;
    let udp_send = udp.clone();
    let c_send = c.clone();
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
                if let Ok((target, _)) = parse_socks5_udp_datagram(pkt) {
                    if let Err(e) = check_target_policy(block_local, &target) {
                        tracing::debug!(error = %e, "WS UDP datagram dropped by target policy");
                        continue;
                    }
                }
                *latest_send.lock().unwrap_or_else(|e| e.into_inner()) = Some(peer);
                let mut packet = crate::mux_writer::acquire_encode_buf();
                packet.extend_from_slice(pkt);
                c_send.apply(&mut packet);
                crate::stats::add_bytes(packet.len() as i64, 0);
                let res = write_frame(&mut wr, &packet, 2, true).await;
                crate::mux_writer::recycle_encode_buf(packet);
                if res.is_err() {
                    return Ok::<(), anyhow::Error>(());
                }
            }
        }
        Ok::<(), anyhow::Error>(())
    });
    let mut dummy = [0u8; 1];
    let mut batch_writer = UdpBatchWriter::new(udp.clone());
    tokio::select! {
        _ = control.read(&mut dummy) => {}
        _ = async {
            loop {
                let Some((opcode, packet)) = read_frame_with_cipher(
                    &mut rd,
                    Option::<&mut OwnedWriteHalf>::None,
                    &mut frame_buf,
                    Some(&c),
                )
                .await?
                else {
                    break;
                };
                if opcode != 2 {
                    crate::mux_writer::recycle_encode_buf(packet);
                    continue;
                }
                let (target, _payload) = match parse_socks5_udp_datagram(&packet) {
                    Ok(res) => res,
                    Err(e) => {
                        crate::mux_writer::recycle_encode_buf(packet);
                        return Err(anyhow!(e.to_string()));
                    }
                };
                if enforce_target_policy(&cfg, &target).is_err() {
                    crate::mux_writer::recycle_encode_buf(packet);
                    continue;
                }
                let peer_opt = *latest.lock().unwrap_or_else(|e| e.into_inner());
                if let Some(peer) = peer_opt {
                    let n = packet.len();
                    let _ = batch_writer.send(&packet, peer).await;
                    crate::stats::add_bytes(0, n as i64);
                }
                crate::mux_writer::recycle_encode_buf(packet);
            }
            let _ = batch_writer.flush().await;
            Ok::<(), anyhow::Error>(())
        } => {}
    }
    crate::mux_writer::recycle_encode_buf(frame_buf);
    upload.abort();
    let _ = control.shutdown().await;
    Ok(())
}
} // mod udp_relay

pub mod ws {
//! RFC 6455 transport pieces used by the GoWay-compatible transport.
//!
//! The codec deliberately does not implement fragmentation: GoWay v1.8.4
//! accepts complete data frames and handles Ping/Pong/Close itself.

use anyhow::{anyhow, bail, Result};
use base64::{engine::general_purpose::STANDARD, Engine as _};
use rand::seq::SliceRandom;
use rand::{Rng, RngCore};
use sha1::{Digest, Sha1};
use std::cell::RefCell;
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};

pub const MAX_WS_FRAME_SIZE: usize = 12 * 1024 * 1024 + 64;
pub const MAX_HTTP_HEADER_SIZE: usize = 8192;
const WS_GUID: &[u8] = b"258EAFA5-E914-47DA-95CA-C5AB0DC85B11";
const BROWSER_ACCEPT_ENCODING: &str = "gzip, deflate, br, zstd";

/// Browser profile bundling UA, TLS cipher/curve preferences, and HTTP
/// headers that must plausibly match each other. Mirrors GoWay's
/// `browserProfiles` (Chrome 136 / Edge 136 / Firefox 138 / Android).
/// A random profile is picked per handshake, like GoWay's
/// `pickBrowserProfile`.
#[derive(Debug, Clone, Copy)]
pub struct BrowserProfile {
    pub ua: &'static str,
    pub accept_lang: &'static str,
    /// Chrome Client Hints; `None` for Firefox (which never sends sec-ch-ua).
    pub sec_ch_ua: Option<&'static str>,
    pub sec_ch_ua_mob: &'static str,
    pub sec_ch_ua_plat: &'static str,
    pub is_chromium: bool,
    /// Mobile marker (currently only asserted in tests; kept for parity
    /// with GoWay's profile table).
    #[allow(dead_code)]
    pub is_mobile: bool,
    /// Which TLS cipher ordering to use (mirrors GoWay's per-profile lists).
    pub tls_chromium_order: bool,
}

pub const BROWSER_PROFILES: [BrowserProfile; 7] = [
    BrowserProfile {
        ua: "Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/136.0.0.0 Safari/537.36",
        accept_lang: "en-US,en;q=0.9",
        sec_ch_ua: Some("\"Chromium\";v=\"136\", \"Google Chrome\";v=\"136\", \"Not.A/Brand\";v=\"99\""),
        sec_ch_ua_mob: "?0",
        sec_ch_ua_plat: "\"Windows\"",
        is_chromium: true,
        is_mobile: false,
        tls_chromium_order: true,
    },
    BrowserProfile {
        ua: "Mozilla/5.0 (Macintosh; Intel Mac OS X 10_15_7) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/136.0.0.0 Safari/537.36",
        accept_lang: "en-US,en;q=0.9",
        sec_ch_ua: Some("\"Chromium\";v=\"136\", \"Google Chrome\";v=\"136\", \"Not.A/Brand\";v=\"99\""),
        sec_ch_ua_mob: "?0",
        sec_ch_ua_plat: "\"macOS\"",
        is_chromium: true,
        is_mobile: false,
        tls_chromium_order: true,
    },
    BrowserProfile {
        ua: "Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/136.0.0.0 Safari/537.36",
        accept_lang: "zh-CN,zh;q=0.9,en;q=0.8",
        sec_ch_ua: Some("\"Chromium\";v=\"136\", \"Google Chrome\";v=\"136\", \"Not.A/Brand\";v=\"99\""),
        sec_ch_ua_mob: "?0",
        sec_ch_ua_plat: "\"Windows\"",
        is_chromium: true,
        is_mobile: false,
        tls_chromium_order: true,
    },
    BrowserProfile {
        ua: "Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/136.0.0.0 Safari/537.36 Edg/136.0.0.0",
        accept_lang: "en-US,en;q=0.9",
        sec_ch_ua: Some("\"Chromium\";v=\"136\", \"Microsoft Edge\";v=\"136\", \"Not.A/Brand\";v=\"99\""),
        sec_ch_ua_mob: "?0",
        sec_ch_ua_plat: "\"Windows\"",
        is_chromium: true,
        is_mobile: false,
        tls_chromium_order: true,
    },
    BrowserProfile {
        ua: "Mozilla/5.0 (Windows NT 10.0; Win64; x64; rv:138.0) Gecko/20100101 Firefox/138.0",
        accept_lang: "en-US,en;q=0.5",
        sec_ch_ua: None,
        sec_ch_ua_mob: "",
        sec_ch_ua_plat: "",
        is_chromium: false,
        is_mobile: false,
        tls_chromium_order: false,
    },
    BrowserProfile {
        ua: "Mozilla/5.0 (Macintosh; Intel Mac OS X 14.7; rv:138.0) Gecko/20100101 Firefox/138.0",
        accept_lang: "en-US,en;q=0.5",
        sec_ch_ua: None,
        sec_ch_ua_mob: "",
        sec_ch_ua_plat: "",
        is_chromium: false,
        is_mobile: false,
        tls_chromium_order: false,
    },
    BrowserProfile {
        ua: "Mozilla/5.0 (Linux; Android 14; Pixel 8 Pro) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/136.0.0.0 Mobile Safari/537.36",
        accept_lang: "en-US,en;q=0.9",
        sec_ch_ua: Some("\"Chromium\";v=\"136\", \"Google Chrome\";v=\"136\", \"Not.A/Brand\";v=\"99\""),
        sec_ch_ua_mob: "?1",
        sec_ch_ua_plat: "\"Android\"",
        is_chromium: true,
        is_mobile: true,
        tls_chromium_order: true,
    },
];

/// Picks a random browser profile for one handshake (GoWay parity:
/// GoWay draws independently for TLS and HTTP, so callers may draw here
/// and pass the result to both layers for a correlated identity).
pub fn pick_browser_profile() -> &'static BrowserProfile {
    &BROWSER_PROFILES[pick_browser_profile_index()]
}

/// Index-based draw backing [`pick_browser_profile`]; the TLS layer draws
/// its own index so each layer can order cipher suites consistently.
pub fn pick_browser_profile_index() -> usize {
    rand::thread_rng().gen_range(0..BROWSER_PROFILES.len())
}

thread_local! {
    static MASK_STATE: RefCell<u64> = const { RefCell::new(0) };
}

pub(crate) fn next_mask() -> [u8; 4] {
    MASK_STATE.with(|state| {
        let mut value = state.borrow_mut();
        if *value == 0 {
            let mut seed = [0u8; 8];
            rand::thread_rng().fill_bytes(&mut seed);
            *value = u64::from_le_bytes(seed);
            if *value == 0 {
                *value = 0x9e3779b97f4a7c15;
            }
        }
        let mut x = *value;
        x ^= x << 13;
        x ^= x >> 7;
        x ^= x << 17;
        *value = x;
        (x as u32).to_ne_bytes()
    })
}

pub fn compute_accept_key(challenge: &str) -> String {
    let mut h = Sha1::new();
    h.update(challenge.trim().as_bytes());
    h.update(WS_GUID);
    STANDARD.encode(h.finalize())
}

fn header_value<'a>(headers: &'a str, name: &str) -> Option<&'a str> {
    headers.lines().find_map(|line| {
        let (k, v) = line.split_once(':')?;
        if k.trim().eq_ignore_ascii_case(name) {
            Some(v.trim())
        } else {
            None
        }
    })
}

fn token_contains(value: &str, token: &str) -> bool {
    value
        .split(',')
        .any(|v| v.trim().eq_ignore_ascii_case(token))
}

pub async fn read_http_headers<R: AsyncRead + Unpin>(r: &mut R) -> Result<Vec<u8>> {
    read_http_headers_timeout(r, std::time::Duration::from_secs(60)).await
}

pub async fn read_http_headers_timeout<R: AsyncRead + Unpin>(
    r: &mut R,
    timeout_duration: std::time::Duration,
) -> Result<Vec<u8>> {
    let mut out = Vec::with_capacity(1024);
    let mut b = [0u8; 1];

    let read_loop = async {
        loop {
            match r.read_exact(&mut b).await {
                Ok(_) => {
                    out.push(b[0]);
                    let len = out.len();
                    if (len >= 4 && out[len - 4..] == *b"\r\n\r\n")
                        || (len >= 2 && out[len - 2..] == *b"\n\n")
                    {
                        return Ok(());
                    }
                    if len >= MAX_HTTP_HEADER_SIZE {
                        bail!("header too large");
                    }
                }
                Err(e) => {
                    return Err(anyhow::Error::from(e));
                }
            }
        }
    };

    match tokio::time::timeout(timeout_duration, read_loop).await {
        Ok(Ok(())) => Ok(out),
        Ok(Err(e)) => {
            if out.is_empty() {
                tracing::warn!(
                    "[WSS] HTTP handshake connection closed by peer; received 0 response bytes ({})",
                    e
                );
                bail!(
                    "WSS HTTP handshake connection closed by peer; received 0 response bytes ({})",
                    e
                );
            } else if e.to_string() == "header too large" {
                Err(e)
            } else {
                let partial = String::from_utf8_lossy(&out);
                tracing::warn!(
                    "[WSS] HTTP handshake connection closed by peer; partial response:\n{}",
                    partial
                );
                bail!(
                    "WSS HTTP handshake connection closed by peer; partial response:\n{}",
                    partial
                );
            }
        }
        Err(_elapsed) => {
            if out.is_empty() {
                tracing::warn!("[WSS] HTTP handshake timeout; received 0 response bytes");
                bail!("WSS HTTP handshake response timeout; received 0 response bytes");
            } else {
                let partial = String::from_utf8_lossy(&out);
                tracing::warn!("[WSS] HTTP handshake timeout; partial response:\n{}", partial);
                bail!(
                    "WSS HTTP handshake response timeout; partial response:\n{}",
                    partial
                );
            }
        }
    }
}

pub fn validate_server_handshake(request: &[u8]) -> Result<String> {
    if request.len() > MAX_HTTP_HEADER_SIZE {
        bail!("header too large");
    }
    let text = std::str::from_utf8(request).map_err(|_| anyhow!("invalid HTTP header"))?;
    if !(text.ends_with("\r\n\r\n") || text.ends_with("\n\n")) {
        bail!("incomplete websocket handshake");
    }
    let first = text
        .lines()
        .next()
        .ok_or_else(|| anyhow!("empty HTTP request"))?;
    let mut parts = first.split_whitespace();
    let method = parts.next().unwrap_or("");
    let _path = parts.next().unwrap_or("");
    let version = parts.next().unwrap_or("");
    if method != "GET" || !version.starts_with("HTTP/") {
        bail!("invalid websocket request line");
    }
    let upgrade =
        header_value(text, "Upgrade").ok_or_else(|| anyhow!("missing upgrade: websocket"))?;
    if !token_contains(upgrade, "websocket") {
        bail!("missing upgrade: websocket");
    }
    let connection =
        header_value(text, "Connection").ok_or_else(|| anyhow!("missing connection: upgrade"))?;
    if !token_contains(connection, "Upgrade") {
        bail!("missing connection: upgrade");
    }
    let key = header_value(text, "Sec-WebSocket-Key")
        .ok_or_else(|| anyhow!("missing sec-websocket-key"))?;
    if key.is_empty() {
        bail!("missing sec-websocket-key");
    }
    Ok(key.to_string())
}

pub fn build_server_handshake_response(key: &str) -> Vec<u8> {
    format!("HTTP/1.1 101 Switching Protocols\r\nUpgrade: websocket\r\nConnection: Upgrade\r\nSec-WebSocket-Accept: {}\r\n\r\n", compute_accept_key(key)).into_bytes()
}

pub fn build_client_handshake_request(
    host: &str,
    path: &str,
    origin: Option<&str>,
    sec_fetch_site: Option<&str>,
) -> (Vec<u8>, String) {
    build_client_handshake_request_with_profile(
        host,
        path,
        origin,
        sec_fetch_site,
        pick_browser_profile(),
    )
}

/// Builds the upgrade request with an explicit profile so callers can use
/// one identity for both TLS and HTTP (GoWay draws them independently;
/// correlating is strictly harder to fingerprint).
pub fn build_client_handshake_request_with_profile(
    host: &str,
    path: &str,
    origin: Option<&str>,
    sec_fetch_site: Option<&str>,
    profile: &BrowserProfile,
) -> (Vec<u8>, String) {
    let mut key_bytes = [0u8; 16];
    rand::thread_rng().fill_bytes(&mut key_bytes);
    let key = STANDARD.encode(key_bytes);
    let path = if path.is_empty() { "/" } else { path };
    let site = sec_fetch_site.unwrap_or("cross-site");
    // Fixed top, mirroring GoWay: Host / Connection / Upgrade stay first.
    let mut req = format!(
        "GET {path} HTTP/1.1\r\nHost: {host}\r\nConnection: Upgrade\r\nUpgrade: websocket\r\n"
    );
    // Shufflable middle section (GoWay Fisher-Yates shuffles these).
    let mut middle: Vec<String> = Vec::with_capacity(9);
    middle.push("Pragma: no-cache".to_string());
    middle.push("Cache-Control: no-cache".to_string());
    middle.push(format!("User-Agent: {}", profile.ua));
    middle.push(format!("Accept-Language: {}", profile.accept_lang));
    middle.push(format!("Accept-Encoding: {BROWSER_ACCEPT_ENCODING}"));
    if let Some(origin) = origin {
        middle.push(format!("Origin: {origin}"));
    }
    if profile.is_chromium {
        if let Some(sec_ch_ua) = profile.sec_ch_ua {
            middle.push(format!("sec-ch-ua: {sec_ch_ua}"));
            middle.push(format!("sec-ch-ua-mobile: {}", profile.sec_ch_ua_mob));
            middle.push(format!("sec-ch-ua-platform: {}", profile.sec_ch_ua_plat));
        }
    }
    middle.shuffle(&mut rand::thread_rng());
    for line in &middle {
        req.push_str(line);
        req.push_str("\r\n");
    }
    // Fixed bottom, mirroring GoWay.
    req.push_str(&format!("Sec-WebSocket-Version: 13\r\nSec-WebSocket-Key: {key}\r\nSec-Fetch-Dest: websocket\r\nSec-Fetch-Mode: websocket\r\nSec-Fetch-Site: {site}\r\n\r\n"));
    (req.into_bytes(), key)
}

pub fn redact_handshake_request(req: &[u8]) -> String {
    let s = String::from_utf8_lossy(req);
    let mut out = String::new();
    for line in s.lines() {
        if line.to_ascii_lowercase().starts_with("sec-websocket-key:") {
            out.push_str("Sec-WebSocket-Key: [REDACTED]\r\n");
        } else {
            out.push_str(line);
            out.push_str("\r\n");
        }
    }
    out
}

pub fn validate_client_handshake_response(response: &[u8], key: &str) -> Result<()> {
    if response.len() > MAX_HTTP_HEADER_SIZE {
        bail!("header too large");
    }
    let text = std::str::from_utf8(response).map_err(|_| anyhow!("invalid HTTP response"))?;
    let first = text
        .lines()
        .next()
        .ok_or_else(|| anyhow!("empty HTTP response"))?;
    let mut parts = first.splitn(3, ' ');
    let proto = parts.next().unwrap_or("");
    let status = parts.next().unwrap_or("");
    let reason = parts.next().unwrap_or("");
    if !proto.starts_with("HTTP/") {
        bail!("malformed websocket handshake response: {first}");
    }
    if status != "101" {
        tracing::warn!(
            "[WSS] HTTP response status not 101: HTTP {}\nFirst line: {}\nHeaders:\n{}",
            status,
            first,
            text.trim_end()
        );
        match status {
            "200" => {
                bail!("handshake failed: server returned 200 OK instead of 101 Switching Protocols (upstream may not support WebSocket)")
            }
            "301" | "302" | "307" | "308" => bail!("handshake failed: HTTP {status} redirect"),
            "400" => bail!("handshake failed: HTTP 400 Bad Request"),
            "403" => bail!("handshake failed: HTTP 403 Forbidden"),
            "404" => bail!("handshake failed: HTTP 404 Not Found"),
            "502" => bail!("handshake failed: HTTP 502 Bad Gateway"),
            "503" => bail!("handshake failed: HTTP 503 Service Unavailable"),
            "504" => bail!("handshake failed: HTTP 504 Gateway Timeout"),
            _ => bail!("handshake failed: unexpected HTTP status {status} ({reason})"),
        }
    }
    if !token_contains(
        header_value(text, "Upgrade").ok_or_else(|| anyhow!("missing Upgrade header"))?,
        "websocket",
    ) {
        bail!("handshake failed: missing Upgrade: websocket");
    }
    if !token_contains(
        header_value(text, "Connection").ok_or_else(|| anyhow!("missing Connection: Upgrade"))?,
        "Upgrade",
    ) {
        bail!("handshake failed: missing Connection: Upgrade");
    }
    let accept = header_value(text, "Sec-WebSocket-Accept")
        .ok_or_else(|| anyhow!("handshake failed: missing Sec-WebSocket-Accept"))?;
    if accept != compute_accept_key(key) {
        bail!("handshake failed: invalid Sec-WebSocket-Accept");
    }
    Ok(())
}

/// Writes a WebSocket frame header for `payload_len` bytes into `header`,
/// returning the header length (2/4/10). Shared by [`encode_ws_frame`] and
/// the fused MUX encoder so the layout logic lives in one place.
pub(crate) fn ws_header_into(
    header: &mut [u8; 14],
    payload_len: usize,
    opcode: u8,
    masked: bool,
) -> usize {
    let header_len = if payload_len <= 125 {
        2
    } else if payload_len <= u16::MAX as usize {
        4
    } else {
        10
    };
    header[0] = 0x80 | (opcode & 0x0f);
    let mask_bit = if masked { 0x80 } else { 0 };
    match header_len {
        2 => header[1] = mask_bit | payload_len as u8,
        4 => {
            header[1] = mask_bit | 126;
            header[2..4].copy_from_slice(&(payload_len as u16).to_be_bytes());
        }
        10 => {
            header[1] = mask_bit | 127;
            header[2..10].copy_from_slice(&(payload_len as u64).to_be_bytes());
        }
        _ => unreachable!(),
    }
    header_len
}

fn check_frame_args(payload_len: usize, opcode: u8) -> Result<()> {
    if payload_len > MAX_WS_FRAME_SIZE {
        return Err(anyhow!("websocket frame too large"));
    }
    if opcode >= 0x8 && payload_len > 125 {
        return Err(anyhow!("control frame payload exceeds 125 bytes"));
    }
    Ok(())
}

/// Applies or removes WebSocket 4-byte XOR masking in place.
/// Uses runtime-detected AVX2 (256-bit) or SSE2 (128-bit) unrolled vector loops.
pub(crate) fn apply_ws_mask(buf: &mut [u8], key: [u8; 4]) {
    if buf.is_empty() {
        return;
    }
    #[cfg(target_arch = "x86_64")]
    {
        if crate::crypto::has_avx2() {
            unsafe {
                apply_ws_mask_avx2(buf, key);
            }
            return;
        } else {
            unsafe {
                apply_ws_mask_sse2(buf, key);
            }
            return;
        }
    }
    #[cfg(target_arch = "aarch64")]
    {
        unsafe {
            apply_ws_mask_neon(buf, key);
        }
        return;
    }
    #[cfg(not(any(target_arch = "x86_64", target_arch = "aarch64")))]
    {
        apply_ws_mask_fallback(buf, key);
    }
}

#[cfg(target_arch = "x86_64")]
#[target_feature(enable = "avx2")]
unsafe fn apply_ws_mask_avx2(buf: &mut [u8], key: [u8; 4]) {
    use std::arch::x86_64::*;
    let len = buf.len();
    let k32 = u32::from_ne_bytes(key) as i32;
    let k_vec = _mm256_set1_epi32(k32);
    let mut i = 0;
    while i + 128 <= len {
        let d0 = _mm256_loadu_si256(buf.as_ptr().add(i) as *const __m256i);
        let d1 = _mm256_loadu_si256(buf.as_ptr().add(i + 32) as *const __m256i);
        let d2 = _mm256_loadu_si256(buf.as_ptr().add(i + 64) as *const __m256i);
        let d3 = _mm256_loadu_si256(buf.as_ptr().add(i + 96) as *const __m256i);

        _mm256_storeu_si256(buf.as_mut_ptr().add(i) as *mut __m256i, _mm256_xor_si256(d0, k_vec));
        _mm256_storeu_si256(buf.as_mut_ptr().add(i + 32) as *mut __m256i, _mm256_xor_si256(d1, k_vec));
        _mm256_storeu_si256(buf.as_mut_ptr().add(i + 64) as *mut __m256i, _mm256_xor_si256(d2, k_vec));
        _mm256_storeu_si256(buf.as_mut_ptr().add(i + 96) as *mut __m256i, _mm256_xor_si256(d3, k_vec));
        i += 128;
    }
    while i + 32 <= len {
        let d = _mm256_loadu_si256(buf.as_ptr().add(i) as *const __m256i);
        _mm256_storeu_si256(buf.as_mut_ptr().add(i) as *mut __m256i, _mm256_xor_si256(d, k_vec));
        i += 32;
    }
    let k_sse = _mm_set1_epi32(k32);
    while i + 16 <= len {
        let d = _mm_loadu_si128(buf.as_ptr().add(i) as *const __m128i);
        _mm_storeu_si128(buf.as_mut_ptr().add(i) as *mut __m128i, _mm_xor_si128(d, k_sse));
        i += 16;
    }
    let k64 = (u32::from_ne_bytes(key) as u64) | ((u32::from_ne_bytes(key) as u64) << 32);
    while i + 8 <= len {
        let d = (buf.as_ptr().add(i) as *const u64).read_unaligned();
        (buf.as_mut_ptr().add(i) as *mut u64).write_unaligned(d ^ k64);
        i += 8;
    }
    while i < len {
        *buf.get_unchecked_mut(i) ^= key[i & 3];
        i += 1;
    }
}

#[cfg(target_arch = "x86_64")]
#[target_feature(enable = "sse2")]
unsafe fn apply_ws_mask_sse2(buf: &mut [u8], key: [u8; 4]) {
    use std::arch::x86_64::*;
    let len = buf.len();
    let k32 = u32::from_ne_bytes(key) as i32;
    let k_sse = _mm_set1_epi32(k32);
    let mut i = 0;
    while i + 64 <= len {
        let d0 = _mm_loadu_si128(buf.as_ptr().add(i) as *const __m128i);
        let d1 = _mm_loadu_si128(buf.as_ptr().add(i + 16) as *const __m128i);
        let d2 = _mm_loadu_si128(buf.as_ptr().add(i + 32) as *const __m128i);
        let d3 = _mm_loadu_si128(buf.as_ptr().add(i + 48) as *const __m128i);

        _mm_storeu_si128(buf.as_mut_ptr().add(i) as *mut __m128i, _mm_xor_si128(d0, k_sse));
        _mm_storeu_si128(buf.as_mut_ptr().add(i + 16) as *mut __m128i, _mm_xor_si128(d1, k_sse));
        _mm_storeu_si128(buf.as_mut_ptr().add(i + 32) as *mut __m128i, _mm_xor_si128(d2, k_sse));
        _mm_storeu_si128(buf.as_mut_ptr().add(i + 48) as *mut __m128i, _mm_xor_si128(d3, k_sse));
        i += 64;
    }
    while i + 16 <= len {
        let d = _mm_loadu_si128(buf.as_ptr().add(i) as *const __m128i);
        _mm_storeu_si128(buf.as_mut_ptr().add(i) as *mut __m128i, _mm_xor_si128(d, k_sse));
        i += 16;
    }
    let k64 = (u32::from_ne_bytes(key) as u64) | ((u32::from_ne_bytes(key) as u64) << 32);
    while i + 8 <= len {
        let d = (buf.as_ptr().add(i) as *const u64).read_unaligned();
        (buf.as_mut_ptr().add(i) as *mut u64).write_unaligned(d ^ k64);
        i += 8;
    }
    while i < len {
        *buf.get_unchecked_mut(i) ^= key[i & 3];
        i += 1;
    }
}

#[cfg(target_arch = "aarch64")]
unsafe fn apply_ws_mask_neon(buf: &mut [u8], key: [u8; 4]) {
    use std::arch::aarch64::*;
    let len = buf.len();
    let mut key_arr = [0u8; 16];
    key_arr[0..4].copy_from_slice(&key);
    key_arr[4..8].copy_from_slice(&key);
    key_arr[8..12].copy_from_slice(&key);
    key_arr[12..16].copy_from_slice(&key);
    let mask_vec = vld1q_u8(key_arr.as_ptr());

    let mut i = 0;
    while i + 64 <= len {
        let d0 = vld1q_u8(buf.as_ptr().add(i));
        let d1 = vld1q_u8(buf.as_ptr().add(i + 16));
        let d2 = vld1q_u8(buf.as_ptr().add(i + 32));
        let d3 = vld1q_u8(buf.as_ptr().add(i + 48));

        vst1q_u8(buf.as_mut_ptr().add(i), veorq_u8(d0, mask_vec));
        vst1q_u8(buf.as_mut_ptr().add(i + 16), veorq_u8(d1, mask_vec));
        vst1q_u8(buf.as_mut_ptr().add(i + 32), veorq_u8(d2, mask_vec));
        vst1q_u8(buf.as_mut_ptr().add(i + 48), veorq_u8(d3, mask_vec));
        i += 64;
    }
    while i + 16 <= len {
        let d = vld1q_u8(buf.as_ptr().add(i));
        vst1q_u8(buf.as_mut_ptr().add(i), veorq_u8(d, mask_vec));
        i += 16;
    }
    let mask64 = u64::from_ne_bytes([
        key[0], key[1], key[2], key[3], key[0], key[1], key[2], key[3],
    ]);
    while i + 8 <= len {
        let p = buf.as_mut_ptr().add(i) as *mut u64;
        let v = p.read_unaligned();
        p.write_unaligned(v ^ mask64);
        i += 8;
    }
    while i < len {
        *buf.get_unchecked_mut(i) ^= key[i & 3];
        i += 1;
    }
}

pub(crate) fn apply_ws_mask_fallback(buf: &mut [u8], key: [u8; 4]) {
    let mut mask32 = [0u8; 32];
    for i in 0..8 {
        mask32[i * 4..i * 4 + 4].copy_from_slice(&key);
    }
    let (chunks32, tail32) = buf.as_chunks_mut::<32>();
    let chunks32_len = chunks32.len();
    for chunk in chunks32 {
        for b in 0..32 {
            chunk[b] ^= mask32[b];
        }
    }
    let (words, tail) = tail32.as_chunks_mut::<8>();
    let words_len = words.len();
    let mask64 = u64::from_ne_bytes(mask32[..8].try_into().unwrap());
    for chunk in words {
        let w = u64::from_ne_bytes(*chunk);
        *chunk = (w ^ mask64).to_ne_bytes();
    }
    let start = chunks32_len * 32 + words_len * 8;
    for (j, b) in tail.iter_mut().enumerate() {
        *b ^= key[(start + j) & 3];
    }
}

/// Encodes one complete WebSocket frame (header + optional mask + masked
/// payload) into an owned byte buffer. The masking PRNG call happens here,
/// on the caller's task, so a dedicated writer task can flush pre-encoded
/// frames without touching thread-local RNG state.
pub fn encode_ws_frame(payload: &[u8], opcode: u8, masked: bool) -> Result<Vec<u8>> {
    check_frame_args(payload.len(), opcode)?;

    let mut header = [0u8; 14];
    let header_len = ws_header_into(&mut header, payload.len(), opcode, masked);

    let total = header_len
        + if masked {
            4 + payload.len()
        } else {
            payload.len()
        };
    let mut frame = crate::mux_writer::acquire_encode_buf();
    frame.reserve(total);
    frame.extend_from_slice(&header[..header_len]);
    if !masked {
        frame.extend_from_slice(payload);
        return Ok(frame);
    }
    let key = next_mask();
    frame.extend_from_slice(&key);
    frame.extend_from_slice(payload);
    apply_ws_mask(&mut frame[header_len + 4..], key);
    Ok(frame)
}

pub async fn write_frame<W: AsyncWrite + Unpin>(
    w: &mut W,
    payload: &[u8],
    opcode: u8,
    mask: bool,
) -> Result<()> {
    let frame = encode_ws_frame(payload, opcode, mask)?;
    let res = w.write_all(&frame).await;
    crate::mux_writer::recycle_encode_buf(frame);
    res?;
    Ok(())
}

/// Writes a frame borrowing its payload from reusable scratch: masks the
/// scratch region in place and emits with a single vectored write, with
/// zero allocation. The caller must fully overwrite the region on the next
/// read (plain `read` into `[..]` satisfies this); only the used prefix is
/// ever consumed downstream.
pub async fn write_frame_borrowed<W: AsyncWrite + Unpin>(
    w: &mut W,
    scratch: &mut [u8],
    opcode: u8,
    masked: bool,
) -> Result<()> {
    check_frame_args(scratch.len(), opcode)?;
    let mut header = [0u8; 14];
    let header_len = ws_header_into(&mut header, scratch.len(), opcode, masked);
    let key_opt = if masked {
        let key = next_mask();
        apply_ws_mask(scratch, key);
        Some(key)
    } else {
        None
    };
    write_frame_parts_vectored(w, &header, header_len, key_opt, scratch).await
}

async fn write_frame_parts_vectored<W: AsyncWrite + Unpin>(
    w: &mut W,
    header: &[u8; 14],
    header_len: usize,
    key: Option<[u8; 4]>,
    payload: &[u8],
) -> Result<()> {
    use std::future::poll_fn;
    use std::io::IoSlice;
    use std::pin::Pin;
    // (part, off) progress across partial vectored writes, where the parts
    // are [header][key][payload]. Empty leading parts are skipped so the
    // slice list is never all-empty before completion.
    let key_slice: &[u8] = key.as_ref().map(|k| &k[..]).unwrap_or(&[]);
    let key_len = key_slice.len();
    let lens = [header_len, key_len, payload.len()];
    let mut part = 0u8;
    let mut off = 0usize;
    while part < 3 {
        while part < 3 && lens[part as usize] == off {
            part += 1;
            off = 0;
        }
        if part >= 3 {
            break;
        }
        let mut slices: [IoSlice<'_>; 3] = [
            IoSlice::new(&[]),
            IoSlice::new(&[]),
            IoSlice::new(&[]),
        ];
        let count = match part {
            0 => {
                slices[0] = IoSlice::new(&header[off..header_len]);
                if !key_slice.is_empty() {
                    slices[1] = IoSlice::new(key_slice);
                    slices[2] = IoSlice::new(payload);
                    3
                } else {
                    slices[1] = IoSlice::new(payload);
                    2
                }
            }
            1 => {
                slices[0] = IoSlice::new(&key_slice[off..]);
                slices[1] = IoSlice::new(payload);
                2
            }
            _ => {
                slices[0] = IoSlice::new(&payload[off..]);
                1
            }
        };
        let n = poll_fn(|cx| Pin::new(&mut *w).poll_write_vectored(cx, &slices[..count])).await?;
        if n == 0 {
            return Err(anyhow!("vectored frame write returned zero"));
        }
        let mut remaining = n;
        while remaining > 0 && part < 3 {
            let available = lens[part as usize] - off;
            if remaining < available {
                off += remaining;
                remaining = 0;
            } else {
                remaining -= available;
                part += 1;
                off = 0;
            }
        }
    }
    Ok(())
}

pub async fn read_frame<R, W>(
    r: &mut R,
    reply: Option<&mut W>,
    buf: &mut Vec<u8>,
) -> Result<Option<(u8, Vec<u8>)>>
where
    R: AsyncRead + Unpin,
    W: AsyncWrite + Unpin,
{
    read_frame_with_cipher(r, reply, buf, None).await
}

// `clippy::uninit_vec` is allowed here: the `reserve` + `set_len` below is
// intentional — `read_exact` fully initializes `[start, start+chunk)` before
// any of it is observed, and the buffer is truncated back on error (see the
// SAFETY comment at the `set_len` site). `resize(.., 0)` was measured ~7% CPU
// in pprof 2026-09-23, so the zero-fill is deliberately skipped.
#[allow(clippy::uninit_vec)]
pub async fn read_frame_with_cipher<R, W>(
    r: &mut R,
    mut reply: Option<&mut W>,
    buf: &mut Vec<u8>,
    cipher: Option<&crate::crypto::XorCipher>,
) -> Result<Option<(u8, Vec<u8>)>>
where
    R: AsyncRead + Unpin,
    W: AsyncWrite + Unpin,
{
    loop {
        // Header batched into at most 2 reads: the 2-byte base, then
        // extended-length + mask key in one go.
        let mut head = [0u8; 2];
        match r.read(&mut head).await {
            Ok(0) => return Ok(None),
            Ok(1) => {
                r.read_exact(&mut head[1..2]).await?;
            }
            Ok(_) => {}
            Err(e) => return Err(e.into()),
        }
        let b0 = head[0];
        let b1 = head[1];
        let fin = b0 & 0x80 != 0;
        let opcode = b0 & 0x0f;
        let masked = b1 & 0x80 != 0;
        let len_marker = b1 & 0x7f;
        let ext_len = match len_marker {
            126 => 2,
            127 => 8,
            _ => 0,
        };
        if opcode >= 0x8 {
            if !fin || len_marker > 125 {
                return Err(anyhow!("invalid websocket control frame"));
            }
        } else if opcode == 0 || !fin || (opcode != 1 && opcode != 2) {
            return Err(anyhow!("unsupported or fragmented websocket frame"));
        }

        // Match ENCODE_POOL_MAX_CAP (80 KiB) so a full 64 KiB MUX frame
        // (65535 + 7 = 65542 bytes, or up to 66942 bytes with obfs) is read
        // in a single `read_exact` call and never triggers `grow_amortized`
        // to 128 KiB (which would exceed the pool cap and force reallocs).
        const READ_SEGMENT: usize = 80 * 1024;
        buf.clear();

        let mut key = [0u8; 4];
        if ext_len == 0 && masked {
            // Fast path for short masked frames (<= 125 bytes: SYN, WINDOW,
            // FIN, RST, VERSION, Ping/Pong, and small DATA): read the 4-byte
            // mask key and payload together in a single read_exact syscall.
            let total = len_marker as usize;
            let mut small = [0u8; 4 + 125];
            if r.read_exact(&mut small[..4 + total]).await.is_err() {
                return Err(anyhow!("websocket payload truncated"));
            }
            key.copy_from_slice(&small[..4]);
            buf.extend_from_slice(&small[4..4 + total]);
        } else {
            let mut rest = [0u8; 12];
            let rest_len = ext_len + if masked { 4 } else { 0 };
            if rest_len > 0 {
                r.read_exact(&mut rest[..rest_len]).await?;
            }
            let mut len = len_marker as u64;
            if ext_len == 2 {
                len = u16::from_be_bytes(rest[..2].try_into().unwrap()) as u64;
            } else if ext_len == 8 {
                len = u64::from_be_bytes(rest[..8].try_into().unwrap());
            }
            if len > MAX_WS_FRAME_SIZE as u64 {
                return Err(anyhow!("frame too large"));
            }
            if masked {
                key.copy_from_slice(&rest[ext_len..ext_len + 4]);
            }
            let total = len as usize;
            if total > buf.capacity() {
                if total > 4096 && total <= READ_SEGMENT {
                    buf.reserve_exact(READ_SEGMENT);
                } else {
                    buf.reserve(total.min(READ_SEGMENT));
                }
            }
            let mut read: usize = 0;
            while read < total {
                let chunk = (total - read).min(READ_SEGMENT);
                let start = buf.len();
                if buf.capacity() - start < chunk {
                    buf.reserve(chunk);
                }
                // SAFETY: `set_len` exposes `[start, start+chunk)`, which the
                // `read_exact` below fully initializes before any of it is
                // observed; on error we `truncate` back to `start` first.
                unsafe { buf.set_len(start + chunk) };
                if r.read_exact(&mut buf[start..start + chunk]).await.is_err() {
                    buf.truncate(start);
                    return Err(anyhow!("websocket payload truncated"));
                }
                read += chunk;
            }
        }
        match opcode {
            1 | 2 => {
                match (masked, cipher) {
                    (true, Some(c)) if c.is_enabled() => {
                        crate::crypto::apply_fused_xor(buf, c.keystream(), key);
                    }
                    (true, _) => {
                        apply_ws_mask(buf, key);
                    }
                    (false, Some(c)) => {
                        c.apply(buf);
                    }
                    (false, None) => {}
                }
                let owned = std::mem::take(buf);
                *buf = crate::mux_writer::acquire_encode_buf();
                return Ok(Some((opcode, owned)));
            }
            8 => return Ok(None),
            9 => {
                if masked {
                    apply_ws_mask(buf, key);
                }
                if let Some(w) = reply.as_deref_mut() {
                    write_frame(w, buf, 0xA, !masked).await?;
                }
            }
            10 => {}
            // Reserved opcodes (0x3-0x7, 0xB-0xF): a buggy or hostile peer
            // must cause an error, never a panic (cf. obfs desync probe).
            _ => return Err(anyhow!("reserved websocket opcode {}", opcode)),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tokio::io::duplex;

    #[test]
    fn rfc6455_accept_key_vector() {
        assert_eq!(
            compute_accept_key("dGhlIHNhbXBsZSBub25jZQ=="),
            "s3pPLMBiTxaQ9kYGzzhZRbK+xOo="
        );
    }

    #[test]
    fn handshake_request_and_response_validate() {
        let (request, key) = build_client_handshake_request(
            "example.com",
            "/ws",
            Some("https://example.com"),
            Some("same-origin"),
        );
        assert_eq!(validate_server_handshake(&request).unwrap(), key);
        validate_client_handshake_response(&build_server_handshake_response(&key), &key).unwrap();
    }

    #[test]
    fn browser_headers_present() {
        // Deterministic profile: Chromium headers must be present (order varies).
        let profile = &BROWSER_PROFILES[0];
        let (request, _) = build_client_handshake_request_with_profile(
            "example.com",
            "/ws",
            Some("https://example.com"),
            Some("same-origin"),
            profile,
        );
        let text = std::str::from_utf8(&request).unwrap();
        for header in [
            "Pragma: no-cache",
            "Cache-Control: no-cache",
            "User-Agent:",
            "Accept-Language:",
            "Accept-Encoding:",
            "sec-ch-ua:",
            "sec-ch-ua-mobile: ?0",
            "sec-ch-ua-platform: \"Windows\"",
        ] {
            assert!(text.contains(header), "missing {header}");
        }
    }

    #[test]
    fn all_profiles_build_valid_handshakes() {
        for profile in &BROWSER_PROFILES {
            let (request, key) = build_client_handshake_request_with_profile(
                "example.com",
                "/ws",
                Some("https://example.com"),
                Some("same-origin"),
                profile,
            );
            // Every profile must pass server-side validation.
            assert_eq!(validate_server_handshake(&request).unwrap(), key);
            let text = std::str::from_utf8(&request).unwrap();
            assert!(text.starts_with("GET /ws HTTP/1.1\r\n"));
            assert!(text.contains("Upgrade: websocket\r\n"));
            // Firefox never sends Client Hints; Chromium always does.
            if profile.is_chromium {
                assert!(
                    text.contains("sec-ch-ua:"),
                    "chromium profile missing sec-ch-ua"
                );
            } else {
                assert!(
                    !text.contains("sec-ch-ua:"),
                    "firefox profile must not send sec-ch-ua"
                );
            }
        }
    }

    #[test]
    fn mobile_profile_signals_mobile() {
        let mobile = BROWSER_PROFILES.iter().find(|p| p.is_mobile).unwrap();
        assert_eq!(mobile.sec_ch_ua_mob, "?1");
        let (request, _) = build_client_handshake_request_with_profile(
            "example.com",
            "/",
            None,
            Some("cross-site"),
            mobile,
        );
        let text = std::str::from_utf8(&request).unwrap();
        assert!(text.contains("sec-ch-ua-mobile: ?1\r\n"));
        assert!(!text.contains("Origin:"));
    }

    #[test]
    fn redact_handshake_request_redacts_key() {
        let (request, key) = build_client_handshake_request(
            "dedi.4467107.xyz",
            "/pyway",
            Some("https://dedi.4467107.xyz"),
            Some("same-origin"),
        );
        let redacted = redact_handshake_request(&request);
        assert!(!redacted.contains(&key));
        assert!(redacted.contains("Sec-WebSocket-Key: [REDACTED]\r\n"));
        assert!(redacted.contains("Host: dedi.4467107.xyz\r\n"));
        assert!(redacted.contains("Origin: https://dedi.4467107.xyz\r\n"));
    }

    #[tokio::test]
    async fn read_http_headers_timeout_zero_bytes_error() {
        let (mut a, _b) = duplex(64);
        drop(_b); // Closed immediately
        let err = read_http_headers_timeout(&mut a, std::time::Duration::from_millis(50))
            .await
            .unwrap_err();
        assert!(err.to_string().contains("received 0 response bytes"));
    }

    #[test]
    fn mask_generator_is_nonzero() {
        assert_ne!(next_mask(), [0, 0, 0, 0]);
    }

    #[test]
    fn header_limit_is_hard() {
        assert!(validate_server_handshake(&vec![b'x'; MAX_HTTP_HEADER_SIZE + 1]).is_err());
    }

    #[tokio::test]
    async fn round_trip_unmasked_binary() {
        let (mut a, mut b) = duplex(1024 * 1024);
        let data = vec![7u8; 70000];
        let writer = tokio::spawn(async move { write_frame(&mut a, &data, 2, false).await });
        let mut buf = Vec::new();
        let got = read_frame(
            &mut b,
            Option::<&mut tokio::io::DuplexStream>::None,
            &mut buf,
        )
        .await
        .unwrap()
        .unwrap();
        writer.await.unwrap().unwrap();
        assert_eq!(got.0, 2);
        assert_eq!(got.1, vec![7u8; 70000]);
        assert!(buf.is_empty());
    }

    #[tokio::test]
    async fn round_trip_masked_binary() {
        let (mut a, mut b) = duplex(1024 * 1024);
        let data = vec![11u8; 70000];
        let writer = tokio::spawn(async move { write_frame(&mut a, &data, 2, true).await });
        let mut buf = Vec::new();
        let got = read_frame(
            &mut b,
            Option::<&mut tokio::io::DuplexStream>::None,
            &mut buf,
        )
        .await
        .unwrap()
        .unwrap();
        writer.await.unwrap().unwrap();
        assert_eq!(got.0, 2);
        assert_eq!(got.1, vec![11u8; 70000]);
        assert!(buf.is_empty());
    }

    #[tokio::test]
    async fn borrowed_write_reuses_scratch_safely() {
        let (mut a, mut b) = duplex(1024 * 1024);
        // One scratch buffer reused across frames of different sizes:
        // each frame must decode cleanly with no cross-frame leakage.
        let mut scratch = vec![0u8; 70000];
        for (len, byte) in [(70000usize, 0x11u8), (13, 0x22), (50000, 0x33)] {
            scratch[..len].fill(byte);
            write_frame_borrowed(&mut a, &mut scratch[..len], 2, true)
                .await
                .unwrap();
            let mut buf = Vec::new();
            let got = read_frame(
                &mut b,
                Option::<&mut tokio::io::DuplexStream>::None,
                &mut buf,
            )
            .await
            .unwrap()
            .unwrap();
            assert_eq!(got.0, 2);
            assert_eq!(got.1, vec![byte; len]);
            assert!(buf.is_empty());
        }
    }

    #[tokio::test]
    async fn owned_binary_frame_transfers_storage_without_clone() {
        let (mut a, mut b) = duplex(1024 * 1024);
        let data = vec![23u8; 70000];
        let writer = tokio::spawn(async move { write_frame(&mut a, &data, 2, false).await });
        let mut buf = Vec::with_capacity(70000);
        let (opcode, owned) = read_frame(
            &mut b,
            Option::<&mut tokio::io::DuplexStream>::None,
            &mut buf,
        )
        .await
        .unwrap()
        .unwrap();
        writer.await.unwrap().unwrap();
        assert_eq!(opcode, 2);
        assert_eq!(owned, vec![23u8; 70000]);
        assert!(buf.is_empty());
    }

    #[tokio::test]
    async fn close_frame_returns_eof_marker() {
        let (mut a, mut b) = duplex(1024);
        let writer = tokio::spawn(async move { write_frame(&mut a, b"", 8, false).await });
        let mut buf = Vec::new();
        let got = read_frame(
            &mut b,
            Option::<&mut tokio::io::DuplexStream>::None,
            &mut buf,
        )
        .await
        .unwrap();
        writer.await.unwrap().unwrap();
        assert!(got.is_none());
    }

    #[tokio::test]
    async fn clean_eof_returns_none() {
        let (a, mut b) = duplex(1024);
        drop(a); // Dropped without sending any frames
        let mut buf = Vec::new();
        let got = read_frame(
            &mut b,
            Option::<&mut tokio::io::DuplexStream>::None,
            &mut buf,
        )
        .await
        .unwrap();
        assert!(got.is_none(), "clean connection close must yield Ok(None)");
    }

    /// Adversarial wire inputs: malformed frames must always resolve to
    /// `Err` (never panic, never hang). Covers the length state machine,
    /// reserved opcodes (previously an `unreachable!()` panic), fragmented
    /// and oversized control frames, and truncation at every header stage.
    #[tokio::test]
    async fn read_frame_rejects_malformed() {
        // (wire bytes, must_error). Writer drops after sending so truncated
        // reads terminate via EOF instead of hanging.
        let cases: Vec<(&[u8], &str)> = vec![
            (&[0x82], "truncated 1-byte header"),
            (&[0x82, 0x7e], "truncated extended-16 length"),
            (
                &[0x82, 0x7f, 0, 0, 0, 0, 0, 0, 0, 5, b'h', b'i'],
                "truncated 64-bit-length payload",
            ),
            (&[0x8b, 0x00], "reserved opcode 0xB"),
            (&[0x8f, 0x00], "reserved opcode 0xF"),
            (&[0x02, 0x01, b'x'], "fragmented data frame (fin=0)"),
            (&[0x00, 0x01, b'x'], "continuation opcode"),
            (&[0x03, 0x01, b'x'], "reserved data opcode 0x3"),
            (&[0x89, 126, 0, 126], "oversized ping (>125)"),
            (
                &[0x82, 0x7e, 0x10, 0x00],
                "declared 4KiB payload, zero delivered",
            ),
        ];
        for (wire, what) in cases {
            let (mut a, mut b) = duplex(64 * 1024);
            let owned = wire.to_vec();
            let writer = tokio::spawn(async move {
                use tokio::io::AsyncWriteExt;
                let _ = a.write_all(&owned).await;
                drop(a);
            });
            let mut buf = Vec::new();
            let res = tokio::time::timeout(
                std::time::Duration::from_secs(5),
                read_frame(
                    &mut b,
                    Option::<&mut tokio::io::DuplexStream>::None,
                    &mut buf,
                ),
            )
            .await
            .expect("read_frame hung");
            assert!(res.is_err(), "{what}: expected Err, decoded {res:?}");
            let _ = writer.await;
        }
    }

    #[test]
    fn test_apply_ws_mask_matches_reference() {
        let key = [0x12, 0x34, 0x56, 0x78];
        for len in [
            0usize, 1, 2, 3, 4, 7, 8, 9, 15, 16, 17, 31, 32, 33, 63, 64, 65, 127, 128, 129,
            255, 256, 1000, 4096, 65535,
        ] {
            let orig: Vec<u8> = (0..len).map(|x| (x * 11 + 5) as u8).collect();
            let mut expected = orig.clone();
            for (i, b) in expected.iter_mut().enumerate() {
                *b ^= key[i & 3];
            }
            let mut actual = orig.clone();
            apply_ws_mask(&mut actual, key);
            assert_eq!(actual, expected, "apply_ws_mask mismatch at len {len}");
            // Round-trip back to original
            apply_ws_mask(&mut actual, key);
            assert_eq!(actual, orig, "apply_ws_mask round-trip failed at len {len}");
        }
    }
}
} // mod ws

pub mod wss_client {
//! WSS client paths for GoWay-compatible upstreams.

use crate::common::{
    configured_cipher, relay_non_mux_ws, send_mux_parts, send_mux_parts_reuse,
    socks5_udp_associate_reply, HTTP_200_CONNECTION_ESTABLISHED, NON_MUX_POOL_SIZE,
    NonMuxOkRejection, NonMuxPool, NonMuxRelaySpec,
};
use crate::crypto::XorCipher;
use crate::dns;
use crate::flow::CreditGate;
use crate::mux_writer::{
    recycle_encode_buf, recycle_encode_bufs, write_owned_frames_vectored, MuxFrameWriter,
};
use crate::protocol::{
    decode_version_payload, decode_window_payload, encode_version_payload, encode_window_payload,
    MuxCommand, MuxFrame, OwnedMuxFrame, SynPayload, U32HashMap, MUX_INITIAL_WINDOW_KIB,
    MUX_WINDOW_REFRESH,
};
use crate::proxy::{
    parse_authority_with_default, parse_socks5_udp_datagram, read_client_proxy_request,
    socks5_success_response, SocksCommand, TargetAddr,
};
use crate::runtime::{
    apply_listener_options, apply_socket_options_raw, check_target_policy, drain_join_set,
    recycle_buf, relay_buf, wait_shutdown, RuntimeConfig,
};
use crate::tls;
use crate::udp_batch::UdpBatchReader;
use crate::ws::{
    build_client_handshake_request, encode_ws_frame, read_frame, read_frame_with_cipher,
    read_http_headers_timeout, redact_handshake_request, validate_client_handshake_response,
    write_frame,
};
use anyhow::{anyhow, bail, Context, Result};
use std::future::Future;
use std::net::SocketAddr;
use std::pin::Pin;
use std::sync::{
    atomic::{AtomicBool, AtomicU32, AtomicUsize, Ordering},
    Arc, Mutex as StdMutex,
};
use tokio::io::{AsyncReadExt, AsyncWriteExt, ReadHalf, WriteHalf};
use tokio::net::{TcpListener, TcpStream, UdpSocket};
use tokio::sync::{mpsc, Mutex, Notify, Semaphore};
use tokio::time::{timeout, Duration};

pub(crate) type TlsReader = ReadHalf<tls::RushTlsStream>;
pub(crate) type TlsWriter = WriteHalf<tls::RushTlsStream>;

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
    pub(crate) max_connections: usize,
    /// Mirrors `RuntimeConfig::block_local`. The WSS client paths had no
    /// target policy at all, so `-block-local` was silently ignored whenever
    /// the upstream was `wss://` — the only transport where the flag matters
    /// most for a client behind a CDN.
    pub(crate) block_local: bool,
}
impl WssConfig {
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
            max_connections: cfg.max_connections,
            block_local: cfg.block_local,
        })
    }
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

    apply_socket_options_raw(&tcp, cfg.tcp_nodelay, cfg.socket_buffer, cfg.tcp_keepalive);
    let tls_stream = timeout(
        Duration::from_secs(cfg.connection_timeout.max(1)),
        tls::connect(tcp, tls_name, cfg.verify_ssl),
    )
    .await
    .context("WSS upstream TLS handshake timeout")?
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

pub(crate) async fn connect_wss_upstream(cfg: &WssConfig) -> Result<(TlsReader, TlsWriter)> {
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

    let (mut rd, mut wr) = tokio::io::split(tls_stream);
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
    wr.flush().await.context("WebSocket request flush failed")?;

    tracing::info!("[WSS] Waiting for WebSocket 101");
    let handshake_timeout = Duration::from_secs(cfg.connection_timeout.max(1));
    let response = read_http_headers_timeout(&mut rd, handshake_timeout).await?;
    validate_client_handshake_response(&response, &key)?;
    tracing::info!("[WSS] WebSocket handshake completed");

    Ok((rd, wr))
}

pub(crate) async fn open_upstream(cfg: &WssConfig) -> Result<(TlsReader, TlsWriter)> {
    connect_wss_upstream(cfg).await
}

/// Probe helper to test standalone WSS TCP -> TLS -> HTTP Upgrade -> 101 without entering MUX.
#[cfg(test)]
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
    let resp = socks5_udp_associate_reply(bound);
    control.write_all(&resp).await?;
    let (mut rd, mut wr) = open_upstream(&cfg).await?;
    let c = configured_cipher(&cfg.key);
    let mut hello = b"UDP\n".to_vec();
    c.apply(&mut hello);
    write_frame(&mut wr, &hello, 2, true).await?;
    let writer = Arc::new(Mutex::new(wr));
    let mut frame_buf = crate::mux_writer::acquire_encode_buf();
    let Some((opcode, mut ok)) =
        read_frame(&mut rd, Option::<&mut TlsWriter>::None, &mut frame_buf).await?
    else {
        recycle_encode_buf(frame_buf);
        bail!("WSS upstream closed during UDP handshake")
    };
    if opcode != 2 {
        recycle_encode_buf(ok);
        recycle_encode_buf(frame_buf);
        bail!("invalid WSS UDP handshake response opcode")
    };
    c.apply(&mut ok);
    let is_ok = ok == b"OK\n";
    recycle_encode_buf(ok);
    if !is_ok {
        recycle_encode_buf(frame_buf);
        bail!("WSS upstream rejected UDP handshake")
    };
    let latest_client = Arc::new(StdMutex::new(None::<SocketAddr>));
    let block_local = cfg.block_local;
    let udp_send = udp.clone();
    let writer_send = writer.clone();
    let cipher_send = c.clone();
    let latest_send = latest_client.clone();
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
                // readable here. The WSS client used to forward every
                // datagram unchecked; enforce `-block-local` per packet.
                if let Ok((target, _)) = parse_socks5_udp_datagram(pkt) {
                    if let Err(e) = check_target_policy(block_local, &target) {
                        tracing::debug!(error = %e, "WSS UDP datagram dropped by target policy");
                        continue;
                    }
                }
                *latest_send.lock().unwrap_or_else(|e| e.into_inner()) = Some(peer);
                let mut packet = crate::mux_writer::acquire_encode_buf();
                packet.extend_from_slice(pkt);
                cipher_send.apply(&mut packet);
                crate::stats::add_bytes(packet.len() as i64, 0);
                let mut w = writer_send.lock().await;
                let res = write_frame(&mut *w, &packet, 2, true).await;
                recycle_encode_buf(packet);
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
                let Some((opcode, packet)) = read_frame_with_cipher(
                    &mut rd,
                    Option::<&mut TlsWriter>::None,
                    &mut frame_buf,
                    Some(&c),
                )
                .await?
                else {
                    break;
                };
                if opcode != 2 {
                    recycle_encode_buf(packet);
                    continue;
                }
                // Copy the peer out first: a std Mutex guard must never be
                // held across the await below.
                let peer_opt = *latest_client.lock().unwrap_or_else(|e| e.into_inner());
                if let Some(peer) = peer_opt {
                    let _ = udp.send_to(&packet, peer).await;
                    crate::stats::add_bytes(0, packet.len() as i64);
                }
                recycle_encode_buf(packet);
            }
            Ok::<(), anyhow::Error>(())
        } => {}
    }
    recycle_encode_buf(frame_buf);
    upload.abort();
    let _ = control.shutdown().await;
    Ok(())
}

type NonMuxWssPool = NonMuxPool<WssConfig, TlsReader, TlsWriter>;

/// Boxed adapter so the shared [`NonMuxPool`] can dial the WSS opener.
fn dial_pooled_wss(
    cfg: &WssConfig,
) -> Pin<Box<dyn Future<Output = Result<(TlsReader, TlsWriter)>> + Send + '_>> {
    Box::pin(open_upstream(cfg))
}

const WSS_NON_MUX_RELAY: NonMuxRelaySpec = NonMuxRelaySpec {
    closed_error: "WSS upstream closed before non-MUX OK",
    bad_opcode_error: "invalid WSS non-MUX handshake opcode",
    ok_rejection: NonMuxOkRejection::RespondLocal {
        socks5_failure: [5, 1, 0, 1, 0, 0, 0, 0, 0, 0],
    },
};

async fn handle_non_mux_connection(
    mut local: TcpStream,
    cfg: WssConfig,
    pool: Arc<NonMuxWssPool>,
) -> Result<()> {
    let req = timeout(
        Duration::from_secs(cfg.connection_timeout.max(1)),
        read_client_proxy_request(&mut local),
    )
    .await
    .map_err(|_| anyhow!("WSS non-MUX client proxy request timed out"))??;
    if req.command == SocksCommand::UdpAssociate {
        return handle_udp_proxy(local, cfg, req.target).await;
    }
    if req.command != SocksCommand::Connect {
        bail!("WSS non-MUX supports CONNECT or UDP ASSOCIATE only")
    };
    // WSS client paths had no target policy; enforce `-block-local` here so
    // the flag behaves the same as on the plain-WS and QUIC clients.
    check_target_policy(cfg.block_local, &req.target)?;
    let (rd, wr) = pool.get_or_dial().await?;
    relay_non_mux_ws(
        local,
        &req.target,
        req.is_socks5,
        req.is_connect,
        req.initial_payload,
        &cfg.key,
        cfg.buffer_size,
        rd,
        wr,
        WSS_NON_MUX_RELAY,
    )
    .await
}

pub async fn run_non_mux_from_config(cfg: RuntimeConfig, verify_ssl: bool) -> Result<()> {
    let wc = WssConfig::from_runtime_config(&cfg, verify_ssl)?;
    let pool = NonMuxPool::new(wc.clone(), Box::new(dial_pooled_wss), "[WSS non-MUX]");
    let maintainer = pool.clone();
    tokio::spawn(async move {
        maintainer.maintain().await;
    });
    let listener = TcpListener::bind(format!("{}:{}", wc.proxy_host, wc.proxy_port)).await?;
    apply_listener_options(&listener);
    let semaphore = Arc::new(Semaphore::new(wc.max_connections.max(1)));
    tracing::info!(
        "RushWay WSS non-MUX client proxy listening on {}:{} ({} pre-warmed upstream connections)",
        wc.proxy_host,
        wc.proxy_port,
        NON_MUX_POOL_SIZE,
    );
    let mut set = tokio::task::JoinSet::new();
    loop {
        tokio::select! {
            _ = wait_shutdown() => {
                tracing::info!("shutdown requested, draining WSS non-MUX connections");
                break;
            }
            res = listener.accept() => {
                // A transient accept error (EMFILE, ENFILE, ENOBUFS) must not
                // kill the whole proxy: log, back off briefly, and retry.
                let (stream, peer) = match res {
                    Ok(v) => v,
                    Err(e) => {
                        tracing::warn!(error=%e, "WSS non-MUX client listener accept failed; retrying in 100ms");
                        tokio::time::sleep(Duration::from_millis(100)).await;
                        continue;
                    }
                };
                let permit = match semaphore.clone().try_acquire_owned() {
                    Ok(v) => v,
                    Err(_) => {
                        tracing::debug!(%peer,"maximum client connections reached");
                        continue;
                    }
                };
                let cfg2 = wc.clone();
                apply_socket_options_raw(
                    &stream,
                    cfg2.tcp_nodelay,
                    cfg2.socket_buffer,
                    cfg2.tcp_keepalive,
                );
                let pool2 = pool.clone();
                set.spawn(async move {
                    let _permit = permit;
                    let _conn = crate::stats::ConnGuard::new();
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

struct ClientStreamEntry {
    tx: mpsc::Sender<OwnedMuxFrame>,
    gate: Arc<CreditGate>,
}

struct WssSessionState {
    writer: Arc<MuxFrameWriter>,
    cipher: XorCipher,
    obfs: bool,
    // Read-mostly under concurrency (one shared lookup per DATA or WINDOW frame),
    // so a single RwLock<U32HashMap> holds both the downstream channel and upload gate.
    streams: Arc<std::sync::RwLock<U32HashMap<ClientStreamEntry>>>,
    /// Peer-advertised receive window in KiB once its VERSION arrived
    /// (0 => un-negotiated: v1 unbounded sends, no WINDOW refunds).
    peer_window_kib: AtomicU32,
    next_id: AtomicU32,
    active: AtomicUsize,
    closed: AtomicBool,
    capacity_notify: Arc<Notify>,
}
impl WssSessionState {
    async fn connect(cfg: &WssConfig, capacity_notify: Arc<Notify>) -> Result<Arc<Self>> {
        let (mut rd, mut wr) = open_upstream(cfg).await?;
        let c = configured_cipher(&cfg.key);
        let mut hello = b"MUX\n".to_vec();
        c.apply(&mut hello);

        tracing::info!("[WSS] Sending MUX handshake");
        write_frame(&mut wr, &hello, 2, true)
            .await
            .context("MUX handshake request write failed")?;
        let mut frame_buf = crate::mux_writer::acquire_encode_buf();
        let mux_timeout = Duration::from_secs(cfg.connection_timeout.max(1));
        let read_result = timeout(
            mux_timeout,
            read_frame(&mut rd, Option::<&mut TlsWriter>::None, &mut frame_buf),
        )
        .await
        .context("MUX OK timeout");
        recycle_encode_buf(frame_buf);
        let read_result = read_result??;

        let Some((opcode, mut ok)) = read_result else {
            bail!("WSS upstream closed during MUX handshake")
        };
        if opcode != 2 {
            recycle_encode_buf(ok);
            bail!("invalid WSS MUX response opcode: {opcode}")
        };
        c.apply(&mut ok);
        if ok != b"OK\n" {
            let msg = String::from_utf8_lossy(&ok).trim().to_string();
            recycle_encode_buf(ok);
            bail!("MUX rejected: {msg:?}")
        };
        recycle_encode_buf(ok);
        tracing::info!("[WSS] MUX handshake completed");
        // Hand the write half to the dedicated writer task; from here on
        // all MUX frames go through the serialized, coalescing queue.
        let (writer, _writer_task) = MuxFrameWriter::spawn(wr);
        let cipher_for_version = c.clone();
        let obfs_for_version = cfg.obfs;
        let session = Arc::new(Self {
            writer: writer.clone(),
            cipher: c.clone(),
            obfs: cfg.obfs,
            streams: Arc::new(std::sync::RwLock::new(U32HashMap::default())),
            peer_window_kib: AtomicU32::new(0),
            next_id: AtomicU32::new(1),
            active: AtomicUsize::new(0),
            closed: AtomicBool::new(false),
            capacity_notify,
        });
        // W3 version negotiation: advertise our version + initial receive
        // window right after the session handshake (mirrors mux_pool).
        let version_payload = encode_version_payload(MUX_INITIAL_WINDOW_KIB);
        send_mux_parts(
            &writer,
            &cipher_for_version,
            0,
            MuxCommand::Version,
            &version_payload,
            obfs_for_version,
        )
        .await?;
        let reader_session = session.clone();
        tokio::spawn(async move {
            if let Err(error) = wss_reader_loop(&mut rd, reader_session.clone()).await {
                tracing::debug!(%error,"WSS physical session reader stopped")
            };
            reader_session.closed.store(true, Ordering::Release);
            // Unblock upload tasks parked on credit: no WINDOW will arrive.
            let mut streams = reader_session
                .streams
                .write()
                .unwrap_or_else(|e| e.into_inner());
            for entry in streams.values() {
                entry.gate.close();
            }
            streams.clear();
            drop(streams);
            reader_session.active.store(0, Ordering::Release);
            reader_session.capacity_notify.notify_waiters();
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
                // Ping unconditionally: active-but-idle streams (a suspended
                // SSH session, for instance) carry no traffic, so waiting for
                // a fully idle session lets NAT/CDN idle timeouts reset the
                // link and every stream riding on it.
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
        initial_data: &[u8],
    ) -> Result<(u32, mpsc::Receiver<OwnedMuxFrame>, Arc<CreditGate>)> {
        if !self.try_reserve() {
            bail!("WSS physical session is full or closed")
        };
        // Pre-allocate channel and credit gate outside the RwLock write guard.
        let (tx, rx) = mpsc::channel(crate::protocol::MUX_STREAM_QUEUE_CAP);
        let gate = CreditGate::new();

        let id = {
            let mut streams = self.streams.write().unwrap_or_else(|e| e.into_inner());
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
            let kib = self.peer_window_kib.load(Ordering::Acquire);
            if kib > 0 {
                gate.enable(i64::from(kib) * 1024);
            }
            streams.insert(
                stream_id,
                ClientStreamEntry {
                    tx,
                    gate: gate.clone(),
                },
            );
            stream_id
        };
        let port_digits = if target.port >= 10000 {
            5
        } else if target.port >= 1000 {
            4
        } else if target.port >= 100 {
            3
        } else if target.port >= 10 {
            2
        } else {
            1
        };
        let target_len = target.host.len() + 1 + port_digits;
        let max_syn_initial = (u16::MAX as usize).saturating_sub(2 + target_len);
        let (syn_initial, remaining_initial) =
            initial_data.split_at(initial_data.len().min(max_syn_initial));

        let syn_payload = match SynPayload::encode_parts(&target.host, target.port, syn_initial) {
            Ok(v) => v,
            Err(e) => {
                self.close_stream(id).await;
                return Err(anyhow!(e.to_string()));
            }
        };
        if let Err(error) = send_mux_parts(
            &self.writer,
            &self.cipher,
            id,
            MuxCommand::Syn,
            &syn_payload,
            self.obfs,
        )
        .await
        {
            self.closed.store(true, Ordering::Release);
            self.close_stream(id).await;
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
                self.closed.store(true, Ordering::Release);
                self.close_stream(id).await;
                return Err(error);
            }
        }
        Ok((id, rx, gate))
    }
    async fn close_stream(&self, id: u32) {
        if let Some(entry) = self
            .streams
            .write()
            .unwrap_or_else(|e| e.into_inner())
            .remove(&id)
        {
            entry.gate.close();
            self.active.fetch_sub(1, Ordering::AcqRel);
            self.capacity_notify.notify_waiters();
        }
    }
}
async fn wss_reader_loop(rd: &mut TlsReader, session: Arc<WssSessionState>) -> Result<()> {
    let mut frame_buf = crate::mux_writer::acquire_encode_buf();
    let res = async {
        loop {
            let Some((opcode, payload)) = read_frame_with_cipher(
                rd,
                Option::<&mut TlsWriter>::None,
                &mut frame_buf,
                Some(&session.cipher),
            )
            .await?
            else {
                return Ok(());
            };
            if opcode != 2 {
                recycle_encode_buf(payload);
                continue;
            }
            let frame = match MuxFrame::decode_owned(payload) {
                Ok(v) => v,
                Err(_) => continue,
            };
            // W3 control frames are session/stream-level: handle them here
            // instead of routing into per-stream channels (VERSION uses
            // stream id 0 and would otherwise be dropped silently).
            match frame.command {
                MuxCommand::Version => {
                    let decoded = decode_version_payload(frame.payload());
                    recycle_encode_buf(frame.into_storage());
                    if let Some((version, kib)) = decoded {
                        if version >= 1 {
                            let prev = session.peer_window_kib.swap(kib as u32, Ordering::AcqRel);
                            if prev == 0 {
                                let streams = session
                                    .streams
                                    .read()
                                    .unwrap_or_else(|e| e.into_inner());
                                let window = i64::from(kib) * 1024;
                                for entry in streams.values() {
                                    entry.gate.enable(window);
                                }
                            }
                        }
                    }
                    continue;
                }
                MuxCommand::Window => {
                    let stream_id = frame.stream_id;
                    let decoded = decode_window_payload(frame.payload());
                    recycle_encode_buf(frame.into_storage());
                    if let Some(credit) = decoded {
                        if let Some(entry) = session
                            .streams
                            .read()
                            .unwrap_or_else(|e| e.into_inner())
                            .get(&stream_id)
                        {
                            entry.gate.release(i64::from(credit));
                        }
                    }
                    continue;
                }
                _ => {}
            }
            let id = frame.stream_id;
            let send_res = {
                let streams = session.streams.read().unwrap_or_else(|e| e.into_inner());
                streams.get(&id).map(|entry| entry.tx.try_send(frame))
            };
            match send_res {
                Some(Ok(())) => {}
                Some(Err(mpsc::error::TrySendError::Full(f))) => {
                    recycle_encode_buf(f.into_storage());
                    tracing::warn!(
                        stream_id = id,
                        "mux RST: stream queue full; resetting stalled stream"
                    );
                    let _ = send_mux_parts(
                        &session.writer,
                        &session.cipher,
                        id,
                        MuxCommand::Rst,
                        &[],
                        session.obfs,
                    )
                    .await;
                    session.close_stream(id).await;
                }
                Some(Err(mpsc::error::TrySendError::Closed(f))) => {
                    recycle_encode_buf(f.into_storage());
                    session.close_stream(id).await;
                }
                None => {}
            }
        }
    }
    .await;
    recycle_encode_buf(frame_buf);
    res
}

struct WssSessionPool {
    cfg: WssConfig,
    /// Target physical session count, resolved once at startup (see
    /// `run_client_from_config`); re-read per acquire would touch the
    /// environment on every stream open.
    session_count: usize,
    sessions: Mutex<Vec<Arc<WssSessionState>>>,
    session_creation: Mutex<()>,
    consecutive_failures: AtomicU32,
    /// Signalled whenever session capacity may have appeared: a stream was
    /// released, a session closed, or a replenished session joined the pool.
    capacity_notify: Arc<Notify>,
}
impl WssSessionPool {
    fn new(cfg: WssConfig, session_count: usize) -> Arc<Self> {
        Arc::new(Self {
            cfg,
            session_count: session_count.clamp(1, MAX_SESSION_COUNT),
            sessions: Mutex::new(Vec::new()),
            session_creation: Mutex::new(()),
            consecutive_failures: AtomicU32::new(0),
            capacity_notify: Arc::new(Notify::new()),
        })
    }
    async fn replenish(self: &Arc<Self>) -> bool {
        let target = self.session_count;
        let _guard = self.session_creation.lock().await;
        let missing = {
            let mut sessions = self.sessions.lock().await;
            sessions.retain(|s| !s.closed.load(Ordering::Acquire));
            target.saturating_sub(sessions.len())
        };
        if missing == 0 {
            self.consecutive_failures.store(0, Ordering::Release);
            return false;
        }
        let mut set = tokio::task::JoinSet::new();
        for _ in 0..missing {
            let cfg = self.cfg.clone();
            let notify = self.capacity_notify.clone();
            set.spawn(async move {
                WssSessionState::connect(&cfg, notify).await
            });
        }
        let mut added = 0usize;
        let mut last_err = None;
        while let Some(res) = set.join_next().await {
            match res {
                Ok(Ok(s)) => {
                    self.sessions.lock().await.push(s);
                    added += 1;
                    self.capacity_notify.notify_waiters();
                }
                Ok(Err(e)) => last_err = Some(e),
                Err(e) => last_err = Some(e.into()),
            }
        }
        if added > 0 {
            self.consecutive_failures.store(0, Ordering::Release);
            false
        } else {
            let failures = self.consecutive_failures.fetch_add(1, Ordering::AcqRel) + 1;
            if let Some(error) = last_err {
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
            false
        }
    }
    async fn maintain(self: &Arc<Self>) {
        loop {
            let more_needed = self.replenish().await;
            if more_needed {
                tokio::time::sleep(Duration::from_millis(10)).await;
                continue;
            }
            let failures = self.consecutive_failures.load(Ordering::Acquire);
            let backoff = (failures.saturating_mul(500)).min(5000);
            tokio::time::sleep(Duration::from_millis(250 + backoff as u64)).await;
        }
    }
    async fn acquire(
        self: &Arc<Self>,
        target: &TargetAddr,
        initial_data: Vec<u8>,
    ) -> Result<(
        Arc<WssSessionState>,
        u32,
        mpsc::Receiver<OwnedMuxFrame>,
        Arc<CreditGate>,
    )> {
        let n = self.session_count;
        loop {
            let notified = self.capacity_notify.notified();
            tokio::pin!(notified);
            notified.as_mut().enable();

            let (chosen, need_new) = {
                let mut sessions = self.sessions.lock().await;
                sessions.retain(|s| !s.closed.load(Ordering::Acquire));
                let chosen = sessions
                    .iter()
                    .filter(|s| s.available())
                    .min_by_key(|s| s.active.load(Ordering::Acquire))
                    .cloned();
                let need_new = sessions.len() < n;
                (chosen, need_new)
            };
            if let Some(session) = chosen {
                match session.open_stream(target, &initial_data).await {
                    Ok(opened) => {
                        recycle_encode_buf(initial_data);
                        if need_new {
                            let pool = self.clone();
                            tokio::spawn(async move {
                                pool.replenish().await;
                            });
                        }
                        return Ok((session, opened.0, opened.1, opened.2));
                    }
                    Err(_) if session.closed.load(Ordering::Acquire) || !session.available() => {
                        continue;
                    }
                    Err(e) => {
                        recycle_encode_buf(initial_data);
                        return Err(e);
                    }
                }
            }
            if !need_new {
                let _ = timeout(Duration::from_millis(500), &mut notified).await;
                continue;
            }

            let Ok(_creation_guard) = self.session_creation.try_lock() else {
                let _ = timeout(Duration::from_millis(200), &mut notified).await;
                continue;
            };
            let mut sessions = self.sessions.lock().await;
            sessions.retain(|s| !s.closed.load(Ordering::Acquire));
            if sessions.iter().any(|s| s.available()) || sessions.len() >= n {
                continue;
            }
            drop(sessions);

            let session = WssSessionState::connect(&self.cfg, self.capacity_notify.clone()).await.map_err(|e| {
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
            let opened = session.open_stream(target, &initial_data).await?;
            recycle_encode_buf(initial_data);
            self.sessions.lock().await.push(session.clone());
            self.capacity_notify.notify_waiters();
            return Ok((session, opened.0, opened.1, opened.2));
        }
    }
}

async fn handle_connection(mut local: TcpStream, pool: Arc<WssSessionPool>) -> Result<()> {
    let req = timeout(
        Duration::from_secs(pool.cfg.connection_timeout.max(1)),
        read_client_proxy_request(&mut local),
    )
    .await
    .map_err(|_| anyhow!("WSS client proxy request timed out"))??;
    if req.command == SocksCommand::UdpAssociate {
        return handle_udp_proxy(local, pool.cfg.clone(), req.target).await;
    }
    if req.command != SocksCommand::Connect {
        bail!("WSS path only supports CONNECT or UDP ASSOCIATE")
    };
    check_target_policy(pool.cfg.block_local, &req.target)?;
    if req.is_socks5 {
        local.write_all(&socks5_success_response()).await?;
    } else if req.is_connect {
        local.write_all(HTTP_200_CONNECTION_ESTABLISHED).await?;
    };
    let mut initial_data = req.initial_payload.unwrap_or_default();
    if initial_data.is_empty() {
        let mut first_pkt = crate::mux_writer::acquire_encode_buf();
        first_pkt.resize(16384, 0);
        match timeout(Duration::from_millis(2), local.read(&mut first_pkt)).await {
            Ok(Ok(n)) if n > 0 => {
                crate::stats::add_bytes(n as i64, 0);
                first_pkt.truncate(n);
                initial_data = first_pkt;
            }
            _ => {
                recycle_encode_buf(first_pkt);
            }
        }
    }
    let (session, stream_id, mut rx, gate) = pool.acquire(&req.target, initial_data).await?;
    let writer = session.writer.clone();
    let cipher = session.cipher.clone();
    let obfs = session.obfs;
    let (mut local_rd, mut local_wr) = local.into_split();
    let buffer_size = pool.cfg.buffer_size;
    let upload = tokio::spawn(async move {
        let mut buf = relay_buf(buffer_size).await;
        let mut frame_scratch = crate::mux_writer::acquire_encode_buf();
        let res = async {
            loop {
                let n = local_rd.read(&mut buf).await?;
                if n == 0 {
                    let _ = send_mux_parts_reuse(
                        &writer,
                        &cipher,
                        stream_id,
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
                        stream_id,
                        MuxCommand::Data,
                        &buf[off..end],
                        &mut frame_scratch,
                        obfs,
                    )
                    .await?;
                    off = end
                }
            }
            Result::<()>::Ok(())
        }
        .await;
        recycle_encode_buf(frame_scratch);
        recycle_buf(buf).await;
        res
    });
    // W3 receive-side refund accumulator (client -> server WINDOW).
    let mut refund_pending: u32 = 0;
    let mut result = Result::<()>::Ok(());
    let mut remote_fin = false;
    let mut data_batch: Vec<OwnedMuxFrame> = Vec::with_capacity(16);
    let mut deferred_ctrl: Option<OwnedMuxFrame> = None;
    loop {
        let frame = if let Some(ctrl) = deferred_ctrl.take() {
            ctrl
        } else {
            match rx.recv().await {
                Some(f) => f,
                None => break,
            }
        };
        match frame.command {
            MuxCommand::Data => {
                data_batch.push(frame);
                while data_batch.len() < 16 {
                    match rx.try_recv() {
                        Ok(next) if next.command == MuxCommand::Data => data_batch.push(next),
                        Ok(other) => {
                            deferred_ctrl = Some(other);
                            break;
                        }
                        Err(_) => break,
                    }
                }
                let write_res = write_owned_frames_vectored(&mut local_wr, &data_batch).await;
                recycle_encode_bufs(data_batch.drain(..).map(OwnedMuxFrame::into_storage));
                let written = match write_res {
                    Ok(n) => n,
                    Err(e) => {
                        if let Some(ctrl) = deferred_ctrl.take() {
                            recycle_encode_buf(ctrl.into_storage());
                        }
                        result = Err(e.into());
                        break;
                    }
                };
                if written > 0 {
                    crate::stats::add_bytes(0, written as i64);
                    let negotiated = session.peer_window_kib.load(Ordering::Relaxed) > 0;
                    if negotiated {
                        refund_pending = refund_pending.saturating_add(written as u32);
                        if refund_pending as usize >= MUX_WINDOW_REFRESH {
                            let credit = std::mem::take(&mut refund_pending);
                            if let Err(e) = send_mux_parts(
                                &session.writer,
                                &session.cipher,
                                stream_id,
                                MuxCommand::Window,
                                &encode_window_payload(credit),
                                session.obfs,
                            )
                            .await
                            {
                                if let Some(ctrl) = deferred_ctrl.take() {
                                    recycle_encode_buf(ctrl.into_storage());
                                }
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
                recycle_encode_buf(frame.into_storage());
                let _ = local_wr.shutdown().await;
                remote_fin = true;
                break;
            }
            MuxCommand::Rst => {
                recycle_encode_buf(frame.into_storage());
                result = Err(anyhow!("upstream reset after CONNECT established"));
                break;
            }
            MuxCommand::Syn | MuxCommand::Version | MuxCommand::Window => {
                recycle_encode_buf(frame.into_storage());
            }
        }
    }
    upload.abort();
    if remote_fin {
        let _ = send_mux_parts(
            &session.writer,
            &session.cipher,
            stream_id,
            MuxCommand::Fin,
            &[],
            session.obfs,
        )
        .await;
    } else if result.is_err() {
        let _ = send_mux_parts(
            &session.writer,
            &session.cipher,
            stream_id,
            MuxCommand::Rst,
            &[],
            session.obfs,
        )
        .await;
    }

    session.close_stream(stream_id).await;
    result
}

pub async fn run_client_from_config(
    cfg: RuntimeConfig,
    verify_ssl: bool,
    session_count: usize,
) -> Result<()> {
    let wc = WssConfig::from_runtime_config(&cfg, verify_ssl)?;
    let pool = WssSessionPool::new(wc.clone(), session_count);
    let listener = TcpListener::bind(format!("{}:{}", wc.proxy_host, wc.proxy_port)).await?;
    apply_listener_options(&listener);
    let semaphore = Arc::new(Semaphore::new(wc.max_connections.max(1)));
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
                // A transient accept error (EMFILE, ENFILE, ENOBUFS) must not
                // kill the whole proxy: log, back off briefly, and retry.
                let (stream, peer) = match res {
                    Ok(v) => v,
                    Err(e) => {
                        tracing::warn!(error=%e, "WSS client listener accept failed; retrying in 100ms");
                        tokio::time::sleep(Duration::from_millis(100)).await;
                        continue;
                    }
                };
                let permit = match semaphore.clone().try_acquire_owned() {
                    Ok(v) => v,
                    Err(_) => {
                        tracing::debug!(%peer,"maximum client connections reached");
                        continue;
                    }
                };
                apply_socket_options_raw(
                    &stream,
                    pool.cfg.tcp_nodelay,
                    pool.cfg.socket_buffer,
                    pool.cfg.tcp_keepalive,
                );
                let pool2 = pool.clone();
                set.spawn(async move {
                    let _permit = permit;
                    let _conn = crate::stats::ConnGuard::new();
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
    fn parse_wss_url_with_ip_and_path() {
        let (connect_addr, host, path) =
            parse_wss_url("wss://172.64.229.105:443/pyway").expect("valid wss url");
        assert_eq!(connect_addr, "172.64.229.105:443");
        assert_eq!(host, "172.64.229.105");
        assert_eq!(path, "/pyway");
    }

    #[test]
    fn parse_wss_url_standard_port_and_root_path() {
        let (connect_addr, host, path) = parse_wss_url("wss://example.com").expect("valid wss url");
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
        let sec_fetch_site = if tls_name
            .eq_ignore_ascii_case(header_host.split(':').next().unwrap_or(header_host))
        {
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

        let (request_bytes, key) =
            build_client_handshake_request(header_host, &path, Some(&origin), Some(sec_fetch_site));
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
    // Intentionally exercises the `None` branch of the production
    // fakehost-selection expressions with a literal None.
    #[allow(clippy::unnecessary_literal_unwrap)]
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
            let c = configured_cipher(&Some("secretkey".into()));
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
            max_connections: 16,
            block_local: false,
        };

        let session = WssSessionState::connect(&cfg, Arc::new(Notify::new())).await.unwrap();
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
            max_connections: 16,
            block_local: false,
        };

        let res = probe_wss_handshake(&cfg).await.unwrap();
        assert_eq!(res, "HTTP 101 Switching Protocols");
        server_task.await.unwrap();
    }
}
} // mod wss_client


use anyhow::{anyhow, bail, Context, Result};
use clap::Parser;
use runtime::RuntimeConfig;
use std::path::PathBuf;
use std::sync::Arc;
use tokio::net::TcpListener;
use tokio::sync::Semaphore;
use tokio::time::{sleep, timeout, Duration};
use tracing_subscriber::layer::SubscriberExt;
use tracing_subscriber::util::SubscriberInitExt;
use tracing_subscriber::EnvFilter;

#[cfg(all(
    any(target_arch = "x86_64", target_arch = "aarch64"),
    target_os = "linux"
))]
#[global_allocator]
static GLOBAL: mimalloc::MiMalloc = mimalloc::MiMalloc;

#[derive(Debug, Parser)]
#[command(
    name = "rushway",
    version,
    about = "Rust forwarding service compatible with GoWay"
)]
struct Args {
    #[arg(short, long)]
    config: Option<PathBuf>,
    #[arg(short = 'l', long = "local")]
    local_host: Option<String>,
    #[arg(short = 'p')]
    port: Option<String>,
    #[arg(short = 'u', long = "up")]
    upstream: Option<String>,
    #[arg(short = 'k')]
    key: Option<String>,
    #[arg(
        long = "fakehost",
        help = "Cloudflare/CDN hostname used for TLS SNI and HTTP Host; when upstream is an IP, failed connections may retry through DNS-resolved Cloudflare edges"
    )]
    fakehost: Option<String>,
    #[arg(long = "mux", default_value_t = true)]
    mux: bool,
    #[arg(long = "no-mux", default_value_t = false)]
    no_mux: bool,
    #[arg(long = "mux-sessions", default_value_t = 8, value_parser = clap::value_parser!(usize))]
    mux_sessions: usize,
    #[arg(
        long = "obfs",
        default_value_t = false,
        help = "Pad MUX DATA frames with random lengths to resist packet-size fingerprinting (GoWay parity)"
    )]
    obfs: bool,
    #[arg(long = "allow-open", default_value_t = false)]
    allow_open: bool,
    #[arg(
        long = "verify-ssl",
        default_value_t = true,
        help = "Verify the upstream TLS certificate against the bundled webpki (Mozilla) root set [default: on]"
    )]
    verify_ssl: bool,
    #[arg(
        long = "no-verify-ssl",
        default_value_t = false,
        help = "Skip upstream TLS certificate verification. The connection is then unauthenticated: anyone on path can impersonate the upstream and read the -k key."
    )]
    no_verify_ssl: bool,
    #[arg(
        long = "cert",
        help = "PEM certificate chain for server mode. Must be given together with --key; without it server mode keeps generating a self-signed certificate that no verifying client will accept."
    )]
    cert: Option<PathBuf>,
    #[arg(
        long = "key",
        help = "PEM private key for server mode (PKCS#8, SEC1 or PKCS#1). Must be given together with --cert."
    )]
    key_file: Option<PathBuf>,
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
    #[arg(
        long = "max-conn",
        value_parser = clap::value_parser!(usize),
        help = "Maximum concurrent client connections [default: 1500]"
    )]
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

impl Args {
    /// Effective upstream certificate verification setting.
    ///
    /// Verification defaults to **on**; `--no-verify-ssl` is the documented
    /// escape hatch for self-signed or IP-only upstreams and wins when both
    /// flags are given.
    fn verify_ssl_effective(&self) -> bool {
        self.verify_ssl && !self.no_verify_ssl
    }
}

const LEGACY_LONG_FLAGS: &[&str] = &[
    "up",
    "fakehost",
    "mux",
    "no-mux",
    "mux-sessions",
    "obfs",
    "allow-open",
    "verify-ssl",
    "no-verify-ssl",
    "cert",
    "key",
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
    "local",
    "wss-server",
    "tui",
    "version",
    "help",
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
                            "no-mux"
                            | "no-block-local"
                            | "no-tcp-nodelay"
                            | "no-tcp-keepalive"
                            | "no-verify-ssl" => {
                                if value {
                                    return format!("--{name}");
                                }
                                return match *name {
                                    "no-mux" => "--mux".to_string(),
                                    "no-block-local" => "--block-local".to_string(),
                                    "no-verify-ssl" => "--verify-ssl".to_string(),
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
    if let Some(x) = v.get("obfs").and_then(|x| x.as_bool()) {
        cfg.obfs = x
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
    runtime::apply_listener_options(&listener);

    let acceptor = tls::standalone_server_acceptor()?;
    tracing::info!("RushWay WSS server listening on {public_bind}");
    // This listener used to be the only one without a connection ceiling, so
    // `-max-connections` never applied to WSS and peers that completed TCP
    // but never spoke TLS parked a task forever.
    let semaphore = Arc::new(Semaphore::new(cfg.max_connections.max(1)));
    let mut set = tokio::task::JoinSet::new();
    loop {
        tokio::select! {
            _ = runtime::wait_shutdown() => {
                tracing::info!("shutdown requested, draining WSS server connections");
                break;
            }
            res = listener.accept() => {
                // A transient accept error (EMFILE, ENFILE, ENOBUFS) must not
                // kill the whole proxy: log, back off briefly, and retry.
                let (stream, peer) = match res {
                    Ok(v) => v,
                    Err(e) => {
                        tracing::warn!(error=%e, "WSS server listener accept failed; retrying in 100ms");
                        sleep(Duration::from_millis(100)).await;
                        continue;
                    }
                };
                // Fail fast at capacity instead of stalling the accept loop
                // and letting the TCP backlog fill up.
                let permit = match semaphore.clone().try_acquire_owned() {
                    Ok(v) => v,
                    Err(_) => {
                        tracing::debug!(%peer, "maximum WSS connections reached");
                        continue;
                    }
                };
                runtime::apply_socket_options(&stream, &cfg);
                let acceptor = acceptor.clone();
                let cfg2 = cfg.clone();
                set.spawn(async move {
                    let _permit = permit;
                    let _conn = stats::ConnGuard::new();
                    let result = async {
                        // Bound the TLS handshake: a peer that connects and
                        // then stalls mid-handshake otherwise holds its
                        // semaphore permit and its task indefinitely.
                        let tls_stream = timeout(
                            Duration::from_secs(cfg2.connection_timeout.max(1)),
                            acceptor.accept(stream),
                        )
                        .await
                        .context("TLS handshake timeout")??;
                        let (rd, wr) = tokio::io::split(tls_stream);
                        runtime::dispatch_server_ws_parts(rd, wr, cfg2).await
                    }
                    .await;
                    if let Err(error) = result {
                        tracing::debug!(%peer, %error, "WSS connection closed");
                    }
                });
            }
        }
    }
    runtime::drain_join_set(&mut set).await;
    Ok(())
}

/// Startup banner mirroring GoWay's dashboard header: always printed,
/// even at `--log ERROR`, so the version and effective mode are visible.
fn print_banner(cfg: &RuntimeConfig, mux_sessions: usize, dns_display: &str, verify_ssl: bool) {
    const CYAN: &str = "\x1b[36m";
    const RED: &str = "\x1b[31;1m";
    const RESET: &str = "\x1b[0m";
    let (mode, mux) = match cfg.upstream.as_deref() {
        None => ("Server".to_string(), "Enabled (server)".to_string()),
        Some(u) if u.starts_with("wss://") => (
            "Client (WSS + SOCKS5/HTTP)".to_string(),
            mux_summary(cfg.mux, mux_sessions),
        ),
        Some(u) if u.starts_with("quic") => (
            "Client (QUIC + SOCKS5/HTTP)".to_string(),
            mux_summary(cfg.mux, mux_sessions),
        ),
        Some(_) => (
            "Client (WS + SOCKS5/HTTP)".to_string(),
            mux_summary(cfg.mux, mux_sessions),
        ),
    };
    let upstream = cfg.upstream.as_deref().unwrap_or("N/A (Server mode)");
    let auth = if cfg.key.is_some() {
        "Enabled (XOR)"
    } else {
        "Disabled (Open)"
    };
    println!("{CYAN}RushWay v{}{RESET}", env!("CARGO_PKG_VERSION"));
    println!("{CYAN}------------------------------------------------------------{RESET}");
    println!(" [+] Mode:        {mode}");
    println!(" [+] Listen:      {}:{}", cfg.proxy_host, cfg.proxy_port);
    println!(" [+] Upstream:    {upstream}");
    println!(" [+] Mux:         {mux}");
    println!(
        " [+] Obfs:        {}",
        if cfg.obfs { "Enabled" } else { "Disabled" }
    );
    println!(" [+] DNS:         {dns_display}");
    println!(" [+] Auth:        {auth}");
    println!(" [+] Buffer:      {} KB", cfg.buffer_size / 1024);
    println!(" [+] Max Conns:   {}", cfg.max_connections);
    if cfg.upstream.is_some() && !verify_ssl {
        println!(
            "{RED} [!] TLS certificate verification is DISABLED (--no-verify-ssl).{RESET}"
        );
        println!(
            "{RED} [!] The upstream is unauthenticated; a man-in-the-middle can read the -k key.{RESET}"
        );
    }
    println!("{CYAN}------------------------------------------------------------{RESET}");
}

fn mux_summary(enabled: bool, sessions: usize) -> String {
    if enabled {
        format!("Enabled (sessions: {sessions})")
    } else {
        "Disabled (1:1)".to_string()
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
    let env_filter = EnvFilter::new(filter);
    // GoWay gates terminal escapes on ModeCharDevice; mirror that so piped
    // output stays clean and `-tui` degrades gracefully.
    let tui_active = args.tui && tui::stdout_is_tty();
    if args.tui && !tui_active {
        tracing::warn!("-tui requested but stdout is not a terminal; falling back to log output");
    }
    let ux_layer = tui::UxLayer::new(tui_active, args.log_file.is_some());
    if tui_active {
        tracing_subscriber::registry()
            .with(env_filter)
            .with(ux_layer)
            .init();
        tui::enable_tui();
    } else {
        tracing_subscriber::registry()
            .with(env_filter)
            .with(ux_layer)
            .with(tracing_subscriber::fmt::layer().with_target(false))
            .init();
    }
    if let Some(path) = args.log_file.clone() {
        tui::configure_log_file(path);
    }
    let mut cfg = if let Some(path) = args.config.clone() {
        load_json(path).await?
    } else {
        RuntimeConfig::default()
    };
    if args.config.is_none() && args.max_conn.is_none() {
        cfg.max_connections = 1500;
    }
    if let Some(path) = args.cpu_profile.clone() {
        profile::start(path, args.cpu_profile_duration)?;
    }
    if let Some(v) = args.local_host.clone() {
        cfg.proxy_host = v;
    }
    if let Some(v) = args.port.as_deref() {
        apply_listen_arg(&mut cfg, v)?;
    }
    if let Some(v) = args.upstream.clone() {
        cfg.upstream = Some(v);
    }
    if let Some(v) = args.key.clone() {
        cfg.key = Some(v);
    }
    if let Some(v) = args.fakehost.clone() {
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
    if args.obfs {
        cfg.obfs = true;
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
    let dns_opt = args.dns.clone();
    let dns_display = dns_opt
        .clone()
        .unwrap_or_else(|| "System default".to_string());
    if let Some(v) = dns_opt {
        dns::configure(Some(v)).await?;
    }
    // Resolve the MUX session count once at startup: RUSHWAY_MUX_SESSIONS
    // stays honored for external configuration, otherwise --mux-sessions
    // applies. The resolved value is handed straight to the session-pool
    // constructors, so nothing re-reads the environment on the hot path.
    let mux_sessions = std::env::var("RUSHWAY_MUX_SESSIONS")
        .ok()
        .and_then(|v| v.parse::<usize>().ok())
        .map(|v| v.clamp(1, 64))
        .unwrap_or(args.mux_sessions);
    // Fail fast on a bad --cert/--key before any listener comes up; running
    // after tracing init so the "loaded server certificate" line is visible.
    tls::configure_server_identity(args.cert.clone(), args.key_file.clone())?;
    let verify_ssl = args.verify_ssl_effective();
    if cfg.upstream.is_some() && !verify_ssl {
        tracing::warn!(
            "TLS certificate verification is disabled (--no-verify-ssl): the upstream is \
             unauthenticated and the -k key can be read by anyone on path"
        );
    }
    print_banner(&cfg, mux_sessions, &dns_display, verify_ssl);

    // GoWay `monitorStats`: 3 s `[STATS]` line (1 s cadence feeds the TUI).
    let level_up = args.log_level.trim().to_ascii_uppercase();
    let stats_line = !tui_active && matches!(level_up.as_str(), "DEBUG" | "INFO" | "");
    stats::spawn_monitor(tui_active, stats_line);
    if tui_active {
        let tui_cfg = tui::TuiConfig::from_runtime(
            &cfg,
            mux_sessions,
            &dns_display,
            verify_ssl,
            &args.log_level,
        );
        tui::spawn_refresh_loop(tui_cfg);
    }

    let result = run_forwarding(cfg, &args, mux_sessions).await;

    // Shutdown hygiene: flush `-log-file` ring and stop `-cpuprofile` sampler.
    tui::save_log_file();
    profile::finish();
    if tui::tui_enabled() {
        // Leave the alternate screen cleanly so the shell prompt is usable.
        use std::io::Write as _;
        let mut out = std::io::stdout();
        let _ = out.write_all(b"\x1b[2J\x1b[H");
        let _ = out.flush();
    }
    result
}

async fn run_forwarding(cfg: RuntimeConfig, args: &Args, mux_sessions: usize) -> Result<()> {
    let verify_ssl = args.verify_ssl_effective();
    if let Some(upstream) = cfg.upstream.as_deref() {
        if upstream.starts_with("wss://") {
            if cfg.mux {
                return wss_client::run_client_from_config(cfg, verify_ssl, mux_sessions).await;
            }
            return wss_client::run_non_mux_from_config(cfg, verify_ssl).await;
        }
        if upstream.starts_with("quic://") || upstream.starts_with("quic+tls://") {
            return quic::run_client(cfg, verify_ssl).await;
        }
        if cfg.mux {
            return mux_pool::run_client(cfg, mux_sessions).await;
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
        assert_eq!(args.mux_sessions, 8);
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
        assert!(args.verify_ssl_effective());
    }
    #[test]
    fn verify_ssl_defaults_on_and_no_verify_ssl_disables_it() {
        let default_args = Args::parse_from(["rushway", "--up", "wss://example.com/ws"]);
        assert!(default_args.verify_ssl);
        assert!(default_args.verify_ssl_effective());

        let off = Args::parse_from([
            "rushway",
            "--up",
            "wss://example.com/ws",
            "--no-verify-ssl",
        ]);
        assert!(!off.verify_ssl_effective());

        // GoWay spelling with an explicit value still normalizes, and the
        // legacy `-verify-ssl=false` maps onto the new opt-out flag.
        let normalized =
            normalize_legacy_args(["rushway", "--up", "wss://example.com/ws", "-verify-ssl=false"]);
        let off = Args::parse_from(&normalized);
        assert!(!off.verify_ssl_effective());

        // --no-verify-ssl wins when both are given: opting out is the
        // deliberate act, opting in on the same command line is not.
        let both = Args::parse_from([
            "rushway",
            "--up",
            "wss://example.com/ws",
            "--verify-ssl",
            "--no-verify-ssl",
        ]);
        assert!(!both.verify_ssl_effective());
    }
    #[test]
    fn server_identity_flags_are_parsed_together_or_not_at_all() {
        let args = Args::parse_from([
            "rushway",
            "--cert",
            "fullchain.pem",
            "--key",
            "privkey.pem",
        ]);
        assert_eq!(args.cert.as_deref(), Some(std::path::Path::new("fullchain.pem")));
        assert_eq!(
            args.key_file.as_deref(),
            Some(std::path::Path::new("privkey.pem"))
        );

        let err = crate::tls::configure_server_identity(
            Some(std::path::PathBuf::from("fullchain.pem")),
            None,
        )
        .unwrap_err();
        assert!(err.to_string().contains("--cert and --key"));
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
    #[test]
    fn parses_local_listen_flag() {
        let raw = ["rushway", "-l", "0.0.0.0", "-p", "8100"];
        let normalized = normalize_legacy_args(raw);
        let args = Args::parse_from(&normalized);
        assert_eq!(args.local_host.as_deref(), Some("0.0.0.0"));
        assert_eq!(args.port.as_deref(), Some("8100"));
    }
    #[test]
    fn normalizes_legacy_help_flag() {
        let got = normalize_legacy_args(["rushway", "-help"]);
        assert_eq!(got, vec!["rushway", "--help"]);
    }
    #[test]
    fn parses_cloudflare_fakehost_full_command() {
        let raw = [
            "rushway",
            "-k",
            "a6835181",
            "-up",
            "wss://172.64.229.105:443/pyway",
            "-fakehost",
            "colo.4467107.xyz",
            "-p",
            ":9195",
            "-log",
            "INFO",
            "-W",
            "1024",
            "--socket-buffer",
            "4096",
            "-block-local",
            "-tui",
            "-dns",
            "8.8.8.8",
            "-mux-sessions",
            "8",
        ];
        let normalized = normalize_legacy_args(raw);
        let args = Args::parse_from(&normalized);
        assert_eq!(args.key.as_deref(), Some("a6835181"));
        assert_eq!(
            args.upstream.as_deref(),
            Some("wss://172.64.229.105:443/pyway")
        );
        assert_eq!(args.fakehost.as_deref(), Some("colo.4467107.xyz"));
        assert_eq!(args.port.as_deref(), Some(":9195"));
        assert_eq!(args.log_level, "INFO");
        assert_eq!(args.buffer_kib, Some(1024));
        assert_eq!(args.socket_buffer_kib, Some(4096));
        assert!(args.block_local);
        assert!(args.tui);
        assert_eq!(args.dns.as_deref(), Some("8.8.8.8"));
        assert_eq!(args.mux_sessions, 8);
    }
    #[test]
    fn default_max_connections_is_1500() {
        let cfg = RuntimeConfig::default();
        assert_eq!(cfg.max_connections, 1500);
    }
}
