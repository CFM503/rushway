//! Dedicated per-session WebSocket frame writer.
//!
//! GoWay `muxOutboundWriter` parity: instead of every relay stream locking a
//! shared `Mutex<WriteHalf>` per frame (which collapses under many-stream
//! concurrency — measured c32 persistently below c8), each MUX session owns
//! one writer task fed by a bounded channel. Callers pre-encode complete WS
//! frames (see [`crate::ws::encode_ws_frame`], masking included) so the hot
//! loop is pure I/O, and consecutive queued frames are coalesced into
//! vectored writes.

use crate::crypto::XorCipher;
use crate::protocol::{write_frame_parts, MuxCommand};
use crate::ws::{next_mask, ws_header_into};
use anyhow::{anyhow, Result};
use std::future::poll_fn;
use std::io::IoSlice;
use std::pin::Pin;
use std::sync::{
    atomic::{AtomicBool, Ordering},
    Arc,
};
use tokio::io::{AsyncWrite, AsyncWriteExt};
use tokio::sync::mpsc;
use tokio::task::JoinHandle;

const WRITER_QUEUE: usize = 256;
const BATCH_MAX_FRAMES: usize = 32;
const BATCH_MAX_BYTES: usize = 1024 * 1024;

pub(crate) struct MuxFrameWriter {
    tx: mpsc::Sender<Vec<u8>>,
    closed: Arc<AtomicBool>,
}

impl MuxFrameWriter {
    /// Spawns the writer task owning `w` and returns the shared handle plus
    /// the task. Dropping the last handle closes the channel; the task then
    /// flushes leftovers best-effort, shuts the write half down, and exits.
    ///
    /// NB: the task holds only the `closed` flag, never a full `Arc<Self>`
    /// (which would keep a sender alive and prevent channel close forever).
    pub fn spawn<W>(w: W) -> (Arc<Self>, JoinHandle<()>)
    where
        W: AsyncWrite + Unpin + Send + 'static,
    {
        let (tx, rx) = mpsc::channel(WRITER_QUEUE);
        let closed = Arc::new(AtomicBool::new(false));
        let this = Arc::new(Self {
            tx,
            closed: closed.clone(),
        });
        let handle = tokio::spawn(async move {
            writer_loop(w, rx).await;
            closed.store(true, Ordering::Release);
        });
        (this, handle)
    }

    /// Queues one pre-encoded frame. Fails fast once the transport died;
    /// otherwise back-pressures when the queue is full.
    pub async fn send(&self, frame: Vec<u8>) -> Result<()> {
        if self.closed.load(Ordering::Acquire) {
            return Err(anyhow!("mux writer closed"));
        }
        self.tx.send(frame).await.map_err(|_| {
            self.closed.store(true, Ordering::Release);
            anyhow!("mux writer closed")
        })
    }

    pub fn is_closed(&self) -> bool {
        self.closed.load(Ordering::Acquire)
    }
}

/// Encodes one MUX frame already wrapped in its WebSocket frame, in a
/// single buffer with a single allocation: `[WS header][mask?][MUX header |
/// payload]`.
///
/// Previously this took two allocations and a full extra copy (MUX `Vec`
/// then WS `Vec`). Layout: the WS header describes the MUX frame length
/// (`7 + payload.len()`), the cipher covers exactly the MUX region, and
/// the WS mask (client side) covers it as well.
pub(crate) fn encode_mux_ws_frame(
    out: &mut Vec<u8>,
    stream_id: u32,
    command: MuxCommand,
    payload: &[u8],
    cipher: &XorCipher,
    masked: bool,
) -> Result<()> {
    let mux_len = 7 + payload.len();
    let mut ws_header = [0u8; 14];
    let ws_header_len = ws_header_into(&mut ws_header, mux_len, 0x2, masked);
    let mask_len = if masked { 4 } else { 0 };
    out.clear();
    out.reserve(ws_header_len + mask_len + mux_len);
    out.extend_from_slice(&ws_header[..ws_header_len]);
    let mask_key = if masked {
        let key = next_mask();
        out.extend_from_slice(&key);
        Some(key)
    } else {
        None
    };
    let mux_start = out.len();
    write_frame_parts(out, stream_id, command, payload).map_err(|e| {
        // Keep `out` reusable on error: only the WS header (and key) were
        // written before the MUX payload, so truncate back to empty.
        out.clear();
        anyhow!(e.to_string())
    })?;
    debug_assert_eq!(out.len() - mux_start, mux_len);
    cipher.apply(&mut out[mux_start..]);
    if let Some(key) = mask_key {
        for (i, byte) in out[mux_start..].iter_mut().enumerate() {
            *byte ^= key[i & 3];
        }
    }
    Ok(())
}

async fn writer_loop<W>(mut w: W, mut rx: mpsc::Receiver<Vec<u8>>)
where
    W: AsyncWrite + Unpin + Send + 'static,
{
    let mut batch: Vec<Vec<u8>> = Vec::with_capacity(BATCH_MAX_FRAMES);
    loop {
        batch.clear();
        // Block for the first frame; channel close means all owners gone.
        let Some(first) = rx.recv().await else {
            break;
        };
        let mut bytes = first.len();
        batch.push(first);
        // Drain whatever else is already queued (coalescing window).
        while batch.len() < BATCH_MAX_FRAMES && bytes < BATCH_MAX_BYTES {
            match rx.try_recv() {
                Ok(frame) => {
                    bytes += frame.len();
                    batch.push(frame);
                }
                Err(_) => break,
            }
        }
        if write_batch(&mut w, &batch).await.is_err() {
            break;
        }
    }
    let _ = w.shutdown().await;
}

async fn write_batch<W>(w: &mut W, batch: &[Vec<u8>]) -> std::io::Result<()>
where
    W: AsyncWrite + Unpin,
{
    if batch.len() == 1 {
        w.write_all(&batch[0]).await?;
        return Ok(());
    }
    let mut idx = 0usize;
    let mut off = 0usize;
    while idx < batch.len() {
        let n = poll_fn(|cx| {
            let mut slices = Vec::with_capacity(batch.len() - idx);
            slices.push(IoSlice::new(&batch[idx][off..]));
            for buf in &batch[idx + 1..] {
                slices.push(IoSlice::new(buf));
            }
            Pin::new(&mut *w).poll_write_vectored(cx, &slices)
        })
        .await?;
        if n == 0 {
            return Err(std::io::Error::new(
                std::io::ErrorKind::WriteZero,
                "vectored write returned zero",
            ));
        }
        let mut remaining = n;
        while remaining > 0 {
            let available = batch[idx].len() - off;
            if remaining < available {
                off += remaining;
                remaining = 0;
            } else {
                remaining -= available;
                idx += 1;
                off = 0;
            }
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::protocol::MuxFrame;
    use crate::ws::{encode_ws_frame, read_frame};
    use tokio::io::{duplex, AsyncWriteExt};

    #[tokio::test]
    async fn fused_encoding_matches_two_step_path() {
        let cipher = XorCipher::new("fuse-key");
        for (command, payload_len) in [
            (MuxCommand::Syn, 0),
            (MuxCommand::Data, 7),
            (MuxCommand::Data, 60000),
            (MuxCommand::Fin, 0),
        ] {
            let payload = vec![0xABu8; payload_len];
            for masked in [false, true] {
                // Fused single-buffer path.
                let mut fused = Vec::new();
                encode_mux_ws_frame(&mut fused, 0x01020304, command, &payload, &cipher, masked)
                    .unwrap();
                // Lengths must match the legacy path exactly (mask keys
                // are random per frame, so bytes themselves differ).
                let mut mux = Vec::new();
                write_frame_parts(&mut mux, 0x01020304, command, &payload).unwrap();
                cipher.apply(&mut mux);
                let legacy = encode_ws_frame(&mux, 2, masked).unwrap();
                assert_eq!(
                    fused.len(),
                    legacy.len(),
                    "length mismatch for {command:?}/{masked}"
                );
                // Round-trip: WS decode, XOR off, MUX decode.
                let (mut a, mut b) = duplex(128 * 1024);
                a.write_all(&fused).await.unwrap();
                drop(a);
                let mut buf = Vec::new();
                let (opcode, mut got) =
                    read_frame(&mut b, Option::<&mut tokio::io::DuplexStream>::None, &mut buf)
                        .await
                        .unwrap()
                        .unwrap();
                assert_eq!(opcode, 2);
                cipher.apply(&mut got);
                let frame = MuxFrame::decode(&got).unwrap();
                assert_eq!(frame.stream_id, 0x01020304);
                assert_eq!(frame.command, command);
                assert_eq!(frame.payload, payload);
            }
        }
    }

    #[tokio::test]
    async fn writer_task_exits_when_handles_dropped() {
        let (a, _b) = duplex(1024 * 1024);
        let (writer, handle) = MuxFrameWriter::spawn(a);
        drop(writer);
        tokio::time::timeout(std::time::Duration::from_secs(5), handle)
            .await
            .expect("writer task must exit after last handle dropped")
            .unwrap();
    }

    #[tokio::test]
    async fn preserves_order_across_coalesced_batch() {
        let (a, mut b) = duplex(1024 * 1024);
        let (writer, handle) = MuxFrameWriter::spawn(a);
        // Flood the queue so the loop is forced to coalesce.
        for i in 0u8..64 {
            let payload = vec![i; 1024];
            writer
                .send(encode_ws_frame(&payload, 2, false).unwrap())
                .await
                .unwrap();
        }
        drop(writer);
        let mut buf = Vec::new();
        for i in 0u8..64 {
            let (opcode, payload) =
                read_frame(&mut b, Option::<&mut tokio::io::DuplexStream>::None, &mut buf)
                    .await
                    .unwrap()
                    .unwrap();
            assert_eq!(opcode, 2);
            assert_eq!(payload, vec![i; 1024]);
        }
        handle.await.unwrap();
    }

    #[tokio::test]
    async fn send_fails_after_transport_death() {
        let (a, b) = duplex(64);
        let (writer, handle) = MuxFrameWriter::spawn(a);
        drop(b);
        // Small queue + dead peer: sends must eventually fail, never hang.
        let mut failed = false;
        for _ in 0..(WRITER_QUEUE + 8) {
            let payload = vec![9u8; 1024];
            if writer
                .send(encode_ws_frame(&payload, 2, false).unwrap())
                .await
                .is_err()
            {
                failed = true;
                break;
            }
        }
        assert!(failed, "writer must report transport death");
        assert!(writer.is_closed());
        handle.await.unwrap();
    }
}
