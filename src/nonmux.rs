//! Plain WebSocket 1:1 relay compatibility path.
//!
//! GoWay v1.8.4 non-MUX mode dedicates one physical WebSocket to one target.
//! The first binary payload is `host:port\n` (XOR-transformed when a key is set),
//! the server answers `OK\n`, and subsequent binary frames carry raw TCP data.

use crate::crypto::XorCipher;
use crate::proxy::{parse_http_connect, parse_socks5_request, socks5_success_response, SocksCommand, TargetAddr, SOCKS5_CONNECT, SOCKS5_UDP_ASSOCIATE, SOCKS5_VERSION};
use crate::runtime::RuntimeConfig;
use crate::ws::{build_client_handshake_request, build_server_handshake_response, read_frame, read_http_headers, validate_client_handshake_response, validate_server_handshake, write_frame};
use anyhow::{anyhow, bail, Context, Result};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::Mutex;
use tokio::time::{timeout, Duration};
use std::sync::Arc;

fn cipher(key: &Option<String>) -> XorCipher {
    XorCipher::new(key.as_deref().unwrap_or(""))
}

fn parse_ws_url(input: &str) -> Result<(String, String)> {
    let rest = input.strip_prefix("ws://").ok_or_else(|| anyhow!("non-MUX plain client requires ws:// upstream"))?;
    let (authority, path) = match rest.split_once('/') {
        Some((a, p)) => (a.to_string(), format!("/{}", p)),
        None => (rest.to_string(), "/".to_string()),
    };
    let authority = if authority.contains(':') { authority } else { format!("{}:80", authority) };
    Ok((authority, path))
}

async fn read_proxy_request(stream: &mut TcpStream) -> Result<(SocksCommand, TargetAddr, bool)> {
    let first = stream.read_u8().await?;
    if first == SOCKS5_VERSION {
        let n = stream.read_u8().await? as usize;
        let mut methods = vec![0u8; n];
        stream.read_exact(&mut methods).await?;
        if !methods.contains(&0) {
            stream.write_all(&[5, 0xff]).await?;
            bail!("SOCKS5 no-auth unavailable");
        }
        stream.write_all(&[5, 0]).await?;
        let mut head = [0u8; 4];
        stream.read_exact(&mut head).await?;
        if head[1] != SOCKS5_CONNECT && head[1] != SOCKS5_UDP_ASSOCIATE {
            stream.write_all(&[5, 7, 0, 1, 0, 0, 0, 0, 0, 0]).await?;
            bail!("unsupported SOCKS5 command");
        }
        let mut req = head.to_vec();
        match head[3] {
            1 => { let mut b = [0u8; 6]; stream.read_exact(&mut b).await?; req.extend_from_slice(&b); }
            3 => {
                let mut n = [0u8; 1];
                stream.read_exact(&mut n).await?;
                req.extend_from_slice(&n);
                let mut b = vec![0u8; n[0] as usize + 2];
                stream.read_exact(&mut b).await?;
                req.extend_from_slice(&b);
            }
            4 => { let mut b = [0u8; 18]; stream.read_exact(&mut b).await?; req.extend_from_slice(&b); }
            _ => { stream.write_all(&[5, 8, 0, 1, 0, 0, 0, 0, 0, 0]).await?; bail!("unsupported SOCKS5 address type"); }
        }
        let parsed = parse_socks5_request(&req).map_err(|e| anyhow!(e.to_string()))?;
        return Ok((parsed.command, parsed.target, true));
    }
    if first == b'C' {
        let mut buf = vec![first];
        let mut tail = read_http_headers(stream).await?;
        buf.append(&mut tail);
        return Ok((SocksCommand::Connect, parse_http_connect(&buf).map_err(|e| anyhow!(e.to_string()))?, false));
    }
    bail!("unsupported local proxy protocol")
}

async fn open_upstream(cfg: &RuntimeConfig) -> Result<(tokio::io::ReadHalf<TcpStream>, Arc<Mutex<tokio::io::WriteHalf<TcpStream>>>, String)> {
    let upstream = cfg.upstream.as_deref().ok_or_else(|| anyhow!("client mode requires upstream"))?;
    let (addr, path) = parse_ws_url(upstream)?;
    let host = addr.rsplit_once(':').map(|(h, _)| h).unwrap_or(addr.as_str()).to_string();
    let header_host = cfg.fakehost.as_deref().unwrap_or(&host).to_string();
    let tcp = timeout(Duration::from_secs(cfg.connection_timeout.max(1)), TcpStream::connect(&addr))
        .await
        .context("upstream connection timeout")??;
    tcp.set_nodelay(true).ok();
    let (mut rd, mut wr) = tokio::io::split(tcp);
    let origin = format!("https://{}", host);
    let sec_fetch_site = if header_host.eq_ignore_ascii_case(&host) { "same-origin" } else { "cross-site" };
    let (request, key) = build_client_handshake_request(&header_host, &path, Some(&origin), Some(sec_fetch_site));
    wr.write_all(&request).await?;
    wr.flush().await?;
    let response = read_http_headers(&mut rd).await?;
    validate_client_handshake_response(&response, &key)?;
    Ok((rd, Arc::new(Mutex::new(wr)), host))
}

async fn relay_client(mut local: TcpStream, cfg: RuntimeConfig, target: TargetAddr, is_socks5: bool) -> Result<()> {
    let (mut rd, writer, _) = open_upstream(&cfg).await?;
    let c = cipher(&cfg.key);
    let mut hello = format!("{}:{}\n", target.host, target.port).into_bytes();
    c.apply(&mut hello);
    {
        let mut w = writer.lock().await;
        write_frame(&mut *w, &hello, 2, true).await?;
    }
    let mut frame_buf = Vec::with_capacity(64 * 1024);
    let Some((opcode, mut ok)) = read_frame(&mut rd, Option::<&mut tokio::io::WriteHalf<TcpStream>>::None, &mut frame_buf).await? else {
        bail!("upstream closed before non-MUX OK");
    };
    if opcode != 2 { bail!("invalid non-MUX handshake opcode"); }
    c.apply(&mut ok);
    if ok != b"OK\n" { bail!("upstream rejected non-MUX target"); }

    if is_socks5 { local.write_all(&socks5_success_response()).await?; }
    else { local.write_all(b"HTTP/1.1 200 Connection Established\r\n\r\n").await?; }

    let (mut local_rd, mut local_wr) = tokio::io::split(local);
    let writer_up = writer.clone();
    let upload = tokio::spawn(async move {
        let mut buf = vec![0u8; cfg.buffer_size.clamp(16 * 1024, 1024 * 1024)];
        loop {
            let n = local_rd.read(&mut buf).await?;
            if n == 0 { break; }
            let mut w = writer_up.lock().await;
            write_frame(&mut *w, &buf[..n], 2, true).await?;
        }
        Result::<()>::Ok(())
    });

    loop {
        let Some((opcode, mut payload)) = read_frame(&mut rd, Option::<&mut tokio::io::WriteHalf<TcpStream>>::None, &mut frame_buf).await? else { break; };
        match opcode {
            2 => { c.apply(&mut payload); local_wr.write_all(&payload).await?; }
            8 => break,
            _ => {}
        }
    }
    upload.abort();
    Ok(())
}

pub async fn run_client(cfg: RuntimeConfig) -> Result<()> {
    let listener = TcpListener::bind(format!("{}:{}", cfg.proxy_host, cfg.proxy_port)).await?;
    tracing::info!("RushWay non-MUX client proxy listening on {}:{}", cfg.proxy_host, cfg.proxy_port);
    loop {
        let (stream, peer) = listener.accept().await?;
        let cfg2 = cfg.clone();
        tokio::spawn(async move {
            match read_proxy_request(&mut stream.try_clone().unwrap_or_else(|_| unreachable!())) {
                Ok(_) => {}
                Err(_) => {}
            }
            drop(peer);
            let _ = cfg2;
        });
    }
}

pub async fn run_server(cfg: RuntimeConfig) -> Result<()> {
    let listener = TcpListener::bind(format!("{}:{}", cfg.proxy_host, cfg.proxy_port)).await?;
    tracing::info!("RushWay non-MUX server listening on {}:{}", cfg.proxy_host, cfg.proxy_port);
    loop {
        let (stream, peer) = listener.accept().await?;
        let cfg2 = cfg.clone();
        tokio::spawn(async move {
            if let Err(error) = handle_server(stream, cfg2).await {
                tracing::debug!(%peer, %error, "non-MUX transport closed");
            }
        });
    }
}

async fn handle_server(stream: TcpStream, cfg: RuntimeConfig) -> Result<()> {
    let (mut rd, mut wr) = tokio::io::split(stream);
    let request = read_http_headers(&mut rd).await?;
    let key = validate_server_handshake(&request)?;
    wr.write_all(&build_server_handshake_response(&key)).await?;
    wr.flush().await?;
    let writer = Arc::new(Mutex::new(wr));
    let mut frame_buf = Vec::with_capacity(64 * 1024);
    let Some((opcode, mut target_frame)) = read_frame(&mut rd, Option::<&mut tokio::io::WriteHalf<TcpStream>>::None, &mut frame_buf).await? else { bail!("missing non-MUX target"); };
    if opcode != 2 { bail!("invalid non-MUX target opcode"); }
    let c = cipher(&cfg.key);
    c.apply(&mut target_frame);
    let target_line = String::from_utf8(target_frame).map_err(|_| anyhow!("invalid non-MUX target UTF-8"))?;
    let target_line = target_line.trim();
    let target = target_line.strip_prefix(&format!("{} ", cfg.key.as_deref().unwrap_or("__NO_KEY__")))
        .unwrap_or(target_line);
    if cfg.key.is_some() && !target_line.starts_with(&format!("{} ", cfg.key.as_deref().unwrap())) {
        bail!("non-MUX authentication failed");
    }
    let target = target.trim();
    let stream_target = target.parse::<std::net::SocketAddr>()
        .map(|a| TargetAddr { host: a.ip().to_string(), port: a.port() })
        .or_else(|_| {
            let (host, port) = target.rsplit_once(':').ok_or_else(|| anyhow!("invalid non-MUX target"))?;
            Ok(TargetAddr { host: host.to_string(), port: port.parse::<u16>().map_err(|_| anyhow!("invalid non-MUX port"))? })
        })?;
    let addr = format!("{}:{}", stream_target.host, stream_target.port);
    let target_stream = timeout(Duration::from_secs(cfg.connection_timeout.max(1)), TcpStream::connect(&addr)).await??;
    let mut ok = b"OK\n".to_vec();
    c.apply(&mut ok);
    { let mut w = writer.lock().await; write_frame(&mut *w, &ok, 2, false).await?; }

    let (mut target_rd, mut target_wr) = tokio::io::split(target_stream);
    let writer_down = writer.clone();
    let download = tokio::spawn(async move {
        let mut buf = vec![0u8; cfg.buffer_size.clamp(16 * 1024, 1024 * 1024)];
        loop {
            let n = target_rd.read(&mut buf).await?;
            if n == 0 { break; }
            let mut payload = buf[..n].to_vec();
            // Server->client frames are XOR-transformed by the transport cipher.
            c.apply(&mut payload);
            let mut w = writer_down.lock().await;
            write_frame(&mut *w, &payload, 2, false).await?;
        }
        Result::<()>::Ok(())
    });

    loop {
        let Some((opcode, mut payload)) = read_frame(&mut rd, Option::<&mut tokio::io::WriteHalf<TcpStream>>::None, &mut frame_buf).await? else { break; };
        if opcode == 8 { break; }
        if opcode != 2 { continue; }
        c.apply(&mut payload);
        target_wr.write_all(&payload).await?;
    }
    download.abort();
    Ok(())
}
