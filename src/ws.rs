//! RFC 6455 transport pieces used by the GoWay-compatible transport.
//!
//! The codec deliberately does not implement fragmentation: GoWay v1.8.4
//! accepts complete data frames and handles Ping/Pong/Close itself.

use anyhow::{anyhow, bail, Result};
use base64::{engine::general_purpose::STANDARD, Engine as _};
use rand::seq::SliceRandom;
use rand::{Rng, RngCore};
use sha1::{Digest, Sha1};
use std::cell::RefCell;
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};

pub const MAX_WS_FRAME_SIZE: usize = 64 * 1024 * 1024;
pub const MAX_HTTP_HEADER_SIZE: usize = 8192;
const WS_GUID: &[u8] = b"258EAFA5-E914-47DA-95CA-C5AB0DC85B11";
const BROWSER_ACCEPT_ENCODING: &str = "gzip, deflate, br, zstd";

/// Browser profile bundling UA, TLS cipher/curve preferences, and HTTP
/// headers that must plausibly match each other. Mirrors GoWay's
/// `browserProfiles` (Chrome 136 / Edge 136 / Firefox 138 / Android).
/// A random profile is picked per handshake, like GoWay's
/// `pickBrowserProfile`.
#[derive(Debug, Clone, Copy)]
pub struct BrowserProfile {
    pub ua: &'static str,
    pub accept_lang: &'static str,
    /// Chrome Client Hints; `None` for Firefox (which never sends sec-ch-ua).
    pub sec_ch_ua: Option<&'static str>,
    pub sec_ch_ua_mob: &'static str,
    pub sec_ch_ua_plat: &'static str,
    pub is_chromium: bool,
    /// Mobile marker (currently only asserted in tests; kept for parity
    /// with GoWay's profile table).
    #[allow(dead_code)]
    pub is_mobile: bool,
    /// Which TLS cipher ordering to use (mirrors GoWay's per-profile lists).
    pub tls_chromium_order: bool,
}

pub const BROWSER_PROFILES: [BrowserProfile; 7] = [
    BrowserProfile {
        ua: "Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/136.0.0.0 Safari/537.36",
        accept_lang: "en-US,en;q=0.9",
        sec_ch_ua: Some("\"Chromium\";v=\"136\", \"Google Chrome\";v=\"136\", \"Not.A/Brand\";v=\"99\""),
        sec_ch_ua_mob: "?0",
        sec_ch_ua_plat: "\"Windows\"",
        is_chromium: true,
        is_mobile: false,
        tls_chromium_order: true,
    },
    BrowserProfile {
        ua: "Mozilla/5.0 (Macintosh; Intel Mac OS X 10_15_7) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/136.0.0.0 Safari/537.36",
        accept_lang: "en-US,en;q=0.9",
        sec_ch_ua: Some("\"Chromium\";v=\"136\", \"Google Chrome\";v=\"136\", \"Not.A/Brand\";v=\"99\""),
        sec_ch_ua_mob: "?0",
        sec_ch_ua_plat: "\"macOS\"",
        is_chromium: true,
        is_mobile: false,
        tls_chromium_order: true,
    },
    BrowserProfile {
        ua: "Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/136.0.0.0 Safari/537.36",
        accept_lang: "zh-CN,zh;q=0.9,en;q=0.8",
        sec_ch_ua: Some("\"Chromium\";v=\"136\", \"Google Chrome\";v=\"136\", \"Not.A/Brand\";v=\"99\""),
        sec_ch_ua_mob: "?0",
        sec_ch_ua_plat: "\"Windows\"",
        is_chromium: true,
        is_mobile: false,
        tls_chromium_order: true,
    },
    BrowserProfile {
        ua: "Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/136.0.0.0 Safari/537.36 Edg/136.0.0.0",
        accept_lang: "en-US,en;q=0.9",
        sec_ch_ua: Some("\"Chromium\";v=\"136\", \"Microsoft Edge\";v=\"136\", \"Not.A/Brand\";v=\"99\""),
        sec_ch_ua_mob: "?0",
        sec_ch_ua_plat: "\"Windows\"",
        is_chromium: true,
        is_mobile: false,
        tls_chromium_order: true,
    },
    BrowserProfile {
        ua: "Mozilla/5.0 (Windows NT 10.0; Win64; x64; rv:138.0) Gecko/20100101 Firefox/138.0",
        accept_lang: "en-US,en;q=0.5",
        sec_ch_ua: None,
        sec_ch_ua_mob: "",
        sec_ch_ua_plat: "",
        is_chromium: false,
        is_mobile: false,
        tls_chromium_order: false,
    },
    BrowserProfile {
        ua: "Mozilla/5.0 (Macintosh; Intel Mac OS X 14.7; rv:138.0) Gecko/20100101 Firefox/138.0",
        accept_lang: "en-US,en;q=0.5",
        sec_ch_ua: None,
        sec_ch_ua_mob: "",
        sec_ch_ua_plat: "",
        is_chromium: false,
        is_mobile: false,
        tls_chromium_order: false,
    },
    BrowserProfile {
        ua: "Mozilla/5.0 (Linux; Android 14; Pixel 8 Pro) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/136.0.0.0 Mobile Safari/537.36",
        accept_lang: "en-US,en;q=0.9",
        sec_ch_ua: Some("\"Chromium\";v=\"136\", \"Google Chrome\";v=\"136\", \"Not.A/Brand\";v=\"99\""),
        sec_ch_ua_mob: "?1",
        sec_ch_ua_plat: "\"Android\"",
        is_chromium: true,
        is_mobile: true,
        tls_chromium_order: true,
    },
];

/// Picks a random browser profile for one handshake (GoWay parity:
/// GoWay draws independently for TLS and HTTP, so callers may draw here
/// and pass the result to both layers for a correlated identity).
pub fn pick_browser_profile() -> &'static BrowserProfile {
    &BROWSER_PROFILES[pick_browser_profile_index()]
}

/// Index-based draw backing [`pick_browser_profile`]; the TLS layer draws
/// its own index so each layer can order cipher suites consistently.
pub fn pick_browser_profile_index() -> usize {
    rand::thread_rng().gen_range(0..BROWSER_PROFILES.len())
}

thread_local! {
    static MASK_STATE: RefCell<u64> = const { RefCell::new(0) };
}

pub(crate) fn next_mask() -> [u8; 4] {
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
    build_client_handshake_request_with_profile(
        host,
        path,
        origin,
        sec_fetch_site,
        pick_browser_profile(),
    )
}

/// Builds the upgrade request with an explicit profile so callers can use
/// one identity for both TLS and HTTP (GoWay draws them independently;
/// correlating is strictly harder to fingerprint).
pub fn build_client_handshake_request_with_profile(
    host: &str,
    path: &str,
    origin: Option<&str>,
    sec_fetch_site: Option<&str>,
    profile: &BrowserProfile,
) -> (Vec<u8>, String) {
    let mut key_bytes = [0u8; 16];
    rand::thread_rng().fill_bytes(&mut key_bytes);
    let key = STANDARD.encode(key_bytes);
    let path = if path.is_empty() { "/" } else { path };
    let site = sec_fetch_site.unwrap_or("cross-site");
    // Fixed top, mirroring GoWay: Host / Connection / Upgrade stay first.
    let mut req = format!(
        "GET {path} HTTP/1.1\r\nHost: {host}\r\nConnection: Upgrade\r\nUpgrade: websocket\r\n"
    );
    // Shufflable middle section (GoWay Fisher-Yates shuffles these).
    let mut middle: Vec<String> = Vec::with_capacity(9);
    middle.push("Pragma: no-cache".to_string());
    middle.push("Cache-Control: no-cache".to_string());
    middle.push(format!("User-Agent: {}", profile.ua));
    middle.push(format!("Accept-Language: {}", profile.accept_lang));
    middle.push(format!("Accept-Encoding: {BROWSER_ACCEPT_ENCODING}"));
    if let Some(origin) = origin {
        middle.push(format!("Origin: {origin}"));
    }
    if profile.is_chromium {
        if let Some(sec_ch_ua) = profile.sec_ch_ua {
            middle.push(format!("sec-ch-ua: {sec_ch_ua}"));
            middle.push(format!("sec-ch-ua-mobile: {}", profile.sec_ch_ua_mob));
            middle.push(format!("sec-ch-ua-platform: {}", profile.sec_ch_ua_plat));
        }
    }
    middle.shuffle(&mut rand::thread_rng());
    for line in &middle {
        req.push_str(line);
        req.push_str("\r\n");
    }
    // Fixed bottom, mirroring GoWay.
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

/// Writes a WebSocket frame header for `payload_len` bytes into `header`,
/// returning the header length (2/4/10). Shared by [`encode_ws_frame`] and
/// the fused MUX encoder so the layout logic lives in one place.
pub(crate) fn ws_header_into(
    header: &mut [u8; 14],
    payload_len: usize,
    opcode: u8,
    masked: bool,
) -> usize {
    let header_len = if payload_len <= 125 {
        2
    } else if payload_len <= u16::MAX as usize {
        4
    } else {
        10
    };
    header[0] = 0x80 | (opcode & 0x0f);
    let mask_bit = if masked { 0x80 } else { 0 };
    match header_len {
        2 => header[1] = mask_bit | payload_len as u8,
        4 => {
            header[1] = mask_bit | 126;
            header[2..4].copy_from_slice(&(payload_len as u16).to_be_bytes());
        }
        10 => {
            header[1] = mask_bit | 127;
            header[2..10].copy_from_slice(&(payload_len as u64).to_be_bytes());
        }
        _ => unreachable!(),
    }
    header_len
}

fn check_frame_args(payload_len: usize, opcode: u8) -> Result<()> {
    if payload_len > MAX_WS_FRAME_SIZE {
        return Err(anyhow!("websocket frame too large"));
    }
    if opcode >= 0x8 && payload_len > 125 {
        return Err(anyhow!("control frame payload exceeds 125 bytes"));
    }
    Ok(())
}

/// Encodes one complete WebSocket frame (header + optional mask + masked
/// payload) into an owned byte buffer. The masking PRNG call happens here,
/// on the caller's task, so a dedicated writer task can flush pre-encoded
/// frames without touching thread-local RNG state.
pub fn encode_ws_frame(payload: &[u8], opcode: u8, masked: bool) -> Result<Vec<u8>> {
    check_frame_args(payload.len(), opcode)?;

    let mut header = [0u8; 14];
    let header_len = ws_header_into(&mut header, payload.len(), opcode, masked);

    let total = header_len + if masked { 4 + payload.len() } else { payload.len() };
    let mut frame = Vec::with_capacity(total);
    frame.extend_from_slice(&header[..header_len]);
    if !masked {
        frame.extend_from_slice(payload);
        return Ok(frame);
    }
    let key = next_mask();
    frame.extend_from_slice(&key);
    frame.extend_from_slice(payload);
    for (i, byte) in frame[header_len + 4..].iter_mut().enumerate() {
        *byte ^= key[i & 3];
    }
    Ok(frame)
}

pub async fn write_frame<W: AsyncWrite + Unpin>(
    w: &mut W,
    payload: &[u8],
    opcode: u8,
    mask: bool,
) -> Result<()> {
    let frame = encode_ws_frame(payload, opcode, mask)?;
    w.write_all(&frame).await?;
    Ok(())
}

/// Writes a frame borrowing its payload from reusable scratch: masks the
/// scratch region in place and emits with a single vectored write, with
/// zero allocation. The caller must fully overwrite the region on the next
/// read (plain `read` into `[..]` satisfies this); only the used prefix is
/// ever consumed downstream.
pub async fn write_frame_borrowed<W: AsyncWrite + Unpin>(
    w: &mut W,
    scratch: &mut [u8],
    opcode: u8,
    masked: bool,
) -> Result<()> {
    check_frame_args(scratch.len(), opcode)?;
    let mut header = [0u8; 14];
    let header_len = ws_header_into(&mut header, scratch.len(), opcode, masked);
    let key_opt = if masked {
        let key = next_mask();
        for (i, byte) in scratch.iter_mut().enumerate() {
            *byte ^= key[i & 3];
        }
        Some(key)
    } else {
        None
    };
    write_frame_parts_vectored(w, &header, header_len, key_opt, scratch).await
}

async fn write_frame_parts_vectored<W: AsyncWrite + Unpin>(
    w: &mut W,
    header: &[u8; 14],
    header_len: usize,
    key: Option<[u8; 4]>,
    payload: &[u8],
) -> Result<()> {
    use std::future::poll_fn;
    use std::io::IoSlice;
    use std::pin::Pin;
    // (part, off) progress across partial vectored writes, where the parts
    // are [header][key][payload]. Empty leading parts are skipped so the
    // slice list is never all-empty before completion.
    let key_slice: &[u8] = key.as_ref().map(|k| &k[..]).unwrap_or(&[]);
    let key_len = key_slice.len();
    let lens = [header_len, key_len, payload.len()];
    let mut part = 0u8;
    let mut off = 0usize;
    while part < 3 {
        while part < 3 && lens[part as usize] == off {
            part += 1;
            off = 0;
        }
        if part >= 3 {
            break;
        }
        let mut slices = Vec::with_capacity(3 - part as usize);
        match part {
            0 => {
                slices.push(IoSlice::new(&header[off..header_len]));
                if !key_slice.is_empty() {
                    slices.push(IoSlice::new(key_slice));
                }
                slices.push(IoSlice::new(payload));
            }
            1 => {
                slices.push(IoSlice::new(&key_slice[off..]));
                slices.push(IoSlice::new(payload));
            }
            _ => {
                slices.push(IoSlice::new(&payload[off..]));
            }
        }
        let n = poll_fn(|cx| Pin::new(&mut *w).poll_write_vectored(cx, &slices)).await?;
        if n == 0 {
            return Err(anyhow!("vectored frame write returned zero"));
        }
        let mut remaining = n;
        while remaining > 0 && part < 3 {
            let available = lens[part as usize] - off;
            if remaining < available {
                off += remaining;
                remaining = 0;
            } else {
                remaining -= available;
                part += 1;
                off = 0;
            }
        }
    }
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
        // Header batched into at most 3 reads (was up to 5): the 2-byte
        // base, then extended-length + mask key in one go. Matters most
        // for small-frame traffic (tens of thousands of frames/s).
        let mut head = [0u8; 2];
        match r.read(&mut head).await {
            Ok(0) => return Ok(None),
            Ok(1) => {
                r.read_exact(&mut head[1..2]).await?;
            }
            Ok(_) => {}
            Err(e) => return Err(e.into()),
        }
        let b0 = head[0];
        let b1 = head[1];
        let fin = b0 & 0x80 != 0;
        let opcode = b0 & 0x0f;
        let masked = b1 & 0x80 != 0;
        let len_marker = b1 & 0x7f;
        let ext_len = match len_marker {
            126 => 2,
            127 => 8,
            _ => 0,
        };
        let mut rest = [0u8; 12];
        let rest_len = ext_len + if masked { 4 } else { 0 };
        if rest_len > 0 {
            r.read_exact(&mut rest[..rest_len]).await?;
        }
        let mut len = len_marker as u64;
        if ext_len == 2 {
            len = u16::from_be_bytes(rest[..2].try_into().unwrap()) as u64;
        } else if ext_len == 8 {
            len = u64::from_be_bytes(rest[..8].try_into().unwrap());
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
            key.copy_from_slice(&rest[ext_len..ext_len + 4]);
        }
        // Grow incrementally in 64 KiB segments instead of a single
        // `resize(len)`: a bogus 64 MB length prefix no longer causes an
        // instant OOM —the peer must actually send the bytes (and hit the
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
        // Deterministic profile: Chromium headers must be present (order varies).
        let profile = &BROWSER_PROFILES[0];
        let (request, _) = build_client_handshake_request_with_profile(
            "example.com",
            "/ws",
            Some("https://example.com"),
            Some("same-origin"),
            profile,
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
    fn all_profiles_build_valid_handshakes() {
        for profile in &BROWSER_PROFILES {
            let (request, key) = build_client_handshake_request_with_profile(
                "example.com",
                "/ws",
                Some("https://example.com"),
                Some("same-origin"),
                profile,
            );
            // Every profile must pass server-side validation.
            assert_eq!(validate_server_handshake(&request).unwrap(), key);
            let text = std::str::from_utf8(&request).unwrap();
            assert!(text.starts_with("GET /ws HTTP/1.1\r\n"));
            assert!(text.contains("Upgrade: websocket\r\n"));
            // Firefox never sends Client Hints; Chromium always does.
            if profile.is_chromium {
                assert!(text.contains("sec-ch-ua:"), "chromium profile missing sec-ch-ua");
            } else {
                assert!(
                    !text.contains("sec-ch-ua:"),
                    "firefox profile must not send sec-ch-ua"
                );
            }
        }
    }

    #[test]
    fn mobile_profile_signals_mobile() {
        let mobile = BROWSER_PROFILES.iter().find(|p| p.is_mobile).unwrap();
        assert_eq!(mobile.sec_ch_ua_mob, "?1");
        let (request, _) = build_client_handshake_request_with_profile(
            "example.com",
            "/",
            None,
            Some("cross-site"),
            mobile,
        );
        let text = std::str::from_utf8(&request).unwrap();
        assert!(text.contains("sec-ch-ua-mobile: ?1\r\n"));
        assert!(!text.contains("Origin:"));
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
    async fn borrowed_write_reuses_scratch_safely() {
        let (mut a, mut b) = duplex(1024 * 1024);
        // One scratch buffer reused across frames of different sizes:
        // each frame must decode cleanly with no cross-frame leakage.
        let mut scratch = vec![0u8; 70000];
        for (len, byte) in [(70000usize, 0x11u8), (13, 0x22), (50000, 0x33)] {
            for i in 0..len {
                scratch[i] = byte;
            }
            write_frame_borrowed(&mut a, &mut scratch[..len], 2, true)
                .await
                .unwrap();
            let mut buf = Vec::new();
            let got = read_frame(
                &mut b,
                Option::<&mut tokio::io::DuplexStream>::None,
                &mut buf,
            )
            .await
            .unwrap()
            .unwrap();
            assert_eq!(got.0, 2);
            assert_eq!(got.1, vec![byte; len]);
            assert!(buf.is_empty());
        }
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
