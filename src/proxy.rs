//! Proxy front-end protocol primitives.
//!
//! These parsers intentionally stop at protocol decoding. Network forwarding,
//! MUX routing, DNS policy and connection lifecycle are implemented separately.
//! The shapes follow the GoWay v1.8.4 client-facing protocols observed in the
//! compatibility baseline.

use std::fmt;

pub const SOCKS5_VERSION: u8 = 0x05;
pub const SOCKS5_NO_AUTH: u8 = 0x00;
pub const SOCKS5_CONNECT: u8 = 0x01;
pub const SOCKS5_UDP_ASSOCIATE: u8 = 0x03;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TargetAddr {
    pub host: String,
    pub port: u16,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SocksCommand {
    Connect,
    UdpAssociate,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SocksRequest {
    pub command: SocksCommand,
    pub target: TargetAddr,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ProxyParseError {
    Truncated,
    InvalidVersion(u8),
    NoAcceptableMethod,
    UnsupportedCommand(u8),
    UnsupportedAddressType(u8),
    InvalidUtf8,
    InvalidPort,
    InvalidHttpRequest,
    HeaderTooLarge,
}

impl fmt::Display for ProxyParseError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Truncated => write!(f, "truncated proxy request"),
            Self::InvalidVersion(v) => write!(f, "invalid SOCKS version: {v}"),
            Self::NoAcceptableMethod => write!(f, "no acceptable SOCKS5 authentication method"),
            Self::UnsupportedCommand(c) => write!(f, "unsupported SOCKS5 command: 0x{c:02x}"),
            Self::UnsupportedAddressType(a) => write!(f, "unsupported SOCKS5 address type: 0x{a:02x}"),
            Self::InvalidUtf8 => write!(f, "invalid SOCKS5 domain name"),
            Self::InvalidPort => write!(f, "invalid target port"),
            Self::InvalidHttpRequest => write!(f, "invalid HTTP CONNECT request"),
            Self::HeaderTooLarge => write!(f, "proxy header too large"),
        }
    }
}
impl std::error::Error for ProxyParseError {}

/// Parse the SOCKS5 greeting. GoWay accepts the RFC1928 no-auth method.
pub fn parse_socks5_greeting(buf: &[u8]) -> Result<(), ProxyParseError> {
    if buf.len() < 2 { return Err(ProxyParseError::Truncated); }
    if buf[0] != SOCKS5_VERSION { return Err(ProxyParseError::InvalidVersion(buf[0])); }
    let n = buf[1] as usize;
    if buf.len() < 2 + n { return Err(ProxyParseError::Truncated); }
    if buf[2..2+n].contains(&SOCKS5_NO_AUTH) { Ok(()) } else { Err(ProxyParseError::NoAcceptableMethod) }
}

/// Parse a complete RFC1928 request (CONNECT or UDP ASSOCIATE).
pub fn parse_socks5_request(buf: &[u8]) -> Result<SocksRequest, ProxyParseError> {
    if buf.len() < 4 { return Err(ProxyParseError::Truncated); }
    if buf[0] != SOCKS5_VERSION { return Err(ProxyParseError::InvalidVersion(buf[0])); }
    let command = match buf[1] {
        SOCKS5_CONNECT => SocksCommand::Connect,
        SOCKS5_UDP_ASSOCIATE => SocksCommand::UdpAssociate,
        other => return Err(ProxyParseError::UnsupportedCommand(other)),
    };
    if buf[2] != 0 { return Err(ProxyParseError::InvalidHttpRequest); }
    let (host, consumed) = match buf[3] {
        0x01 => {
            if buf.len() < 10 { return Err(ProxyParseError::Truncated); }
            let h = format!("{}.{}.{}.{}", buf[4], buf[5], buf[6], buf[7]);
            (h, 8)
        }
        0x03 => {
            if buf.len() < 5 { return Err(ProxyParseError::Truncated); }
            let n = buf[4] as usize;
            if buf.len() < 5 + n { return Err(ProxyParseError::Truncated); }
            let h = std::str::from_utf8(&buf[5..5+n]).map_err(|_| ProxyParseError::InvalidUtf8)?.to_owned();
            (h, 5 + n)
        }
        0x04 => {
            if buf.len() < 22 { return Err(ProxyParseError::Truncated); }
            let mut octets = [0u8; 16];
            octets.copy_from_slice(&buf[4..20]);
            (std::net::Ipv6Addr::from(octets).to_string(), 20)
        }
        other => return Err(ProxyParseError::UnsupportedAddressType(other)),
    };
    if buf.len() < consumed + 2 { return Err(ProxyParseError::Truncated); }
    let port = u16::from_be_bytes([buf[consumed], buf[consumed + 1]]);
    if port == 0 { return Err(ProxyParseError::InvalidPort); }
    Ok(SocksRequest { command, target: TargetAddr { host, port } })
}

/// SOCKS5 success response used by the GoWay client path: IPv4 zero bind addr.
pub fn socks5_success_response() -> [u8; 10] {
    [0x05, 0x00, 0x00, 0x01, 0, 0, 0, 0, 0, 0]
}

/// Parse one SOCKS5 UDP datagram envelope. Returns target and payload.
pub fn parse_socks5_udp_datagram(buf: &[u8]) -> Result<(TargetAddr, &[u8]), ProxyParseError> {
    if buf.len() < 4 { return Err(ProxyParseError::Truncated); }
    if buf[0] != 0 || buf[1] != 0 { return Err(ProxyParseError::InvalidHttpRequest); }
    if buf[2] != 0 { return Err(ProxyParseError::InvalidHttpRequest); }
    let (host, consumed) = match buf[3] {
        0x01 => {
            if buf.len() < 10 { return Err(ProxyParseError::Truncated); }
            (format!("{}.{}.{}.{}", buf[4], buf[5], buf[6], buf[7]), 8)
        }
        0x03 => {
            if buf.len() < 5 { return Err(ProxyParseError::Truncated); }
            let n = buf[4] as usize;
            if buf.len() < 5 + n { return Err(ProxyParseError::Truncated); }
            (std::str::from_utf8(&buf[5..5+n]).map_err(|_| ProxyParseError::InvalidUtf8)?.to_owned(), 5 + n)
        }
        0x04 => {
            if buf.len() < 22 { return Err(ProxyParseError::Truncated); }
            let mut octets = [0u8; 16];
            octets.copy_from_slice(&buf[4..20]);
            (std::net::Ipv6Addr::from(octets).to_string(), 20)
        }
        other => return Err(ProxyParseError::UnsupportedAddressType(other)),
    };
    if buf.len() < consumed + 2 { return Err(ProxyParseError::Truncated); }
    let port = u16::from_be_bytes([buf[consumed], buf[consumed + 1]]);
    if port == 0 { return Err(ProxyParseError::InvalidPort); }
    Ok((TargetAddr { host, port }, &buf[consumed + 2..]))
}

/// Parse the request line of an HTTP CONNECT request.
/// Header framing itself must be bounded by MAX_HEADER_SIZE before calling this.
pub fn parse_http_connect(buf: &[u8]) -> Result<TargetAddr, ProxyParseError> {
    let end = buf.windows(4).position(|w| w == b"\r\n\r\n").map(|n| n + 4)
        .or_else(|| buf.windows(2).position(|w| w == b"\n\n").map(|n| n + 2))
        .ok_or(ProxyParseError::Truncated)?;
    if end > 8192 { return Err(ProxyParseError::HeaderTooLarge); }
    let first_line_end = buf[..end].iter().position(|&b| b == b'\n').ok_or(ProxyParseError::InvalidHttpRequest)?;
    let line = std::str::from_utf8(&buf[..first_line_end]).map_err(|_| ProxyParseError::InvalidHttpRequest)?.trim_end_matches('\r');
    let mut parts = line.split_whitespace();
    if !parts.next().is_some_and(|m| m.eq_ignore_ascii_case("CONNECT")) { return Err(ProxyParseError::InvalidHttpRequest); }
    let authority = parts.next().ok_or(ProxyParseError::InvalidHttpRequest)?;
    if !parts.next().is_some_and(|v| v.starts_with("HTTP/")) { return Err(ProxyParseError::InvalidHttpRequest); }
    let authority = authority.trim();
    if authority.starts_with('[') {
        let close = authority.find(']').ok_or(ProxyParseError::InvalidHttpRequest)?;
        let host = &authority[1..close];
        let port_text = authority.get(close + 1..).unwrap_or("").strip_prefix(':').ok_or(ProxyParseError::InvalidHttpRequest)?;
        let port = port_text.parse::<u16>().map_err(|_| ProxyParseError::InvalidPort)?;
        if port == 0 { return Err(ProxyParseError::InvalidPort); }
        return Ok(TargetAddr { host: host.to_owned(), port });
    }
    let (host, port_text) = authority.rsplit_once(':').ok_or(ProxyParseError::InvalidHttpRequest)?;
    if host.is_empty() { return Err(ProxyParseError::InvalidHttpRequest); }
    let port = port_text.parse::<u16>().map_err(|_| ProxyParseError::InvalidPort)?;
    if port == 0 { return Err(ProxyParseError::InvalidPort); }
    Ok(TargetAddr { host: host.to_owned(), port })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn socks_greeting_accepts_no_auth() {
        assert!(parse_socks5_greeting(&[5, 1, 0]).is_ok());
        assert!(matches!(parse_socks5_greeting(&[5, 1, 2]), Err(ProxyParseError::NoAcceptableMethod)));
    }

    #[test]
    fn socks_connect_ipv4() {
        let req = [5, 1, 0, 1, 127, 0, 0, 1, 0x01, 0xbb];
        assert_eq!(parse_socks5_request(&req).unwrap().target, TargetAddr { host: "127.0.0.1".into(), port: 443 });
    }

    #[test]
    fn socks_connect_domain() {
        let mut req = vec![5, 1, 0, 3, 11];
        req.extend_from_slice(b"example.com");
        req.extend_from_slice(&443u16.to_be_bytes());
        assert_eq!(parse_socks5_request(&req).unwrap().target.host, "example.com");
    }

    #[test]
    fn socks_udp_domain_and_payload() {
        let mut pkt = vec![0, 0, 0, 3, 11];
        pkt.extend_from_slice(b"example.com");
        pkt.extend_from_slice(&53u16.to_be_bytes());
        pkt.extend_from_slice(b"dns");
        let (target, data) = parse_socks5_udp_datagram(&pkt).unwrap();
        assert_eq!(target.port, 53);
        assert_eq!(data, b"dns");
    }

    #[test]
    fn http_connect_ipv4_and_ipv6() {
        let req = b"CONNECT 127.0.0.1:443 HTTP/1.1\r\nHost: 127.0.0.1:443\r\n\r\n";
        assert_eq!(parse_http_connect(req).unwrap().port, 443);
        let req6 = b"CONNECT [::1]:8443 HTTP/1.1\r\n\r\n";
        assert_eq!(parse_http_connect(req6).unwrap().host, "::1");
    }

    #[test]
    fn http_header_limit_is_enforced() {
        let mut req = b"CONNECT example.com:443 HTTP/1.1\r\nX: ".to_vec();
        req.resize(8193, b'x');
        req.extend_from_slice(b"\r\n\r\n");
        assert!(matches!(parse_http_connect(&req), Err(ProxyParseError::HeaderTooLarge)));
    }
}
