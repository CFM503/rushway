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

pub(crate) const NON_MUX_POOL_SIZE: usize = 4;
pub(crate) const NON_MUX_MAX_AGE: Duration = Duration::from_secs(5 * 60);
pub(crate) const NON_MUX_IDLE_TIMEOUT: Duration = Duration::from_secs(30);
pub(crate) const NON_MUX_MAINTAIN_INTERVAL: Duration = Duration::from_secs(5);

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

/// Call-site specifics of [`relay_non_mux_ws`]: error texts plus the two
/// behaviors that are not identical between the plain-WS and WSS relays
/// (rejection handling and downstream payload recycling).
pub(crate) struct NonMuxRelaySpec {
    /// Error when the upstream closes before answering the target frame.
    pub(crate) closed_error: &'static str,
    /// Error when the OK frame's opcode is not binary.
    pub(crate) bad_opcode_error: &'static str,
    /// Reaction to an upstream rejection of the target frame.
    pub(crate) ok_rejection: NonMuxOkRejection,
    /// Whether downstream payloads are returned to the encode pool after the
    /// local write (plain-WS recycles them; WSS drops them).
    pub(crate) recycle_downstream_payloads: bool,
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
    let mut frame_buf = Vec::with_capacity(64 * 1024);
    let Some((opcode, mut ok)) = read_frame(&mut rd, Option::<&mut W>::None, &mut frame_buf).await?
    else {
        bail!(spec.closed_error)
    };
    if opcode != 2 {
        bail!(spec.bad_opcode_error)
    };
    c.apply(&mut ok);
    if ok != b"OK\n" {
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
    let (mut local_rd, mut local_wr) = tokio::io::split(local);
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
                        if spec.recycle_downstream_payloads {
                            crate::mux_writer::recycle_encode_buf(payload);
                        }
                        if write_res.is_err() {
                            break;
                        }
                        crate::stats::add_bytes(0, len as i64);
                    }
                    8 => {
                        if spec.recycle_downstream_payloads {
                            crate::mux_writer::recycle_encode_buf(payload);
                        }
                        break;
                    }
                    _ => {
                        if spec.recycle_downstream_payloads {
                            crate::mux_writer::recycle_encode_buf(payload);
                        }
                    }
                }
            }
            Result::<()>::Ok(())
        } => {}
    }
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
