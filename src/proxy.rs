//! Proxy front-end protocol primitives.
//!
//! These parsers intentionally stop at protocol decoding. Network forwarding,
//! MUX routing, DNS policy and connection lifecycle are implemented separately.
//! The shapes follow the GoWay v1.8.4 client-facing protocols observed in the
//! compatibility baseline.

use std::fmt;
use tokio::io::{AsyncReadExt, AsyncWriteExt};

pub const SOCKS5_VERSION: u8 = 0x05;
#[allow(dead_code)]
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
pub struct ClientProxyRequest {
    pub command: SocksCommand,
    pub target: TargetAddr,
    pub is_socks5: bool,
    pub is_connect: bool,
    pub initial_payload: Option<Vec<u8>>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ProxyParseError {
    Truncated,
    InvalidVersion(u8),
    #[allow(dead_code)]
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
            Self::UnsupportedAddressType(a) => {
                write!(f, "unsupported SOCKS5 address type: 0x{a:02x}")
            }
            Self::InvalidUtf8 => write!(f, "invalid SOCKS5 domain name"),
            Self::InvalidPort => write!(f, "invalid target port"),
            Self::InvalidHttpRequest => write!(f, "invalid HTTP CONNECT request"),
            Self::HeaderTooLarge => write!(f, "proxy header too large"),
        }
    }
}
impl std::error::Error for ProxyParseError {}

#[allow(dead_code)]
pub fn parse_socks5_greeting(buf: &[u8]) -> Result<(), ProxyParseError> {
    if buf.len() < 2 {
        return Err(ProxyParseError::Truncated);
    }
    if buf[0] != SOCKS5_VERSION {
        return Err(ProxyParseError::InvalidVersion(buf[0]));
    }
    let n = buf[1] as usize;
    if buf.len() < 2 + n {
        return Err(ProxyParseError::Truncated);
    }
    if buf[2..2 + n].contains(&SOCKS5_NO_AUTH) {
        Ok(())
    } else {
        Err(ProxyParseError::NoAcceptableMethod)
    }
}

pub fn parse_socks5_request(buf: &[u8]) -> Result<SocksRequest, ProxyParseError> {
    if buf.len() < 4 {
        return Err(ProxyParseError::Truncated);
    }
    if buf[0] != SOCKS5_VERSION {
        return Err(ProxyParseError::InvalidVersion(buf[0]));
    }
    let command = match buf[1] {
        SOCKS5_CONNECT => SocksCommand::Connect,
        SOCKS5_UDP_ASSOCIATE => SocksCommand::UdpAssociate,
        other => return Err(ProxyParseError::UnsupportedCommand(other)),
    };
    if buf[2] != 0 {
        return Err(ProxyParseError::InvalidHttpRequest);
    }
    let (host, consumed) = match buf[3] {
        0x01 => {
            if buf.len() < 10 {
                return Err(ProxyParseError::Truncated);
            }
            let h = format!("{}.{}.{}.{}", buf[4], buf[5], buf[6], buf[7]);
            (h, 8)
        }
        0x03 => {
            if buf.len() < 5 {
                return Err(ProxyParseError::Truncated);
            }
            let n = buf[4] as usize;
            if buf.len() < 5 + n {
                return Err(ProxyParseError::Truncated);
            }
            let h = std::str::from_utf8(&buf[5..5 + n])
                .map_err(|_| ProxyParseError::InvalidUtf8)?
                .to_owned();
            (h, 5 + n)
        }
        0x04 => {
            if buf.len() < 22 {
                return Err(ProxyParseError::Truncated);
            }
            let mut octets = [0u8; 16];
            octets.copy_from_slice(&buf[4..20]);
            (std::net::Ipv6Addr::from(octets).to_string(), 20)
        }
        other => return Err(ProxyParseError::UnsupportedAddressType(other)),
    };
    if buf.len() < consumed + 2 {
        return Err(ProxyParseError::Truncated);
    }
    let port = u16::from_be_bytes([buf[consumed], buf[consumed + 1]]);
    if port == 0 && matches!(command, SocksCommand::Connect) {
        return Err(ProxyParseError::InvalidPort);
    }
    Ok(SocksRequest {
        command,
        target: TargetAddr { host, port },
    })
}

pub fn socks5_success_response() -> [u8; 10] {
    [0x05, 0x00, 0x00, 0x01, 0, 0, 0, 0, 0, 0]
}

/// RFC 1928 general SOCKS5 failure, with an IPv4 zero bind address.
pub fn socks5_failure_response() -> [u8; 10] {
    [0x05, 0x05, 0x00, 0x01, 0, 0, 0, 0, 0, 0]
}

pub fn parse_socks5_udp_datagram(buf: &[u8]) -> Result<(TargetAddr, &[u8]), ProxyParseError> {
    if buf.len() < 4 {
        return Err(ProxyParseError::Truncated);
    }
    if buf[0] != 0 || buf[1] != 0 {
        return Err(ProxyParseError::InvalidHttpRequest);
    }
    if buf[2] != 0 {
        return Err(ProxyParseError::InvalidHttpRequest);
    }
    let (host, consumed) = match buf[3] {
        0x01 => {
            if buf.len() < 10 {
                return Err(ProxyParseError::Truncated);
            }
            (format!("{}.{}.{}.{}", buf[4], buf[5], buf[6], buf[7]), 8)
        }
        0x03 => {
            if buf.len() < 5 {
                return Err(ProxyParseError::Truncated);
            }
            let n = buf[4] as usize;
            if buf.len() < 5 + n {
                return Err(ProxyParseError::Truncated);
            }
            (
                std::str::from_utf8(&buf[5..5 + n])
                    .map_err(|_| ProxyParseError::InvalidUtf8)?
                    .to_owned(),
                5 + n,
            )
        }
        0x04 => {
            if buf.len() < 22 {
                return Err(ProxyParseError::Truncated);
            }
            let mut octets = [0u8; 16];
            octets.copy_from_slice(&buf[4..20]);
            (std::net::Ipv6Addr::from(octets).to_string(), 20)
        }
        other => return Err(ProxyParseError::UnsupportedAddressType(other)),
    };
    if buf.len() < consumed + 2 {
        return Err(ProxyParseError::Truncated);
    }
    let port = u16::from_be_bytes([buf[consumed], buf[consumed + 1]]);
    if port == 0 {
        return Err(ProxyParseError::InvalidPort);
    }
    Ok((TargetAddr { host, port }, &buf[consumed + 2..]))
}

pub fn parse_target_authority(authority: &str) -> Result<TargetAddr, ProxyParseError> {
    let authority = authority.trim();
    if authority.is_empty() {
        return Err(ProxyParseError::InvalidHttpRequest);
    }
    if !authority.starts_with('[') && authority.matches(':').count() > 1 {
        return Err(ProxyParseError::InvalidHttpRequest);
    }
    if authority.starts_with('[') {
        let close = authority
            .find(']')
            .ok_or(ProxyParseError::InvalidHttpRequest)?;
        let host = &authority[1..close];
        if host.is_empty() {
            return Err(ProxyParseError::InvalidHttpRequest);
        }
        let port_text = authority
            .get(close + 1..)
            .unwrap_or("")
            .strip_prefix(':')
            .ok_or(ProxyParseError::InvalidHttpRequest)?;
        let port = port_text
            .parse::<u16>()
            .map_err(|_| ProxyParseError::InvalidPort)?;
        if port == 0 {
            return Err(ProxyParseError::InvalidPort);
        }
        return Ok(TargetAddr {
            host: host.to_owned(),
            port,
        });
    }
    let (host, port_text) = authority
        .rsplit_once(':')
        .ok_or(ProxyParseError::InvalidHttpRequest)?;
    if host.is_empty() {
        return Err(ProxyParseError::InvalidHttpRequest);
    }
    let port = port_text
        .parse::<u16>()
        .map_err(|_| ProxyParseError::InvalidPort)?;
    if port == 0 {
        return Err(ProxyParseError::InvalidPort);
    }
    Ok(TargetAddr {
        host: host.to_owned(),
        port,
    })
}

pub fn parse_authority_with_default(
    authority: &str,
    default_port: u16,
) -> Result<TargetAddr, ProxyParseError> {
    let authority = authority.trim();
    if authority.is_empty() {
        return Err(ProxyParseError::InvalidHttpRequest);
    }
    if !authority.starts_with('[') && authority.matches(':').count() > 1 {
        return Err(ProxyParseError::InvalidHttpRequest);
    }
    if authority.starts_with('[') {
        let close = authority
            .find(']')
            .ok_or(ProxyParseError::InvalidHttpRequest)?;
        let host = &authority[1..close];
        if host.is_empty() {
            return Err(ProxyParseError::InvalidHttpRequest);
        }
        let port = match authority.get(close + 1..).and_then(|t| t.strip_prefix(':')) {
            Some(p) => {
                let val = p.parse::<u16>().map_err(|_| ProxyParseError::InvalidPort)?;
                if val == 0 {
                    return Err(ProxyParseError::InvalidPort);
                }
                val
            }
            None => default_port,
        };
        return Ok(TargetAddr {
            host: host.to_owned(),
            port,
        });
    }
    if let Some((host, port_text)) = authority.rsplit_once(':') {
        if host.is_empty() {
            return Err(ProxyParseError::InvalidHttpRequest);
        }
        let port = port_text
            .parse::<u16>()
            .map_err(|_| ProxyParseError::InvalidPort)?;
        if port == 0 {
            return Err(ProxyParseError::InvalidPort);
        }
        Ok(TargetAddr {
            host: host.to_owned(),
            port,
        })
    } else {
        Ok(TargetAddr {
            host: authority.to_owned(),
            port: default_port,
        })
    }
}

pub fn parse_http_connect(buf: &[u8]) -> Result<TargetAddr, ProxyParseError> {
    let end = buf
        .windows(4)
        .position(|w| w == b"\r\n\r\n")
        .map(|n| n + 4)
        .or_else(|| buf.windows(2).position(|w| w == b"\n\n").map(|n| n + 2))
        .ok_or(ProxyParseError::Truncated)?;
    if end > 8192 {
        return Err(ProxyParseError::HeaderTooLarge);
    }
    let first_line_end = buf[..end]
        .iter()
        .position(|&b| b == b'\n')
        .ok_or(ProxyParseError::InvalidHttpRequest)?;
    let line = std::str::from_utf8(&buf[..first_line_end])
        .map_err(|_| ProxyParseError::InvalidHttpRequest)?
        .trim_end_matches('\r');
    let mut parts = line.split_whitespace();
    if !parts
        .next()
        .is_some_and(|m| m.eq_ignore_ascii_case("CONNECT"))
    {
        return Err(ProxyParseError::InvalidHttpRequest);
    }
    let authority = parts.next().ok_or(ProxyParseError::InvalidHttpRequest)?;
    if !parts.next().is_some_and(|v| v.starts_with("HTTP/")) {
        return Err(ProxyParseError::InvalidHttpRequest);
    }
    parse_target_authority(authority)
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HttpProxyRequest {
    pub target: TargetAddr,
    pub is_connect: bool,
    pub initial_payload: Option<Vec<u8>>,
}

pub fn parse_http_proxy_request(buf: &[u8]) -> Result<HttpProxyRequest, ProxyParseError> {
    let end = buf
        .windows(4)
        .position(|w| w == b"\r\n\r\n")
        .map(|n| n + 4)
        .or_else(|| buf.windows(2).position(|w| w == b"\n\n").map(|n| n + 2))
        .ok_or(ProxyParseError::Truncated)?;
    if end > 8192 {
        return Err(ProxyParseError::HeaderTooLarge);
    }
    let first_line_end = buf[..end]
        .iter()
        .position(|&b| b == b'\n')
        .ok_or(ProxyParseError::InvalidHttpRequest)?;
    let line = std::str::from_utf8(&buf[..first_line_end])
        .map_err(|_| ProxyParseError::InvalidHttpRequest)?
        .trim_end_matches('\r');
    let mut parts = line.split_whitespace();
    let method = parts.next().ok_or(ProxyParseError::InvalidHttpRequest)?;
    let uri = parts.next().ok_or(ProxyParseError::InvalidHttpRequest)?;
    let proto = parts.next().ok_or(ProxyParseError::InvalidHttpRequest)?;
    if !proto.starts_with("HTTP/") {
        return Err(ProxyParseError::InvalidHttpRequest);
    }

    if method.eq_ignore_ascii_case("CONNECT") {
        let target = parse_http_connect(buf)?;
        return Ok(HttpProxyRequest {
            target,
            is_connect: true,
            initial_payload: None,
        });
    }

    let mut target_host = String::new();
    let mut target_port = 80u16;

    if let Some(rest) = uri
        .strip_prefix("http://")
        .or_else(|| uri.strip_prefix("https://"))
    {
        let is_https = uri.starts_with("https://");
        let default_port = if is_https { 443 } else { 80 };
        let authority = rest.split('/').next().unwrap_or(rest);
        if let Ok(target) = parse_authority_with_default(authority, default_port) {
            target_host = target.host;
            target_port = target.port;
        }
    }

    if target_host.is_empty() {
        let headers_str = std::str::from_utf8(&buf[first_line_end + 1..end])
            .map_err(|_| ProxyParseError::InvalidHttpRequest)?;
        for line in headers_str.lines() {
            let line = line.trim();
            if let Some(val) = line
                .strip_prefix("Host:")
                .or_else(|| line.strip_prefix("host:"))
            {
                if let Ok(target) = parse_authority_with_default(val.trim(), target_port) {
                    target_host = target.host;
                    target_port = target.port;
                }
                break;
            }
        }
    }

    if target_host.is_empty() {
        return Err(ProxyParseError::InvalidHttpRequest);
    }

    Ok(HttpProxyRequest {
        target: TargetAddr {
            host: target_host,
            port: target_port,
        },
        is_connect: false,
        initial_payload: Some(buf.to_vec()),
    })
}

pub async fn read_client_proxy_request<R>(stream: &mut R) -> anyhow::Result<ClientProxyRequest>
where
    R: AsyncReadExt + AsyncWriteExt + Unpin,
{
    let first = stream.read_u8().await?;
    if first == SOCKS5_VERSION {
        let n = stream.read_u8().await? as usize;
        let mut methods = vec![0u8; n];
        stream.read_exact(&mut methods).await?;
        if !methods.contains(&0) {
            stream.write_all(&[5, 0xff]).await?;
            anyhow::bail!("SOCKS5 no-auth unavailable")
        }
        stream.write_all(&[5, 0]).await?;
        let mut head = [0u8; 4];
        stream.read_exact(&mut head).await?;
        if head[1] != SOCKS5_CONNECT && head[1] != SOCKS5_UDP_ASSOCIATE {
            stream.write_all(&[5, 7, 0, 1, 0, 0, 0, 0, 0, 0]).await?;
            anyhow::bail!("unsupported SOCKS5 command")
        }
        let mut req = head.to_vec();
        match head[3] {
            1 => {
                let mut b = [0u8; 6];
                stream.read_exact(&mut b).await?;
                req.extend_from_slice(&b);
            }
            3 => {
                let mut n = [0u8; 1];
                stream.read_exact(&mut n).await?;
                req.extend_from_slice(&n);
                let mut b = vec![0u8; n[0] as usize + 2];
                stream.read_exact(&mut b).await?;
                req.extend_from_slice(&b);
            }
            4 => {
                let mut b = [0u8; 18];
                stream.read_exact(&mut b).await?;
                req.extend_from_slice(&b);
            }
            _ => {
                stream.write_all(&[5, 8, 0, 1, 0, 0, 0, 0, 0, 0]).await?;
                anyhow::bail!("unsupported SOCKS5 address type")
            }
        }
        let parsed = parse_socks5_request(&req).map_err(|e| anyhow::anyhow!(e.to_string()))?;
        return Ok(ClientProxyRequest {
            command: parsed.command,
            target: parsed.target,
            is_socks5: true,
            is_connect: false,
            initial_payload: None,
        });
    }

    let mut buf = vec![first];
    let mut temp = [0u8; 1024];
    while buf.len() < 8192 {
        if buf.windows(4).any(|w| w == b"\r\n\r\n") || buf.windows(2).any(|w| w == b"\n\n") {
            break;
        }
        let max_to_read = (8192 - buf.len()).min(temp.len());
        let n = stream.read(&mut temp[..max_to_read]).await?;
        if n == 0 {
            break;
        }
        buf.extend_from_slice(&temp[..n]);
    }
    let req = parse_http_proxy_request(&buf).map_err(|e| anyhow::anyhow!(e.to_string()))?;
    Ok(ClientProxyRequest {
        command: SocksCommand::Connect,
        target: req.target,
        is_socks5: false,
        is_connect: req.is_connect,
        initial_payload: req.initial_payload,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn socks_greeting_accepts_no_auth() {
        assert!(parse_socks5_greeting(&[5, 1, 0]).is_ok());
        assert!(matches!(
            parse_socks5_greeting(&[5, 1, 2]),
            Err(ProxyParseError::NoAcceptableMethod)
        ));
    }

    #[test]
    fn socks_connect_ipv4() {
        let req = [5, 1, 0, 1, 127, 0, 0, 1, 0x01, 0xbb];
        assert_eq!(
            parse_socks5_request(&req).unwrap().target,
            TargetAddr {
                host: "127.0.0.1".into(),
                port: 443
            }
        );
    }

    #[test]
    fn socks_udp_associate_allows_zero_port() {
        let req = [5, 3, 0, 1, 0, 0, 0, 0, 0, 0];
        let parsed = parse_socks5_request(&req).unwrap();
        assert_eq!(parsed.command, SocksCommand::UdpAssociate);
        assert_eq!(parsed.target.port, 0);
    }

    #[test]
    fn socks_connect_domain() {
        let mut req = vec![5, 1, 0, 3, 11];
        req.extend_from_slice(b"example.com");
        req.extend_from_slice(&443u16.to_be_bytes());
        assert_eq!(
            parse_socks5_request(&req).unwrap().target.host,
            "example.com"
        );
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
    fn socks_failure_response_is_general_failure() {
        assert_eq!(socks5_failure_response(), [5, 5, 0, 1, 0, 0, 0, 0, 0, 0]);
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
        assert!(matches!(
            parse_http_connect(&req),
            Err(ProxyParseError::HeaderTooLarge)
        ));
    }

    #[test]
    fn http_proxy_request_plain_get_and_post() {
        let get_req = b"GET http://example.com/index.html HTTP/1.1\r\nHost: example.com\r\n\r\n";
        let parsed = parse_http_proxy_request(get_req).unwrap();
        assert_eq!(parsed.target.host, "example.com");
        assert_eq!(parsed.target.port, 80);
        assert!(!parsed.is_connect);
        assert_eq!(parsed.initial_payload.unwrap(), get_req);

        let post_req = b"POST /api/test HTTP/1.1\r\nHost: api.example.com:8080\r\n\r\n";
        let parsed_post = parse_http_proxy_request(post_req).unwrap();
        assert_eq!(parsed_post.target.host, "api.example.com");
        assert_eq!(parsed_post.target.port, 8080);
        assert!(!parsed_post.is_connect);
    }

    #[tokio::test]
    async fn test_read_client_proxy_request_socks5_and_http() {
        let (mut client, mut server) = tokio::io::duplex(1024);
        tokio::spawn(async move {
            client.write_all(&[5, 1, 0]).await.unwrap();
            let mut auth_resp = [0u8; 2];
            client.read_exact(&mut auth_resp).await.unwrap();
            assert_eq!(auth_resp, [5, 0]);
            client
                .write_all(&[5, 1, 0, 1, 127, 0, 0, 1, 0x01, 0xbb])
                .await
                .unwrap();
        });
        let req = read_client_proxy_request(&mut server).await.unwrap();
        assert_eq!(req.command, SocksCommand::Connect);
        assert!(req.is_socks5);
        assert_eq!(req.target.port, 443);

        let (mut client, mut server) = tokio::io::duplex(1024);
        tokio::spawn(async move {
            client
                .write_all(b"GET http://example.com/ HTTP/1.1\r\nHost: example.com\r\n\r\n")
                .await
                .unwrap();
        });
        let req2 = read_client_proxy_request(&mut server).await.unwrap();
        assert_eq!(req2.command, SocksCommand::Connect);
        assert!(!req2.is_socks5);
        assert!(!req2.is_connect);
        assert_eq!(req2.target.host, "example.com");
        assert_eq!(req2.target.port, 80);

        let (mut client, mut server) = tokio::io::duplex(1024);
        tokio::spawn(async move {
            client.write_all(&[5, 1, 0]).await.unwrap();
            let mut auth_resp = [0u8; 2];
            client.read_exact(&mut auth_resp).await.unwrap();
            assert_eq!(auth_resp, [5, 0]);
            client
                .write_all(&[5, 3, 0, 1, 127, 0, 0, 1, 0x00, 0x00])
                .await
                .unwrap();
        });
        let req3 = read_client_proxy_request(&mut server).await.unwrap();
        assert_eq!(req3.command, SocksCommand::UdpAssociate);
        assert!(req3.is_socks5);

        let (mut client, mut server) = tokio::io::duplex(1024);
        tokio::spawn(async move {
            client
                .write_all(b"POST http://example.com/api HTTP/1.1\r\nHost: example.com\r\nContent-Length: 5\r\n\r\nhello")
                .await
                .unwrap();
        });
        let req4 = read_client_proxy_request(&mut server).await.unwrap();
        assert_eq!(req4.command, SocksCommand::Connect);
        assert!(!req4.is_socks5);
        assert!(!req4.is_connect);
        assert_eq!(req4.target.host, "example.com");
        let initial = req4.initial_payload.expect("must have initial payload");
        assert!(initial.ends_with(b"hello"));
    }

    #[test]
    fn test_parse_target_authority_and_default() {
        let t1 = parse_target_authority("127.0.0.1:8080").unwrap();
        assert_eq!(t1.host, "127.0.0.1");
        assert_eq!(t1.port, 8080);

        let t2 = parse_target_authority("example.com:443").unwrap();
        assert_eq!(t2.host, "example.com");
        assert_eq!(t2.port, 443);

        let t3 = parse_target_authority("[2001:db8::1]:8443").unwrap();
        assert_eq!(t3.host, "2001:db8::1");
        assert_eq!(t3.port, 8443);

        let t4 = parse_authority_with_default("[2001:db8::1]", 80).unwrap();
        assert_eq!(t4.host, "2001:db8::1");
        assert_eq!(t4.port, 80);

        let t5 = parse_authority_with_default("example.com", 8080).unwrap();
        assert_eq!(t5.host, "example.com");
        assert_eq!(t5.port, 8080);

        assert!(parse_target_authority("example.com").is_err());
        assert!(parse_target_authority("example.com:0").is_err());
        assert!(parse_target_authority("2001:db8::1").is_err());
        assert!(parse_authority_with_default("2001:db8::1", 80).is_err());
    }
}
