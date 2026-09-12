//! WSS client path for GoWay-compatible upstreams.
//!
//! This module deliberately keeps the existing plain `ws://` runtime untouched.
//! It terminates TLS before feeding the resulting stream into the same RFC6455
//! and MUX primitives. Server-side TLS is not claimed here; GoWay deployments
//! commonly terminate WSS at the TLS edge and forward WebSocket to the proxy.

use crate::crypto::XorCipher;
use crate::protocol::{MuxCommand, MuxFrame, SynPayload};
use crate::proxy::{parse_http_connect, parse_socks5_request, socks5_success_response, SocksCommand, TargetAddr, SOCKS5_CONNECT, SOCKS5_VERSION};
use crate::tls;
use crate::ws::{build_client_handshake_request, read_frame, read_http_headers, validate_client_handshake_response, write_frame};
use anyhow::{anyhow, bail, Context, Result};
use std::sync::Arc;
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::Mutex;
use tokio::time::{timeout, Duration};

trait Transport: AsyncRead + AsyncWrite + Unpin + Send {}
impl<T: AsyncRead + AsyncWrite + Unpin + Send> Transport for T {}
type BoxTransport = Box<dyn Transport>;

type BoxReader = tokio::io::ReadHalf<BoxTransport>;
type BoxWriter = tokio::io::WriteHalf<BoxTransport>;

#[derive(Debug, Clone)]
struct WssConfig {
    proxy_host: String,
    proxy_port: u16,
    upstream: String,
    key: Option<String>,
    fakehost: Option<String>,
    buffer_size: usize,
    connection_timeout: u64,
    verify_ssl: bool,
}

fn cipher(key: &Option<String>) -> XorCipher { XorCipher::new(key.as_deref().unwrap_or("")) }

fn parse_wss_url(input: &str) -> Result<(String, String)> {
    let rest = input.strip_prefix("wss://").ok_or_else(|| anyhow!("WSS client requires wss:// upstream"))?;
    let (authority, path) = match rest.split_once('/') {
        Some((a, p)) => (a, format!("/{}", p)),
        None => (rest, "/".to_string()),
    };
    if authority.is_empty() { bail!("empty WSS upstream authority"); }
    let host = if authority.starts_with('[') {
        let close = authority.find(']').ok_or_else(|| anyhow!("invalid IPv6 WSS authority"))?;
        authority[..=close].to_string()
    } else {
        authority.split(':').next().unwrap_or(authority).to_string()
    };
    let connect_addr = if authority.contains(':') {
        authority.to_string()
    } else {
        format!("{}:443", authority)
    };
    Ok((connect_addr, path_with_host(path, host)))
}

fn path_with_host(path: String, _host: String) -> String { path }

async fn open_upstream(cfg: &WssConfig) -> Result<(BoxReader, Arc<Mutex<BoxWriter>>)> {
    let (addr, path) = parse_wss_url(&cfg.upstream)?;
    let connect_host = addr.rsplit_once(':').map(|(h, _)| h.trim_matches(&['[', ']'][..])).unwrap_or(addr.as_str());
    let tcp = timeout(Duration::from_secs(cfg.connection_timeout.max(1)), TcpStream::connect(&addr)).await.context("WSS upstream TCP timeout")??;
    let tls_stream = tls::connect(tcp, cfg.fakehost.as_deref().unwrap_or(connect_host), cfg.verify_ssl).await?;
    let boxed: BoxTransport = Box::new(tls_stream);
    let (mut rd, mut wr) = tokio::io::split(boxed);
    let header_host = cfg.fakehost.as_deref().unwrap_or(connect_host);
    let origin = format!("https://{}", header_host);
    let (request, key) = build_client_handshake_request(header_host, &path, Some(&origin));
    wr.write_all(&request).await?;
    wr.flush().await?;
    let response = read_http_headers(&mut rd).await?;
    validate_client_handshake_response(&response, &key)?;
    Ok((rd, Arc::new(Mutex::new(wr))))
}

async fn read_proxy_request(local: &mut TcpStream) -> Result<(SocksCommand, TargetAddr)> {
    let first = local.read_u8().await?;
    if first == SOCKS5_VERSION {
        let n = local.read_u8().await? as usize;
        let mut methods = vec![0u8; n];
        local.read_exact(&mut methods).await?;
        if !methods.contains(&0) { local.write_all(&[5, 0xff]).await?; bail!("SOCKS5 no-auth unavailable"); }
        local.write_all(&[5, 0]).await?;
        let mut head = [0u8; 4];
        local.read_exact(&mut head).await?;
        if head[1] != SOCKS5_CONNECT { local.write_all(&[5, 7, 0, 1, 0, 0, 0, 0, 0, 0]).await?; bail!("WSS path supports SOCKS5 CONNECT only"); }
        let mut req = head.to_vec();
        match head[3] {
            1 => { let mut b = [0u8; 6]; local.read_exact(&mut b).await?; req.extend_from_slice(&b); }
            3 => { let mut n = [0u8; 1]; local.read_exact(&mut n).await?; req.extend_from_slice(&n); let mut b = vec![0u8; n[0] as usize + 2]; local.read_exact(&mut b).await?; req.extend_from_slice(&b); }
            4 => { let mut b = [0u8; 18]; local.read_exact(&mut b).await?; req.extend_from_slice(&b); }
            _ => bail!("unsupported SOCKS5 address type"),
        }
        let parsed = parse_socks5_request(&req).map_err(|e| anyhow!(e.to_string()))?;
        return Ok((parsed.command, parsed.target));
    }
    if first == b'C' {
        let mut buf = vec![first];
        let mut rest = Vec::new();
        let mut one = [0u8; 1];
        while rest.len() < 8192 {
            local.read_exact(&mut one).await?;
            rest.push(one[0]);
            if rest.ends_with(b"\r\n\r\n") || rest.ends_with(b"\n\n") { break; }
        }
        buf.extend_from_slice(&rest);
        return Ok((SocksCommand::Connect, parse_http_connect(&buf).map_err(|e| anyhow!(e.to_string()))?));
    }
    bail!("unsupported local proxy protocol")
}

async fn send_mux(writer: &Arc<Mutex<BoxWriter>>, frame: &MuxFrame) -> Result<()> {
    let mut data = Vec::with_capacity(7 + frame.payload.len());
    frame.encode(&mut data).map_err(|e| anyhow!(e.to_string()))?;
    let mut w = writer.lock().await;
    write_frame(&mut *w, &data, 2, true).await
}

async fn handle_connection(mut local: TcpStream, cfg: WssConfig, id: u32) -> Result<()> {
    let (command, target) = read_proxy_request(&mut local).await?;
    if command != SocksCommand::Connect { bail!("WSS path only supports CONNECT"); }
    let (mut rd, writer) = open_upstream(&cfg).await?;
    let c = cipher(&cfg.key);
    let mut hello = b"MUX\n".to_vec();
    c.apply(&mut hello);
    { let mut w = writer.lock().await; write_frame(&mut *w, &hello, 2, true).await?; }
    let mut frame_buf = Vec::with_capacity(64 * 1024);
    let Some((opcode, mut ok)) = read_frame(&mut rd, Option::<&mut BoxWriter>::None, &mut frame_buf).await? else { bail!("WSS upstream closed during MUX handshake") };
    if opcode != 2 { bail!("invalid WSS MUX response opcode"); }
    c.apply(&mut ok);
    if ok != b"OK\n" { bail!("WSS upstream rejected MUX handshake"); }
    if command == SocksCommand::Connect { local.write_all(&socks5_success_response()).await.ok(); }
    let syn = SynPayload { target: format!("{}:{}", target.host, target.port).into_bytes(), initial_data: Vec::new() };
    let syn = MuxFrame::new(id.max(1), MuxCommand::Syn, syn.encode().map_err(|e| anyhow!(e.to_string()))?).map_err(|e| anyhow!(e.to_string()))?;
    send_mux(&writer, &syn).await?;
    let (mut local_rd, mut local_wr) = tokio::io::split(local);
    let writer_up = writer.clone();
    let buffer_size = cfg.buffer_size.clamp(16 * 1024, 1024 * 1024);
    let upload = tokio::spawn(async move {
        let mut buf = vec![0u8; buffer_size];
        loop {
            let n = local_rd.read(&mut buf).await?;
            if n == 0 { let _ = send_mux(&writer_up, &MuxFrame::new(id.max(1), MuxCommand::Fin, Vec::new()).unwrap()).await; break; }
            let mut off = 0;
            while off < n {
                let end = (off + u16::MAX as usize).min(n);
                let frame = MuxFrame::new(id.max(1), MuxCommand::Data, buf[off..end].to_vec()).unwrap();
                send_mux(&writer_up, &frame).await?;
                off = end;
            }
        }
        Result::<()>::Ok(())
    });
    loop {
        let Some((opcode, payload)) = read_frame(&mut rd, Option::<&mut BoxWriter>::None, &mut frame_buf).await? else { break };
        if opcode != 2 { continue; }
        let frame = MuxFrame::decode(&payload).map_err(|e| anyhow!(e.to_string()))?;
        if frame.stream_id != id.max(1) { continue; }
        match frame.command {
            MuxCommand::Data => local_wr.write_all(&frame.payload).await?,
            MuxCommand::Fin => { local_wr.shutdown().await?; break; }
            MuxCommand::Rst => break,
            MuxCommand::Syn => {}
        }
    }
    upload.abort();
    Ok(())
}

pub async fn run_client(cfg: WssConfig) -> Result<()> {
    let listener = TcpListener::bind(format!("{}:{}", cfg.proxy_host, cfg.proxy_port)).await?;
    tracing::info!("RushWay WSS client proxy listening on {}:{}", cfg.proxy_host, cfg.proxy_port);
    let counter = Arc::new(std::sync::atomic::AtomicU32::new(1));
    loop {
        let (stream, peer) = listener.accept().await?;
        let id = counter.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let cfg2 = cfg.clone();
        tokio::spawn(async move {
            if let Err(e) = handle_connection(stream, cfg2, id).await { tracing::debug!(%peer, error=%e, "WSS proxy connection closed"); }
        });
    }
}
