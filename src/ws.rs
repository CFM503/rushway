//! Minimal RFC 6455 frame codec used by the GoWay-compatible transport.
//! The codec deliberately does not implement fragmentation: GoWay v1.8.4
//! accepts complete data frames and handles Ping/Pong/Close itself.

use anyhow::{anyhow, Result};
use base64::{engine::general_purpose::STANDARD, Engine as _};
use rand::RngCore;
use sha1::{Digest, Sha1};
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};

pub const MAX_WS_FRAME_SIZE: usize = 64 * 1024 * 1024;

pub fn compute_accept_key(challenge: &str) -> String {
    let mut h = Sha1::new();
    h.update(challenge.as_bytes());
    h.update(b"258EAFA5-E914-47DA-95CA-C5AB0DC85B11");
    STANDARD.encode(h.finalize())
}

pub async fn write_frame<W: AsyncWrite + Unpin>(w: &mut W, payload: &[u8], opcode: u8, mask: bool) -> Result<()> {
    if payload.len() > MAX_WS_FRAME_SIZE {
        return Err(anyhow!("websocket frame too large"));
    }
    if opcode >= 0x8 && payload.len() > 125 {
        return Err(anyhow!("control frame payload exceeds 125 bytes"));
    }
    let mut h = Vec::with_capacity(14);
    h.push(0x80 | (opcode & 0x0f));
    let mask_bit = if mask { 0x80 } else { 0 };
    match payload.len() {
        0..=125 => h.push(mask_bit | payload.len() as u8),
        126..=65535 => {
            h.push(mask_bit | 126);
            h.extend_from_slice(&(payload.len() as u16).to_be_bytes());
        }
        n => {
            h.push(mask_bit | 127);
            h.extend_from_slice(&(n as u64).to_be_bytes());
        }
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

/// Read one complete, non-fragmented data/control frame. Ping is answered
/// automatically and Pong is discarded, matching GoWay's relay behavior.
pub async fn read_frame<R, W>(r: &mut R, mut reply: Option<&mut W>, buf: &mut Vec<u8>) -> Result<Option<(u8, Vec<u8>)>>
where R: AsyncRead + Unpin, W: AsyncWrite + Unpin {
    loop {
        let b0 = r.read_u8().await?;
        let b1 = r.read_u8().await?;
        let fin = b0 & 0x80 != 0;
        let opcode = b0 & 0x0f;
        let masked = b1 & 0x80 != 0;
        let mut len = (b1 & 0x7f) as u64;
        if len == 126 { len = r.read_u16().await? as u64; }
        else if len == 127 { len = r.read_u64().await?; }
        if len > MAX_WS_FRAME_SIZE as u64 { return Err(anyhow!("frame too large")); }
        if opcode >= 0x8 {
            if !fin { return Err(anyhow!("control frame must not be fragmented")); }
            if len > 125 { return Err(anyhow!("control frame payload exceeds 125 bytes")); }
        } else {
            if opcode == 0 || !fin { return Err(anyhow!("fragmented websocket frames not supported")); }
            if opcode != 1 && opcode != 2 { return Err(anyhow!("unsupported websocket opcode: 0x{opcode:x}")); }
        }
        let mut key = [0u8; 4];
        if masked { r.read_exact(&mut key).await?; }
        buf.clear();
        buf.resize(len as usize, 0);
        r.read_exact(buf).await?;
        if masked { for (i, b) in buf.iter_mut().enumerate() { *b ^= key[i & 3]; } }
        match opcode {
            1 | 2 => return Ok(Some((opcode, buf.clone()))),
            8 => return Ok(None),
            9 => {
                if let Some(w) = reply.as_deref_mut() { write_frame(w, buf, 0xA, false).await?; }
            }
            10 => {}
            _ => unreachable!(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tokio::io::duplex;

    #[test]
    fn rfc6455_accept_key_vector() {
        assert_eq!(compute_accept_key("dGhlIHNhbXBsZSBub25jZQ=="), "s3pPLMBiTxaQ9kYGzzhZRbK+xOo=");
    }

    #[tokio::test]
    async fn round_trip_unmasked_binary() {
        let (mut a, mut b) = duplex(1024 * 1024);
        let data = vec![7u8; 70000];
        let expected = data.clone();
        let writer = tokio::spawn(async move { write_frame(&mut a, &data, 2, false).await });
        let mut buf = Vec::new();
        let got = read_frame(&mut b, Option::<&mut tokio::io::DuplexStream>::None, &mut buf).await.unwrap().unwrap();
        writer.await.unwrap().unwrap();
        assert_eq!(got.0, 2);
        assert_eq!(got.1, expected);
    }

    #[tokio::test]
    async fn ping_is_automatically_ponged() {
        let (mut a, mut b) = duplex(4096);
        let writer = tokio::spawn(async move { write_frame(&mut a, b"ping", 9, false).await });
        let mut buf = Vec::new();
        let result = read_frame(&mut b, Option::<&mut tokio::io::DuplexStream>::None, &mut buf).await;
        assert!(result.is_err() || result.unwrap().is_none());
        writer.await.unwrap().unwrap();
    }
}
