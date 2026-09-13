//! Runtime forwarding paths for the GoWay-compatible transport slice.
//!
//! Current paths: local SOCKS5/HTTP CONNECT -> WebSocket -> MUX -> target TCP,
//! and SOCKS5 UDP ASSOCIATE -> WebSocket -> UDP relay.
//! TLS/QUIC/pooling/retry remain explicit follow-up layers.

use crate::crypto::XorCipher;
use crate::protocol::{write_frame_parts, MuxCommand, MuxFrame, SynPayload};
use crate::proxy::{parse_http_connect, parse_socks5_request, parse_socks5_udp_datagram, socks5_success_response, SocksCommand, TargetAddr, SOCKS5_CONNECT, SOCKS5_UDP_ASSOCIATE, SOCKS5_VERSION};
use crate::ws::{build_client_handshake_request, build_server_handshake_response, read_frame, read_http_headers, validate_client_handshake_response, validate_server_handshake, write_frame};
use anyhow::{anyhow, bail, Context, Result};
use std::collections::HashMap;
use std::net::{IpAddr, SocketAddr};
use std::sync::Arc;
use tokio::io::{AsyncReadExt, AsyncWriteExt, ReadHalf, WriteHalf};
use tokio::net::{lookup_host, TcpListener, TcpStream, UdpSocket};
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
        Self { proxy_host: "127.0.0.1".into(), proxy_port: 9192, upstream: None, key: None, fakehost: None, mux: true, buffer_size: 128 * 1024, connection_timeout: 60, allow_open: false }
    }
}

#[derive(Debug)]
struct StreamEntry { tx: mpsc::Sender<StreamCommand> }

#[derive(Debug)]
enum StreamCommand { Data(Vec<u8>), Fin, Reset }

fn configured_cipher(key: &Option<String>) -> XorCipher { XorCipher::new(key.as_deref().unwrap_or("")) }
fn transform_payload(cipher: &XorCipher, payload: &mut [u8]) { cipher.apply(payload); }

async fn dial_target(target: &TargetAddr, timeout_secs: u64) -> Result<TcpStream> {
    let addr = format!("{}:{}", target.host, target.port);
    let stream = timeout(Duration::from_secs(timeout_secs.max(1)), TcpStream::connect(&addr)).await.context("target connection timeout")??;
    let _ = stream.set_nodelay(true);
    Ok(stream)
}

async fn send_frame(writer: &Arc<Mutex<WriteHalf<TcpStream>>>, frame: &MuxFrame) -> Result<()> {
    let mut bytes = Vec::with_capacity(7 + frame.payload.len());
    frame.encode(&mut bytes).map_err(|e| anyhow!(e.to_string()))?;
    let mut w = writer.lock().await;
    write_frame(&mut *w, &bytes, 2, false).await
}

async fn send_mux_parts(writer: &Arc<Mutex<WriteHalf<TcpStream>>>, stream_id: u32, command: MuxCommand, payload: &[u8]) -> Result<()> {
    let mut bytes = Vec::with_capacity(7 + payload.len());
    write_frame_parts(&mut bytes, stream_id, command, payload).map_err(|e| anyhow!(e.to_string()))?;
    let mut w = writer.lock().await;
    write_frame(&mut *w, &bytes, 2, false).await
}

async fn send_reset(writer: &Arc<Mutex<WriteHalf<TcpStream>>>, id: u32) -> Result<()> {
    send_frame(writer, &MuxFrame::new(id, MuxCommand::Rst, Vec::new()).map_err(|e| anyhow!(e.to_string()))?).await
}

async fn target_to_mux(id: u32, mut target: ReadHalf<TcpStream>, writer: Arc<Mutex<WriteHalf<TcpStream>>>, buffer_size: usize) {
    let mut buf = vec![0u8; buffer_size.clamp(16 * 1024, 1024 * 1024)];
    loop {
        match target.read(&mut buf).await {
            Ok(0) => { let _ = send_mux_parts(&writer, id, MuxCommand::Fin, &[]).await; break; }
            Ok(n) => {
                let mut off = 0;
                while off < n {
                    let end = (off + u16::MAX as usize).min(n);
                    if send_mux_parts(&writer, id, MuxCommand::Data, &buf[off..end]).await.is_err() { return; }
                    off = end;
                }
            }
            Err(_) => { let _ = send_reset(&writer, id).await; break; }
        }
    }
}

async fn server_stream_task(id: u32, target: TcpStream, mut rx: mpsc::Receiver<StreamCommand>, writer: Arc<Mutex<WriteHalf<TcpStream>>>, buffer_size: usize) {
    let (rd, mut wr) = tokio::io::split(target);
    let reader = tokio::spawn(target_to_mux(id, rd, writer.clone(), buffer_size));
    while let Some(cmd) = rx.recv().await {
        match cmd {
            StreamCommand::Data(data) => { if wr.write_all(&data).await.is_err() { break; } }
            StreamCommand::Fin => { let _ = wr.shutdown().await; break; }
            StreamCommand::Reset => break,
        }
    }
    reader.abort();
}

async fn handle_mux_parts(mut rd: ReadHalf<TcpStream>, writer: Arc<Mutex<WriteHalf<TcpStream>>>, cfg: RuntimeConfig, first_payload: Vec<u8>) -> Result<()> {
    let cipher = configured_cipher(&cfg.key);
    let mut hello = first_payload;
    transform_payload(&cipher, &mut hello);
    if hello != b"MUX\n" { bail!("invalid MUX handshake"); }
    let mut ok = b"OK\n".to_vec();
    transform_payload(&cipher, &mut ok);
    { let mut w = writer.lock().await; write_frame(&mut *w, &ok, 2, false).await?; }
    let streams: Arc<Mutex<HashMap<u32, StreamEntry>>> = Arc::new(Mutex::new(HashMap::new()));
    let mut frame_buf = Vec::with_capacity(64 * 1024);
    loop {
        let Some((opcode, payload)) = read_frame(&mut rd, Option::<&mut WriteHalf<TcpStream>>::None, &mut frame_buf).await? else { break };
        if opcode != 2 { continue; }
        let frame = match MuxFrame::decode(&payload) { Ok(f) => f, Err(_) => continue };
        match frame.command {
            MuxCommand::Syn => {
                let syn = match SynPayload::decode(&frame.payload) { Ok(v) => v, Err(_) => { let _ = send_reset(&writer, frame.stream_id).await; continue; } };
                let target_text = match String::from_utf8(syn.target) { Ok(v) => v, Err(_) => { let _ = send_reset(&writer, frame.stream_id).await; continue; } };
                let (host, port_text) = match target_text.rsplit_once(':') { Some(v) => v, None => { let _ = send_reset(&writer, frame.stream_id).await; continue; } };
                let port = match port_text.parse::<u16>() { Ok(v) if v != 0 => v, _ => { let _ = send_reset(&writer, frame.stream_id).await; continue; } };
                match dial_target(&TargetAddr { host: host.to_string(), port }, cfg.connection_timeout).await {
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
            MuxCommand::Fin => { if let Some(entry) = streams.lock().await.get(&frame.stream_id) { let _ = entry.tx.send(StreamCommand::Fin).await; } }
            MuxCommand::Rst => { if let Some(entry) = streams.lock().await.remove(&frame.stream_id) { let _ = entry.tx.send(StreamCommand::Reset).await; } }
        }
    }
    streams.lock().await.clear();
    Ok(())
}

async fn resolve_udp_target(target: &TargetAddr) -> Result<SocketAddr> {
    if let Ok(ip) = target.host.parse::<IpAddr>() { return Ok(SocketAddr::new(ip, target.port)); }
    let mut addrs = lookup_host((target.host.as_str(), target.port)).await?;
    addrs.next().ok_or_else(|| anyhow!("UDP target DNS returned no address"))
}

fn udp_envelope(source: SocketAddr, payload: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(22 + payload.len());
    out.extend_from_slice(&[0, 0, 0]);
    match source.ip() {
        IpAddr::V4(ip) => { out.push(1); out.extend_from_slice(&ip.octets()); }
        IpAddr::V6(ip) => { out.push(4); out.extend_from_slice(&ip.octets()); }
    }
    out.extend_from_slice(&source.port().to_be_bytes());
    out.extend_from_slice(payload);
    out
}

async fn handle_server_udp_parts(mut rd: ReadHalf<TcpStream>, writer: Arc<Mutex<WriteHalf<TcpStream>>>, cfg: RuntimeConfig, first_payload: Vec<u8>) -> Result<()> {
    let cipher = configured_cipher(&cfg.key);
    let mut hello = first_payload;
    transform_payload(&cipher, &mut hello);
    if hello != b"UDP\n" { bail!("invalid UDP handshake"); }
    let mut ok = b"OK\n".to_vec();
    transform_payload(&cipher, &mut ok);
    { let mut w = writer.lock().await; write_frame(&mut *w, &ok, 2, false).await?; }
    let udp = Arc::new(UdpSocket::bind("0.0.0.0:0").await?);
    let udp_send = udp.clone();
    let writer_send = writer.clone();
    let cipher_send = configured_cipher(&cfg.key);
    let send_task = tokio::spawn(async move {
        let mut buf = vec![0u8; 64 * 1024];
        loop {
            let (n, source) = udp_send.recv_from(&mut buf).await?;
            let mut packet = udp_envelope(source, &buf[..n]);
            transform_payload(&cipher_send, &mut packet);
            let mut w = writer_send.lock().await;
            write_frame(&mut *w, &packet, 2, false).await?;
        }
        Result::<()>::Ok(())
    });
    let mut frame_buf = Vec::with_capacity(64 * 1024);
    loop {
        let Some((opcode, mut packet)) = read_frame(&mut rd, Option::<&mut WriteHalf<TcpStream>>::None, &mut frame_buf).await? else { break };
        if opcode != 2 { continue; }
        transform_payload(&cipher, &mut packet);
        let (target, payload) = parse_socks5_udp_datagram(&packet).map_err(|e| anyhow!(e.to_string()))?;
        let addr = match resolve_udp_target(&target).await { Ok(v) => v, Err(_) => continue };
        let _ = udp.send_to(payload, addr).await;
    }
    send_task.abort();
    Ok(())
}

async fn read_proxy_request(stream: &mut TcpStream) -> Result<(SocksCommand, TargetAddr, bool)> {
    let first = stream.read_u8().await?;
    if first != SOCKS5_VERSION {
        if first == b'C' {
            let mut buf = vec![first];
            let mut tail = read_http_headers(stream).await?;
            buf.append(&mut tail);
            let target = parse_http_connect(&buf).map_err(|e| anyhow!(e.to_string()))?;
            return Ok((SocksCommand::Connect, target, false));
        }
        bail!("unsupported proxy protocol");
    }
    let n = stream.read_u8().await? as usize;
    let mut methods = vec![0u8; n];
    stream.read_exact(&mut methods).await?;
    if !methods.contains(&0) { stream.write_all(&[5, 0xff]).await?; bail!("SOCKS5 no-auth method unavailable"); }
    stream.write_all(&[5, 0]).await?;
    let mut head = [0u8; 4];
    stream.read_exact(&mut head).await?;
    if head[1] != SOCKS5_CONNECT && head[1] != SOCKS5_UDP_ASSOCIATE { stream.write_all(&[5, 7, 0, 1, 0, 0, 0, 0, 0, 0]).await?; bail!("unsupported SOCKS5 command"); }
    let mut rest = match head[3] { 1 => vec![0u8; 6], 3 => vec![0u8; 1], 4 => vec![0u8; 18], _ => { stream.write_all(&[5, 8, 0, 1, 0, 0, 0, 0, 0, 0]).await?; bail!("unsupported SOCKS5 address type"); } };
    stream.read_exact(&mut rest).await?;
    let mut req = head.to_vec(); req.extend_from_slice(&rest);
    if head[3] == 3 { let n = rest[0] as usize; let mut tail = vec![0u8; n + 2]; stream.read_exact(&mut tail).await?; req.extend_from_slice(&tail); }
    let parsed = parse_socks5_request(&req).map_err(|e| anyhow!(e.to_string()))?;
    Ok((parsed.command, parsed.target, true))
}

async fn handle_local_udp_proxy(mut control: TcpStream, cfg: RuntimeConfig, bind_hint: TargetAddr) -> Result<()> {
    let bind_ip = if bind_hint.host == "0.0.0.0" || bind_hint.host == "" { "0.0.0.0" } else { bind_hint.host.as_str() };
    let udp = Arc::new(UdpSocket::bind(format!("{}:0", bind_ip)).await?);
    let bound = udp.local_addr()?;
    let mut resp = [0u8; 10]; resp[0]=5; resp[1]=0; resp[2]=0; resp[3]=1;
    if let IpAddr::V4(ip) = bound.ip() { resp[4..8].copy_from_slice(&ip.octets()); }
    resp[8..10].copy_from_slice(&bound.port().to_be_bytes());
    control.write_all(&resp).await?;
    let upstream = cfg.upstream.clone().ok_or_else(|| anyhow!("client mode requires upstream"))?;
    let (host, path) = parse_ws_url(&upstream)?;
    let actual_host = cfg.fakehost.clone().unwrap_or_else(|| host.clone());
    let socket = timeout(Duration::from_secs(cfg.connection_timeout.max(1)), TcpStream::connect(&host)).await??;
    let (mut rd, mut wr) = tokio::io::split(socket);
    let origin = Some(format!("https://{}", actual_host));
    let sec_fetch_site = if host.eq_ignore_ascii_case(&actual_host) { "same-origin" } else { "cross-site" };
    let (request, key) = build_client_handshake_request(&actual_host, &path, origin.as_deref(), Some(sec_fetch_site));
    wr.write_all(&request).await?; wr.flush().await?;
    let response = read_http_headers(&mut rd).await?;
    validate_client_handshake_response(&response, &key)?;
    let writer = Arc::new(Mutex::new(wr));
    let cipher = configured_cipher(&cfg.key);
    let mut hello = b"UDP\n".to_vec(); transform_payload(&cipher, &mut hello);
    { let mut w=writer.lock().await; write_frame(&mut *w, &hello, 2, true).await?; }
    let mut frame_buf=Vec::with_capacity(64*1024);
    let Some((opcode,mut ok))=read_frame(&mut rd,Option::<&mut WriteHalf<TcpStream>>::None,&mut frame_buf).await? else { bail!("upstream closed during UDP handshake") };
    if opcode!=2 { bail!("invalid UDP handshake response opcode") }
    transform_payload(&cipher,&mut ok); if ok!=b"OK\n" { bail!("upstream rejected UDP handshake") }
    let latest_client: Arc<Mutex<Option<SocketAddr>>> = Arc::new(Mutex::new(None));
    let udp_send=udp.clone(); let writer_send=writer.clone(); let cipher_send=configured_cipher(&cfg.key); let latest_send=latest_client.clone();
    let upload=tokio::spawn(async move {
        let mut buf=vec![0u8;64*1024];
        loop { let (n,peer)=udp_send.recv_from(&mut buf).await?; *latest_send.lock().await=Some(peer); let mut data=buf[..n].to_vec(); transform_payload(&cipher_send,&mut data); let mut w=writer_send.lock().await; write_frame(&mut *w,&data,2,true).await?; }
        Result::<()>::Ok(())
    });
    loop {
        let Some((opcode,mut packet))=read_frame(&mut rd,Option::<&mut WriteHalf<TcpStream>>::None,&mut frame_buf).await? else { break };
        if opcode!=2 { continue; }
        transform_payload(&cipher,&mut packet);
        let (_src,payload)=parse_socks5_udp_datagram(&packet).map_err(|e|anyhow!(e.to_string()))?;
        if let Some(peer)=*latest_client.lock().await { let _=udp.send_to(payload,peer).await; }
    }
    upload.abort(); control.shutdown().await.ok(); Ok(())
}

async fn handle_local_proxy(mut local: TcpStream, cfg: RuntimeConfig, next_id: u32) -> Result<()> {
    let (command, target, is_socks5) = read_proxy_request(&mut local).await?;
    if command == SocksCommand::UdpAssociate { return handle_local_udp_proxy(local, cfg, target).await; }
    let upstream = cfg.upstream.clone().ok_or_else(|| anyhow!("client mode requires upstream"))?;
    let (host, path) = parse_ws_url(&upstream)?;
    let actual_host = cfg.fakehost.clone().unwrap_or_else(|| host.clone());
    let socket = timeout(Duration::from_secs(cfg.connection_timeout.max(1)), TcpStream::connect(&host)).await??;
    let (mut rd, mut wr) = tokio::io::split(socket);
    let origin = Some(format!("https://{}", actual_host));
    let sec_fetch_site = if host.eq_ignore_ascii_case(&actual_host) { "same-origin" } else { "cross-site" };
    let (request, key) = build_client_handshake_request(&actual_host, &path, origin.as_deref(), Some(sec_fetch_site));
    wr.write_all(&request).await?; wr.flush().await?;
    let response = read_http_headers(&mut rd).await?; validate_client_handshake_response(&response, &key)?;
    let writer=Arc::new(Mutex::new(wr)); let cipher=configured_cipher(&cfg.key);
    if is_socks5 { local.write_all(&socks5_success_response()).await?; } else { local.write_all(b"HTTP/1.1 200 Connection Established\r\n\r\n").await?; }
    let mut hello=b"MUX\n".to_vec(); transform_payload(&cipher,&mut hello); { let mut w=writer.lock().await; write_frame(&mut *w,&hello,2,true).await?; }
    let mut frame_buf=Vec::with_capacity(64*1024);
    let Some((opcode,mut ok))=read_frame(&mut rd,Option::<&mut WriteHalf<TcpStream>>::None,&mut frame_buf).await? else { bail!("upstream closed during MUX handshake") };
    if opcode!=2 { bail!("invalid MUX handshake response opcode") }
    transform_payload(&cipher,&mut ok); if ok!=b"OK\n" { bail!("upstream rejected MUX handshake") }
    let id=next_id.max(1); let target_text=format!("{}:{}",target.host,target.port); let syn=SynPayload{target:target_text.into_bytes(),initial_data:Vec::new()}; let syn_frame=MuxFrame::new(id,MuxCommand::Syn,syn.encode().map_err(|e|anyhow!(e.to_string()))?).map_err(|e|anyhow!(e.to_string()))?; send_frame(&writer,&syn_frame).await?;
    let (mut local_rd,mut local_wr)=tokio::io::split(local); let writer_up=writer.clone(); let upload=tokio::spawn(async move { let mut buf=vec![0u8;cfg.buffer_size.clamp(16*1024,1024*1024)]; loop { let n=local_rd.read(&mut buf).await?; if n==0 { let _=send_mux_parts(&writer_up,id,MuxCommand::Fin,&[]).await; break } let mut off=0; while off<n { let end=(off+u16::MAX as usize).min(n); send_mux_parts(&writer_up,id,MuxCommand::Data,&buf[off..end]).await?; off=end; } } Result::<()>::Ok(()) });
    loop { let Some((opcode,payload))=read_frame(&mut rd,Option::<&mut WriteHalf<TcpStream>>::None,&mut frame_buf).await? else { break }; if opcode!=2 { continue } let frame=MuxFrame::decode(&payload).map_err(|e|anyhow!(e.to_string()))?; if frame.stream_id!=id { continue } match frame.command { MuxCommand::Data=>local_wr.write_all(&frame.payload).await?, MuxCommand::Fin=>{local_wr.shutdown().await?;break}, MuxCommand::Rst=>break, MuxCommand::Syn=>{} } }
    upload.abort(); Ok(())
}

fn parse_ws_url(input: &str) -> Result<(String,String)> {
    let rest=input.strip_prefix("ws://").ok_or_else(|| anyhow!("only ws:// upstream is implemented by the current runtime"))?;
    let (authority,path)=match rest.split_once('/') { Some((a,p))=>(a,format!("/{}",p)),None=>(rest,"/".into()) };
    let authority=if authority.contains(':'){authority.to_string()}else{format!("{}:80",authority)};
    Ok((authority,path))
}

pub async fn run_client(cfg: RuntimeConfig) -> Result<()> {
    if !cfg.mux { bail!("non-MUX runtime is not implemented yet"); }
    let listener=TcpListener::bind(format!("{}:{}",cfg.proxy_host,cfg.proxy_port)).await?;
    tracing::info!("RushWay client proxy listening on {}:{}",cfg.proxy_host,cfg.proxy_port);
    let counter=Arc::new(std::sync::atomic::AtomicU32::new(1));
    loop { let (stream,peer)=listener.accept().await?; let cfg2=cfg.clone(); let id=counter.fetch_add(1,std::sync::atomic::Ordering::Relaxed); tokio::spawn(async move { if let Err(e)=handle_local_proxy(stream,cfg2,id).await { tracing::debug!(%peer,error=%e,"proxy connection closed"); } }); }
}

pub async fn run_server(cfg: RuntimeConfig) -> Result<()> {
    if !cfg.mux { bail!("non-MUX runtime is not implemented yet"); }
    let listener=TcpListener::bind(format!("{}:{}",cfg.proxy_host,cfg.proxy_port)).await?;
    tracing::info!("RushWay server listening on {}:{}",cfg.proxy_host,cfg.proxy_port);
    loop {
        let (stream,peer)=listener.accept().await?; let cfg2=cfg.clone();
        tokio::spawn(async move {
            let result=async {
                let (mut rd,mut wr)=tokio::io::split(stream);
                let request=read_http_headers(&mut rd).await?;
                let key=validate_server_handshake(&request)?;
                wr.write_all(&build_server_handshake_response(&key)).await?; wr.flush().await?;
                let writer=Arc::new(Mutex::new(wr));
                let mut buf=Vec::with_capacity(64*1024);
                let Some((opcode,first))=read_frame(&mut rd,Option::<&mut WriteHalf<TcpStream>>::None,&mut buf).await? else { bail!("missing transport handshake") };
                if opcode!=2 { bail!("invalid transport handshake opcode") }
                let mut plain=first.clone(); let cipher=configured_cipher(&cfg2.key); transform_payload(&cipher,&mut plain);
                if plain==b"UDP\n" { return handle_server_udp_parts(rd,writer,cfg2,first).await; }
                if plain==b"MUX\n" { return handle_mux_parts(rd,writer,cfg2,first).await; }
                bail!("unknown transport handshake")
            }.await;
            if let Err(e)=result { tracing::debug!(%peer,error=%e,"transport connection closed"); }
        });
    }
}

#[allow(dead_code)]
fn decode_udp_packet(buf: &[u8]) -> Result<(TargetAddr, &[u8])> { parse_socks5_udp_datagram(buf).map_err(|e| anyhow!(e.to_string())) }
