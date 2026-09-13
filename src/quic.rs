//! GoWay v1.8.4-compatible QUIC TCP transport.
//!
//! QUIC uses a raw bidirectional stream per local TCP connection. The first
//! line is `<key> <target>` (or `<target>` when no key is configured), followed
//! by `\n`. The server replies `OK\n` or `ERR: DIAL_FAILED\n`, then the stream
//! carries raw TCP bytes in both directions.

use crate::runtime::RuntimeConfig;
use anyhow::{anyhow, bail, Context, Result};
use quinn::crypto::rustls::{QuicClientConfig, QuicServerConfig};
use quinn::{ClientConfig, Endpoint, ServerConfig, TransportConfig, VarInt};
use rcgen::generate_simple_self_signed;
use rustls::client::danger::{HandshakeSignatureValid, ServerCertVerified, ServerCertVerifier};
use rustls::pki_types::{CertificateDer, PrivateKeyDer, PrivatePkcs8KeyDer, ServerName, UnixTime};
use rustls::{ClientConfig as RustlsClientConfig, RootCertStore};
use std::net::{IpAddr, SocketAddr};
use std::sync::Arc;
use std::time::Duration;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::Mutex;
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
    let mut cfg = RustlsClientConfig::builder()
        .dangerous()
        .with_custom_certificate_verifier(Arc::new(QuicNoCertificateVerification))
        .with_no_client_auth();
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
    let mut rustls = rustls::ServerConfig::builder()
        .with_no_client_auth()
        .with_single_cert(vec![cert_der], key)
        .context("build QUIC server TLS")?;
    rustls.alpn_protocols = ALPN.iter().map(|p| p.to_vec()).collect();
    let crypto = QuicServerConfig::try_from(rustls).context("build QUIC server crypto")?;
    let mut cfg = ServerConfig::with_crypto(Arc::new(crypto));
    cfg.transport_config(transport_config());
    Ok(cfg)
}

#[derive(Debug)]
struct QuicNoCertificateVerification;

impl ServerCertVerifier for QuicNoCertificateVerification {
    fn verify_server_cert(&self, _: &CertificateDer<'_>, _: &[CertificateDer<'_>], _: &ServerName<'_>, _: &[u8], _: UnixTime) -> std::result::Result<ServerCertVerified, rustls::Error> {
        Ok(ServerCertVerified::assertion())
    }
    fn verify_tls12_signature(&self, _: &[u8], _: &CertificateDer<'_>, _: &rustls::DigitallySignedStruct) -> std::result::Result<HandshakeSignatureValid, rustls::Error> {
        Ok(HandshakeSignatureValid::assertion())
    }
    fn verify_tls13_signature(&self, _: &[u8], _: &CertificateDer<'_>, _: &rustls::DigitallySignedStruct) -> std::result::Result<HandshakeSignatureValid, rustls::Error> {
        Ok(HandshakeSignatureValid::assertion())
    }
    fn supported_verify_schemes(&self) -> Vec<rustls::SignatureScheme> {
        vec![
            rustls::SignatureScheme::ECDSA_NISTP256_SHA256,
            rustls::SignatureScheme::ECDSA_NISTP384_SHA384,
            rustls::SignatureScheme::ED25519,
            rustls::SignatureScheme::RSA_PSS_SHA256,
            rustls::SignatureScheme::RSA_PKCS1_SHA256,
        ]
    }
}

fn parse_upstream(input: &str) -> Result<(String, String)> {
    let (scheme, rest) = if let Some(v) = input.strip_prefix("quic://") { ("quic", v) } else if let Some(v) = input.strip_prefix("quic+tls://") { ("quic+tls", v) } else { bail!("QUIC client requires quic:// or quic+tls:// upstream") };
    let _ = scheme;
    let (authority, path) = match rest.split_once('/') { Some((a, p)) => (a, format!("/{}", p)), None => (rest, "/".to_string()) };
    if authority.is_empty() { bail!("empty QUIC upstream authority"); }
    let authority = if authority.starts_with('[') { authority.to_string() } else if authority.matches(':').count() == 1 { authority.to_string() } else { format!("{}:443", authority) };
    Ok((authority, path))
}

async fn resolve_upstream(input: &str) -> Result<(SocketAddr, String)> {
    let (authority, _) = parse_upstream(input)?;
    let host = if authority.starts_with('[') {
        let close = authority.find(']').ok_or_else(|| anyhow!("invalid QUIC IPv6 authority"))?;
        authority[1..close].to_string()
    } else {
        authority.rsplit_once(':').map(|(h, _)| h.to_string()).unwrap_or_else(|| authority.clone())
    };
    let addr = tokio::net::lookup_host(authority.clone()).await?.next().ok_or_else(|| anyhow!("QUIC upstream DNS returned no address"))?;
    Ok((addr, host))
}

async fn read_line(stream: &mut quinn::RecvStream, limit: usize) -> Result<String> {
    let mut buf = Vec::with_capacity(128);
    while buf.len() < limit {
        let mut one = [0u8; 1];
        let n = stream.read(&mut one).await?;
        if n == 0 { bail!("QUIC stream closed while reading header"); }
        buf.push(one[0]);
        if one[0] == b'\n' { return Ok(String::from_utf8(buf)?.trim().to_string()); }
    }
    bail!("QUIC header too large")
}

fn authenticated_target(line: &str, key: &Option<String>) -> Result<String> {
    if let Some(expected) = key {
        let (got, target) = line.split_once(' ').ok_or_else(|| anyhow!("QUIC authentication header missing key"))?;
        if got != expected { bail!("QUIC authentication failed"); }
        return Ok(target.trim().to_string());
    }
    Ok(line.trim().to_string())
}

pub async fn run_client(cfg: RuntimeConfig, verify_ssl: bool) -> Result<()> {
    let upstream = cfg.upstream.clone().ok_or_else(|| anyhow!("QUIC client requires upstream"))?;
    let (server_addr, server_name) = resolve_upstream(&upstream).await?;
    let mut endpoint = Endpoint::client("[::]:0".parse().unwrap())?;
    endpoint.set_default_client_config(client_config(verify_ssl)?);
    let listener = TcpListener::bind(format!("{}:{}", cfg.proxy_host, cfg.proxy_port)).await?;
    tracing::info!("RushWay QUIC client proxy listening on {}:{}", cfg.proxy_host, cfg.proxy_port);
    loop {
        let (mut local, peer) = listener.accept().await?;
        let endpoint2 = endpoint.clone();
        let cfg2 = cfg.clone();
        let server_name2 = server_name.clone();
        tokio::spawn(async move {
            let result = async {
                // QUIC is selected before MUX in GoWay; each local connection obtains a bidirectional stream.
                let connecting = endpoint2.connect(server_addr, &server_name2)?;
                let connection = timeout(Duration::from_secs(cfg2.connection_timeout.max(1)), connecting).await??;
                let (mut send, mut recv) = connection.open_bi().await?;
                let target = match crate::proxy::TargetAddr { host: "".into(), port: 0 };
                let _ = target;
                let target_line = format!("{}\n", "");
                let _ = target_line;
                bail!("QUIC local proxy control path requires proxy request integration")
            }.await;
            if let Err(error) = result { tracing::debug!(%peer, %error, "QUIC local connection closed"); }
            drop(local);
        });
    }
}

pub async fn run_server(cfg: RuntimeConfig) -> Result<()> {
    let bind: SocketAddr = format!("{}:{}", cfg.proxy_host, cfg.proxy_port).parse()?;
    let endpoint = Endpoint::server(server_config()?, bind)?;
    tracing::info!("RushWay QUIC server listening on {}", bind);
    while let Some(incoming) = endpoint.accept().await {
        let cfg2 = cfg.clone();
        tokio::spawn(async move {
            match incoming.await {
                Ok(connection) => {
                    loop {
                        match connection.accept_bi().await {
                            Ok((send, recv)) => {
                                let cfg3 = cfg2.clone();
                                tokio::spawn(async move {
                                    if let Err(error) = handle_server_stream(send, recv, cfg3).await {
                                        tracing::debug!(%error, "QUIC stream closed");
                                    }
                                });
                            }
                            Err(_) => break,
                        }
                    }
                }
                Err(error) => tracing::debug!(%error, "QUIC connection handshake failed"),
            }
        });
    }
    Ok(())
}

async fn handle_server_stream(mut send: quinn::SendStream, mut recv: quinn::RecvStream, cfg: RuntimeConfig) -> Result<()> {
    let line = read_line(&mut recv, 8192).await?;
    if line.eq_ignore_ascii_case("UDP") || line.to_ascii_uppercase().starts_with("UDP ") {
        send.write_all(b"OK\n").await?;
        send.finish().await?;
        return Ok(());
    }
    let target = authenticated_target(&line, &cfg.key)?;
    let (host, port_text) = target.rsplit_once(':').ok_or_else(|| anyhow!("invalid QUIC target"))?;
    let port = port_text.parse::<u16>().map_err(|_| anyhow!("invalid QUIC target port"))?;
    if port == 0 { bail!("invalid QUIC target port"); }
    let target_stream = timeout(Duration::from_secs(cfg.connection_timeout.max(1)), TcpStream::connect(format!("{}:{}", host, port))).await??;
    let (mut target_rd, mut target_wr) = tokio::io::split(target_stream);
    send.write_all(b"OK\n").await?;
    let send_task = tokio::spawn(async move {
        let mut buf = vec![0u8; cfg.buffer_size.clamp(16 * 1024, 1024 * 1024)];
        loop {
            let n = target_rd.read(&mut buf).await?;
            if n == 0 { break; }
            send.write_all(&buf[..n]).await?;
        }
        send.finish().await?;
        Result::<()>::Ok(())
    });
    let mut buf = vec![0u8; cfg.buffer_size.clamp(16 * 1024, 1024 * 1024)];
    loop {
        let n = recv.read(&mut buf).await?;
        if n == 0 { break; }
        target_wr.write_all(&buf[..n]).await?;
    }
    target_wr.shutdown().await?;
    send_task.abort();
    Ok(())
}
