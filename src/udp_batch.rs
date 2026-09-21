//! Batched UDP reads shared by all UDP relay upload paths.
//!
//! GoWay parity (`#2 UDP batching`): DNS bursts and game traffic arrive as
//! packet trains — one syscall per datagram wastes the pps budget. This
//! drains up to [`UDP_BATCH`] datagrams per syscall via `recvmmsg` on Linux
//! and falls back to a single `recv_from` elsewhere (e.g. Windows, whose
//! kernel has no equivalent). Behavior is identical on all platforms; only
//! Linux goes faster.
//!
//! Design notes:
//! - The socket stays a shared `Arc<tokio::net::UdpSocket>` (senders keep
//!   using `send_to` on their clones); readiness comes from
//!   `readable()` and the batch is pulled with one `recvmmsg`, exactly the
//!   pattern Quinn uses. No lock, no extra task, no new dependency.
//! - Buffers stay owned full-length (`len == cap == 65535`); only the valid
//!   prefix (tracked in `lens`) is ever read, so there is no `set_len`
//!   unsoundness. 64 KiB covers the maximum UDP payload, matching the
//!   single-recv loops this replaces.

use std::io;
use std::net::SocketAddr;
use std::sync::Arc;

/// Datagrams per `recvmmsg` syscall. 8 amortizes the syscall without adding
/// latency: a partial batch returns immediately, `recvmmsg` never waits for
/// a full one.
pub(crate) const UDP_BATCH: usize = 8;
/// Max UDP payload size: a full-size datagram is never truncated.
pub(crate) const UDP_BUF_SIZE: usize = 65535;

pub(crate) struct UdpBatchReader {
    sock: Arc<tokio::net::UdpSocket>,
    bufs: Vec<Vec<u8>>,
    lens: Vec<usize>,
    addrs: Vec<SocketAddr>,
}

impl UdpBatchReader {
    pub(crate) fn new(sock: Arc<tokio::net::UdpSocket>) -> Self {
        Self {
            sock,
            bufs: (0..UDP_BATCH).map(|_| vec![0u8; UDP_BUF_SIZE]).collect(),
            lens: vec![0; UDP_BATCH],
            addrs: vec![SocketAddr::from(([0, 0, 0, 0], 0)); UDP_BATCH],
        }
    }

    /// Receives up to [`UDP_BATCH`] datagrams, returning the count. Access
    /// each via [`UdpBatchReader::packet`].
    pub(crate) async fn recv(&mut self) -> io::Result<usize> {
        loop {
            self.sock.readable().await?;
            match self.try_recv_batch() {
                Err(e) if e.kind() == io::ErrorKind::WouldBlock => continue,
                res => return res,
            }
        }
    }

    #[cfg(target_os = "linux")]
    fn try_recv_batch(&mut self) -> io::Result<usize> {
        recv_batch_linux(&self.sock, &mut self.bufs, &mut self.lens, &mut self.addrs)
    }

    #[cfg(not(target_os = "linux"))]
    fn try_recv_batch(&mut self) -> io::Result<usize> {
        match self.sock.try_recv_from(&mut self.bufs[0]) {
            Ok((n, addr)) => {
                self.lens[0] = n;
                self.addrs[0] = addr;
                Ok(1)
            }
            Err(e) => Err(e),
        }
    }

    /// Payload + source of datagram `i` (valid for `i` below the last
    /// [`UdpBatchReader::recv`] count).
    pub(crate) fn packet(&self, i: usize) -> (&[u8], SocketAddr) {
        (&self.bufs[i][..self.lens[i]], self.addrs[i])
    }
}

#[cfg(target_os = "linux")]
fn recv_batch_linux(
    sock: &tokio::net::UdpSocket,
    bufs: &mut [Vec<u8>],
    lens: &mut [usize],
    addrs: &mut [SocketAddr],
) -> io::Result<usize> {
    use std::os::fd::AsRawFd;

    let fd = sock.as_raw_fd();
    debug_assert_eq!(bufs.len(), UDP_BATCH);

    let mut iovs = [libc::iovec {
        iov_base: std::ptr::null_mut(),
        iov_len: 0,
    }; UDP_BATCH];
    // SAFETY: all-zero is a valid initializer for these structs; every
    // field the kernel reads is set below before the syscall.
    let mut msgs: [libc::mmsghdr; UDP_BATCH] = unsafe { std::mem::zeroed() };
    let mut names: [libc::sockaddr_storage; UDP_BATCH] = unsafe { std::mem::zeroed() };

    for i in 0..UDP_BATCH {
        iovs[i].iov_base = bufs[i].as_mut_ptr() as *mut libc::c_void;
        iovs[i].iov_len = UDP_BUF_SIZE;
        msgs[i].msg_hdr.msg_name = &mut names[i] as *mut _ as *mut libc::c_void;
        msgs[i].msg_hdr.msg_namelen = std::mem::size_of::<libc::sockaddr_storage>() as libc::socklen_t;
        msgs[i].msg_hdr.msg_iov = &mut iovs[i];
        msgs[i].msg_hdr.msg_iovlen = 1;
    }

    // SAFETY: fd is a live non-blocking UDP socket owned by `sock`
    // (outlives this call); all pointers target live stack/heap memory
    // with correct lengths; flags/timeout unused.
    let ret = unsafe { libc::recvmmsg(fd, msgs.as_mut_ptr(), UDP_BATCH as _, 0, std::ptr::null_mut()) };
    if ret < 0 {
        return Err(io::Error::last_os_error());
    }
    for i in 0..ret as usize {
        lens[i] = msgs[i].msg_len as usize;
        addrs[i] = sockaddr_to_std(&names[i], msgs[i].msg_hdr.msg_namelen)?;
    }
    Ok(ret as usize)
}

#[cfg(target_os = "linux")]
fn sockaddr_to_std(ss: &libc::sockaddr_storage, len: libc::socklen_t) -> io::Result<SocketAddr> {
    let len = len as usize;
    match ss.ss_family as i32 {
        libc::AF_INET if len >= std::mem::size_of::<libc::sockaddr_in>() => {
            // SAFETY: sockaddr_storage is suitably aligned/sized for any
            // address family; length was validated above.
            let a: &libc::sockaddr_in = unsafe { &*(ss as *const _ as *const _) };
            let raw = a.sin_addr.s_addr.to_ne_bytes();
            let ip = std::net::Ipv4Addr::new(raw[0], raw[1], raw[2], raw[3]);
            Ok(SocketAddr::new(ip.into(), u16::from_be(a.sin_port)))
        }
        libc::AF_INET6 if len >= std::mem::size_of::<libc::sockaddr_in6>() => {
            let a: &libc::sockaddr_in6 = unsafe { &*(ss as *const _ as *const _) };
            let ip = std::net::Ipv6Addr::from(a.sin6_addr.s6_addr);
            Ok(SocketAddr::new(
                ip.into(),
                u16::from_be(a.sin6_port),
            ))
        }
        _ => Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "recvmmsg returned unsupported address family",
        )),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tokio::net::UdpSocket;

    #[tokio::test]
    async fn batch_reader_delivers_in_order() {
        let rx = Arc::new(UdpSocket::bind("127.0.0.1:0").await.unwrap());
        let tx = UdpSocket::bind("127.0.0.1:0").await.unwrap();
        let tx_addr = tx.local_addr().unwrap();
        tx.connect(rx.local_addr().unwrap()).await.unwrap();
        const K: usize = 64;
        for i in 0..K {
            let msg = [((i >> 8) & 0xff) as u8, (i & 0xff) as u8, 0xAB, 0xCD];
            tx.send(&msg).await.unwrap();
        }
        let mut reader = UdpBatchReader::new(rx);
        let mut got = 0usize;
        while got < K {
            let n = reader.recv().await.unwrap();
            assert!((1..=UDP_BATCH).contains(&n), "batch size {n} out of range");
            // Verify in arrival order across batch boundaries.
            for i in 0..n {
                let (pkt, addr) = reader.packet(i);
                assert_eq!(addr, tx_addr);
                let want = (got + i) as u16;
                assert_eq!(pkt.len(), 4);
                assert_eq!(pkt[0], ((want >> 8) & 0xff) as u8);
                assert_eq!(pkt[1], (want & 0xff) as u8);
                got += 1;
            }
        }
        assert_eq!(got, K);
    }
}
