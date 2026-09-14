//! Runtime forwarding paths for the GoWay-compatible transport slice.

use crate::crypto::XorCipher;
use crate::dns::resolve_socket;
use crate::protocol::{write_frame_parts, MuxCommand, MuxFrame, OwnedMuxFrame, SynPayload};
use crate::proxy::{
    parse_http_connect, parse_socks5_request, parse_socks5_udp_datagram, socks5_success_response,
    SocksCommand, TargetAddr, SOCKS5_CONNECT, SOCKS5_UDP_ASSOCIATE, SOCKS5_VERSION,
};
use crate::ws::{
    build_server_handshake_response, read_frame, read_frame_owned, read_http_headers,
    validate_server_handshake, write_frame,
};
use anyhow::{anyhow, bail, Context, Result};
use socket2::SockRef;
use std::collections::HashMap;
use std::net::{IpAddr, SocketAddr};
use std::sync::{atomic::AtomicUsize, Arc};
use tokio::io::{AsyncReadExt, AsyncWriteExt, ReadHalf, WriteHalf};
use tokio::net::{lookup_host, TcpListener, TcpStream, UdpSocket};
use tokio::sync::{mpsc, Mutex, Semaphore};
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
    pub max_connections: usize,
    pub block_local: bool,
    pub tcp_nodelay: bool,
    pub tcp_keepalive: bool,
    pub socket_buffer: usize,
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
            max_connections: 1000,
            block_local: true,
            tcp_nodelay: true,
            tcp_keepalive: true,
            socket_buffer: 0,
        }
    }
}
#[derive(Debug)]
struct StreamEntry {
    tx: mpsc::Sender<StreamCommand>,
}
#[derive(Debug)]
enum StreamCommand {
    Data(OwnedMuxFrame),
    Fin,
    Reset,
}
fn configured_cipher(key: &Option<String>) -> XorCipher {
    XorCipher::new(key.as_deref().unwrap_or(""))
}
fn transform_payload(cipher: &XorCipher, payload: &mut [u8]) {
    cipher.apply(payload)
}
pub(crate) fn is_blocked_local_host(host: &str) -> bool {
    if let Ok(ip) = host.parse::<IpAddr>() {
        return match ip {
            IpAddr::V4(v4) => {
                v4.is_loopback() || v4.is_private() || v4.is_link_local() || v4.is_unspecified()
            }
            IpAddr::V6(v6) => {
                v6.is_loopback()
                    || v6.is_unspecified()
                    || v6.is_unique_local()
                    || v6.is_unicast_link_local()
            }
        };
    }
    false
}
pub(crate) fn enforce_target_policy(cfg: &RuntimeConfig, target: &TargetAddr) -> Result<()> {
    if cfg.upstream.is_some() && cfg.block_local && is_blocked_local_host(&target.host) {
        bail!("local/LAN target blocked by client policy")
    }
    if target.port == 0 {
        bail!("target port must be non-zero")
    }
    Ok(())
}
pub(crate) fn apply_socket_options(stream: &TcpStream, cfg: &RuntimeConfig) {
    let sock = SockRef::from(stream);
    let _ = sock.set_nodelay(cfg.tcp_nodelay);
    if cfg.socket_buffer > 0 {
        let bytes = cfg.socket_buffer.saturating_mul(1024);
        let _ = sock.set_send_buffer_size(bytes);
        let _ = sock.set_recv_buffer_size(bytes);
    }
    if cfg.tcp_keepalive {
        let ka = socket2::TcpKeepalive::new().with_time(Duration::from_secs(30));
        let _ = sock.set_tcp_keepalive(&ka);
    }
}
async fn dial_target(
    target: &TargetAddr,
    timeout_secs: u64,
    cfg: &RuntimeConfig,
) -> Result<TcpStream> {
    let stream = timeout(Duration::from_secs(timeout_secs.max(1)), async {
        let addr = resolve_socket(&target.host, target.port)
            .await
            .map_err(|e| std::io::Error::new(std::io::ErrorKind::Other, e.to_string()))?;
        TcpStream::connect(addr).await
    })
    .await
    .context("target connection timeout")??;
    apply_socket_options(&stream, cfg);
    Ok(stream)
}
async fn send_frame_encrypted(
    writer: &Arc<Mutex<WriteHalf<TcpStream>>>,
    cipher: &XorCipher,
    frame: &MuxFrame,
) -> Result<()> {
    let mut bytes = Vec::with_capacity(7 + frame.payload.len());
    frame
        .encode(&mut bytes)
        .map_err(|e| anyhow!(e.to_string()))?;
    cipher.apply(&mut bytes);
    let mut w = writer.lock().await;
    write_frame(&mut *w, &bytes, 2, false).await
}
async fn send_mux_parts_encrypted(
    writer: &Arc<Mutex<WriteHalf<TcpStream>>>,
    cipher: &XorCipher,
    stream_id: u32,
    command: MuxCommand,
    payload: &[u8],
) -> Result<()> {
    let mut bytes = Vec::with_capacity(7 + payload.len());
    write_frame_parts(&mut bytes, stream_id, command, payload)
        .map_err(|e| anyhow!(e.to_string()))?;
    cipher.apply(&mut bytes);
    let mut w = writer.lock().await;
    write_frame(&mut *w, &bytes, 2, false).await
}
async fn send_reset_encrypted(
    writer: &Arc<Mutex<WriteHalf<TcpStream>>>,
    cipher: &XorCipher,
    id: u32,
) -> Result<()> {
    send_frame_encrypted(
        writer,
        cipher,
        &MuxFrame::new(id, MuxCommand::Rst, Vec::new()).map_err(|e| anyhow!(e.to_string()))?,
    )
    .await
}
async fn target_to_mux(
    id: u32,
    mut target: ReadHalf<TcpStream>,
    writer: Arc<Mutex<WriteHalf<TcpStream>>>,
    buffer_size: usize,
    cipher: XorCipher,
) {
    let mut buf = vec![0u8; buffer_size.clamp(16 * 1024, 1024 * 1024)];
    loop {
        match target.read(&mut buf).await {
            Ok(0) => {
                let _ = send_mux_parts_encrypted(&writer, &cipher, id, MuxCommand::Fin, &[]).await;
                break;
            }
            Ok(n) => {
                let mut off = 0;
                while off < n {
                    let end = (off + u16::MAX as usize).min(n);
                    if send_mux_parts_encrypted(
                        &writer,
                        &cipher,
                        id,
                        MuxCommand::Data,
                        &buf[off..end],
                    )
                    .await
                    .is_err()
                    {
                        return;
                    }
                    off = end;
                }
            }
            Err(_) => {
                let _ = send_reset_encrypted(&writer, &cipher, id).await;
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
    cipher: XorCipher,
) {
    let (rd, mut wr) = tokio::io::split(target);
    let reader = tokio::spawn(target_to_mux(
        id,
        rd,
        writer.clone(),
        buffer_size,
        cipher.clone(),
    ));
    while let Some(cmd) = rx.recv().await {
        match cmd {
            StreamCommand::Data(frame) => {
                if wr.write_all(frame.payload()).await.is_err() {
                    break;
                }
            }
            StreamCommand::Fin => {
                let _ = wr.shutdown().await;
                break;
            }
            StreamCommand::Reset => break,
        }
    }
    reader.abort()
}
async fn handle_mux_parts(
    mut rd: ReadHalf<TcpStream>,
    writer: Arc<Mutex<WriteHalf<TcpStream>>>,
    cfg: RuntimeConfig,
    first_payload: Vec<u8>,
) -> Result<()> {
    let cipher = configured_cipher(&cfg.key);
    let mut hello = first_payload;
    transform_payload(&cipher, &mut hello);
    if hello != b"MUX\n" {
        bail!("invalid MUX handshake")
    }

    let mut ok = b"OK\n".to_vec();
    transform_payload(&cipher, &mut ok);
    {
        let mut w = writer.lock().await;
        write_frame(&mut *w, &ok, 2, false).await?
    };

    let streams: Arc<Mutex<HashMap<u32, StreamEntry>>> =
        Arc::new(Mutex::new(HashMap::new()));
    let mut stream_tasks = Vec::new();
    let mut frame_buf = Vec::with_capacity(64 * 1024);

    loop {
        let Some((opcode, mut payload)) = read_frame_owned(
            &mut rd,
            Option::<&mut WriteHalf<TcpStream>>::None,
            &mut frame_buf,
        )
        .await?
        else {
            break;
        };

        if opcode != 2 {
            continue;
        }

        transform_payload(&cipher, &mut payload);

        let frame = match MuxFrame::decode_owned(payload) {
            Ok(f) => f,
            Err(_) => continue,
        };

        match frame.command {
            MuxCommand::Syn => {
                let stream_id = frame.stream_id;

                let syn = match SynPayload::decode(frame.payload()) {
                    Ok(v) => v,
                    Err(_) => {
                        let _ = send_reset_encrypted(&writer, &cipher, stream_id).await;
                        continue;
                    }
                };

                let target_text = match String::from_utf8(syn.target) {
                    Ok(v) => v,
                    Err(_) => {
                        let _ = send_reset_encrypted(&writer, &cipher, stream_id).await;
                        continue;
                    }
                };

                let (host, port_text) = match target_text.rsplit_once(':') {
                    Some(v) => v,
                    None => {
                        let _ = send_reset_encrypted(&writer, &cipher, stream_id).await;
                        continue;
                    }
                };

                let port = match port_text.parse::<u16>() {
                    Ok(v) if v != 0 => v,
                    _ => {
                        let _ = send_reset_encrypted(&writer, &cipher, stream_id).await;
                        continue;
                    }
                };

                let target = TargetAddr {
                    host: host.to_string(),
                    port,
                };

                if enforce_target_policy(&cfg, &target).is_err() {
                    let _ = send_reset_encrypted(&writer, &cipher, stream_id).await;
                    continue;
                }

                let stream_limit_reached = {
                    let guard = streams.lock().await;
                    guard.len() >= MAX_STREAMS_PER_SESSION
                };

                if stream_limit_reached {
                    let _ = send_reset_encrypted(&writer, &cipher, stream_id).await;
                    continue;
                }

                let (tx, mut rx) = mpsc::channel(64);

                streams
                    .lock()
                    .await
                    .insert(stream_id, StreamEntry { tx: tx.clone() });

                if !syn.initial_data.is_empty() {
                    let initial =
                        MuxFrame::new(stream_id, MuxCommand::Data, syn.initial_data)
                            .map_err(|e| anyhow!(e.to_string()))?;

                    tx.send(StreamCommand::Data(initial))
                        .await
                        .map_err(|_| anyhow!("stream task exited before initial data"))?;
                }

                let streams_task = streams.clone();
                let writer_task = writer.clone();
                let cipher_task = cipher.clone();
                let cfg_task = cfg.clone();

                let task = tokio::spawn(async move {
                    let mut pending = Vec::new();

                    let target_stream = loop {
                        tokio::select! {
                            dial = dial_target(
                                &target,
                                cfg_task.connection_timeout,
                                &cfg_task,
                            ) => {
                                match dial {
                                    Ok(stream) => break stream,
                                    Err(_) => {
                                        let _ = send_reset_encrypted(
                                            &writer_task,
                                            &cipher_task,
                                            stream_id,
                                        )
                                        .await;
                                        streams_task.lock().await.remove(&stream_id);
                                        return;
                                    }
                                }
                            }

                            command = rx.recv() => {
                                match command {
                                    Some(StreamCommand::Data(frame)) => {
                                        pending.push(frame);
                                    }
                                    Some(StreamCommand::Fin)
                                    | Some(StreamCommand::Reset)
                                    | None => {
                                        streams_task.lock().await.remove(&stream_id);
                                        return;
                                    }
                                }
                            }
                        }
                    };

                    while let Ok(command) = rx.try_recv() {
                        match command {
                            StreamCommand::Data(frame) => pending.push(frame),
                            StreamCommand::Fin | StreamCommand::Reset => {
                                streams_task.lock().await.remove(&stream_id);
                                return;
                            }
                        }
                    }

                    if send_mux_parts_encrypted(
                        &writer_task,
                        &cipher_task,
                        stream_id,
                        MuxCommand::Data,
                        &[],
                    )
                    .await
                    .is_err()
                    {
                        streams_task.lock().await.remove(&stream_id);
                        return;
                    }

                    let (rd_target, mut wr_target) = tokio::io::split(target_stream);

                    let reader = tokio::spawn(target_to_mux(
                        stream_id,
                        rd_target,
                        writer_task.clone(),
                        cfg_task.buffer_size,
                        cipher_task.clone(),
                    ));

                    for frame in pending {
                        if wr_target.write_all(frame.payload()).await.is_err() {
                            reader.abort();
                            streams_task.lock().await.remove(&stream_id);
                            return;
                        }
                    }

                    while let Some(command) = rx.recv().await {
                        match command {
                            StreamCommand::Data(frame) => {
                                if wr_target.write_all(frame.payload()).await.is_err() {
                                    break;
                                }
                            }
                            StreamCommand::Fin => {
                                let _ = wr_target.shutdown().await;
                                break;
                            }
                            StreamCommand::Reset => break,
                        }
                    }

                    reader.abort();
                    streams_task.lock().await.remove(&stream_id);
                });

                stream_tasks.push(task);
            }

            MuxCommand::Data => {
                let id = frame.stream_id;

                if let Some(tx) = streams.lock().await.get(&id).map(|s| s.tx.clone()) {
                    if tx.send(StreamCommand::Data(frame)).await.is_err() {
                        streams.lock().await.remove(&id);
                    }
                }
            }

            MuxCommand::Fin => {
                if let Some(tx) = streams
                    .lock()
                    .await
                    .get(&frame.stream_id)
                    .map(|s| s.tx.clone())
                {
                    let _ = tx.send(StreamCommand::Fin).await;
                }
            }

            MuxCommand::Rst => {
                let tx = streams
                    .lock()
                    .await
                    .remove(&frame.stream_id)
                    .map(|s| s.tx);

                if let Some(tx) = tx {
                    let _ = tx.send(StreamCommand::Reset).await;
                }
            }
        }
    }

    for task in stream_tasks {
        task.abort();
    }

    streams.lock().await.clear();

    Ok(())
}
async fn resolve_udp_target(target: &TargetAddr) -> Result<SocketAddr> {
    resolve_socket(&target.host, target.port).await
}
fn udp_envelope(source: SocketAddr, payload: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(22 + payload.len());
    out.extend_from_slice(&[0, 0, 0]);
    match source.ip() {
        IpAddr::V4(ip) => {
            out.push(1);
            out.extend_from_slice(&ip.octets())
        }
        IpAddr::V6(ip) => {
            out.push(4);
            out.extend_from_slice(&ip.octets())
        }
    }
    out.extend_from_slice(&source.port().to_be_bytes());
    out.extend_from_slice(payload);
    out
}
async fn handle_server_udp_parts(
    mut rd: ReadHalf<TcpStream>,
    writer: Arc<Mutex<WriteHalf<TcpStream>>>,
    cfg: RuntimeConfig,
    first_payload: Vec<u8>,
) -> Result<()> {
    let cipher = configured_cipher(&cfg.key);
    let mut hello = first_payload;
    transform_payload(&cipher, &mut hello);
    if hello != b"UDP\n" {
        bail!("invalid UDP handshake")
    };
    let mut ok = b"OK\n".to_vec();
    transform_payload(&cipher, &mut ok);
    {
        let mut w = writer.lock().await;
        write_frame(&mut *w, &ok, 2, false).await?
    };
    let udp = Arc::new(UdpSocket::bind("0.0.0.0:0").await?);
    let udp_send = udp.clone();
    let writer_send = writer.clone();
    let cipher_send = cipher.clone();
    let send_task = tokio::spawn(async move {
        let mut buf = vec![0u8; 64 * 1024];
        loop {
            let (n, source) = udp_send.recv_from(&mut buf).await?;
            let mut packet = udp_envelope(source, &buf[..n]);
            cipher_send.apply(&mut packet);
            let mut w = writer_send.lock().await;
            write_frame(&mut *w, &packet, 2, false).await?
        }
        Result::<()>::Ok(())
    });
    let mut frame_buf = Vec::with_capacity(64 * 1024);
    loop {
        let Some((opcode, mut packet)) = read_frame(
            &mut rd,
            Option::<&mut WriteHalf<TcpStream>>::None,
            &mut frame_buf,
        )
        .await?
        else {
            break;
        };
        if opcode != 2 {
            continue;
        }
        cipher.apply(&mut packet);
        let (target, payload) =
            parse_socks5_udp_datagram(&packet).map_err(|e| anyhow!(e.to_string()))?;
        if cfg.block_local && is_blocked_local_host(&target.host) {
            continue;
        }
        let addr = match resolve_udp_target(&target).await {
            Ok(v) => v,
            Err(_) => continue,
        };
        let _ = udp.send_to(payload, addr).await?;
    }
    send_task.abort();
    Ok(())
}

pub async fn run_server(cfg: RuntimeConfig) -> Result<()> {
    let listener = TcpListener::bind(format!("{}:{}", cfg.proxy_host, cfg.proxy_port)).await?;
    let semaphore = Arc::new(Semaphore::new(cfg.max_connections.max(1)));
    tracing::info!(
        "RushWay server listening on {}:{}",
        cfg.proxy_host,
        cfg.proxy_port
    );
    loop {
        let (stream, peer) = listener.accept().await?;
        let permit = match semaphore.clone().try_acquire_owned() {
            Ok(v) => v,
            Err(_) => {
                tracing::debug!(%peer,"maximum concurrent connections reached");
                continue;
            }
        };
        let cfg2 = cfg.clone();
        apply_socket_options(&stream, &cfg2);
        tokio::spawn(async move {
            let _permit = permit;
            let result = async {
                let (mut rd, mut wr) = tokio::io::split(stream);
                let request = read_http_headers(&mut rd).await?;
                let key = validate_server_handshake(&request)?;
                wr.write_all(&build_server_handshake_response(&key)).await?;
                wr.flush().await?;
                let writer = Arc::new(Mutex::new(wr));
                let mut buf = Vec::with_capacity(64 * 1024);
                let Some((opcode, first)) =
                    read_frame(&mut rd, Option::<&mut WriteHalf<TcpStream>>::None, &mut buf)
                        .await?
                else {
                    bail!("missing transport handshake")
                };
                if opcode != 2 {
                    bail!("invalid transport handshake opcode")
                };
                let mut plain = first.clone();
                let cipher = configured_cipher(&cfg2.key);
                transform_payload(&cipher, &mut plain);
                if plain == b"UDP\n" {
                    return handle_server_udp_parts(rd, writer, cfg2, first).await;
                }
                if plain == b"MUX\n" {
                    return handle_mux_parts(rd, writer, cfg2, first).await;
                }
                bail!("unknown transport handshake")
            }
            .await;
            if let Err(e) = result {
                tracing::debug!(%peer,error=%e,"transport connection closed")
            }
        });
    }
}
