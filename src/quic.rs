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
    let latest = Arc::new(tokio::sync::Mutex::new(None::<std::net::SocketAddr>));
    let udp_send = udp.clone();
    let latest_send = latest.clone();
    let mut send_task = send;
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
                crate::stats::add_bytes(pkt.len() as i64, 0);
                if write_len_prefixed_udp(&mut send_task, pkt).await.is_err() {
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
                if let Some(peer) = *latest.lock().await {
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
    let (mut target_rd, mut target_wr) = tokio::io::split(target_stream);
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
                crate::stats::add_bytes(0, pkt.len() as i64);
                let mut packet = Vec::with_capacity(22 + pkt.len());
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
                if write_len_prefixed_udp(&mut send, &packet).await.is_err() {
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
        _server_conn: Connection,
        client: (SendStream, RecvStream),
        server: (SendStream, RecvStream),
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
        let handshake = timeout(Duration::from_secs(5), async {
            let conn = client_ep.connect(server_addr, "localhost")?.await?;
            let client = conn.open_bi().await?;
            let Some(incoming) = server_ep.accept().await else {
                bail!("test QUIC server endpoint closed");
            };
            let server_conn = incoming.await?;
            let server = server_conn.accept_bi().await?;
            Ok((conn, client, server_conn, server))
        });
        let (client_conn, client, server_conn, server) = handshake
            .await
            .expect("QUIC test handshake timed out")?;
        Ok(QuicPair {
            _server_ep: server_ep,
            _client_ep: client_ep,
            _client_conn: client_conn,
            _server_conn: server_conn,
            client,
            server,
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
        let mut buf = Vec::with_capacity(8192);
        let n = read_len_prefixed_udp(&mut pair.server.1, &mut buf).await.unwrap().unwrap();
        assert_eq!(n, 5000);
        assert_eq!(&buf[..n], &payload[..]);
        // A second frame right behind the first: the length prefix must not
        // drift, and stale bytes beyond `n` must stay out of the slice.
        write_len_prefixed_udp(&mut pair.client.0, b"tail").await.unwrap();
        let n = read_len_prefixed_udp(&mut pair.server.1, &mut buf).await.unwrap().unwrap();
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
        let mut buf = Vec::with_capacity(1024);
        let n = read_len_prefixed_udp(&mut pair.server.1, &mut buf).await.unwrap().unwrap();
        assert_eq!(n, 0);
        assert!(buf.is_empty());
        // After the last frame, a clean end of stream reads as Ok(None).
        let eof = read_len_prefixed_udp(&mut pair.server.1, &mut buf).await.unwrap();
        assert!(eof.is_none());
    }

    #[tokio::test]
    async fn len_prefixed_max_payload_round_trips() {
        let mut pair = open_quic_pair().await.unwrap();
        // Exactly MAX_QUIC_UDP_PACKET bytes: the largest value the u16 prefix
        // can express and the write-side guard still accepts.
        let payload = vec![0xA5u8; MAX_QUIC_UDP_PACKET];
        write_len_prefixed_udp(&mut pair.client.0, &payload).await.unwrap();
        let mut buf = Vec::with_capacity(MAX_QUIC_UDP_PACKET);
        let n = read_len_prefixed_udp(&mut pair.server.1, &mut buf).await.unwrap().unwrap();
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
        let mut buf = Vec::with_capacity(1024);
        let n = read_len_prefixed_udp(&mut pair.server.1, &mut buf).await.unwrap().unwrap();
        assert_eq!(n, 2);
        assert_eq!(&buf[..n], &b"ok"[..]);
    }
}
