//! GoWay-compatible remote DNS resolver: 5s timeout, UDP->TCP fallback,
//! system-DNS fallback and a 5-minute positive cache.

use anyhow::{anyhow, bail, Result};
use rand::RngCore;
use std::collections::HashMap;
use std::net::{IpAddr, Ipv4Addr, Ipv6Addr, SocketAddr};
use std::sync::{Mutex as StdMutex, OnceLock, RwLock};
use std::time::{Duration, Instant};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{lookup_host, TcpStream, UdpSocket};
use tokio::sync::watch;
use tokio::time::timeout;

const RESOLVE_TIMEOUT: Duration = Duration::from_secs(5);
const CACHE_TTL: Duration = Duration::from_secs(5 * 60);
const DNS_PORT: u16 = 53;
const MAX_PACKET: usize = 4096;

static SERVER: RwLock<Option<IpAddr>> = RwLock::new(None);
static CACHE: OnceLock<RwLock<HashMap<String, (IpAddr, Instant)>>> = OnceLock::new();
static ALL_V4_CACHE: OnceLock<RwLock<HashMap<String, (Vec<Ipv4Addr>, Instant)>>> = OnceLock::new();
static IN_FLIGHT: OnceLock<StdMutex<HashMap<String, watch::Receiver<Option<IpAddr>>>>> =
    OnceLock::new();

fn cache() -> &'static RwLock<HashMap<String, (IpAddr, Instant)>> {
    CACHE.get_or_init(|| RwLock::new(HashMap::new()))
}

fn all_v4_cache() -> &'static RwLock<HashMap<String, (Vec<Ipv4Addr>, Instant)>> {
    ALL_V4_CACHE.get_or_init(|| RwLock::new(HashMap::new()))
}

fn in_flight() -> &'static StdMutex<HashMap<String, watch::Receiver<Option<IpAddr>>>> {
    IN_FLIGHT.get_or_init(|| StdMutex::new(HashMap::new()))
}

pub(crate) async fn configure(server: Option<String>) -> Result<()> {
    let parsed = match server.as_deref() {
        Some(value) => Some(
            value
                .parse::<IpAddr>()
                .map_err(|_| anyhow!("-dns requires a valid IP address, got '{value}'"))?,
        ),
        None => None,
    };
    *SERVER.write().unwrap_or_else(|e| e.into_inner()) = parsed;
    Ok(())
}

pub(crate) async fn resolve_host(host: &str) -> Result<IpAddr> {
    let clean = host.trim().trim_matches(|c| c == '[' || c == ']');
    if let Ok(ip) = clean.parse::<IpAddr>() {
        return Ok(ip);
    }
    let key = clean.trim_end_matches('.').to_ascii_lowercase();
    // Fast path: synchronous read lock, zero tokio yield, nanosecond cache hit
    if let Ok(guard) = cache().read() {
        if let Some(&(ip, expires)) = guard.get(&key) {
            if expires > Instant::now() {
                return Ok(ip);
            }
        }
    }

    // SingleFlight coordination: deduplicate concurrent in-flight DNS lookups for the same host.
    let mut waiter = None;
    let mut tx = None;
    {
        let mut inflight = in_flight().lock().unwrap();
        // Double check cache inside lock
        if let Ok(guard) = cache().read() {
            if let Some(&(ip, expires)) = guard.get(&key) {
                if expires > Instant::now() {
                    return Ok(ip);
                }
            }
        }
        if let Some(rx) = inflight.get(&key) {
            waiter = Some(rx.clone());
        } else {
            let (sender, receiver) = watch::channel(None);
            inflight.insert(key.clone(), receiver);
            tx = Some(sender);
        }
    }

    if let Some(mut rx) = waiter {
        while rx.borrow().is_none() {
            if rx.changed().await.is_err() {
                break;
            }
        }
        if let Some(ip) = *rx.borrow() {
            return Ok(ip);
        }
        if let Ok(guard) = cache().read() {
            if let Some(&(ip, expires)) = guard.get(&key) {
                if expires > Instant::now() {
                    return Ok(ip);
                }
            }
        }
        bail!("concurrent DNS resolution failed for {host}");
    }

    struct InFlightGuard<'a>(&'a str);
    impl<'a> Drop for InFlightGuard<'a> {
        fn drop(&mut self) {
            if let Ok(mut inflight) = in_flight().lock() {
                inflight.remove(self.0);
            }
        }
    }
    let _guard = InFlightGuard(&key);

    let server = *SERVER.read().unwrap_or_else(|e| e.into_inner());
    let remote_result = match server {
        Some(server_ip) => resolve_remote(&key, server_ip).await,
        None => Err(anyhow!("remote DNS not configured")),
    };
    let query_result = match remote_result {
        Ok(ip) => Ok(ip),
        Err(remote_error) => {
            tracing::warn!(host=%host, error=%remote_error, "remote DNS failed; falling back to system DNS");
            match timeout(RESOLVE_TIMEOUT, lookup_host((clean, 0))).await {
                Ok(Ok(mut addresses)) => addresses
                    .next()
                    .map(|addr| addr.ip())
                    .ok_or_else(|| anyhow!("system DNS returned no addresses for {host}")),
                Ok(Err(e)) => Err(e.into()),
                Err(_) => Err(anyhow!("system DNS timeout for {host}")),
            }
        }
    };

    drop(_guard);

    match query_result {
        Ok(resolved) => {
            cache()
                .write()
                .unwrap_or_else(|e| e.into_inner())
                .insert(key, (resolved, Instant::now() + CACHE_TTL));
            if let Some(sender) = tx {
                let _ = sender.send(Some(resolved));
            }
            Ok(resolved)
        }
        Err(e) => {
            if let Some(sender) = tx {
                let _ = sender.send(None);
            }
            Err(e)
        }
    }
}

pub(crate) async fn resolve_socket(host: &str, port: u16) -> Result<SocketAddr> {
    Ok(SocketAddr::new(resolve_host(host).await?, port))
}

pub(crate) async fn resolve_all_ipv4(host: &str) -> Result<Vec<Ipv4Addr>> {
    let clean = host.trim().trim_matches(|c| c == '[' || c == ']');
    if let Ok(ip) = clean.parse::<Ipv4Addr>() {
        return Ok(vec![ip]);
    }
    if clean.parse::<Ipv6Addr>().is_ok() {
        return Ok(Vec::new());
    }
    let key = clean.trim_end_matches('.').to_ascii_lowercase();
    // Fast path: synchronous read lock, zero tokio yield
    if let Ok(guard) = all_v4_cache().read() {
        if let Some((ips, expires)) = guard.get(&key) {
            if *expires > Instant::now() && !ips.is_empty() {
                return Ok(ips.clone());
            }
        }
    }
    let server = *SERVER.read().unwrap_or_else(|e| e.into_inner());
    let remote_result = match server {
        Some(server_ip) => query_remote_all_ipv4(&key, server_ip).await,
        None => Err(anyhow!("remote DNS not configured")),
    };
    let ips = match remote_result {
        Ok(list) if !list.is_empty() => list,
        Err(err) => {
            tracing::warn!(host=%host, error=%err, "remote DNS failed for fakehost; falling back to system DNS");
            let mut result = Vec::new();
            if let Ok(Ok(iter)) = timeout(RESOLVE_TIMEOUT, lookup_host((clean, 0))).await {
                for addr in iter {
                    if let IpAddr::V4(v4) = addr.ip() {
                        if !result.contains(&v4) {
                            result.push(v4);
                        }
                    }
                }
            }
            result
        }
        _ => {
            let mut result = Vec::new();
            if let Ok(Ok(iter)) = timeout(RESOLVE_TIMEOUT, lookup_host((clean, 0))).await {
                for addr in iter {
                    if let IpAddr::V4(v4) = addr.ip() {
                        if !result.contains(&v4) {
                            result.push(v4);
                        }
                    }
                }
            }
            result
        }
    };
    let mut unique = Vec::new();
    for ip in ips {
        if !unique.contains(&ip) {
            unique.push(ip);
        }
    }
    if unique.is_empty() {
        bail!("no IPv4 addresses resolved for {host}");
    }
    all_v4_cache()
        .write()
        .unwrap_or_else(|e| e.into_inner())
        .insert(key, (unique.clone(), Instant::now() + CACHE_TTL));
    Ok(unique)
}

async fn resolve_remote(host: &str, server: IpAddr) -> Result<IpAddr> {
    let mut last_error = None;
    for qtype in [1u16, 28u16] {
        match query_remote(host, server, qtype).await {
            Ok(Some(ip)) => return Ok(ip),
            Ok(None) => {}
            Err(err) => last_error = Some(err),
        }
    }
    Err(last_error.unwrap_or_else(|| anyhow!("remote DNS returned no A/AAAA answer for {host}")))
}

async fn query_remote(host: &str, server: IpAddr, qtype: u16) -> Result<Option<IpAddr>> {
    let request = build_query(host, qtype)?;
    let expected_id = u16::from_be_bytes([request[0], request[1]]);
    let server_addr = SocketAddr::new(server, DNS_PORT);
    let socket = UdpSocket::bind(if server.is_ipv4() {
        "0.0.0.0:0"
    } else {
        "[::]:0"
    })
    .await?;
    socket.send_to(&request, server_addr).await?;
    let mut buf = vec![0u8; MAX_PACKET];
    let (size, _) = match timeout(RESOLVE_TIMEOUT, socket.recv_from(&mut buf)).await {
        Ok(Ok(v)) => v,
        Ok(Err(err)) => return Err(err.into()),
        Err(_) => return Err(anyhow!("remote DNS UDP timeout")),
    };
    buf.truncate(size);
    validate_transaction_id(&buf, expected_id)?;
    let (ip, truncated) = parse_response(&buf, qtype)?;
    if !truncated {
        return Ok(ip);
    }
    let response = dns_tcp_query(server_addr, &request).await?;
    validate_transaction_id(&response, expected_id)?;
    Ok(parse_response(&response, qtype)?.0)
}

async fn query_remote_all_ipv4(host: &str, server: IpAddr) -> Result<Vec<Ipv4Addr>> {
    let request = build_query(host, 1)?;
    let expected_id = u16::from_be_bytes([request[0], request[1]]);
    let server_addr = SocketAddr::new(server, DNS_PORT);
    let socket = UdpSocket::bind(if server.is_ipv4() {
        "0.0.0.0:0"
    } else {
        "[::]:0"
    })
    .await?;
    socket.send_to(&request, server_addr).await?;
    let mut buf = vec![0u8; MAX_PACKET];
    let (size, _) = match timeout(RESOLVE_TIMEOUT, socket.recv_from(&mut buf)).await {
        Ok(Ok(v)) => v,
        Ok(Err(err)) => return Err(err.into()),
        Err(_) => return Err(anyhow!("remote DNS UDP timeout")),
    };
    buf.truncate(size);
    validate_transaction_id(&buf, expected_id)?;
    let (ips, truncated) = parse_all_ipv4_from_response(&buf)?;
    if !truncated && !ips.is_empty() {
        return Ok(ips);
    }
    let response = dns_tcp_query(server_addr, &request).await?;
    validate_transaction_id(&response, expected_id)?;
    Ok(parse_all_ipv4_from_response(&response)?.0)
}

async fn dns_tcp_query(server: SocketAddr, request: &[u8]) -> Result<Vec<u8>> {
    let mut stream = timeout(RESOLVE_TIMEOUT, TcpStream::connect(server)).await??;
    let len = u16::try_from(request.len()).map_err(|_| anyhow!("DNS request too large"))?;
    stream.write_all(&len.to_be_bytes()).await?;
    stream.write_all(request).await?;
    let mut header = [0u8; 2];
    timeout(RESOLVE_TIMEOUT, stream.read_exact(&mut header)).await??;
    let size = u16::from_be_bytes(header) as usize;
    if size == 0 || size > MAX_PACKET {
        bail!("invalid DNS TCP response length: {size}");
    }
    let mut response = vec![0u8; size];
    timeout(RESOLVE_TIMEOUT, stream.read_exact(&mut response)).await??;
    Ok(response)
}

fn validate_transaction_id(buf: &[u8], expected: u16) -> Result<()> {
    if buf.len() < 2 {
        bail!("DNS response too short for transaction ID");
    }
    let actual = u16::from_be_bytes([buf[0], buf[1]]);
    if actual != expected {
        bail!("DNS transaction ID mismatch: expected {expected:#06x}, got {actual:#06x}");
    }
    Ok(())
}

fn build_query(host: &str, qtype: u16) -> Result<Vec<u8>> {
    let labels: Vec<&str> = host.trim_end_matches('.').split('.').collect();
    if labels.is_empty()
        || labels
            .iter()
            .any(|label| label.is_empty() || label.len() > 63)
    {
        bail!("invalid DNS hostname: {host}");
    }
    let mut id_bytes = [0u8; 2];
    rand::thread_rng().fill_bytes(&mut id_bytes);
    let mut out = Vec::with_capacity(64);
    out.extend_from_slice(&id_bytes);
    out.extend_from_slice(&0x0100u16.to_be_bytes());
    out.extend_from_slice(&1u16.to_be_bytes());
    out.extend_from_slice(&0u16.to_be_bytes());
    out.extend_from_slice(&0u16.to_be_bytes());
    out.extend_from_slice(&0u16.to_be_bytes());
    for label in labels {
        out.push(label.len() as u8);
        out.extend_from_slice(label.as_bytes());
    }
    out.push(0);
    out.extend_from_slice(&qtype.to_be_bytes());
    out.extend_from_slice(&1u16.to_be_bytes());
    Ok(out)
}

fn parse_response(buf: &[u8], qtype: u16) -> Result<(Option<IpAddr>, bool)> {
    if buf.len() < 12 {
        bail!("DNS response too short");
    }
    let flags = u16::from_be_bytes([buf[2], buf[3]]);
    let truncated = flags & 0x0200 != 0;
    let rcode = flags & 0x000f;
    if rcode != 0 {
        bail!("DNS response error code {rcode}");
    }
    let qd = u16::from_be_bytes([buf[4], buf[5]]) as usize;
    let an = u16::from_be_bytes([buf[6], buf[7]]) as usize;
    let mut offset = 12usize;
    for _ in 0..qd {
        skip_name(buf, &mut offset)?;
        if offset + 4 > buf.len() {
            bail!("truncated DNS question");
        }
        offset += 4;
    }
    for _ in 0..an {
        skip_name(buf, &mut offset)?;
        if offset + 10 > buf.len() {
            bail!("truncated DNS answer header");
        }
        let rr_type = u16::from_be_bytes([buf[offset], buf[offset + 1]]);
        let class = u16::from_be_bytes([buf[offset + 2], buf[offset + 3]]);
        let rd_len = u16::from_be_bytes([buf[offset + 8], buf[offset + 9]]) as usize;
        offset += 10;
        if offset + rd_len > buf.len() {
            bail!("truncated DNS answer data");
        }
        if class == 1 && rr_type == qtype {
            match qtype {
                1 if rd_len == 4 => {
                    return Ok((
                        Some(IpAddr::V4(Ipv4Addr::new(
                            buf[offset],
                            buf[offset + 1],
                            buf[offset + 2],
                            buf[offset + 3],
                        ))),
                        truncated,
                    ))
                }
                28 if rd_len == 16 => {
                    let mut octets = [0u8; 16];
                    octets.copy_from_slice(&buf[offset..offset + 16]);
                    return Ok((Some(IpAddr::V6(Ipv6Addr::from(octets))), truncated));
                }
                _ => {}
            }
        }
        offset += rd_len;
    }
    Ok((None, truncated))
}

fn parse_all_ipv4_from_response(buf: &[u8]) -> Result<(Vec<Ipv4Addr>, bool)> {
    if buf.len() < 12 {
        bail!("DNS response too short");
    }
    let flags = u16::from_be_bytes([buf[2], buf[3]]);
    let truncated = flags & 0x0200 != 0;
    let rcode = flags & 0x000f;
    if rcode != 0 {
        bail!("DNS response error code {rcode}");
    }
    let qd = u16::from_be_bytes([buf[4], buf[5]]) as usize;
    let an = u16::from_be_bytes([buf[6], buf[7]]) as usize;
    let mut offset = 12usize;
    for _ in 0..qd {
        skip_name(buf, &mut offset)?;
        if offset + 4 > buf.len() {
            bail!("truncated DNS question");
        }
        offset += 4;
    }
    let mut ips = Vec::new();
    for _ in 0..an {
        skip_name(buf, &mut offset)?;
        if offset + 10 > buf.len() {
            bail!("truncated DNS answer header");
        }
        let rr_type = u16::from_be_bytes([buf[offset], buf[offset + 1]]);
        let class = u16::from_be_bytes([buf[offset + 2], buf[offset + 3]]);
        let rd_len = u16::from_be_bytes([buf[offset + 8], buf[offset + 9]]) as usize;
        offset += 10;
        if offset + rd_len > buf.len() {
            bail!("truncated DNS answer data");
        }
        if class == 1 && rr_type == 1 && rd_len == 4 {
            let ip = Ipv4Addr::new(
                buf[offset],
                buf[offset + 1],
                buf[offset + 2],
                buf[offset + 3],
            );
            if !ips.contains(&ip) {
                ips.push(ip);
            }
        }
        offset += rd_len;
    }
    Ok((ips, truncated))
}

fn skip_name(buf: &[u8], offset: &mut usize) -> Result<()> {
    let mut pos = *offset;
    let mut labels = 0usize;
    loop {
        if pos >= buf.len() {
            bail!("truncated DNS name");
        }
        let len = buf[pos];
        if len & 0xc0 == 0xc0 {
            if pos + 1 >= buf.len() {
                bail!("truncated DNS name pointer");
            }
            pos += 2;
            break;
        }
        if len == 0 {
            pos += 1;
            break;
        }
        if len & 0xc0 != 0 || len as usize > 63 {
            bail!("invalid DNS label");
        }
        pos += 1 + len as usize;
        if pos > buf.len() {
            bail!("truncated DNS label");
        }
        labels += 1;
        if labels > 128 {
            bail!("DNS name too deep");
        }
    }
    *offset = pos;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn builds_query_with_question() {
        let q = build_query("example.com", 1).unwrap();
        assert!(q.len() > 16);
        assert_eq!(u16::from_be_bytes([q[4], q[5]]), 1);
    }
    #[test]
    fn transaction_id_mismatch_is_rejected() {
        let response = [0x12, 0x35, 0x81, 0x80, 0, 1, 0, 0, 0, 0, 0, 0];
        assert!(validate_transaction_id(&response, 0x1234).is_err());
        assert!(validate_transaction_id(&response, 0x1235).is_ok());
    }
    #[test]
    fn parses_ipv4_answer() {
        let response = vec![
            0x12, 0x34, 0x81, 0x80, 0x00, 0x01, 0x00, 0x01, 0x00, 0x00, 0x00, 0x00, 0x07, b'e',
            b'x', b'a', b'm', b'p', b'l', b'e', 0x03, b'c', b'o', b'm', 0x00, 0x00, 0x01, 0x00,
            0x01, 0xc0, 0x0c, 0x00, 0x01, 0x00, 0x01, 0x00, 0x00, 0x00, 0x3c, 0x00, 0x04, 1, 2, 3,
            4,
        ];
        let (ip, truncated) = parse_response(&response, 1).unwrap();
        assert!(!truncated);
        assert_eq!(ip, Some(IpAddr::V4(Ipv4Addr::new(1, 2, 3, 4))));
    }
    #[test]
    fn parses_multiple_ipv4_answers() {
        let response = vec![
            0x12, 0x34, 0x81, 0x80, 0x00, 0x01, 0x00, 0x02, 0x00, 0x00, 0x00, 0x00, 0x07, b'e',
            b'x', b'a', b'm', b'p', b'l', b'e', 0x03, b'c', b'o', b'm', 0x00, 0x00, 0x01, 0x00,
            0x01, 0xc0, 0x0c, 0x00, 0x01, 0x00, 0x01, 0x00, 0x00, 0x00, 0x3c, 0x00, 0x04, 104, 21,
            50, 1, 0xc0, 0x0c, 0x00, 0x01, 0x00, 0x01, 0x00, 0x00, 0x00, 0x3c, 0x00, 0x04, 172, 67,
            180, 2,
        ];
        let (ips, truncated) = parse_all_ipv4_from_response(&response).unwrap();
        assert!(!truncated);
        assert_eq!(
            ips,
            vec![
                Ipv4Addr::new(104, 21, 50, 1),
                Ipv4Addr::new(172, 67, 180, 2)
            ]
        );
    }

    #[tokio::test]
    async fn parses_bracketed_ipv6_without_dns_query() {
        let ip = resolve_host("[::1]").await.unwrap();
        assert_eq!(ip, "::1".parse::<IpAddr>().unwrap());

        let ip2 = resolve_host("[2001:db8::1]").await.unwrap();
        assert_eq!(ip2, "2001:db8::1".parse::<IpAddr>().unwrap());

        let v4_list = resolve_all_ipv4("[::1]").await.unwrap();
        assert!(v4_list.is_empty());
    }
}
