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
    drain_join_set, enforce_target_policy, recycle_buf, relay_buf, resolve_target, wait_shutdown,
    RuntimeConfig,
};
use crate::udp_relay::handle_local_udp_proxy;
use crate::ws::{
    build_client_handshake_request, build_server_handshake_response, read_frame, read_http_headers,
    validate_client_handshake_response, validate_server_handshake, write_frame,
    write_frame_borrowed,
};
use anyhow::{anyhow, bail, Result};
use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};
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
    tokio::io::WriteHalf<TcpStream>,
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
    Ok((rd, wr))
}

type PlainNonMuxPool = NonMuxPool<
    RuntimeConfig,
    tokio::io::ReadHalf<TcpStream>,
    tokio::io::WriteHalf<TcpStream>,
>;

/// Boxed adapter so the shared [`NonMuxPool`] can dial the plain-WS opener.
fn dial_pooled_plain(
    cfg: &RuntimeConfig,
) -> Pin<
    Box<
        dyn Future<
                Output = Result<(
                    tokio::io::ReadHalf<TcpStream>,
                    tokio::io::WriteHalf<TcpStream>,
                )>,
            > + Send
            + '_,
    >,
> {
    Box::pin(open_upstream(cfg))
}

const PLAIN_NON_MUX_RELAY: NonMuxRelaySpec = NonMuxRelaySpec {
    closed_error: "upstream closed before non-MUX OK",
    bad_opcode_error: "invalid non-MUX handshake opcode",
    ok_rejection: NonMuxOkRejection::Propagate("upstream rejected non-MUX target"),
    recycle_downstream_payloads: true,
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
    let (mut rd, mut wr) = tokio::io::split(stream);
    let request = read_http_headers(&mut rd).await?;
    let key = validate_server_handshake(&request)?;
    wr.write_all(&build_server_handshake_response(&key)).await?;
    wr.flush().await?;
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
    let c = configured_cipher(&cfg.key);
    c.apply(&mut target_frame);
    let target = String::from_utf8(target_frame)
        .map_err(|_| anyhow!("invalid non-MUX target UTF-8"))?
        .trim()
        .to_string();
    let target_addr = parse_target_authority(&target).map_err(|e| anyhow!(e.to_string()))?;
    // Server-side non-MUX had no target policy at all: `-block-local`
    // defaulted to true yet any authenticated peer could make this process
    // dial loopback / RFC1918 / link-local addresses.
    enforce_target_policy(&cfg, &target_addr)?;
    let resolved = resolve_target(cfg.block_local, &target_addr.host, target_addr.port).await?;
    let target_stream = timeout(
        Duration::from_secs(cfg.connection_timeout.max(1)),
        TcpStream::connect(resolved),
    )
    .await??;
    apply_socket_options(&target_stream, &cfg);
    let mut ok = b"OK\n".to_vec();
    c.apply(&mut ok);
    write_frame(&mut wr, &ok, 2, false).await?;
    let (mut target_rd, mut target_wr) = tokio::io::split(target_stream);
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
    let _ = target_wr.shutdown().await;
    download.abort();
    Ok(())
}
