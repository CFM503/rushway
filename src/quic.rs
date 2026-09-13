//! GoWay v1.8.4-compatible QUIC transport.

use crate::crypto::XorCipher;
use crate::proxy::{parse_http_connect, parse_socks5_request, parse_socks5_udp_datagram, socks5_success_response, SocksCommand, TargetAddr, SOCKS5_CONNECT, SOCKS5_UDP_ASSOCIATE, SOCKS5_VERSION};
use crate::runtime::RuntimeConfig;
use anyhow::{anyhow, bail, Context, Result};
use quinn::crypto::rustls::{QuicClientConfig, QuicServerConfig};
use quinn::{ClientConfig, Connection, Endpoint, RecvStream, SendStream, ServerConfig, TransportConfig, VarInt};
use rcgen::generate_simple_self_signed;
use rustls::client::danger::{HandshakeSignatureValid, ServerCertVerified, ServerCertVerifier};
use rustls::pki_types::{CertificateDer, PrivateKeyDer, PrivatePkcs8KeyDer, ServerName, UnixTime};
use rustls::{ClientConfig as RustlsClientConfig, RootCertStore};
use std::sync::Arc;
use std::time::Duration;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream, UdpSocket};
use tokio::sync::{Mutex, Notify};
use tokio::time::timeout;

const ALPN: &[&[u8]] = &[b"goway-quic", b"h3"];

fn transport_config() -> Arc<TransportConfig> {
    let mut cfg = TransportConfig::default();
    cfg.max_idle_timeout(Some(Duration::from_secs(60).try_into().expect("60s fits QUIC varint")));
    cfg.keep_alive_interval(Some(Duration::from_secs(15)));
    cfg.initial_stream_receive_window(VarInt::from_u64(2 * 1024 * 1024));
    cfg.max_stream_receive_window(VarInt::from_u64(8 * 1024 * 1024));
    cfg.initial_connection_receive_window(VarInt::from_u64(4 * 1024 * 1024));
    cfg.max_connection_receive_window(VarInt::from_u64(16 * 1024 * 1024));
    cfg.max_concurrent_uni_streams(0u32.into());
    Arc::new(cfg)
}

fn insecure_client_crypto() -> Result<RustlsClientConfig> {
    let mut cfg = RustlsClientConfig::builder().dangerous().with_custom_certificate_verifier(Arc::new(QuicNoCertificateVerification)).with_no_client_auth();
    cfg.alpn_protocols = ALPN.iter().map(|p| p.to_vec()).collect();
    Ok(cfg)
}
fn verified_client_crypto() -> Result<RustlsClientConfig> {
    let mut roots = RootCertStore::empty();
    roots.extend(webpki_roots::TLS_SERVER_ROOTS.iter().cloned());
    let mut cfg = RustlsClientConfig::builder().with_root_certificates(roots).with_no_client_auth();
    cfg.alpn_protocols = ALPN.iter().map(|p| p.to_vec()).collect();
    Ok(cfg)
}
fn client_config(verify_ssl: bool) -> Result<ClientConfig> {
    let rustls = if verify_ssl { verified_client_crypto()? } else { insecure_client_crypto()? };
    let crypto = QuicClientConfig::try_from(rustls).context("build QUIC client crypto")?;
    let mut cfg = ClientConfig::new(Arc::new(crypto));
    cfg.transport_config(transport_config());
    Ok(cfg)
}
fn server_config() -> Result<ServerConfig> {
    let cert = generate_simple_self_signed(vec!["localhost".into()]).context("generate QUIC certificate")?;
    let key = PrivateKeyDer::Pkcs8(PrivatePkcs8KeyDer::from(cert.signing_key.serialize_der()));
    let cert_der: CertificateDer<'static> = cert.cert.der().clone();
    let mut rustls = rustls::ServerConfig::builder().with_no_client_auth().with_single_cert(vec![cert_der], key).context("build QUIC server TLS")?;
    rustls.alpn_protocols = ALPN.iter().map(|p| p.to_vec()).collect();
    let crypto = QuicServerConfig::try_from(rustls).context("build QUIC server crypto")?;
    let mut cfg = ServerConfig::with_crypto(Arc::new(crypto));
    cfg.transport_config(transport_config());
    Ok(cfg)
}

#[derive(Debug)]
struct QuicNoCertificateVerification;
impl ServerCertVerifier for QuicNoCertificateVerification {
    fn verify_server_cert(&self, _: &CertificateDer<'_>, _: &[CertificateDer<'_>], _: &ServerName<'_>, _: &[u8], _: UnixTime) -> std::result::Result<ServerCertVerified, rustls::Error> { Ok(ServerCertVerified::assertion()) }
    fn verify_tls12_signature(&self, _: &[u8], _: &CertificateDer<'_>, _: &rustls::DigitallySignedStruct) -> std::result::Result<HandshakeSignatureValid, rustls::Error> { Ok(HandshakeSignatureValid::assertion()) }
    fn verify_tls13_signature(&self, _: &[u8], _: &CertificateDer<'_>, _: &rustls::DigitallySignedStruct) -> std::result::Result<HandshakeSignatureValid, rustls::Error> { Ok(HandshakeSignatureValid::assertion()) }
    fn supported_verify_schemes(&self) -> Vec<rustls::SignatureScheme> { vec![rustls::SignatureScheme::ECDSA_NISTP256_SHA256, rustls::SignatureScheme::ECDSA_NISTP384_SHA384, rustls::SignatureScheme::ED25519, rustls::SignatureScheme::RSA_PSS_SHA256, rustls::SignatureScheme::RSA_PKCS1_SHA256] }
}

fn parse_upstream(input: &str) -> Result<String> {
    let rest = input.strip_prefix("quic://").or_else(|| input.strip_prefix("quic+tls://")).ok_or_else(|| anyhow!("QUIC client requires quic:// or quic+tls:// upstream"))?;
    let authority = match rest.split_once('/') { Some((a, _)) => a, None => rest };
    if authority.is_empty() { bail!("empty QUIC upstream authority"); }
    Ok(if authority.starts_with('[') || authority.matches(':').count() == 1 { authority.to_string() } else { format!("{}:443", authority) })
}
async fn resolve_upstream(input: &str) -> Result<(std::net::SocketAddr, String)> {
    let authority = parse_upstream(input)?;
    let host = if authority.starts_with('[') { let close = authority.find(']').ok_or_else(|| anyhow!("invalid QUIC IPv6 authority"))?; authority[1..close].to_string() } else { authority.rsplit_once(':').map(|(h, _)| h.to_string()).unwrap_or_else(|| authority.clone()) };
    let addr = tokio::net::lookup_host(authority).await?.next().ok_or_else(|| anyhow!("QUIC upstream DNS returned no address"))?;
    Ok((addr, host))
}
async fn read_quic_line(stream: &mut RecvStream, limit: usize) -> Result<String> {
    let mut buf = Vec::with_capacity(128);
    while buf.len() < limit { let mut one = [0u8; 1]; let n = stream.read(&mut one).await?; if n == 0 { bail!("QUIC stream closed while reading header"); } buf.push(one[0]); if one[0] == b'\n' { return Ok(String::from_utf8(buf)?.trim().to_string()); } }
    bail!("QUIC header too large")
}
fn authenticated_target(line: &str, key: &Option<String>) -> Result<String> {
    if let Some(expected) = key { let (got, target) = line.split_once(' ').ok_or_else(|| anyhow!("QUIC authentication header missing key"))?; if got != expected { bail!("QUIC authentication failed"); } return Ok(target.trim().to_string()); }
    Ok(line.trim().to_string())
}
fn cipher(key: &Option<String>) -> XorCipher { XorCipher::new(key.as_deref().unwrap_or("")) }
async fn read_framed_udp(recv: &mut RecvStream, buf: &mut Vec<u8>) -> Result<Option<Vec<u8>>> {
    let mut len_buf = [0u8; 2];
    if recv.read_exact(&mut len_buf).await.is_err() { return Ok(None); }
    let len = u16::from_be_bytes(len_buf) as usize;
    if len > buf.capacity() { buf.reserve(len - buf.capacity()); }
    buf.resize(len, 0);
    recv.read_exact(buf).await?;
    Ok(Some(buf.clone()))
}
async fn write_framed_udp(send: &mut SendStream, payload: &[u8]) -> Result<()> {
    if payload.len() > u16::MAX as usize { bail!("QUIC UDP payload too large"); }
    send.write_all(&(payload.len() as u16).to_be_bytes()).await?;
    send.write_all(payload).await?;
    Ok(())
}

#[derive(Clone)]
struct QuicClientPool { endpoint: Endpoint, server_addr: std::net::SocketAddr, server_name: String, connection: Arc<Mutex<Option<Connection>>>, dialing: Arc<Mutex<bool>>, ready: Arc<Notify>, timeout_secs: u64 }
impl QuicClientPool {
    fn new(endpoint: Endpoint, server_addr: std::net::SocketAddr, server_name: String, timeout_secs: u64) -> Arc<Self> { Arc::new(Self { endpoint, server_addr, server_name, connection: Arc::new(Mutex::new(None)), dialing: Arc::new(Mutex::new(false)), ready: Arc::new(Notify::new()), timeout_secs }) }
    async fn connect_singleflight(&self) -> Result<Connection> {
        loop {
            if let Some(conn) = self.connection.lock().await.clone() { return Ok(conn); }
            let mut dialing = self.dialing.lock().await;
            if *dialing { drop(dialing); self.ready.notified().await; continue; }
            *dialing = true; drop(dialing);
            let result = async { let connecting = self.endpoint.connect(self.server_addr, &self.server_name)?; let conn = timeout(Duration::from_secs(self.timeout_secs.max(1)), connecting).await??; Ok::<Connection, anyhow::Error>(conn) }.await;
            if let Ok(ref conn) = result { *self.connection.lock().await = Some(conn.clone()); }
            *self.dialing.lock().await = false;
            self.ready.notify_waiters();
            return result;
        }
    }
    async fn open_bi(&self) -> Result<(SendStream, RecvStream)> {
        for _ in 0..2 { let conn = self.connect_singleflight().await?; match conn.open_bi().await { Ok(pair) => return Ok(pair), Err(_) => { self.connection.lock().await.take(); } } }
        bail!("QUIC pooled connection unavailable")
    }
}

async fn read_proxy_request(stream: &mut TcpStream) -> Result<(SocksCommand, TargetAddr, bool)> {
    let first = stream.read_u8().await?;
    if first == SOCKS5_VERSION { let n = stream.read_u8().await? as usize; let mut methods = vec![0u8; n]; stream.read_exact(&mut methods).await?; if !methods.contains(&0) { stream.write_all(&[5, 0xff]).await?; bail!("SOCKS5 no-auth unavailable"); } stream.write_all(&[5, 0]).await?; let mut head = [0u8; 4]; stream.read_exact(&mut head).await?; if head[1] != SOCKS5_CONNECT && head[1] != SOCKS5_UDP_ASSOCIATE { stream.write_all(&[5, 7, 0, 1, 0, 0, 0, 0, 0, 0]).await?; bail!("unsupported SOCKS5 command"); } let mut req = head.to_vec(); match head[3] { 1 => { let mut b = [0u8; 6]; stream.read_exact(&mut b).await?; req.extend_from_slice(&b); }, 3 => { let mut n = [0u8; 1]; stream.read_exact(&mut n).await?; req.extend_from_slice(&n); let mut b = vec![0u8; n[0] as usize + 2]; stream.read_exact(&mut b).await?; req.extend_from_slice(&b); }, 4 => { let mut b = [0u8; 18]; stream.read_exact(&mut b).await?; req.extend_from_slice(&b); }, _ => { stream.write_all(&[5, 8, 0, 1, 0, 0, 0, 0, 0, 0]).await?; bail!("unsupported SOCKS5 address type"); } } let parsed = parse_socks5_request(&req).map_err(|e| anyhow!(e.to_string()))?; return Ok((parsed.command, parsed.target, true)); }
    if first == b'C' { let mut buf = vec![first]; let mut tail = crate::ws::read_http_headers(stream).await?; buf.append(&mut tail); return Ok((SocksCommand::Connect, parse_http_connect(&buf).map_err(|e| anyhow!(e.to_string()))?, false)); }
    bail!("unsupported local proxy protocol")
}
fn format_target(target: &TargetAddr) -> String { format!("{}:{}", target.host, target.port) }

async fn relay_quic(mut local: TcpStream, cfg: RuntimeConfig, pool: Arc<QuicClientPool>, target: TargetAddr, is_socks5: bool) -> Result<()> {
    crate::runtime::enforce_target_policy(&cfg, &target)?;
    let (mut send, mut recv) = pool.open_bi().await?;
    let header = if let Some(key) = cfg.key.as_deref() { format!("{} {}\n", key, format_target(&target)) } else { format!("{}\n", format_target(&target)) };
    send.write_all(header.as_bytes()).await?;
    let response = read_quic_line(&mut recv, 8192).await?;
    if response != "OK" { bail!("QUIC upstream rejected target: {}", response); }
    if is_socks5 { local.write_all(&socks5_success_response()).await?; } else { local.write_all(b"HTTP/1.1 200 Connection Established\r\n\r\n").await?; }
    let (lr, lw) = tokio::io::split(local); let buffer_size = cfg.buffer_size.clamp(16 * 1024, 1024 * 1024);
    let upload = tokio::spawn(async move { let mut r = lr; let mut buf = vec![0u8; buffer_size]; loop { let n = r.read(&mut buf).await?; if n == 0 { let _ = send.finish(); break; } send.write_all(&buf[..n]).await?; } Result::<()>::Ok(()) });
    let mut lw2 = lw; let mut buf = vec![0u8; buffer_size]; loop { let n = recv.read(&mut buf).await?; if n == 0 { break; } lw2.write_all(&buf[..n]).await?; }
    let _ = lw2.shutdown().await; upload.abort(); Ok(())
}

async fn relay_quic_udp(mut control: TcpStream, cfg: RuntimeConfig, pool: Arc<QuicClientPool>) -> Result<()> {
    let udp = Arc::new(UdpSocket::bind("0.0.0.0:0").await?); let bound = udp.local_addr()?; let mut resp = [0u8; 10]; resp[0] = 5; resp[1] = 0; resp[2] = 0; resp[3] = 1; if let std::net::IpAddr::V4(ip) = bound.ip() { resp[4..8].copy_from_slice(&ip.octets()); } resp[8..10].copy_from_slice(&bound.port().to_be_bytes()); control.write_all(&resp).await?;
    let (mut send, mut recv) = pool.open_bi().await?; let c = cipher(&cfg.key); let mut hello = if let Some(key) = cfg.key.as_deref() { format!("{} UDP\n", key).into_bytes() } else { b"UDP\n".to_vec() }; c.apply(&mut hello); send.write_all(&hello).await?;
    let ok = read_quic_line(&mut recv, 8192).await?; if ok != "OK" { bail!("QUIC UDP upstream rejected: {}", ok); }
    let latest = Arc::new(Mutex::new(None::<std::net::SocketAddr>)); let udp_send = udp.clone(); let latest_send = latest.clone(); let c_upload = c.clone();
    let mut send_task = send;
    let upload = tokio::spawn(async move { let mut buf = [0u8; 64 * 1024]; loop { let (n, peer) = udp_send.recv_from(&mut buf).await?; *latest_send.lock().await = Some(peer); let mut packet = buf[..n].to_vec(); c_upload.apply(&mut packet); write_framed_udp(&mut send_task, &packet).await?; } Result::<()>::Ok(()) });
    let mut frame_buf = Vec::with_capacity(64 * 1024); loop { let packet = read_framed_udp(&mut recv, &mut frame_buf).await?; let Some(mut packet) = packet else { break; }; c.apply(&mut packet); let (target, payload) = parse_socks5_udp_datagram(&packet).map_err(|e| anyhow!(e.to_string()))?; if cfg.block_local && crate::runtime::is_blocked_local_host(&target.host) { continue; } if let Some(peer) = *latest.lock().await { let _ = udp.send_to(payload, peer).await; } }
    upload.abort(); control.shutdown().await.ok(); Ok(())
}

pub async fn run_client(cfg: RuntimeConfig, verify_ssl: bool) -> Result<()> {
    let upstream = cfg.upstream.clone().ok_or_else(|| anyhow!("QUIC client requires upstream"))?; let (server_addr, server_name) = resolve_upstream(&upstream).await?; let mut endpoint = Endpoint::client("[::]:0".parse().unwrap())?; endpoint.set_default_client_config(client_config(verify_ssl)?); let pool = QuicClientPool::new(endpoint, server_addr, server_name, cfg.connection_timeout);
    let listener = TcpListener::bind(format!("{}:{}", cfg.proxy_host, cfg.proxy_port)).await?; tracing::info!("RushWay QUIC client proxy listening on {}:{}", cfg.proxy_host, cfg.proxy_port);
    loop { let (mut stream, peer) = listener.accept().await?; let (request_command, target, is_socks5) = match read_proxy_request(&mut stream).await { Ok(v) => v, Err(e) => { tracing::debug!(%peer, error=%e, "QUIC proxy request rejected"); continue; } }; let cfg2 = cfg.clone(); let pool2 = pool.clone(); tokio::spawn(async move { match request_command { SocksCommand::UdpAssociate => { if let Err(e) = relay_quic_udp(stream, cfg2, pool2).await { tracing::debug!(%peer, error=%e, "QUIC UDP proxy closed"); } }, SocksCommand::Connect => { if let Err(e) = relay_quic(stream, cfg2, pool2, target, is_socks5).await { tracing::debug!(%peer, error=%e, "QUIC proxy connection closed"); } } } }); }
}

pub async fn run_server(cfg: RuntimeConfig) -> Result<()> {
    let bind: std::net::SocketAddr = format!("{}:{}", cfg.proxy_host, cfg.proxy_port).parse()?; let endpoint = Endpoint::server(server_config()?, bind)?; tracing::info!("RushWay QUIC server listening on {}", bind);
    while let Some(incoming) = endpoint.accept().await { let cfg2 = cfg.clone(); tokio::spawn(async move { match incoming.await { Ok(connection) => { loop { match connection.accept_bi().await { Ok((send, recv)) => { let cfg3 = cfg2.clone(); tokio::spawn(async move { if let Err(error) = handle_server_stream(send, recv, cfg3).await { tracing::debug!(%error, "QUIC stream closed"); } }); }, Err(_) => break } } }, Err(error) => tracing::debug!(%error, "QUIC connection handshake failed") } }); }
    Ok(())
}

async fn handle_server_stream(mut send: SendStream, mut recv: RecvStream, cfg: RuntimeConfig) -> Result<()> {
    let line = read_quic_line(&mut recv, 8192).await?;
    if line.eq_ignore_ascii_case("UDP") || line.eq_ignore_ascii_case("UDP ") || line.to_ascii_uppercase().starts_with("UDP ") { return handle_server_udp_stream(send, recv, cfg).await; }
    let target = authenticated_target(&line, &cfg.key)?; let (host, port_text) = target.rsplit_once(':').ok_or_else(|| anyhow!("invalid QUIC target"))?; let port = port_text.parse::<u16>().map_err(|_| anyhow!("invalid QUIC target port"))?; let target = TargetAddr { host: host.to_string(), port }; crate::runtime::enforce_target_policy(&RuntimeConfig { upstream: Some("quic://".into()), ..cfg.clone() }, &target)?;
    let target_stream = match tokio::time::timeout(Duration::from_secs(cfg.connection_timeout.max(1)), TcpStream::connect(format!("{}:{}", target.host, target.port))).await { Ok(Ok(s)) => s, _ => { send.write_all(b"ERR: DIAL_FAILED\n").await?; send.finish().await?; return Ok(()); } };
    let (mut target_rd, mut target_wr) = tokio::io::split(target_stream); send.write_all(b"OK\n").await?; let buffer_size = cfg.buffer_size.clamp(16 * 1024, 1024 * 1024);
    let send_task = tokio::spawn(async move { let mut buf = vec![0u8; buffer_size]; loop { let n = target_rd.read(&mut buf).await?; if n == 0 { break; } send.write_all(&buf[..n]).await?; } let _ = send.finish(); Result::<()>::Ok(()) });
    let mut buf = vec![0u8; buffer_size]; loop { let n = recv.read(&mut buf).await?; if n == 0 { break; } target_wr.write_all(&buf[..n]).await?; } target_wr.shutdown().await?; send_task.abort(); Ok(())
}

async fn handle_server_udp_stream(mut send: SendStream, mut recv: RecvStream, cfg: RuntimeConfig) -> Result<()> {
    send.write_all(b"OK\n").await?; let udp = Arc::new(UdpSocket::bind("0.0.0.0:0").await?); let udp_send = udp.clone(); let c_down = cipher(&cfg.key);
    let sender = tokio::spawn(async move { let mut buf = [0u8; 64 * 1024]; loop { let (n, src) = udp_send.recv_from(&mut buf).await?; let mut packet = Vec::with_capacity(22 + n); packet.extend_from_slice(&[0, 0, 0]); match src.ip() { std::net::IpAddr::V4(ip) => { packet.push(1); packet.extend_from_slice(&ip.octets()); }, std::net::IpAddr::V6(ip) => { packet.push(4); packet.extend_from_slice(&ip.octets()); } } packet.extend_from_slice(&src.port().to_be_bytes()); packet.extend_from_slice(&buf[..n]); c_down.clone().apply(&mut packet); write_framed_udp(&mut send, &packet).await?; } Result::<()>::Ok(()) });
    let mut frame_buf = Vec::with_capacity(64 * 1024); let c_up = cipher(&cfg.key); while let Some(mut packet) = read_framed_udp(&mut recv, &mut frame_buf).await? { c_up.apply(&mut packet); let (target, payload) = parse_socks5_udp_datagram(&packet).map_err(|e| anyhow!(e.to_string()))?; if cfg.block_local && crate::runtime::is_blocked_local_host(&target.host) { continue; } let addr = tokio::net::lookup_host((target.host.as_str(), target.port)).await?.next().ok_or_else(|| anyhow!("QUIC UDP target DNS returned no address"))?; let _ = udp.send_to(payload, addr).await; }
    sender.abort(); Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test] fn target_formatter_handles_ipv4(){assert_eq!(format_target(&TargetAddr{host:"127.0.0.1".into(),port:80}),"127.0.0.1:80");}
    #[test] fn target_formatter_handles_ipv6(){assert_eq!(format_target(&TargetAddr{host:"::1".into(),port:80}),"::1:80");}
}
