//! GoWay-compatible remote DNS resolver: 5s timeout, UDP->TCP fallback,
//! system-DNS fallback and a 5-minute positive cache.

use anyhow::{anyhow, bail, Result};
use rand::RngCore;
use std::collections::HashMap;
use std::net::{IpAddr, Ipv4Addr, Ipv6Addr, SocketAddr};
use std::sync::{Arc, OnceLock};
use std::time::{Duration, Instant};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{lookup_host, TcpStream, UdpSocket};
use tokio::sync::Mutex;
use tokio::time::timeout;

const RESOLVE_TIMEOUT: Duration = Duration::from_secs(5);
const CACHE_TTL: Duration = Duration::from_secs(5 * 60);
const DNS_PORT: u16 = 53;
const MAX_PACKET: usize = 4096;

type Cache = Arc<Mutex<HashMap<String, (IpAddr, Instant)>>>;
struct State { server: Option<IpAddr>, cache: Cache }
static STATE: OnceLock<Mutex<State>> = OnceLock::new();

fn state() -> &'static Mutex<State> {
    STATE.get_or_init(|| Mutex::new(State { server: None, cache: Arc::new(Mutex::new(HashMap::new())) }))
}

pub(crate) async fn configure(server: Option<String>) -> Result<()> {
    let parsed = match server.as_deref() {
        Some(value) => Some(value.parse::<IpAddr>().map_err(|_| anyhow!("-dns requires a valid IP address, got '{value}'"))?),
        None => None,
    };
    state().lock().await.server = parsed;
    Ok(())
}

pub(crate) async fn resolve_host(host: &str) -> Result<IpAddr> {
    if let Ok(ip) = host.parse::<IpAddr>() { return Ok(ip); }
    let (server, cache) = {
        let guard = state().lock().await;
        (guard.server, guard.cache.clone())
    };
    let key = host.trim_end_matches('.').to_ascii_lowercase();
    {
        let mut guard = cache.lock().await;
        if let Some((ip, expires)) = guard.get(&key).copied() {
            if expires > Instant::now() { return Ok(ip); }
            guard.remove(&key);
        }
    }
    let remote_result = match server {
        Some(server_ip) => resolve_remote(&key, server_ip).await,
        None => Err(anyhow!("remote DNS not configured")),
    };
    let resolved = match remote_result {
        Ok(ip) => ip,
        Err(remote_error) => {
            tracing::warn!(host=%host, error=%remote_error, "remote DNS failed; falling back to system DNS");
            let mut addresses = timeout(RESOLVE_TIMEOUT, lookup_host((host, 0))).await??;
            addresses.next().map(|addr| addr.ip()).ok_or_else(|| anyhow!("system DNS returned no addresses for {host}"))?
        }
    };
    cache.lock().await.insert(key, (resolved, Instant::now() + CACHE_TTL));
    Ok(resolved)
}

pub(crate) async fn resolve_socket(host: &str, port: u16) -> Result<SocketAddr> {
    Ok(SocketAddr::new(resolve_host(host).await?, port))
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
    let server_addr = SocketAddr::new(server, DNS_PORT);
    let socket = UdpSocket::bind(if server.is_ipv4() { "0.0.0.0:0" } else { "[::]:0" }).await?;
    socket.send_to(&request, server_addr).await?;
    let mut buf = vec![0u8; MAX_PACKET];
    let (size, _) = match timeout(RESOLVE_TIMEOUT, socket.recv_from(&mut buf)).await {
        Ok(Ok(v)) => v,
        Ok(Err(err)) => return Err(err.into()),
        Err(_) => return Err(anyhow!("remote DNS UDP timeout")),
    };
    buf.truncate(size);
    let (ip, truncated) = parse_response(&buf, qtype)?;
    if !truncated { return Ok(ip); }
    let response = dns_tcp_query(server_addr, &request).await?;
    Ok(parse_response(&response, qtype)?.0)
}

async fn dns_tcp_query(server: SocketAddr, request: &[u8]) -> Result<Vec<u8>> {
    let mut stream = timeout(RESOLVE_TIMEOUT, TcpStream::connect(server)).await??;
    let len = u16::try_from(request.len()).map_err(|_| anyhow!("DNS request too large"))?;
    stream.write_all(&len.to_be_bytes()).await?;
    stream.write_all(request).await?;
    let mut header = [0u8; 2];
    timeout(RESOLVE_TIMEOUT, stream.read_exact(&mut header)).await??;
    let size = u16::from_be_bytes(header) as usize;
    if size == 0 || size > MAX_PACKET { bail!("invalid DNS TCP response length: {size}"); }
    let mut response = vec![0u8; size];
    timeout(RESOLVE_TIMEOUT, stream.read_exact(&mut response)).await??;
    Ok(response)
}

fn build_query(host: &str, qtype: u16) -> Result<Vec<u8>> {
    let labels: Vec<&str> = host.trim_end_matches('.').split('.').collect();
    if labels.is_empty() || labels.iter().any(|label| label.is_empty() || label.len() > 63) { bail!("invalid DNS hostname: {host}"); }
    let mut id_bytes = [0u8; 2];
    rand::thread_rng().fill_bytes(&mut id_bytes);
    let mut out = Vec::with_capacity(64);
    out.extend_from_slice(&id_bytes);
    out.extend_from_slice(&0x0100u16.to_be_bytes());
    out.extend_from_slice(&1u16.to_be_bytes());
    out.extend_from_slice(&0u16.to_be_bytes());
    out.extend_from_slice(&0u16.to_be_bytes());
    out.extend_from_slice(&0u16.to_be_bytes());
    for label in labels { out.push(label.len() as u8); out.extend_from_slice(label.as_bytes()); }
    out.push(0);
    out.extend_from_slice(&qtype.to_be_bytes());
    out.extend_from_slice(&1u16.to_be_bytes());
    Ok(out)
}

fn parse_response(buf: &[u8], qtype: u16) -> Result<(Option<IpAddr>, bool)> {
    if buf.len() < 12 { bail!("DNS response too short"); }
    let flags = u16::from_be_bytes([buf[2], buf[3]]);
    let truncated = flags & 0x0200 != 0;
    let rcode = flags & 0x000f;
    if rcode != 0 { bail!("DNS response error code {rcode}"); }
    let qd = u16::from_be_bytes([buf[4], buf[5]]) as usize;
    let an = u16::from_be_bytes([buf[6], buf[7]]) as usize;
    let mut offset = 12usize;
    for _ in 0..qd {
        skip_name(buf, &mut offset)?;
        if offset + 4 > buf.len() { bail!("truncated DNS question"); }
        offset += 4;
    }
    for _ in 0..an {
        skip_name(buf, &mut offset)?;
        if offset + 10 > buf.len() { bail!("truncated DNS answer header"); }
        let rr_type = u16::from_be_bytes([buf[offset], buf[offset + 1]]);
        let class = u16::from_be_bytes([buf[offset + 2], buf[offset + 3]]);
        let rd_len = u16::from_be_bytes([buf[offset + 8], buf[offset + 9]]) as usize;
        offset += 10;
        if offset + rd_len > buf.len() { bail!("truncated DNS answer data"); }
        if class == 1 && rr_type == qtype {
            match qtype {
                1 if rd_len == 4 => return Ok((Some(IpAddr::V4(Ipv4Addr::new(buf[offset], buf[offset + 1], buf[offset + 2], buf[offset + 3]))), truncated)),
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

fn skip_name(buf: &[u8], offset: &mut usize) -> Result<()> {
    let mut pos = *offset;
    let mut labels = 0usize;
    loop {
        if pos >= buf.len() { bail!("truncated DNS name"); }
        let len = buf[pos];
        if len & 0xc0 == 0xc0 {
            if pos + 1 >= buf.len() { bail!("truncated DNS name pointer"); }
            pos += 2;
            break;
        }
        if len == 0 { pos += 1; break; }
        if len & 0xc0 != 0 || len as usize > 63 { bail!("invalid DNS label"); }
        pos += 1 + len as usize;
        if pos > buf.len() { bail!("truncated DNS label"); }
        labels += 1;
        if labels > 128 { bail!("DNS name too deep"); }
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
    fn parses_ipv4_answer() {
        let response = vec![
            0x12,0x34,0x81,0x80,0x00,0x01,0x00,0x01,0x00,0x00,0x00,0x00,
            0x07,b'e',b'x',b'a',b'm',b'p',b'l',b'e',0x03,b'c',b'o',b'm',0x00,
            0x00,0x01,0x00,0x01,0xc0,0x0c,0x00,0x01,0x00,0x01,0x00,0x00,0x00,0x3c,0x00,0x04,1,2,3,4,
        ];
        let (ip, truncated) = parse_response(&response, 1).unwrap();
        assert!(!truncated);
        assert_eq!(ip, Some(IpAddr::V4(Ipv4Addr::new(1,2,3,4))));
    }
}
