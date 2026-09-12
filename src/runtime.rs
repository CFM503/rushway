//! Runtime forwarding path for the first GoWay-compatible transport slice.
//!
//! This module intentionally keeps the transport stack small and explicit:
//! local SOCKS5/HTTP CONNECT -> WebSocket -> MUX -> target TCP.
//! TLS/QUIC/pooling/retry are separate follow-up layers and are not falsely
//! advertised as complete here.

use crate::protocol::{MuxCommand, MuxFrame, SynPayload};
use crate::proxy::{parse_http_connect, parse_socks5_request, parse_socks5_udp_datagram, socks5_success_response, SocksCommand, TargetAddr, SOCKS5_CONNECT, SOCKS5_UDP_ASSOCIATE, SOCKS5_VERSION};
use crate::ws::{build_client_handshake_request, build_server_handshake_response, read_frame, read_http_headers, validate_server_handshake, validate_client_handshake_response, write_frame};
use anyhow::{anyhow, bail, Context, Result};
use std::collections::HashMap;
use std::net::SocketAddr;
use std::sync::Arc;
use tokio::io::{AsyncReadExt, AsyncWriteExt, ReadHalf, WriteHalf};
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::{mpsc, Mutex};
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
        }
    }
}

#[derive(Debug)]
struct StreamEntry {
    tx: mpsc::Sender<StreamCommand>,
}

#[derive(Debug)]
enum StreamCommand {
    Data(Vec<u8>),
    Fin,
    Reset,
}

async fn dial_target(target: &TargetAddr, timeout_secs: u64) -> Result<TcpStream> {
    let addr = format!("{}:{}", target.host, target.port);
    let stream = timeout(Duration::from_secs(timeout_secs.max(1)), TcpStream::connect(&addr))
        .await
        .context("target connection timeout")??;
    let _ = stream.set_nodelay(true);
    Ok(stream)
}

async fn send_frame(writer: &Arc<Mutex<WriteHalf<TcpStream>>>, frame: &MuxFrame) -> Result<()> {
    let mut bytes = Vec::with_capacity(7 + frame.payload.len());
    frame.encode(&mut bytes).map_err(|e| anyhow!(e.to_string()))?;
    let mut w = writer.lock().await;
    write_frame(&mut *w, &bytes, 2, false).await
}

async fn send_reset(writer: &Arc<Mutex<WriteHalf<TcpStream>>>, id: u32) -> Result<()> {
    send_frame(writer, &MuxFrame::new(id, MuxCommand::Rst, Vec::new()).map_err(|e| anyhow!(e.to_string()))?).await
}

async fn target_to_mux(
    id: u32,
    mut target: ReadHalf<TcpStream>,
    writer: Arc<Mutex<WriteHalf<TcpStream>>>,
    buffer_size: usize,
) {
    let mut buf = vec![0u8; buffer_size.clamp(16 * 1024, 1024 * 1024)];
    loop {
        match target.read(&mut buf).await {
            Ok(0) => {
                let _ = send_frame(&writer, &MuxFrame::new(id, MuxCommand::Fin, Vec::new()).unwrap()).await;
                break;
            }
            Ok(n) => {
                let mut off = 0;
                while off < n {
                    let end = (off + u16::MAX as usize).min(n);
                    let frame = match MuxFrame::new(id, MuxCommand::Data, buf[off..end].to_vec()) {
                        Ok(f) => f,
                        Err(_) => return,
                    };
                    if send_frame(&writer, &frame).await.is_err() { return; }
                    off = end;
                }
            }
            Err(_) => {
                let _ = send_reset(&writer, id).await;
                break;
            }
        }
    }
}

async fn server_stream_task(
    id: u32,
    target: TcpStream,
    mut rx: mpsc::Receiver<StreamCommand>,
    writer: Arc<Mutex<WriteHalf<TcpStream>>>,
    buffer_size: usize,
) {
    let (rd, mut wr) = tokio::io::split(target);
    let reader_writer = writer.clone();
    let reader = tokio::spawn(target_to_mux(id, rd, reader_writer, buffer_size));
    while let Some(cmd) = rx.recv().await {
        match cmd {
            StreamCommand::Data(data) => {
                if wr.write_all(&data).await.is_err() { break; }
            }
            StreamCommand::Fin => {
                let _ = wr.shutdown().await;
                break;
            }
            StreamCommand::Reset => break,
        }
    }
    reader.abort();
}

async fn handle_mux_server(stream: TcpStream, cfg: RuntimeConfig, request: Vec<u8>) -> Result<()> {
    let key = validate_server_handshake(&request)?;
    let (mut rd, mut wr) = tokio::io::split(stream);
    wr.write_all(&build_server_handshake_response(&key)).await?;
    wr.flush().await?;
    let writer = Arc::new(Mutex::new(wr));
    let streams: Arc<Mutex<HashMap<u32, StreamEntry>>> = Arc::new(Mutex::new(HashMap::new()));
    let mut frame_buf = Vec::with_capacity(64 * 1024);

    loop {
        let Some((_, payload)) = read_frame(&mut rd, Option::<&mut WriteHalf<TcpStream>>::None, &mut frame_buf).await? else { break };
        let frame = match MuxFrame::decode(&payload) {
            Ok(f) => f,
            Err(_) => continue,
        };
        match frame.command {
            MuxCommand::Syn => {
                let syn = match SynPayload::decode(&frame.payload) { Ok(v) => v, Err(_) => { let _ = send_reset(&writer, frame.stream_id).await; continue; } };
                let target_text = match String::from_utf8(syn.target) { Ok(v) => v, Err(_) => { let _ = send_reset(&writer, frame.stream_id).await; continue; } };
                let (host, port_text) = match target_text.rsplit_once(':') { Some(v) => v, None => { let _ = send_reset(&writer, frame.stream_id).await; continue; } };
                let port = match port_text.parse::<u16>() { Ok(v) if v != 0 => v, _ => { let _ = send_reset(&writer, frame.stream_id).await; continue; } };
                let target = TargetAddr { host: host.to_string(), port };
                match dial_target(&target, cfg.connection_timeout).await {
                    Ok(target_stream) => {
                        let (tx, rx) = mpsc::channel(64);
                        streams.lock().await.insert(frame.stream_id, StreamEntry { tx: tx.clone() });
                        tokio::spawn(server_stream_task(frame.stream_id, target_stream, rx, writer.clone(), cfg.buffer_size));
                        if !syn.initial_data.is_empty() { let _ = tx.send(StreamCommand::Data(syn.initial_data)).await; }
                    }
                    Err(_) => { let _ = send_reset(&writer, frame.stream_id).await; }
                }
            }
            MuxCommand::Data => {
                let tx = streams.lock().await.get(&frame.stream_id).map(|s| s.tx.clone());
                if let Some(tx) = tx { if tx.send(StreamCommand::Data(frame.payload)).await.is_err() { streams.lock().await.remove(&frame.stream_id); } }
            }
            MuxCommand::Fin => {
                if let Some(entry) = streams.lock().await.get(&frame.stream_id) { let _ = entry.tx.send(StreamCommand::Fin).await; }
            }
            MuxCommand::Rst => {
                if let Some(entry) = streams.lock().await.remove(&frame.stream_id) { let _ = entry.tx.send(StreamCommand::Reset).await; }
            }
        }
    }
    streams.lock().await.clear();
    Ok(())
}

async fn read_proxy_request(stream: &mut TcpStream) -> Result<(TargetAddr, Option<Vec<u8>>)> {
    let first = stream.read_u8().await?;
    if first == SOCKS5_VERSION {
        let n = stream.read_u8().await? as usize;
        let mut methods = vec![0u8; n];
        stream.read_exact(&mut methods).await?;
        if !methods.contains(&0) { stream.write_all(&[5, 0xff]).await?; bail!("SOCKS5 no-auth method unavailable"); }
        stream.write_all(&[5, 0]).await?;
        let mut head = [0u8; 4];
        stream.read_exact(&mut head).await?;
        if head[1] != SOCKS5_CONNECT { stream.write_all(&[5, 7, 0, 1, 0, 0, 0, 0, 0, 0]).await?; bail!("only SOCKS5 CONNECT is supported in the TCP front-end"); }
        let mut rest = match head[3] { 1 => vec![0u8; 6], 3 => vec![0u8; 1], 4 => vec![0u8; 18], _ => { stream.write_all(&[5, 8, 0, 1, 0, 0, 0, 0, 0, 0]).await?; bail!("unsupported SOCKS5 address type"); } };
        stream.read_exact(&mut rest).await?;
        let mut req = head.to_vec(); req.extend_from_slice(&rest);
        if head[3] == 3 { let n = rest[0] as usize; let mut tail = vec![0u8; n + 2]; stream.read_exact(&mut tail).await?; req.extend_from_slice(&tail); } else if head[3] == 1 { } else { }
        let parsed = parse_socks5_request(&req).map_err(|e| anyhow!(e.to_string()))?;
        Ok((parsed.target, Some(socks5_success_response().to_vec())))
    } else if first == b'G' {
        let mut buf = vec![first];
        let mut tail = read_http_headers(stream).await?;
        buf.append(&mut tail);
        let target = parse_http_connect(&buf).map_err(|e| anyhow!(e.to_string()))?;
        Ok((target, None))
    } else {
        bail!("unsupported proxy protocol")
    }
}

async fn handle_local_proxy(mut local: TcpStream, cfg: RuntimeConfig, next_id: u32) -> Result<()> {
    let (target, socks_reply) = read_proxy_request(&mut local).await?;
    let upstream = cfg.upstream.clone().ok_or_else(|| anyhow!("client mode requires upstream"))?;
    let (host, path) = parse_ws_url(&upstream)?;
    let actual_host = cfg.fakehost.clone().unwrap_or_else(|| host.clone());
    let addr = if host.contains(':') { host.clone() } else { host.clone() };
    let socket = timeout(Duration::from_secs(cfg.connection_timeout.max(1)), TcpStream::connect(&addr)).await??;
    let (mut rd, mut wr) = tokio::io::split(socket);
    let origin = Some(format!("https://{}", actual_host));
    let (request, key) = build_client_handshake_request(&actual_host, &path, origin.as_deref());
    wr.write_all(&request).await?;
    wr.flush().await?;
    let response = read_http_headers(&mut rd).await?;
    validate_client_handshake_response(&response, &key)?;
    let writer = Arc::new(Mutex::new(wr));

    if let Some(reply) = socks_reply {
        local.write_all(&reply).await?;
    } else {
        local.write_all(b"HTTP/1.1 200 Connection Established\r\n\r\n").await?;
    }

    let id = next_id.max(1);
    let target_text = format!("{}:{}", target.host, target.port);
    let syn = SynPayload { target: target_text.into_bytes(), initial_data: Vec::new() };
    let syn_frame = MuxFrame::new(id, MuxCommand::Syn, syn.encode().map_err(|e| anyhow!(e.to_string()))?).map_err(|e| anyhow!(e.to_string()))?;
    send_frame(&writer, &syn_frame).await?;

    let (mut local_rd, mut local_wr) = tokio::io::split(local);
    let writer_up = writer.clone();
    let upload = tokio::spawn(async move {
        let mut buf = vec![0u8; cfg.buffer_size.clamp(16 * 1024, 1024 * 1024)];
        loop {
            let n = local_rd.read(&mut buf).await?;
            if n == 0 { let _ = send_frame(&writer_up, &MuxFrame::new(id, MuxCommand::Fin, Vec::new()).unwrap()).await; break; }
            let mut off = 0;
            while off < n {
                let end = (off + u16::MAX as usize).min(n);
                let f = MuxFrame::new(id, MuxCommand::Data, buf[off..end].to_vec()).unwrap();
                send_frame(&writer_up, &f).await?;
                off = end;
            }
        }
        Result::<()>::Ok(())
    });

    let mut frame_buf = Vec::with_capacity(64 * 1024);
    loop {
        let Some((_, payload)) = read_frame(&mut rd, Option::<&mut WriteHalf<TcpStream>>::None, &mut frame_buf).await? else { break };
        let frame = MuxFrame::decode(&payload).map_err(|e| anyhow!(e.to_string()))?;
        if frame.stream_id != id { continue; }
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

fn parse_ws_url(input: &str) -> Result<(String, String)> {
    let rest = input.strip_prefix("ws://").ok_or_else(|| anyhow!("only ws:// upstream is implemented by the current runtime"))?;
    let (authority, path) = match rest.split_once('/') { Some((a, p)) => (a, format!("/{}", p)), None => (rest, "/".into()) };
    let authority = if authority.contains(':') { authority.to_string() } else { format!("{}:80", authority) };
    Ok((authority, path))
}

pub async fn run_client(cfg: RuntimeConfig) -> Result<()> {
    if !cfg.mux { bail!("non-MUX runtime is not implemented yet"); }
    let listener = TcpListener::bind(format!("{}:{}", cfg.proxy_host, cfg.proxy_port)).await?;
    tracing::info!("RushWay client proxy listening on {}:{}", cfg.proxy_host, cfg.proxy_port);
    let counter = Arc::new(std::sync::atomic::AtomicU32::new(1));
    loop {
        let (stream, peer) = listener.accept().await?;
        let cfg2 = cfg.clone();
        let id = counter.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        tokio::spawn(async move {
            if let Err(e) = handle_local_proxy(stream, cfg2, id).await { tracing::debug!(%peer, error=%e, "proxy connection closed"); }
        });
    }
}

pub async fn run_server(cfg: RuntimeConfig) -> Result<()> {
    if !cfg.mux { bail!("non-MUX runtime is not implemented yet"); }
    let listener = TcpListener::bind(format!("{}:{}", cfg.proxy_host, cfg.proxy_port)).await?;
    tracing::info!("RushWay server listening on {}:{}", cfg.proxy_host, cfg.proxy_port);
    loop {
        let (mut stream, peer) = listener.accept().await?;
        let cfg2 = cfg.clone();
        tokio::spawn(async move {
            let result = async {
                let request = read_http_headers(&mut stream).await?;
                handle_mux_server(stream, cfg2, request).await
            }.await;
            if let Err(e) = result { tracing::debug!(%peer, error=%e, "transport connection closed"); }
        });
    }
}

// Keep the UDP parser reachable in the runtime layer so the later UDP relay
// implementation can share exactly the same RFC1928 envelope decoder.
#[allow(dead_code)]
fn decode_udp_packet(buf: &[u8]) -> Result<(TargetAddr, &[u8])> {
    parse_socks5_udp_datagram(buf).map_err(|e| anyhow!(e.to_string()))
}
