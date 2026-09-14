use crate::crypto::XorCipher;
use crate::proxy::{parse_socks5_udp_datagram, TargetAddr};
use crate::runtime::{enforce_target_policy, RuntimeConfig};
use crate::ws::{build_client_handshake_request, read_frame, read_http_headers, validate_client_handshake_response, write_frame};
use anyhow::{anyhow, bail, Result};
use std::net::{IpAddr, SocketAddr};
use std::sync::Arc;
use tokio::io::{AsyncReadExt, AsyncWriteExt, WriteHalf};
use tokio::net::{TcpStream, UdpSocket};
use tokio::sync::Mutex;
use tokio::time::{timeout, Duration};

fn cipher(key: &Option<String>) -> XorCipher {
    XorCipher::new(key.as_deref().unwrap_or(""))
}

fn parse_ws_url(input: &str) -> Result<(String, String)> {
    let rest = input
        .strip_prefix("ws://")
        .ok_or_else(|| anyhow!("UDP relay requires ws:// upstream"))?;
    let (authority, path) = match rest.split_once('/') {
        Some((a, p)) => (a.to_string(), format!("/{}", p)),
        None => (rest.to_string(), "/".into()),
    };
    let authority = if authority.contains(':') {
        authority
    } else {
        format!("{}:80", authority)
    };
    Ok((authority, path))
}

pub async fn handle_local_udp_proxy(
    mut control: TcpStream,
    cfg: RuntimeConfig,
    bind_hint: TargetAddr,
) -> Result<()> {
    let bind_ip = if bind_hint.host == "0.0.0.0" || bind_hint.host.is_empty() {
        "0.0.0.0"
    } else {
        bind_hint.host.as_str()
    };
    let udp = Arc::new(UdpSocket::bind(format!("{}:0", bind_ip)).await?);
    let bound = udp.local_addr()?;
    let mut resp = [0u8; 10];
    resp[0] = 5;
    resp[1] = 0;
    resp[2] = 0;
    resp[3] = 1;
    if let IpAddr::V4(ip) = bound.ip() {
        resp[4..8].copy_from_slice(&ip.octets())
    }
    resp[8..10].copy_from_slice(&bound.port().to_be_bytes());
    control.write_all(&resp).await?;
    let upstream = cfg
        .upstream
        .clone()
        .ok_or_else(|| anyhow!("client mode requires upstream"))?;
    let (host, path) = parse_ws_url(&upstream)?;
    let actual_host = cfg.fakehost.clone().unwrap_or_else(|| host.clone());
    let socket = timeout(
        Duration::from_secs(cfg.connection_timeout.max(1)),
        TcpStream::connect(&host),
    )
    .await??;
    if cfg.tcp_nodelay {
        socket.set_nodelay(true).ok();
    }
    let (mut rd, mut wr) = tokio::io::split(socket);
    let origin = Some(format!("https://{}", actual_host));
    let sec_fetch_site = if host.eq_ignore_ascii_case(&actual_host) {
        "same-origin"
    } else {
        "cross-site"
    };
    let (request, key) = build_client_handshake_request(
        &actual_host,
        &path,
        origin.as_deref(),
        Some(sec_fetch_site),
    );
    wr.write_all(&request).await?;
    wr.flush().await?;
    let response = read_http_headers(&mut rd).await?;
    validate_client_handshake_response(&response, &key)?;
    let writer = Arc::new(Mutex::new(wr));
    let c = cipher(&cfg.key);
    let mut hello = b"UDP\n".to_vec();
    c.apply(&mut hello);
    {
        let mut w = writer.lock().await;
        write_frame(&mut *w, &hello, 2, true).await?
    };
    let mut frame_buf = Vec::with_capacity(64 * 1024);
    let Some((opcode, mut ok)) =
        read_frame(&mut rd, Option::<&mut WriteHalf<TcpStream>>::None, &mut frame_buf).await?
    else {
        bail!("upstream closed during UDP handshake")
    };
    if opcode != 2 {
        bail!("invalid UDP handshake response opcode")
    }
    c.apply(&mut ok);
    if ok != b"OK\n" {
        bail!("upstream rejected UDP handshake")
    }
    let latest = Arc::new(Mutex::new(None::<SocketAddr>));
    let udp_send = udp.clone();
    let writer_send = writer.clone();
    let c_send = c.clone();
    let latest_send = latest.clone();
    let upload = tokio::spawn(async move {
        let mut buf = vec![0u8; 64 * 1024];
        loop {
            let (n, peer) = udp_send.recv_from(&mut buf).await?;
            *latest_send.lock().await = Some(peer);
            let mut packet = buf[..n].to_vec();
            c_send.apply(&mut packet);
            let mut w = writer_send.lock().await;
            write_frame(&mut *w, &packet, 2, true).await?
        }
        Result::<()>::Ok(())
    });
    loop {
        let Some((opcode, mut packet)) =
            read_frame(&mut rd, Option::<&mut WriteHalf<TcpStream>>::None, &mut frame_buf).await?
        else {
            break
        };
        if opcode != 2 {
            continue
        }
        c.apply(&mut packet);
        let (target, payload) =
            parse_socks5_udp_datagram(&packet).map_err(|e| anyhow!(e.to_string()))?;
        if enforce_target_policy(&cfg, &target).is_err() {
            continue
        }
        if let Some(peer) = *latest.lock().await {
            let _ = udp.send_to(payload, peer).await?;
        }
    }
    upload.abort();
    control.shutdown().await.ok();
    Ok(())
}
