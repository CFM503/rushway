//! RFC 6455 transport pieces used by the GoWay-compatible transport.
//!
//! The codec deliberately does not implement fragmentation: GoWay v1.8.4
//! accepts complete data frames and handles Ping/Pong/Close itself.

use anyhow::{anyhow, bail, Result};
use base64::{engine::general_purpose::STANDARD, Engine as _};
use rand::RngCore;
use sha1::{Digest, Sha1};
use std::cell::RefCell;
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};

pub const MAX_WS_FRAME_SIZE: usize = 64 * 1024 * 1024;
pub const MAX_HTTP_HEADER_SIZE: usize = 8192;
const WS_GUID: &[u8] = b"258EAFA5-E914-47DA-95CA-C5AB0DC85B11";
const BROWSER_UA: &str = "Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/136.0.0.0 Safari/537.36";
const BROWSER_ACCEPT_LANGUAGE: &str = "en-US,en;q=0.9";
const BROWSER_ACCEPT_ENCODING: &str = "gzip, deflate, br, zstd";
const BROWSER_SEC_CH_UA: &str =
    "\"Chromium\";v=\"136\", \"Google Chrome\";v=\"136\", \"Not.A/Brand\";v=\"99\"";
const SMALL_FRAME_SIZE: usize = 512;

thread_local! {
    static MASK_STATE: RefCell<u64> = const { RefCell::new(0) };
}

fn next_mask() -> [u8; 4] {
    MASK_STATE.with(|state| {
        let mut value = state.borrow_mut();
        if *value == 0 {
            let mut seed = [0u8; 8];
            rand::thread_rng().fill_bytes(&mut seed);
            *value = u64::from_le_bytes(seed);
            if *value == 0 {
                *value = 0x9e3779b97f4a7c15;
            }
        }
        let mut x = *value;
        x ^= x << 13;
        x ^= x >> 7;
        x ^= x << 17;
        *value = x;
        (x as u32).to_ne_bytes()
    })
}

pub fn compute_accept_key(challenge: &str) -> String {
    let mut h = Sha1::new();
    h.update(challenge.trim().as_bytes());
    h.update(WS_GUID);
    STANDARD.encode(h.finalize())
}

fn header_value<'a>(headers: &'a str, name: &str) -> Option<&'a str> {
    headers.lines().find_map(|line| {
        let (k, v) = line.split_once(':')?;
        if k.trim().eq_ignore_ascii_case(name) {
            Some(v.trim())
        } else {
            None
        }
    })
}

fn token_contains(value: &str, token: &str) -> bool {
    value
        .split(',')
        .any(|v| v.trim().eq_ignore_ascii_case(token))
}

pub async fn read_http_headers<R: AsyncRead + Unpin>(r: &mut R) -> Result<Vec<u8>> {
    read_http_headers_timeout(r, std::time::Duration::from_secs(60)).await
}

pub async fn read_http_headers_timeout<R: AsyncRead + Unpin>(
    r: &mut R,
    timeout_duration: std::time::Duration,
) -> Result<Vec<u8>> {
    let mut out = Vec::with_capacity(1024);
    let mut b = [0u8; 1];
    let deadline = tokio::time::Instant::now() + timeout_duration;

    loop {
        let now = tokio::time::Instant::now();
        if now >= deadline {
            if out.is_empty() {
                tracing::warn!("[WSS] HTTP handshake timeout; received 0 response bytes");
                bail!("WSS HTTP handshake response timeout; received 0 response bytes");
            } else {
                let partial = String::from_utf8_lossy(&out);
                tracing::warn!("[WSS] HTTP handshake timeout; partial response:\n{}", partial);
                bail!(
                    "WSS HTTP handshake response timeout; partial response:\n{}",
                    partial
                );
            }
        }
        let remaining = deadline - now;
        match tokio::time::timeout(remaining, r.read_exact(&mut b)).await {
            Ok(Ok(_)) => {
                out.push(b[0]);
                if out.len() >= 4 && out[out.len() - 4..] == *b"\r\n\r\n" {
                    return Ok(out);
                }
                if out.len() >= 2 && out[out.len() - 2..] == *b"\n\n" {
                    return Ok(out);
                }
                if out.len() >= MAX_HTTP_HEADER_SIZE {
                    bail!("header too large");
                }
            }
            Ok(Err(e)) => {
                if out.is_empty() {
                    tracing::warn!(
                        "[WSS] HTTP handshake connection closed by peer; received 0 response bytes ({})",
                        e
                    );
                    bail!(
                        "WSS HTTP handshake connection closed by peer; received 0 response bytes ({})",
                        e
                    );
                } else {
                    let partial = String::from_utf8_lossy(&out);
                    tracing::warn!(
                        "[WSS] HTTP handshake connection closed by peer; partial response:\n{}",
                        partial
                    );
                    bail!(
                        "WSS HTTP handshake connection closed by peer; partial response:\n{}",
                        partial
                    );
                }
            }
            Err(_elapsed) => {
                if out.is_empty() {
                    tracing::warn!("[WSS] HTTP handshake timeout; received 0 response bytes");
                    bail!("WSS HTTP handshake response timeout; received 0 response bytes");
                } else {
                    let partial = String::from_utf8_lossy(&out);
                    tracing::warn!("[WSS] HTTP handshake timeout; partial response:\n{}", partial);
                    bail!(
                        "WSS HTTP handshake response timeout; partial response:\n{}",
                        partial
                    );
                }
            }
        }
    }
}

pub fn validate_server_handshake(request: &[u8]) -> Result<String> {
    if request.len() > MAX_HTTP_HEADER_SIZE {
        bail!("header too large");
    }
    let text = std::str::from_utf8(request).map_err(|_| anyhow!("invalid HTTP header"))?;
    if !(text.ends_with("\r\n\r\n") || text.ends_with("\n\n")) {
        bail!("incomplete websocket handshake");
    }
    let first = text
        .lines()
        .next()
        .ok_or_else(|| anyhow!("empty HTTP request"))?;
    let mut parts = first.split_whitespace();
    let method = parts.next().unwrap_or("");
    let _path = parts.next().unwrap_or("");
    let version = parts.next().unwrap_or("");
    if method != "GET" || !version.starts_with("HTTP/") {
        bail!("invalid websocket request line");
    }
    let upgrade =
        header_value(text, "Upgrade").ok_or_else(|| anyhow!("missing upgrade: websocket"))?;
    if !token_contains(upgrade, "websocket") {
        bail!("missing upgrade: websocket");
    }
    let connection =
        header_value(text, "Connection").ok_or_else(|| anyhow!("missing connection: upgrade"))?;
    if !token_contains(connection, "Upgrade") {
        bail!("missing connection: upgrade");
    }
    let key = header_value(text, "Sec-WebSocket-Key")
        .ok_or_else(|| anyhow!("missing sec-websocket-key"))?;
    if key.is_empty() {
        bail!("missing sec-websocket-key");
    }
    Ok(key.to_string())
}

pub fn build_server_handshake_response(key: &str) -> Vec<u8> {
    format!("HTTP/1.1 101 Switching Protocols\r\nUpgrade: websocket\r\nConnection: Upgrade\r\nSec-WebSocket-Accept: {}\r\n\r\n", compute_accept_key(key)).into_bytes()
}

pub fn build_client_handshake_request(
    host: &str,
    path: &str,
    origin: Option<&str>,
    sec_fetch_site: Option<&str>,
) -> (Vec<u8>, String) {
    let mut key_bytes = [0u8; 16];
    rand::thread_rng().fill_bytes(&mut key_bytes);
    let key = STANDARD.encode(key_bytes);
    let path = if path.is_empty() { "/" } else { path };
    let site = sec_fetch_site.unwrap_or("cross-site");
    let mut req = format!(
        "GET {path} HTTP/1.1\r\nHost: {host}\r\nConnection: Upgrade\r\nUpgrade: websocket\r\n"
    );
    if let Some(origin) = origin {
        req.push_str(&format!("Origin: {origin}\r\n"));
    }
    req.push_str("Pragma: no-cache\r\nCache-Control: no-cache\r\n");
    req.push_str(&format!("User-Agent: {BROWSER_UA}\r\nAccept-Language: {BROWSER_ACCEPT_LANGUAGE}\r\nAccept-Encoding: {BROWSER_ACCEPT_ENCODING}\r\n"));
    req.push_str(&format!("sec-ch-ua: {BROWSER_SEC_CH_UA}\r\nsec-ch-ua-mobile: ?0\r\nsec-ch-ua-platform: \"Windows\"\r\n"));
    req.push_str(&format!("Sec-WebSocket-Version: 13\r\nSec-WebSocket-Key: {key}\r\nSec-Fetch-Dest: websocket\r\nSec-Fetch-Mode: websocket\r\nSec-Fetch-Site: {site}\r\n\r\n"));
    (req.into_bytes(), key)
}

pub fn redact_handshake_request(req: &[u8]) -> String {
    let s = String::from_utf8_lossy(req);
    let mut out = String::new();
    for line in s.lines() {
        if line.to_ascii_lowercase().starts_with("sec-websocket-key:") {
            out.push_str("Sec-WebSocket-Key: [REDACTED]\r\n");
        } else {
            out.push_str(line);
            out.push_str("\r\n");
        }
    }
    out
}

pub fn validate_client_handshake_response(response: &[u8], key: &str) -> Result<()> {
    if response.len() > MAX_HTTP_HEADER_SIZE {
        bail!("header too large");
    }
    let text = std::str::from_utf8(response).map_err(|_| anyhow!("invalid HTTP response"))?;
    let first = text
        .lines()
        .next()
        .ok_or_else(|| anyhow!("empty HTTP response"))?;
    let mut parts = first.splitn(3, ' ');
    let proto = parts.next().unwrap_or("");
    let status = parts.next().unwrap_or("");
    let reason = parts.next().unwrap_or("");
    if !proto.starts_with("HTTP/") {
        bail!("malformed websocket handshake response: {first}");
    }
    if status != "101" {
        tracing::warn!(
            "[WSS] HTTP response status not 101: HTTP {}\nFirst line: {}\nHeaders:\n{}",
            status,
            first,
            text.trim_end()
        );
        match status {
            "200" => {
                bail!("handshake failed: server returned 200 OK instead of 101 Switching Protocols (upstream may not support WebSocket)")
            }
            "301" | "302" | "307" | "308" => bail!("handshake failed: HTTP {status} redirect"),
            "400" => bail!("handshake failed: HTTP 400 Bad Request"),
            "403" => bail!("handshake failed: HTTP 403 Forbidden"),
            "404" => bail!("handshake failed: HTTP 404 Not Found"),
            "502" => bail!("handshake failed: HTTP 502 Bad Gateway"),
            "503" => bail!("handshake failed: HTTP 503 Service Unavailable"),
            "504" => bail!("handshake failed: HTTP 504 Gateway Timeout"),
            _ => bail!("handshake failed: unexpected HTTP status {status} ({reason})"),
        }
    }
    if !token_contains(
        header_value(text, "Upgrade").ok_or_else(|| anyhow!("missing Upgrade header"))?,
        "websocket",
    ) {
        bail!("handshake failed: missing Upgrade: websocket");
    }
    if !token_contains(
        header_value(text, "Connection").ok_or_else(|| anyhow!("missing Connection: Upgrade"))?,
        "Upgrade",
    ) {
        bail!("handshake failed: missing Connection: Upgrade");
    }
    let accept = header_value(text, "Sec-WebSocket-Accept")
        .ok_or_else(|| anyhow!("handshake failed: missing Sec-WebSocket-Accept"))?;
    if accept != compute_accept_key(key) {
        bail!("handshake failed: invalid Sec-WebSocket-Accept");
    }
    Ok(())
}

pub async fn write_frame<W: AsyncWrite + Unpin>(
    w: &mut W,
    payload: &[u8],
    opcode: u8,
    mask: bool,
) -> Result<()> {
    if payload.len() > MAX_WS_FRAME_SIZE {
        return Err(anyhow!("websocket frame too large"));
    }
    if opcode >= 0x8 && payload.len() > 125 {
        return Err(anyhow!("control frame payload exceeds 125 bytes"));
    }

    let mut header = [0u8; 14];
    let header_len = if payload.len() <= 125 {
        2
    } else if payload.len() <= u16::MAX as usize {
        4
    } else {
        10
    };
    header[0] = 0x80 | (opcode & 0x0f);
    let mask_bit = if mask { 0x80 } else { 0 };
    match header_len {
        2 => header[1] = mask_bit | payload.len() as u8,
        4 => {
            header[1] = mask_bit | 126;
            header[2..4].copy_from_slice(&(payload.len() as u16).to_be_bytes());
        }
        10 => {
            header[1] = mask_bit | 127;
            header[2..10].copy_from_slice(&(payload.len() as u64).to_be_bytes());
        }
        _ => unreachable!(),
    }

    if !mask {
        w.write_all(&header[..header_len]).await?;
        w.write_all(payload).await?;
        return Ok(());
    }

    let total = header_len + 4 + payload.len();
    if total <= SMALL_FRAME_SIZE + 14 {
        let mut frame = [0u8; SMALL_FRAME_SIZE + 14];
        frame[..header_len].copy_from_slice(&header[..header_len]);
        let key = next_mask();
        frame[header_len..header_len + 4].copy_from_slice(&key);
        frame[header_len + 4..total].copy_from_slice(payload);
        for (i, byte) in frame[header_len + 4..total].iter_mut().enumerate() {
            *byte ^= key[i & 3];
        }
        w.write_all(&frame[..total]).await?;
        return Ok(());
    }

    let mut frame = Vec::with_capacity(total);
    frame.extend_from_slice(&header[..header_len]);
    let key = next_mask();
    frame.extend_from_slice(&key);
    frame.extend_from_slice(payload);
    for (i, byte) in frame[header_len + 4..].iter_mut().enumerate() {
        *byte ^= key[i & 3];
    }
    w.write_all(&frame).await?;
    Ok(())
}

pub async fn read_frame<R, W>(
    r: &mut R,
    mut reply: Option<&mut W>,
    buf: &mut Vec<u8>,
) -> Result<Option<(u8, Vec<u8>)>>
where
    R: AsyncRead + Unpin,
    W: AsyncWrite + Unpin,
{
    loop {
        let mut first = [0u8; 1];
        match r.read(&mut first).await {
            Ok(0) => return Ok(None),
            Ok(_) => {}
            Err(e) => return Err(e.into()),
        }
        let b0 = first[0];
        let b1 = r.read_u8().await?;
        let fin = b0 & 0x80 != 0;
        let opcode = b0 & 0x0f;
        let masked = b1 & 0x80 != 0;
        let mut len = (b1 & 0x7f) as u64;
        if len == 126 {
            len = r.read_u16().await? as u64;
        } else if len == 127 {
            len = r.read_u64().await?;
        }
        if len > MAX_WS_FRAME_SIZE as u64 {
            return Err(anyhow!("frame too large"));
        }
        if opcode >= 0x8 {
            if !fin || len > 125 {
                return Err(anyhow!("invalid websocket control frame"));
            }
        } else if opcode == 0 || !fin || (opcode != 1 && opcode != 2) {
            return Err(anyhow!("unsupported or fragmented websocket frame"));
        }
        let mut key = [0u8; 4];
        if masked {
            r.read_exact(&mut key).await?;
        }
        // Grow incrementally in 64 KiB segments instead of a single
        // `resize(len)`: a bogus 64 MB length prefix no longer causes an
        // instant OOM — the peer must actually send the bytes (and hit the
        // read timeout) to consume memory.
        const READ_SEGMENT: usize = 64 * 1024;
        buf.clear();
        let total = len as usize;
        buf.reserve(total.min(READ_SEGMENT));
        let mut read: usize = 0;
        while read < total {
            let chunk = (total - read).min(READ_SEGMENT);
            let start = buf.len();
            buf.resize(start + chunk, 0);
            if r.read_exact(&mut buf[start..]).await.is_err() {
                buf.truncate(start);
                return Err(anyhow!("websocket payload truncated"));
            }
            read += chunk;
        }
        if masked {
            for (i, b) in buf.iter_mut().enumerate() {
                *b ^= key[i & 3];
            }
        }
        match opcode {
            1 | 2 => {
                let owned = std::mem::take(buf);
                *buf = Vec::new();
                return Ok(Some((opcode, owned)));
            }
            8 => return Ok(None),
            9 => {
                if let Some(w) = reply.as_deref_mut() {
                    write_frame(w, buf, 0xA, !masked).await?;
                }
            }
            10 => {}
            _ => unreachable!(),
        }
    }
}

pub async fn read_frame_owned<R, W>(
    r: &mut R,
    reply: Option<&mut W>,
    buf: &mut Vec<u8>,
) -> Result<Option<(u8, Vec<u8>)>>
where
    R: AsyncRead + Unpin,
    W: AsyncWrite + Unpin,
{
    read_frame(r, reply, buf).await
}

#[cfg(test)]
mod tests {
    use super::*;
    use tokio::io::duplex;

    #[test]
    fn rfc6455_accept_key_vector() {
        assert_eq!(
            compute_accept_key("dGhlIHNhbXBsZSBub25jZQ=="),
            "s3pPLMBiTxaQ9kYGzzhZRbK+xOo="
        );
    }

    #[test]
    fn handshake_request_and_response_validate() {
        let (request, key) = build_client_handshake_request(
            "example.com",
            "/ws",
            Some("https://example.com"),
            Some("same-origin"),
        );
        assert_eq!(validate_server_handshake(&request).unwrap(), key);
        validate_client_handshake_response(&build_server_handshake_response(&key), &key).unwrap();
    }

    #[test]
    fn browser_headers_present() {
        let (request, _) = build_client_handshake_request(
            "example.com",
            "/ws",
            Some("https://example.com"),
            Some("same-origin"),
        );
        let text = std::str::from_utf8(&request).unwrap();
        for header in [
            "Pragma: no-cache",
            "Cache-Control: no-cache",
            "User-Agent:",
            "Accept-Language:",
            "Accept-Encoding:",
            "sec-ch-ua:",
            "sec-ch-ua-mobile: ?0",
            "sec-ch-ua-platform: \"Windows\"",
        ] {
            assert!(text.contains(header), "missing {header}");
        }
    }

    #[test]
    fn redact_handshake_request_redacts_key() {
        let (request, key) = build_client_handshake_request(
            "dedi.4467107.xyz",
            "/pyway",
            Some("https://dedi.4467107.xyz"),
            Some("same-origin"),
        );
        let redacted = redact_handshake_request(&request);
        assert!(!redacted.contains(&key));
        assert!(redacted.contains("Sec-WebSocket-Key: [REDACTED]\r\n"));
        assert!(redacted.contains("Host: dedi.4467107.xyz\r\n"));
        assert!(redacted.contains("Origin: https://dedi.4467107.xyz\r\n"));
    }

    #[tokio::test]
    async fn read_http_headers_timeout_zero_bytes_error() {
        let (mut a, _b) = duplex(64);
        drop(_b); // Closed immediately
        let err = read_http_headers_timeout(&mut a, std::time::Duration::from_millis(50))
            .await
            .unwrap_err();
        assert!(err.to_string().contains("received 0 response bytes"));
    }

    #[test]
    fn mask_generator_is_nonzero() {
        assert_ne!(next_mask(), [0, 0, 0, 0]);
    }

    #[test]
    fn header_limit_is_hard() {
        assert!(validate_server_handshake(&vec![b'x'; MAX_HTTP_HEADER_SIZE + 1]).is_err());
    }

    #[tokio::test]
    async fn round_trip_unmasked_binary() {
        let (mut a, mut b) = duplex(1024 * 1024);
        let data = vec![7u8; 70000];
        let writer = tokio::spawn(async move { write_frame(&mut a, &data, 2, false).await });
        let mut buf = Vec::new();
        let got = read_frame(
            &mut b,
            Option::<&mut tokio::io::DuplexStream>::None,
            &mut buf,
        )
        .await
        .unwrap()
        .unwrap();
        writer.await.unwrap().unwrap();
        assert_eq!(got.0, 2);
        assert_eq!(got.1, vec![7u8; 70000]);
        assert!(buf.is_empty());
    }

    #[tokio::test]
    async fn round_trip_masked_binary() {
        let (mut a, mut b) = duplex(1024 * 1024);
        let data = vec![11u8; 70000];
        let writer = tokio::spawn(async move { write_frame(&mut a, &data, 2, true).await });
        let mut buf = Vec::new();
        let got = read_frame(
            &mut b,
            Option::<&mut tokio::io::DuplexStream>::None,
            &mut buf,
        )
        .await
        .unwrap()
        .unwrap();
        writer.await.unwrap().unwrap();
        assert_eq!(got.0, 2);
        assert_eq!(got.1, vec![11u8; 70000]);
        assert!(buf.is_empty());
    }

    #[tokio::test]
    async fn owned_binary_frame_transfers_storage_without_clone() {
        let (mut a, mut b) = duplex(1024 * 1024);
        let data = vec![23u8; 70000];
        let writer = tokio::spawn(async move { write_frame(&mut a, &data, 2, false).await });
        let mut buf = Vec::with_capacity(70000);
        let (opcode, owned) = read_frame_owned(
            &mut b,
            Option::<&mut tokio::io::DuplexStream>::None,
            &mut buf,
        )
        .await
        .unwrap()
        .unwrap();
        writer.await.unwrap().unwrap();
        assert_eq!(opcode, 2);
        assert_eq!(owned, vec![23u8; 70000]);
        assert!(buf.is_empty());
    }

    #[tokio::test]
    async fn close_frame_returns_eof_marker() {
        let (mut a, mut b) = duplex(1024);
        let writer = tokio::spawn(async move { write_frame(&mut a, b"", 8, false).await });
        let mut buf = Vec::new();
        let got = read_frame(
            &mut b,
            Option::<&mut tokio::io::DuplexStream>::None,
            &mut buf,
        )
        .await
        .unwrap();
        writer.await.unwrap().unwrap();
        assert!(got.is_none());
    }

    #[tokio::test]
    async fn clean_eof_returns_none() {
        let (a, mut b) = duplex(1024);
        drop(a); // Dropped without sending any frames
        let mut buf = Vec::new();
        let got = read_frame(
            &mut b,
            Option::<&mut tokio::io::DuplexStream>::None,
            &mut buf,
        )
        .await
        .unwrap();
        assert!(got.is_none(), "clean connection close must yield Ok(None)");
    }
}
