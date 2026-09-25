//! Plain WebSocket 1:1 relay compatibility path.

use crate::crypto::XorCipher;
use crate::dns::{resolve_all_ipv4, resolve_socket};
use crate::proxy::{
    parse_authority_with_default, parse_target_authority, read_client_proxy_request,
    socks5_success_response, ClientProxyRequest, SocksCommand,
};
use crate::runtime::{
    drain_join_set, enforce_target_policy, recycle_buf, relay_buf, wait_shutdown, RuntimeConfig,
};
use crate::udp_relay::handle_local_udp_proxy;
use crate::ws::{
    build_client_handshake_request, build_server_handshake_response, read_frame, read_http_headers,
    validate_client_handshake_response, validate_server_handshake, write_frame,
    write_frame_borrowed,
};
use anyhow::{anyhow, bail, Result};
use socket2::SockRef;
use std::sync::Arc;
use std::time::Instant;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::{Mutex, Semaphore};
use tokio::time::{timeout, Duration};

fn cipher(key: &Option<String>) -> XorCipher {
    XorCipher::new(key.as_deref().unwrap_or(""))
}
fn parse_ws_url(input: &str) -> Result<(String, String)> {
    let rest = input
        .strip_prefix("ws://")
        .ok_or_else(|| anyhow!("non-MUX plain client requires ws:// upstream"))?;
    let (authority, path) = match rest.split_once('/') {
        Some((a, p)) => (a.to_string(), format!("/{}", p)),
        None => (rest.to_string(), "/".to_string()),
    };
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
        for edge in resolve_all_ipv4(sni).await? {
            let candidate = std::net::SocketAddr::new(std::net::IpAddr::V4(edge), target_port);
            if candidate.ip() == primary.ip() {
                continue;
            }
            tracing::info!(
                "[DNS] Trying fallback Cloudflare edge: {} (fakehost: {})",
                candidate,
                sni
            );
            match timeout(conn_timeout, TcpStream::connect(candidate)).await {
                Ok(Ok(socket)) => return Ok(socket),
                Ok(Err(e)) => {
                    tracing::debug!(%candidate, error=%e, "[WS] Fallback edge dial failed")
                }
                Err(_) => tracing::debug!(%candidate, "[WS] Fallback edge dial timed out"),
            }
        }
        bail!("primary {primary} unreachable and all Cloudflare fallback edges failed for fakehost: {sni}");
    }
    Err(failed)
}
fn apply_socket_options(stream: &TcpStream, cfg: &RuntimeConfig) {
    crate::runtime::apply_socket_options_raw(
        stream,
        cfg.tcp_nodelay,
        cfg.socket_buffer,
        cfg.tcp_keepalive,
    );
}

async fn open_upstream(
    cfg: &RuntimeConfig,
) -> Result<(
    tokio::io::ReadHalf<TcpStream>,
    Arc<Mutex<tokio::io::WriteHalf<TcpStream>>>,
)> {
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
    let (mut rd, mut wr) = tokio::io::split(tcp);
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
    Ok((rd, Arc::new(Mutex::new(wr))))
}

/// Pre-warmed non-MUX upstream pool, mirroring GoWay `ConnPool`.
///
/// Each pooled transport has completed TCP and the WebSocket upgrade
/// handshake but has NOT sent the target frame yet. Transports are
/// single-use: a grabbed transport is relayed once and then closed, while
/// a background task refills the pool (GoWay parity: the pool is a
/// pre-warmed dial cache, not a reuse pool).
struct PooledUpstream {
    rd: tokio::io::ReadHalf<TcpStream>,
    wr: Arc<Mutex<tokio::io::WriteHalf<TcpStream>>>,
    created: Instant,
    last_used: Instant,
}

const NON_MUX_POOL_SIZE: usize = 4;
const NON_MUX_MAX_AGE: Duration = Duration::from_secs(5 * 60);
const NON_MUX_IDLE_TIMEOUT: Duration = Duration::from_secs(30);
const NON_MUX_MAINTAIN_INTERVAL: Duration = Duration::from_secs(5);

fn pooled_usable(created: Instant, last_used: Instant, now: Instant) -> bool {
    now.duration_since(created) <= NON_MUX_MAX_AGE
        && now.duration_since(last_used) <= NON_MUX_IDLE_TIMEOUT
}

struct NonMuxPool {
    cfg: RuntimeConfig,
    conns: Mutex<Vec<PooledUpstream>>,
    creation: Mutex<()>,
}

impl NonMuxPool {
    fn new(cfg: RuntimeConfig) -> Arc<Self> {
        Arc::new(Self {
            cfg,
            conns: Mutex::new(Vec::new()),
            creation: Mutex::new(()),
        })
    }

    async fn dial_pooled(cfg: &RuntimeConfig) -> Result<PooledUpstream> {
        let (rd, wr) = open_upstream(cfg).await?;
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
    async fn get_or_dial(
        self: &Arc<Self>,
    ) -> Result<(
        tokio::io::ReadHalf<TcpStream>,
        Arc<Mutex<tokio::io::WriteHalf<TcpStream>>>,
    )> {
        let pooled = {
            let mut conns = self.conns.lock().await;
            let now = Instant::now();
            conns.retain(|c| pooled_usable(c.created, c.last_used, now));
            conns.pop()
        };
        if let Some(mut c) = pooled {
            c.last_used = Instant::now();
            tracing::debug!("[non-MUX] using pre-warmed upstream connection");
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
                conns.retain(|c| pooled_usable(c.created, c.last_used, Instant::now()));
                NON_MUX_POOL_SIZE.saturating_sub(conns.len())
            };
            if need == 0 {
                return;
            }
            match Self::dial_pooled(&self.cfg).await {
                Ok(c) => self.conns.lock().await.push(c),
                // GoWay parity: stop refilling for this tick on first
                // failure instead of hammering a downed upstream.
                Err(error) => {
                    tracing::debug!(error=%error, "[non-MUX] pre-warm dial failed; will retry next tick");
                    return;
                }
            }
        }
    }

    async fn maintain(self: Arc<Self>) {
        loop {
            self.replenish().await;
            tokio::time::sleep(NON_MUX_MAINTAIN_INTERVAL).await;
        }
    }
}

async fn relay_client(
    mut local: TcpStream,
    cfg: RuntimeConfig,
    req: ClientProxyRequest,
    pool: Arc<NonMuxPool>,
) -> Result<()> {
    enforce_target_policy(&cfg, &req.target)?;
    let (mut rd, writer) = pool.get_or_dial().await?;
    let c = cipher(&cfg.key);
    let mut hello = format!("{}:{}\n", req.target.host, req.target.port).into_bytes();
    c.apply(&mut hello);
    {
        let mut w = writer.lock().await;
        write_frame(&mut *w, &hello, 2, true).await?
    };
    let mut frame_buf = Vec::with_capacity(64 * 1024);
    let Some((opcode, mut ok)) = read_frame(
        &mut rd,
        Option::<&mut tokio::io::WriteHalf<TcpStream>>::None,
        &mut frame_buf,
    )
    .await?
    else {
        bail!("upstream closed before non-MUX OK")
    };
    if opcode != 2 {
        bail!("invalid non-MUX handshake opcode")
    };
    c.apply(&mut ok);
    if ok != b"OK\n" {
        bail!("upstream rejected non-MUX target")
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
    // GoWay v1.8.5 non-MUX TCP: only the target handshake ("host:port\n")
    // and "OK\n" are XOR-encrypted. All subsequent data frames are plaintext.
    if let Some(initial) = req.initial_payload {
        let payload = initial;
        let mut w = writer_up.lock().await;
        write_frame(&mut *w, &payload, 2, true).await?;
    }
    let mut upload = tokio::spawn(async move {
        let mut buf = relay_buf(cfg.buffer_size).await;
        loop {
            let n = local_rd.read(&mut buf).await?;
            if n == 0 {
                break;
            }
            crate::stats::add_bytes(n as i64, 0);
            let mut w = writer_up.lock().await;
            write_frame_borrowed(&mut *w, &mut buf[..n], 2, true).await?
        }
        recycle_buf(buf).await;
        Result::<()>::Ok(())
    });
    tokio::select! {
        _ = &mut upload => {}
        _ = async {
            loop {
                let Some((opcode, payload)) = read_frame(
                    &mut rd,
                    Option::<&mut tokio::io::WriteHalf<TcpStream>>::None,
                    &mut frame_buf,
                )
                .await?
                else {
                    break;
                };
                match opcode {
                    2 => {
                        // Non-MUX data frames are plaintext per GoWay server.
                        if local_wr.write_all(&payload).await.is_err() {
                            break;
                        }
                        crate::stats::add_bytes(0, payload.len() as i64);
                    }
                    8 => break,
                    _ => {}
                }
            }
            Result::<()>::Ok(())
        } => {}
    }
    upload.abort();
    Ok(())
}
async fn handle_client_connection(
    mut local: TcpStream,
    cfg: RuntimeConfig,
    pool: Arc<NonMuxPool>,
) -> Result<()> {
    let req = read_client_proxy_request(&mut local).await?;
    if req.command == SocksCommand::UdpAssociate {
        return handle_local_udp_proxy(local, cfg, req.target).await;
    }
    if req.command != SocksCommand::Connect {
        bail!("non-MUX client supports CONNECT or UDP ASSOCIATE only")
    }
    relay_client(local, cfg, req, pool).await
}
pub async fn run_client(cfg: RuntimeConfig) -> Result<()> {
    let pool = NonMuxPool::new(cfg.clone());
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
                let (stream, peer) = res?;
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
                let (stream, peer) = res?;
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
    let (mut rd, mut wr) = tokio::io::split(stream);
    let request = read_http_headers(&mut rd).await?;
    let key = validate_server_handshake(&request)?;
    wr.write_all(&build_server_handshake_response(&key)).await?;
    wr.flush().await?;
    let writer = Arc::new(Mutex::new(wr));
    let mut frame_buf = Vec::with_capacity(64 * 1024);
    let Some((opcode, mut target_frame)) = read_frame(
        &mut rd,
        Option::<&mut tokio::io::WriteHalf<TcpStream>>::None,
        &mut frame_buf,
    )
    .await?
    else {
        bail!("missing non-MUX target")
    };
    if opcode != 2 {
        bail!("invalid non-MUX target opcode")
    };
    let c = cipher(&cfg.key);
    c.apply(&mut target_frame);
    let target = String::from_utf8(target_frame)
        .map_err(|_| anyhow!("invalid non-MUX target UTF-8"))?
        .trim()
        .to_string();
    let target_addr = parse_target_authority(&target).map_err(|e| anyhow!(e.to_string()))?;
    let resolved = resolve_socket(&target_addr.host, target_addr.port).await?;
    let target_stream = timeout(
        Duration::from_secs(cfg.connection_timeout.max(1)),
        TcpStream::connect(resolved),
    )
    .await??;
    apply_socket_options(&target_stream, &cfg);
    let mut ok = b"OK\n".to_vec();
    c.apply(&mut ok);
    {
        let mut w = writer.lock().await;
        write_frame(&mut *w, &ok, 2, false).await?
    };
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
            // Non-MUX data frames are plaintext per GoWay server.
            let mut w = writer_down.lock().await;
            write_frame_borrowed(&mut *w, &mut buf[..n], 2, false).await?
        }
        recycle_buf(buf).await;
        Result::<()>::Ok(())
    });
    loop {
        tokio::select! {
            _ = &mut download => {
                break;
            }
            res = read_frame(
                &mut rd,
                Option::<&mut tokio::io::WriteHalf<TcpStream>>::None,
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
                // Non-MUX data frames are plaintext per GoWay server.
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
