//! RFC 6455 transport pieces used by the GoWay-compatible transport.
//! The codec deliberately does not implement fragmentation: GoWay v1.8.4
//! accepts complete data frames and handles Ping/Pong/Close itself.

use anyhow::{anyhow, bail, Result};
use base64::{engine::general_purpose::STANDARD, Engine as _};
use rand::RngCore;
use sha1::{Digest, Sha1};
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};

pub const MAX_WS_FRAME_SIZE: usize = 64 * 1024 * 1024;
pub const MAX_HTTP_HEADER_SIZE: usize = 8192;
const WS_GUID: &[u8] = b"258EAFA5-E914-47DA-95CA-C5AB0DC85B11";

pub fn compute_accept_key(challenge: &str) -> String {
    let mut h = Sha1::new();
    h.update(challenge.trim().as_bytes());
    h.update(WS_GUID);
    STANDARD.encode(h.finalize())
}

fn header_value<'a>(headers: &'a str, name: &str) -> Option<&'a str> {
    headers.lines().find_map(|line| {
        let (k, v) = line.split_once(':')?;
        if k.trim().eq_ignore_ascii_case(name) { Some(v.trim()) } else { None }
    })
}

fn token_contains(value: &str, token: &str) -> bool {
    value.split(',').any(|v| v.trim().eq_ignore_ascii_case(token))
}

pub async fn read_http_headers<R: AsyncRead + Unpin>(r: &mut R) -> Result<Vec<u8>> {
    let mut out = Vec::with_capacity(1024);
    let mut b = [0u8; 1];
    while out.len() < MAX_HTTP_HEADER_SIZE {
        r.read_exact(&mut b).await?;
        out.push(b[0]);
        if out.len() >= 4 && out[out.len() - 4..] == *b"\r\n\r\n" { return Ok(out); }
        if out.len() >= 2 && out[out.len() - 2..] == *b"\n\n" { return Ok(out); }
    }
    bail!("header too large")
}

pub fn validate_server_handshake(request: &[u8]) -> Result<String> {
    if request.len() > MAX_HTTP_HEADER_SIZE { bail!("header too large"); }
    let text = std::str::from_utf8(request).map_err(|_| anyhow!("invalid HTTP header"))?;
    if !(text.ends_with("\r\n\r\n") || text.ends_with("\n\n")) { bail!("incomplete websocket handshake"); }
    let first = text.lines().next().ok_or_else(|| anyhow!("empty HTTP request"))?;
    let mut parts = first.split_whitespace();
    let method = parts.next().unwrap_or("");
    let _path = parts.next().unwrap_or("");
    let version = parts.next().unwrap_or("");
    if method != "GET" || !version.starts_with("HTTP/") { bail!("invalid websocket request line"); }
    let upgrade = header_value(text, "Upgrade").ok_or_else(|| anyhow!("missing upgrade: websocket"))?;
    if !token_contains(upgrade, "websocket") { bail!("missing upgrade: websocket"); }
    let connection = header_value(text, "Connection").ok_or_else(|| anyhow!("missing connection: upgrade"))?;
    if !token_contains(connection, "Upgrade") { bail!("missing connection: upgrade"); }
    let key = header_value(text, "Sec-WebSocket-Key").ok_or_else(|| anyhow!("missing sec-websocket-key"))?;
    if key.is_empty() { bail!("missing sec-websocket-key"); }
    Ok(key.to_string())
}

pub fn build_server_handshake_response(key: &str) -> Vec<u8> {
    format!("HTTP/1.1 101 Switching Protocols\r\nUpgrade: websocket\r\nConnection: Upgrade\r\nSec-WebSocket-Accept: {}\r\n\r\n", compute_accept_key(key)).into_bytes()
}

pub fn build_client_handshake_request(host: &str, path: &str, origin: Option<&str>, sec_fetch_site: Option<&str>) -> (Vec<u8>, String) {
    let mut key_bytes = [0u8; 16];
    rand::thread_rng().fill_bytes(&mut key_bytes);
    let key = STANDARD.encode(key_bytes);
    let path = if path.is_empty() { "/" } else { path };
    let site = sec_fetch_site.unwrap_or("cross-site");
    let mut req = format!("GET {path} HTTP/1.1\r\nHost: {host}\r\nConnection: Upgrade\r\nUpgrade: websocket\r\n");
    if let Some(origin) = origin { req.push_str(&format!("Origin: {origin}\r\n")); }
    req.push_str(&format!("Sec-WebSocket-Version: 13\r\nSec-WebSocket-Key: {key}\r\nSec-Fetch-Dest: websocket\r\nSec-Fetch-Mode: websocket\r\nSec-Fetch-Site: {site}\r\n\r\n"));
    (req.into_bytes(), key)
}

pub fn validate_client_handshake_response(response: &[u8], key: &str) -> Result<()> {
    if response.len() > MAX_HTTP_HEADER_SIZE { bail!("header too large"); }
    let text = std::str::from_utf8(response).map_err(|_| anyhow!("invalid HTTP response"))?;
    let first = text.lines().next().ok_or_else(|| anyhow!("empty websocket handshake response"))?;
    let mut parts = first.splitn(3, ' ');
    let proto = parts.next().unwrap_or("");
    let status = parts.next().unwrap_or("");
    let reason = parts.next().unwrap_or("");
    if !proto.starts_with("HTTP/") { bail!("malformed websocket handshake response: {first}"); }
    if status != "101" {
        match status {
            "200" => bail!("handshake failed: server returned 200 OK instead of 101 Switching Protocols"),
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
    if !token_contains(header_value(text, "Upgrade").ok_or_else(|| anyhow!("missing Upgrade header"))?, "websocket") { bail!("handshake failed: missing Upgrade: websocket"); }
    if !token_contains(header_value(text, "Connection").ok_or_else(|| anyhow!("missing Connection header"))?, "Upgrade") { bail!("handshake failed: missing Connection: Upgrade"); }
    let accept = header_value(text, "Sec-WebSocket-Accept").ok_or_else(|| anyhow!("handshake failed: missing Sec-WebSocket-Accept"))?;
    if accept != compute_accept_key(key) { bail!("handshake failed: invalid Sec-WebSocket-Accept"); }
    Ok(())
}

pub async fn write_frame<W: AsyncWrite + Unpin>(w: &mut W, payload: &[u8], opcode: u8, mask: bool) -> Result<()> {
    if payload.len() > MAX_WS_FRAME_SIZE { return Err(anyhow!("websocket frame too large")); }
    if opcode >= 0x8 && payload.len() > 125 { return Err(anyhow!("control frame payload exceeds 125 bytes")); }
    let mut h = Vec::with_capacity(14);
    h.push(0x80 | (opcode & 0x0f));
    let mask_bit = if mask { 0x80 } else { 0 };
    match payload.len() {
        0..=125 => h.push(mask_bit | payload.len() as u8),
        126..=65535 => { h.push(mask_bit | 126); h.extend_from_slice(&(payload.len() as u16).to_be_bytes()); }
        n => { h.push(mask_bit | 127); h.extend_from_slice(&(n as u64).to_be_bytes()); }
    }
    let mut data = payload.to_vec();
    if mask {
        let mut key = [0u8; 4];
        rand::thread_rng().fill_bytes(&mut key);
        h.extend_from_slice(&key);
        for (i, b) in data.iter_mut().enumerate() { *b ^= key[i & 3]; }
    }
    w.write_all(&h).await?;
    w.write_all(&data).await?;
    w.flush().await?;
    Ok(())
}

pub async fn read_frame<R, W>(r: &mut R, mut reply: Option<&mut W>, buf: &mut Vec<u8>) -> Result<Option<(u8, Vec<u8>)>>
where R: AsyncRead + Unpin, W: AsyncWrite + Unpin {
    loop {
        let b0 = r.read_u8().await?;
        let b1 = r.read_u8().await?;
        let fin = b0 & 0x80 != 0;
        let opcode = b0 & 0x0f;
        let masked = b1 & 0x80 != 0;
        let mut len = (b1 & 0x7f) as u64;
        if len == 126 { len = r.read_u16().await? as u64; } else if len == 127 { len = r.read_u64().await?; }
        if len > MAX_WS_FRAME_SIZE as u64 { return Err(anyhow!("frame too large")); }
        if opcode >= 0x8 { if !fin || len > 125 { return Err(anyhow!("invalid websocket control frame")); } }
        else if opcode == 0 || !fin || (opcode != 1 && opcode != 2) { return Err(anyhow!("unsupported or fragmented websocket frame")); }
        let mut key = [0u8; 4];
        if masked { r.read_exact(&mut key).await?; }
        buf.clear(); buf.resize(len as usize, 0); r.read_exact(buf).await?;
        if masked { for (i, b) in buf.iter_mut().enumerate() { *b ^= key[i & 3]; } }
        match opcode { 1 | 2 => return Ok(Some((opcode, buf.clone()))), 8 => return Ok(None), 9 => { if let Some(w) = reply.as_deref_mut() { write_frame(w, buf, 0xA, false).await?; } }, 10 => {}, _ => unreachable!() }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tokio::io::duplex;
    #[test]
    fn rfc6455_accept_key_vector() { assert_eq!(compute_accept_key("dGhlIHNhbXBsZSBub25jZQ=="), "s3pPLMBiTxaQ9kYGzzhZRbK+xOo="); }
    #[test]
    fn handshake_request_and_response_validate() {
        let (request, key) = build_client_handshake_request("example.com", "/ws", Some("https://example.com"), Some("same-origin"));
        assert_eq!(validate_server_handshake(&request).unwrap(), key);
        validate_client_handshake_response(&build_server_handshake_response(&key), &key).unwrap();
    }
    #[test]
    fn header_limit_is_hard() { assert!(validate_server_handshake(&vec![b'x'; MAX_HTTP_HEADER_SIZE + 1]).is_err()); }
    #[tokio::test]
    async fn round_trip_unmasked_binary() {
        let (mut a, mut b) = duplex(1024 * 1024); let data = vec![7u8; 70000]; let expected = data.clone();
        let writer = tokio::spawn(async move { write_frame(&mut a, &data, 2, false).await });
        let mut buf = Vec::new(); let got = read_frame(&mut b, Option::<&mut tokio::io::DuplexStream>::None, &mut buf).await.unwrap().unwrap();
        writer.await.unwrap().unwrap(); assert_eq!(got.0, 2); assert_eq!(got.1, expected);
    }
    #[tokio::test]
    async fn close_frame_returns_eof_marker() {
        let (mut a, mut b) = duplex(1024); let writer = tokio::spawn(async move { write_frame(&mut a, b"", 8, false).await });
        let mut buf = Vec::new(); let got = read_frame(&mut b, Option::<&mut tokio::io::DuplexStream>::None, &mut buf).await.unwrap();
        writer.await.unwrap().unwrap(); assert!(got.is_none());
    }
}
