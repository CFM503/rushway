//! GoWay v1.8.4-compatible QUIC transport.

use crate::dns;
use crate::proxy::{
    parse_authority_with_default, parse_socks5_udp_datagram, parse_target_authority,
    read_client_proxy_request, socks5_failure_response, socks5_success_response,
    ClientProxyRequest, SocksCommand, TargetAddr,
};
use crate::runtime::{
    apply_socket_options, drain_join_set, enforce_target_policy, recycle_buf, relay_buf,
    wait_shutdown, RuntimeConfig,
};
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
use std::sync::Arc;
use std::time::Duration;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream, UdpSocket};
use tokio::sync::{Mutex, Semaphore};
use tokio::time::timeout;

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
    sock.bind(&bind.into())
        .context("QUIC UDP bind failed")?;
    Ok(sock.into())
}

fn transport_config() -> Arc<TransportConfig> {
    let mut cfg = TransportConfig::default();
    cfg.max_idle_timeout(Some(Duration::from_secs(60).try_into().expect("60s fits")));
    cfg.keep_alive_interval(Some(Duration::from_secs(15)));
    cfg.stream_receive_window(VarInt::from_u64(8 * 1024 * 1024).expect("8MiB fits QUIC VarInt"));
    cfg.receive_window(VarInt::from_u64(16 * 1024 * 1024).expect("16MiB fits QUIC VarInt"));
    cfg.max_concurrent_uni_streams(0u32.into());
    // Without path MTU discovery every datagram stays near 1200 bytes;
    // discovering larger MTUs cuts per-packet crypto/scheduling overhead.
    cfg.mtu_discovery_config(Some(quinn::MtuDiscoveryConfig::default()));
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
    let cert = generate_simple_self_signed(vec!["localhost".into()])
        .context("generate QUIC certificate")?;
    let key = PrivateKeyDer::Pkcs8(PrivatePkcs8KeyDer::from(cert.key_pair.serialize_der()));
    let cert_der: CertificateDer<'static> = cert.cert.der().clone();
    let mut rustls = rustls::ServerConfig::builder()
        .with_no_client_auth()
        .with_single_cert(vec![cert_der], key)
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
async fn read_len_prefixed_udp(
    recv: &mut RecvStream,
    buf: &mut Vec<u8>,
) -> Result<Option<Vec<u8>>> {
    let mut len_buf = [0u8; 2];
    match recv.read_exact(&mut len_buf).await {
        Ok(()) => {}
        Err(_) => return Ok(None),
    }
    let len = u16::from_be_bytes(len_buf) as usize;
    if len > buf.capacity() {
        buf.reserve(len - buf.capacity())
    }
    buf.resize(len, 0);
    recv.read_exact(buf).await?;
    Ok(Some(buf.clone()))
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
#[derive(Clone)]
struct QuicClientPool {
    endpoint: Endpoint,
    server_addr: std::net::SocketAddr,
    server_name: String,
    connection: Arc<Mutex<Option<Connection>>>,
    timeout_secs: u64,
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
        })
    }
    async fn open_bi(&self) -> Result<(SendStream, RecvStream)> {
        for _ in 0..2 {
            let connection = {
                let mut guard = self.connection.lock().await;
                if let Some(existing) = guard.clone() {
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
                    self.connection.lock().await.take();
                }
            }
        }
        bail!("QUIC pooled connection unavailable")
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
            if req.is_socks5 {
                local.write_all(&socks5_failure_response()).await?
            } else {
                local.write_all(b"HTTP/1.1 502 Bad Gateway\r\nConnection: close\r\nContent-Length: 0\r\n\r\n").await?
            }
            local.shutdown().await.ok();
            return Err(error);
        }
    };
    let header = if let Some(key) = cfg.key.as_deref() {
        format!("{} {}\n", key, format_target(&req.target))
    } else {
        format!("{}\n", format_target(&req.target))
    };
    send.write_all(header.as_bytes()).await?;
    let response = read_quic_line(&mut recv, 8192).await?;
    if response != "OK" {
        if req.is_socks5 {
            local.write_all(&socks5_failure_response()).await?
        } else {
            local
                .write_all(
                    b"HTTP/1.1 502 Bad Gateway\r\nConnection: close\r\nContent-Length: 0\r\n\r\n",
                )
                .await?
        }
        local.shutdown().await.ok();
        bail!("QUIC upstream rejected target: {}", response)
    }
    if req.is_socks5 {
        local.write_all(&socks5_success_response()).await?
    } else if req.is_connect {
        local
            .write_all(b"HTTP/1.1 200 Connection Established\r\n\r\n")
            .await?
    }
    if let Some(initial) = req.initial_payload {
        send.write_all(&initial).await?;
    }
    let (lr, lw) = tokio::io::split(local);
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
            Some(n) => lw2.write_all(&buf[..n]).await?,
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
    let (mut send, mut recv) = pool.open_bi().await?;
    let header = if let Some(key) = cfg.key.as_deref() {
        format!("{} UDP\n", key)
    } else {
        "UDP\n".to_string()
    };
    send.write_all(header.as_bytes()).await?;
    let ok = read_quic_line(&mut recv, 8192).await?;
    if ok != "OK" {
        bail!("QUIC UDP upstream rejected: {}", ok)
    }
    let latest = Arc::new(tokio::sync::Mutex::new(None::<std::net::SocketAddr>));
    let udp_send = udp.clone();
    let latest_send = latest.clone();
    let mut send_task = send;
    let upload = tokio::spawn(async move {
        let mut buf = [0u8; 64 * 1024];
        while let Ok((n, peer)) = udp_send.recv_from(&mut buf).await {
            *latest_send.lock().await = Some(peer);
            if write_len_prefixed_udp(&mut send_task, &buf[..n]).await.is_err() {
                break;
            }
        }
        Ok::<(), anyhow::Error>(())
    });
    let mut frame_buf = Vec::with_capacity(64 * 1024);
    let mut dummy = [0u8; 1];
    tokio::select! {
        _ = control.read(&mut dummy) => {}
        _ = async {
            loop {
                let Some(packet) = read_len_prefixed_udp(&mut recv, &mut frame_buf).await? else {
                    break;
                };
                let (target, _payload) =
                    parse_socks5_udp_datagram(&packet).map_err(|e| anyhow!(e.to_string()))?;
                if enforce_target_policy(&cfg, &target).is_err() {
                    continue;
                }
                if let Some(peer) = *latest.lock().await {
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
    tracing::debug!(bytes = socket_buf, "QUIC client UDP socket buffers requested");
    let pool = QuicClientPool::new(endpoint, server_addr, server_name, cfg.connection_timeout);
    let listener = TcpListener::bind(format!("{}:{}", cfg.proxy_host, cfg.proxy_port)).await?;
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
                let (mut stream, peer) = res?;
                let permit = match semaphore.clone().try_acquire_owned() {
                    Ok(v) => v,
                    Err(_) => {
                        tracing::debug!(%peer,"maximum QUIC client connections reached");
                        continue;
                    }
                };
                let req = match read_client_proxy_request(&mut stream).await {
                    Ok(v) => v,
                    Err(e) => {
                        drop(permit);
                        tracing::debug!(%peer,error=%e,"QUIC proxy request rejected");
                        continue;
                    }
                };
                let cfg2 = cfg.clone();
                let pool2 = pool.clone();
                set.spawn(async move {
                    let _permit = permit;
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
    tracing::debug!(bytes = socket_buf, "QUIC server UDP socket buffers requested");
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
                    match incoming.await {
                        Ok(connection) => {
                            let _permit = permit;
                            loop {
                                match connection.accept_bi().await {
                                    Ok((send, recv)) => {
                                        let cfg3 = cfg2.clone();
                                        tokio::spawn(async move {
                                            if let Err(error) = handle_server_stream(send, recv, cfg3).await
                                            {
                                                tracing::debug!(%error,"QUIC stream closed")
                                            }
                                        });
                                    }
                                    Err(_) => break,
                                }
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
    let target_str = if let Some(key) = cfg.key.as_deref() {
        let mut parts = line.splitn(2, ' ');
        let k = parts.next().unwrap_or("");
        let rest = parts.next().unwrap_or("");
        if k != key {
            send.write_all(b"ERR: AUTH_FAILED\n").await?;
            let _ = send.finish();
            bail!("QUIC authentication failed");
        }
        rest
    } else {
        line.as_str()
    };
    if target_str.eq_ignore_ascii_case("UDP") || target_str.to_ascii_uppercase().starts_with("UDP") {
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
    if cfg.upstream.is_some() && enforce_target_policy(&cfg, &target_addr).is_err() {
        send.write_all(b"ERR: DIAL_FAILED\n").await?;
        let _ = send.finish();
        return Ok(());
    }
    let target_socket = dns::resolve_socket(&target_addr.host, target_addr.port).await?;
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
    let (mut target_rd, mut target_wr) = tokio::io::split(target_stream);
    send.write_all(b"OK\n").await?;
    let buffer_size = cfg.buffer_size;
    let mut send_task = tokio::spawn(async move {
        let mut buf = relay_buf(buffer_size).await;
        loop {
            match target_rd.read(&mut buf).await {
                Ok(0) | Err(_) => break,
                Ok(n) => {
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
                    Some(n) => target_wr.write_all(&buf[..n]).await?,
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
    let udp_send = udp.clone();
    let mut sender = tokio::spawn(async move {
        let mut buf = [0u8; 64 * 1024];
        while let Ok((n, src)) = udp_send.recv_from(&mut buf).await {
            let mut packet = Vec::with_capacity(22 + n);
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
            packet.extend_from_slice(&buf[..n]);
            if write_len_prefixed_udp(&mut send, &packet).await.is_err() {
                break;
            }
        }
        Ok::<(), anyhow::Error>(())
    });
    let mut frame_buf = Vec::with_capacity(64 * 1024);
    tokio::select! {
        _ = &mut sender => {}
        _ = async {
            while let Some(packet) = read_len_prefixed_udp(&mut recv, &mut frame_buf).await? {
                let (target, payload) =
                    parse_socks5_udp_datagram(&packet).map_err(|e| anyhow!(e.to_string()))?;
                if cfg.upstream.is_some() && enforce_target_policy(&cfg, &target).is_err() {
                    continue;
                }
                let addr = dns::resolve_socket(&target.host, target.port).await?;
                let _ = udp.send_to(payload, addr).await;
            }
            Ok::<(), anyhow::Error>(())
        } => {}
    }
    sender.abort();
    Ok(())
}
