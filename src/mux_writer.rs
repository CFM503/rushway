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
use std::collections::{HashMap, VecDeque};
use std::future::poll_fn;
use std::io::IoSlice;
use std::pin::Pin;
use std::sync::{
    atomic::{AtomicBool, Ordering},
    Arc, Mutex as StdMutex, OnceLock,
};
use tokio::io::{AsyncWrite, AsyncWriteExt};
use tokio::sync::mpsc;
use tokio::task::JoinHandle;

const WRITER_QUEUE: usize = 256;
const FEED_CAP: usize = 256;
const BATCH_MAX_FRAMES: usize = 32;
const BATCH_MAX_BYTES: usize = 1024 * 1024;
/// Encode-buffer pool: outbound frames are pre-encoded into an owned `Vec`
/// that the writer task only reads, then drops. Recycling those buffers
/// after a successful write removes the per-frame malloc/free that showed
/// up as `realloc`/`grow_amortized` on the encode hot path.
///
/// Kept deliberately small: one batch is at most [`BATCH_MAX_FRAMES`], and
/// a larger pool showed up as a clear setup-mode RSS regression (8:0) in
/// the first A/B — 32 × ~80 KiB ≈ 2.5 MiB retained ceiling.
const ENCODE_POOL_MAX_COUNT: usize = 32;
/// Frames top out near 67 KiB + WS header + obfs pad; pool only up to
/// ~80 KiB so a rare huge frame cannot pin memory.
const ENCODE_POOL_MAX_CAP: usize = 80 * 1024;
/// Per-stream byte credit added each deficit round (GoWay parity).
/// Set to 128KB so that even maximum-size MUX frames (~67KB) can be
/// dispatched in a single round without deficit underflow stall.
const DRR_QUANTUM: usize = 128 * 1024;
/// Cap accumulated credit so a long-idle stream cannot hog the link.
const DRR_MAX_DEFICIT: usize = 512 * 1024;

static ENCODE_POOL: OnceLock<StdMutex<Vec<Vec<u8>>>> = OnceLock::new();

fn encode_pool() -> &'static StdMutex<Vec<Vec<u8>>> {
    ENCODE_POOL.get_or_init(|| StdMutex::new(Vec::new()))
}

/// Takes a pooled encode buffer (cleared, capacity retained) or a fresh
/// empty `Vec` when the pool is empty. The writer returns written frames
/// via [`recycle_encode_bufs`].
pub(crate) fn acquire_encode_buf() -> Vec<u8> {
    let mut pool = encode_pool()
        .lock()
        .unwrap_or_else(|e| e.into_inner());
    match pool.pop() {
        Some(mut buf) => {
            buf.clear();
            buf
        }
        None => Vec::new(),
    }
}

/// Returns a post-write frame buffer to the pool (best-effort: zero-cap or
/// oversized buffers are dropped).
#[allow(dead_code)]
pub(crate) fn recycle_encode_buf(mut buf: Vec<u8>) {
    if buf.capacity() == 0 || buf.capacity() > ENCODE_POOL_MAX_CAP {
        return;
    }
    buf.clear();
    let mut pool = encode_pool()
        .lock()
        .unwrap_or_else(|e| e.into_inner());
    if pool.len() < ENCODE_POOL_MAX_COUNT {
        pool.push(buf);
    }
}

/// Returns a batch of post-write frame buffers to the pool under a single
/// mutex acquisition, with zero allocation.
pub(crate) fn recycle_encode_bufs<I>(bufs: I)
where
    I: IntoIterator<Item = Vec<u8>>,
{
    let mut pool = encode_pool()
        .lock()
        .unwrap_or_else(|e| e.into_inner());
    for mut buf in bufs {
        if buf.capacity() == 0 || buf.capacity() > ENCODE_POOL_MAX_CAP {
            continue;
        }
        if pool.len() < ENCODE_POOL_MAX_COUNT {
            buf.clear();
            pool.push(buf);
        } else {
            break;
        }
    }
}

/// One pre-encoded outbound frame with scheduling identity.
///
/// `priority` frames (WS handshake hellos/pings via [`MuxFrameWriter::send`]
/// and non-DATA MUX commands via [`MuxFrameWriter::send_mux`]) drain from a
/// dedicated lane ahead of bulk DATA; DATA frames are deficit-round-robin
/// scheduled per `stream_id` (GoWay `muxOutboundWriter` parity).
///
/// `stream_id` is `Some` for MUX control frames (SYN/FIN/RST) so the
/// scheduler can keep a stream-closing control behind its own queued DATA.
/// `yield_to_data` is set ONLY for FIN/RST: a SYN must always lead (it
/// creates the server-side stream — yielding it behind its own DATA makes
/// the peer drop that DATA as unknown-stream, a live-caught burst bug).
/// System frames (hello/ping) carry `None` and always jump the queue.
pub(crate) struct OutboundFrame {
    stream_id: Option<u32>,
    priority: bool,
    yield_to_data: bool,
    bytes: Vec<u8>,
}

pub(crate) struct MuxFrameWriter {
    tx: mpsc::Sender<OutboundFrame>,
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
        let (tx, rx) = mpsc::channel(FEED_CAP);
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

    /// Queues one pre-encoded system frame (WS handshake hello, heartbeat
    /// ping). System frames take the priority lane, ahead of MUX DATA.
    /// Fails fast once the transport died; otherwise back-pressures when
    /// the queue is full.
    pub async fn send(&self, frame: Vec<u8>) -> Result<()> {
        self.send_frame(OutboundFrame {
            stream_id: None,
            priority: true,
            yield_to_data: false,
            bytes: frame,
        })
        .await
    }

    /// Queues one pre-encoded MUX frame. Non-DATA commands take the priority
    /// lane; DATA frames are DRR-scheduled per stream. Same failure and
    /// backpressure contract as [`MuxFrameWriter::send`].
    pub async fn send_mux(
        &self,
        stream_id: u32,
        command: MuxCommand,
        frame: Vec<u8>,
    ) -> Result<()> {
        // Only stream-CLOSING controls yield to their own queued DATA.
        // SYN must lead (it creates the peer-side stream); DATA never
        // takes the priority lane at all.
        let yield_to_data = command == MuxCommand::Fin || command == MuxCommand::Rst;
        self.send_frame(OutboundFrame {
            stream_id: Some(stream_id),
            priority: command != MuxCommand::Data,
            yield_to_data,
            bytes: frame,
        })
        .await
    }

    async fn send_frame(&self, frame: OutboundFrame) -> Result<()> {
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

struct StreamQueue {
    frames: VecDeque<Vec<u8>>,
    deficit: usize,
}

/// Deficit-round-robin scheduler over one MUX session's outbound frames.
///
/// Mirrors GoWay's `muxOutboundWriter`: a priority lane (system frames plus
/// SYN/FIN/RST) drains first; DATA frames rotate strictly across streams
/// with a 64KB quantum each. Per-stream FIFOs preserve in-order delivery;
/// queue depth is bounded at [`WRITER_QUEUE`] frames total (priority + all
/// streams), preserving the old backpressure contract.
struct Scheduler {
    priority: VecDeque<(Option<u32>, bool, Vec<u8>)>,
    streams: HashMap<u32, StreamQueue>,
    rotation: Vec<u32>,
    pos: usize,
    total: usize,
}

impl Scheduler {
    fn new() -> Self {
        Self {
            priority: VecDeque::new(),
            streams: HashMap::new(),
            rotation: Vec::new(),
            pos: 0,
            total: 0,
        }
    }

    fn len(&self) -> usize {
        self.total
    }

    fn is_empty(&self) -> bool {
        self.total == 0
    }

    fn push(&mut self, frame: OutboundFrame) {
        // Termination guard: the deficit caps at DRR_MAX_DEFICIT, so an
        // oversized frame could never afford emission and would wedge the
        // stream. MUX frames top out near 67KB — far below the cap.
        debug_assert!(frame.bytes.len() <= DRR_MAX_DEFICIT);
        if frame.priority {
            self.priority
                .push_back((frame.stream_id, frame.yield_to_data, frame.bytes));
            self.total += 1;
            return;
        }
        let Some(stream_id) = frame.stream_id else {
            // Unreachable: only send_mux (always Some) reaches here with
            // priority == false. Guard anyway to never strand a frame.
            self.priority.push_back((None, false, frame.bytes));
            self.total += 1;
            return;
        };
        let q = self.streams.entry(stream_id).or_insert_with(|| {
            self.rotation.push(stream_id);
            StreamQueue {
                frames: VecDeque::new(),
                deficit: 0,
            }
        });
        q.frames.push_back(frame.bytes);
        self.total += 1;
    }

    /// Pops the next frame to emit: priority lane first, then one DRR step.
    ///
    /// Intra-stream order wins over cross-stream priority, but ONLY for
    /// stream-closing controls (FIN/RST): those yield while their own
    /// stream still has queued DATA (it stays queued), otherwise the peer
    /// would close early and drop the tail. SYN never yields — it creates
    /// the peer-side stream, so yielding it makes the peer drop its own
    /// DATA as unknown-stream. System frames (`None`) and controls for
    /// streams with empty queues jump immediately.
    fn next(&mut self) -> Option<Vec<u8>> {
        if let Some(&(sid, yield_to_data, _)) = self.priority.front() {
            let blocked = yield_to_data
                && sid
                    .is_some_and(|id| self.streams.get(&id).is_some_and(|q| !q.frames.is_empty()));
            if !blocked {
                let (sid_opt, yield_flag, frame) = self.priority.pop_front().unwrap();
                self.total -= 1;
                if yield_flag {
                    if let Some(id) = sid_opt {
                        self.remove_stream(&id);
                    }
                }
                return Some(frame);
            }
        }
        if self.total == 0 {
            if !self.streams.is_empty() {
                self.streams.clear();
                self.rotation.clear();
                self.pos = 0;
            }
            return None;
        }
        if self.rotation.is_empty() {
            return None;
        }
        // Strict rotation: every stream gets +quantum per round and the
        // first affordable head-of-queue wins. Deficits persist across
        // calls, so no stream starves; unaffordable rounds simply emit
        // nothing until credit accumulates.
        let mut checked = 0;
        while checked < self.rotation.len() {
            if self.pos >= self.rotation.len() {
                self.pos = 0;
            }
            let sid = self.rotation[self.pos];
            self.pos += 1;
            checked += 1;
            let Some(q) = self.streams.get_mut(&sid) else {
                continue;
            };
            if q.frames.is_empty() {
                continue;
            }
            q.deficit = (q.deficit + DRR_QUANTUM).min(DRR_MAX_DEFICIT);
            let affordable = q.frames.front().map(|f| f.len()).unwrap_or(usize::MAX);
            if affordable <= q.deficit {
                q.deficit -= affordable;
                let frame = q.frames.pop_front();
                self.total -= 1;
                if frame.is_some() {
                    return frame;
                }
            }
        }
        None
    }

    fn remove_stream(&mut self, sid: &u32) {
        self.streams.remove(sid);
        if let Some(idx) = self.rotation.iter().position(|s| s == sid) {
            self.rotation.remove(idx);
            if self.pos > idx {
                self.pos -= 1;
            }
            if self.pos >= self.rotation.len() && !self.rotation.is_empty() {
                self.pos = 0;
            }
        }
    }
}

/// Max random padding appended to MUX DATA frames when obfuscation is on
/// (GoWay `-obfs` parity: same 1400-byte cap).
pub(crate) const OBFS_PAD_MAX: usize = 1400;

/// Encodes one MUX frame already wrapped in its WebSocket frame, in a
/// single buffer with a single allocation: `[WS header][mask?][MUX header |
/// payload][pad?]`.
///
/// Previously this took two allocations and a full extra copy (MUX `Vec`
/// then WS `Vec`). Layout: the WS header describes the MUX frame length
/// (`7 + payload.len()`, plus obfs pad when enabled), and the
/// cipher + WS mask (client side) cover the whole region including pad.
///
/// When `obfs` is set, DATA frames carry `[0, OBFS_PAD_MAX]` random pad
/// bytes inside the WS payload past the declared MUX length. Receivers
/// (old and new, Go and Rust) slice by the declared length, so padding is
/// transparently ignored — unilateral deployment is safe.
pub(crate) fn encode_mux_ws_frame(
    out: &mut Vec<u8>,
    stream_id: u32,
    command: MuxCommand,
    payload: &[u8],
    cipher: &XorCipher,
    masked: bool,
    obfs: bool,
) -> Result<()> {
    let mux_len = 7 + payload.len();
    // Unilateral obfuscation: random pad past the declared MUX length.
    // Computed up-front so the WS header covers it — otherwise the peer
    // would leave pad bytes in the stream and desync the next frame.
    // Receivers slice by the declared MUX length, so the tail is ignored
    // by old and new peers alike. DATA only — control frames stay exact.
    use rand::RngCore;
    let pad_len = if obfs && command == MuxCommand::Data {
        (rand::thread_rng().next_u32() as usize) % (OBFS_PAD_MAX + 1)
    } else {
        0
    };
    let total = mux_len + pad_len;
    let mut ws_header = [0u8; 14];
    let ws_header_len = ws_header_into(&mut ws_header, total, 0x2, masked);
    let mask_len = if masked { 4 } else { 0 };
    out.clear();
    out.reserve(ws_header_len + mask_len + total);
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
    // Pad lands inside the WS payload and gets masked/ciphered with the
    // rest; the receiver slices by the declared MUX length and ignores it.
    if pad_len > 0 {
        let start = out.len();
        out.resize(start + pad_len, 0);
        rand::thread_rng().fill_bytes(&mut out[start..]);
    }
    // Single pass over the payload: cipher keystream XOR WS mask.
    // Offsets are region-relative (key byte `i` maps to
    // `keystream[i % XOR_KEY_SIZE]`), exactly matching two sequential
    // `TransformInPlace` + mask passes. Mask period 4 divides the 8-byte
    // word, so bulk stays word-at-a-time.
    let region = &mut out[mux_start..];
    match mask_key {
        None => cipher.apply(region),
        Some(key) => {
            let ks = cipher.keystream();
            let mut mask32 = [0u8; 32];
            for i in 0..8 {
                mask32[i * 4..i * 4 + 4].copy_from_slice(&key);
            }
            if ks.is_empty() {
                let (chunks32, tail32) = region.as_chunks_mut::<32>();
                let chunks_len = chunks32.len();
                for chunk in chunks32 {
                    for b in 0..32 {
                        chunk[b] ^= mask32[b];
                    }
                }
                let start = chunks_len * 32;
                for (j, byte) in tail32.iter_mut().enumerate() {
                    *byte ^= key[(start + j) & 3];
                }
            } else {
                let clen = region.len().min(ks.len());
                let (dst_chunks, dst_tail) = region[..clen].as_chunks_mut::<32>();
                let (ks_chunks, _) = ks[..clen].as_chunks::<32>();
                let chunks_len = dst_chunks.len();
                for (d, k) in dst_chunks.iter_mut().zip(ks_chunks) {
                    for b in 0..32 {
                        d[b] ^= k[b] ^ mask32[b];
                    }
                }
                let start = chunks_len * 32;
                for (j, byte) in dst_tail.iter_mut().enumerate() {
                    *byte ^= ks[start + j] ^ key[(start + j) & 3];
                }
            }
        }
    }
    Ok(())
}

async fn writer_loop<W>(mut w: W, mut rx: mpsc::Receiver<OutboundFrame>)
where
    W: AsyncWrite + Unpin + Send + 'static,
{
    let mut sched = Scheduler::new();
    let mut batch: Vec<Vec<u8>> = Vec::with_capacity(BATCH_MAX_FRAMES);
    loop {
        // Admit: block for the first frame when the scheduler is empty
        // (channel close here means all owners gone); otherwise drain the
        // feed opportunistically while under the total queue cap. A full
        // scheduler/feed makes senders block, preserving backpressure.
        if sched.is_empty() {
            match rx.recv().await {
                Some(frame) => sched.push(frame),
                None => break,
            }
        }
        while sched.len() < WRITER_QUEUE {
            match rx.try_recv() {
                Ok(frame) => sched.push(frame),
                Err(_) => break,
            }
        }
        // Emit one DRR-ordered batch (priority lane first, then one frame
        // per affordable stream); vectored write preserved.
        //
        // A `None` here only means "nothing affordable THIS round" — each
        // next() call adds a quantum to every stream, so retrying while the
        // scheduler is non-empty always reaches an emission within a few
        // rounds (frames are far below the deficit cap). Breaking at the
        // first None instead would strand the tail whenever no further
        // arrivals come to wake us (caught live: 4MiB tail stall).
        // `batch` is drained (not cleared) after each write so frame buffers
        // return to the encode pool instead of being freed.
        debug_assert!(batch.is_empty());
        let mut bytes = 0usize;
        while batch.len() < BATCH_MAX_FRAMES && bytes < BATCH_MAX_BYTES {
            match sched.next() {
                Some(frame) => {
                    bytes += frame.len();
                    batch.push(frame);
                }
                None if sched.is_empty() => break,
                None => continue,
            }
        }
        if batch.is_empty() {
            // Scheduler non-empty but nothing affordable (DRR credit still
            // accumulating) and feed drained: park until a new frame
            // arrives or the channel closes. recv() also returns frames
            // already sitting in the feed, so this cannot deadlock.
            // Deficits persist across rounds — no stream starves.
            match rx.recv().await {
                Some(frame) => {
                    sched.push(frame);
                    continue;
                }
                // Feed closed: drain everything still scheduled before
                // exiting — abandoning queued frames here would silently
                // drop tails (caught live: 56 stranded frames at close).
                // Terminates: every next() either emits (total--) or
                // accrues deficit, and frames are far below the deficit
                // cap, so emission is always reached within a few calls.
                // The iteration bound is pure paranoia against a hang at
                // teardown (a hang there is worse than a drop).
                None => {
                    let mut spins = 0usize;
                    let max_spins = sched.len() * 8 + 32;
                    while !sched.is_empty() && spins < max_spins {
                        spins += 1;
                        if let Some(frame) = sched.next() {
                            batch.push(frame);
                            if batch.len() >= BATCH_MAX_FRAMES {
                                let _ = write_batch(&mut w, &batch).await;
                                recycle_encode_bufs(batch.drain(..));
                            }
                        }
                    }
                    if !batch.is_empty() {
                        let _ = write_batch(&mut w, &batch).await;
                        recycle_encode_bufs(batch.drain(..));
                    }
                    break;
                }
            }
        }
        if write_batch(&mut w, &batch).await.is_err() {
            recycle_encode_bufs(batch.drain(..));
            break;
        }
        recycle_encode_bufs(batch.drain(..));
    }
    // Shutdown drain path already wrote leftovers above; recycle anything
    // still sitting in `batch` from a partial fill that never wrote.
    recycle_encode_bufs(batch.drain(..));
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
            const MAX: usize = 64;
            let count = (batch.len() - idx).min(MAX);
            let mut slices: [IoSlice<'_>; MAX] = [IoSlice::new(&[]); MAX];
            slices[0] = IoSlice::new(&batch[idx][off..]);
            for (i, buf) in batch[idx + 1..idx + count].iter().enumerate() {
                slices[i + 1] = IoSlice::new(buf);
            }
            Pin::new(&mut *w).poll_write_vectored(cx, &slices[..count])
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
                // Fused single-buffer path (obfs off: exact lengths).
                let mut fused = Vec::new();
                encode_mux_ws_frame(
                    &mut fused, 0x01020304, command, &payload, &cipher, masked, false,
                )
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
                let (opcode, mut got) = read_frame(
                    &mut b,
                    Option::<&mut tokio::io::DuplexStream>::None,
                    &mut buf,
                )
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
    async fn obfs_pads_data_only_within_cap() {
        let cipher = XorCipher::new("obfs-key");
        let payload = vec![0xCDu8; 100];
        // Control frames are never padded.
        for command in [MuxCommand::Syn, MuxCommand::Fin, MuxCommand::Rst] {
            let mut out = Vec::new();
            encode_mux_ws_frame(&mut out, 1, command, &payload, &cipher, true, true).unwrap();
            assert_eq!(out.len(), mux_ws_len(100, true), "control must stay exact");
        }
        // DATA frames gain [0, OBFS_PAD_MAX] random tail past the declared
        // length; the declared prefix always decodes.
        let mut seen_pad = false;
        for _ in 0..16 {
            let mut out = Vec::new();
            encode_mux_ws_frame(&mut out, 9, MuxCommand::Data, &payload, &cipher, true, true)
                .unwrap();
            let base = mux_ws_len(100, true);
            assert!((base..=base + OBFS_PAD_MAX).contains(&out.len()));
            if out.len() > base {
                seen_pad = true;
            }
            // Strip the WS layer using the header's own length field
            // (pad is inside the WS payload now): unmask, decrypt, then
            // verify the MUX header still declares exactly 100 bytes.
            assert_eq!(out[0] & 0x0F, 0x2);
            let (ws_len, header_len) = match out[1] & 0x7F {
                126 => (
                    u16::from_be_bytes(out[2..4].try_into().unwrap()) as usize,
                    4,
                ),
                127 => (
                    u64::from_be_bytes(out[2..10].try_into().unwrap()) as usize,
                    10,
                ),
                n => (n as usize, 2),
            };
            assert!((107..=107 + OBFS_PAD_MAX).contains(&ws_len));
            let mask: [u8; 4] = out[header_len..header_len + 4].try_into().unwrap();
            let mut mux_frame = out[header_len + 4..header_len + 4 + ws_len].to_vec();
            for (i, byte) in mux_frame.iter_mut().enumerate() {
                *byte ^= mask[i & 3];
            }
            cipher.apply(&mut mux_frame);
            let frame = MuxFrame::decode(&mux_frame).unwrap();
            assert_eq!(frame.stream_id, 9);
            assert_eq!(frame.payload, payload);
        }
        assert!(seen_pad, "16 padded frames all came out unpadded (p ~= 0)");
    }

    /// Wire length of an unpadded MUX+WS frame (mirrors the encoder).
    fn mux_ws_len(payload_len: usize, masked: bool) -> usize {
        let mux_len = 7 + payload_len;
        let ws_header_len = if mux_len <= 125 {
            2
        } else if mux_len <= u16::MAX as usize {
            4
        } else {
            10
        };
        ws_header_len + if masked { 4 } else { 0 } + mux_len
    }

    #[test]
    fn encode_pool_reuses_capacity() {
        let mut a = acquire_encode_buf();
        a.reserve(64 * 1024);
        a.extend_from_slice(&[0xAB; 64 * 1024]);
        let cap = a.capacity();
        recycle_encode_buf(a);
        let b = acquire_encode_buf();
        assert_eq!(b.len(), 0, "pooled buf must be cleared");
        assert!(
            b.capacity() >= cap,
            "pooled buf must retain capacity (got {} want >= {})",
            b.capacity(),
            cap
        );
        // Oversized buffers are not retained.
        let mut huge = Vec::with_capacity(ENCODE_POOL_MAX_CAP + 1);
        huge.extend_from_slice(&[1u8; ENCODE_POOL_MAX_CAP + 1]);
        recycle_encode_buf(huge);
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
            let (opcode, payload) = read_frame(
                &mut b,
                Option::<&mut tokio::io::DuplexStream>::None,
                &mut buf,
            )
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

    fn drr_frame(stream_id: u32, priority: bool, len: usize, tag: u8) -> OutboundFrame {
        drr_frame_yield(stream_id, priority, priority, len, tag)
    }

    /// priority + yield_to_data set independently (mirrors send_mux: only
    /// FIN/RST yield; SYN never does even though it is priority).
    fn drr_frame_yield(
        stream_id: u32,
        priority: bool,
        yield_to_data: bool,
        len: usize,
        tag: u8,
    ) -> OutboundFrame {
        let mut bytes = vec![tag; len];
        if !bytes.is_empty() {
            bytes[0] = tag;
        }
        OutboundFrame {
            stream_id: Some(stream_id),
            priority,
            yield_to_data,
            bytes,
        }
    }

    fn drr_system(len: usize, tag: u8) -> OutboundFrame {
        OutboundFrame {
            stream_id: None,
            priority: true,
            yield_to_data: false,
            bytes: vec![tag; len],
        }
    }

    #[test]
    fn drr_interactive_not_starved_by_bulk() {
        // 4 bulk streams queue 8 x 64KB DATA each BEFORE the interactive
        // stream arrives. Without fairness the interactive frames would sit
        // at positions 32..35; DRR must interleave them from round one.
        let mut sched = Scheduler::new();
        for sid in 1..=4u32 {
            for _ in 0..8 {
                sched.push(drr_frame(sid, false, 65536, sid as u8));
            }
        }
        for _ in 0..4 {
            sched.push(drr_frame(9, false, 100, 9));
        }
        assert_eq!(sched.len(), 36);
        // Drain-until-empty: a `None` only means "nothing affordable this
        // round" (credit still accumulating), never "done".
        let mut order = Vec::new();
        while !sched.is_empty() {
            if let Some(frame) = sched.next() {
                order.push(frame[0]);
            }
        }
        assert_eq!(order.len(), 36);
        // Strict rotation over insertion order 1,2,3,4,9: each stream sends
        // one frame per round, so stream 9 first appears at index 4.
        let first_interactive = order.iter().position(|&t| t == 9).unwrap();
        assert_eq!(first_interactive, 4);
        // Per-stream FIFO: bulk tags appear in stream rounds, interactive
        // frames spread across rounds (never clumped at the tail).
        let last_interactive = order.iter().rposition(|&t| t == 9).unwrap();
        assert!(
            last_interactive < 20,
            "interactive tail at {last_interactive}"
        );
        for sid in 1..=4u32 {
            assert_eq!(order.iter().filter(|&&t| t == sid as u8).count(), 8);
        }
    }

    #[test]
    fn drr_control_never_overtakes_own_data() {
        // Regression: a FIN must not jump ahead of its own stream's queued
        // DATA (peer would close early and drop the tail), but it still
        // jumps ahead of OTHER streams' bulk. System frames always go first
        // among ready controls.
        let mut sched = Scheduler::new();
        sched.push(drr_system(2, 3)); // ping, queued first
        sched.push(drr_frame(5, false, 65536, 5)); // other stream's bulk
        sched.push(drr_frame(1, false, 100, 1)); // own DATA
        sched.push(drr_frame(1, true, 7, 2)); // own FIN
        let mut order = Vec::new();
        while !sched.is_empty() {
            if let Some(frame) = sched.next() {
                order.push(frame[0]);
            }
        }
        // Ping first; the FIN yields to DRR while its own DATA is queued
        // (stream 5's bulk wins that round), but it still precedes nothing
        // of its own stream: D1 always comes before FIN. Intra-stream order
        // is what correctness needs; cross-stream order stays fair.
        assert_eq!(order, vec![3, 5, 1, 2]);
    }

    #[test]
    fn drr_syn_never_yields_to_own_data() {
        // SYN creates the peer-side stream: it must lead even when its own
        // DATA is already queued. Yielding it (like FIN/RST) makes the peer
        // drop that DATA as unknown-stream — live-caught with full 64KB
        // flows vanishing under burst while zero probes fired.
        let mut sched = Scheduler::new();
        sched.push(drr_frame(1, false, 100, 1));
        sched.push(drr_frame_yield(1, true, false, 7, 2));
        sched.push(drr_frame(1, false, 100, 1));
        let mut order = Vec::new();
        while !sched.is_empty() {
            if let Some(frame) = sched.next() {
                order.push(frame[0]);
            }
        }
        assert_eq!(order, vec![2, 1, 1]);
    }

    /// Regression: 8 bulk streams racing one writer must deliver every
    /// frame, including the close-time drain (a prior revision stranded
    /// queued frames when the feed closed, and an earlier one wedged the
    /// tail when no further arrivals came).
    #[tokio::test]
    async fn concurrent_bulk_all_frames_delivered() {
        use crate::crypto::XorCipher;
        use crate::ws::read_frame;
        use tokio::io::duplex;
        let (a, mut b) = duplex(256 * 1024 * 1024);
        let (writer, handle) = MuxFrameWriter::spawn(a);
        let cipher = XorCipher::new("repro");
        let mut tasks = Vec::new();
        for sid in 1..=8u32 {
            let w = writer.clone();
            let c = cipher.clone();
            tasks.push(tokio::spawn(async move {
                for _ in 0..70 {
                    let mut enc = Vec::new();
                    encode_mux_ws_frame(
                        &mut enc,
                        sid,
                        MuxCommand::Data,
                        &[sid as u8; 65535],
                        &c,
                        true,
                        false,
                    )
                    .unwrap();
                    w.send_mux(sid, MuxCommand::Data, enc).await.unwrap();
                }
                let mut enc = Vec::new();
                encode_mux_ws_frame(&mut enc, sid, MuxCommand::Fin, &[], &c, true, false).unwrap();
                w.send_mux(sid, MuxCommand::Fin, enc).await.unwrap();
            }));
        }
        drop(writer);
        let reader = tokio::spawn(async move {
            let mut buf = Vec::new();
            let mut count = 0usize;
            while count < 8 * 71 {
                let (op, mut payload) = read_frame(
                    &mut b,
                    Option::<&mut tokio::io::DuplexStream>::None,
                    &mut buf,
                )
                .await
                .unwrap()
                .unwrap();
                assert_eq!(op, 2);
                cipher.apply(&mut payload);
                let _ = MuxFrame::decode(&payload).unwrap();
                count += 1;
            }
            count
        });
        let (_senders, received) = tokio::join!(
            async {
                for t in tasks {
                    t.await.unwrap();
                }
            },
            tokio::time::timeout(std::time::Duration::from_secs(30), reader),
        );
        let n = received.expect("writer stalled: reader timed out").unwrap();
        assert_eq!(n, 8 * 71);
        handle.await.unwrap();
    }

    #[tokio::test]
    async fn send_mux_flows_through_scheduler_live_task() {
        use crate::crypto::XorCipher;
        use crate::ws::read_frame;
        use tokio::io::duplex;
        let (a, mut b) = duplex(1024 * 1024);
        let (writer, handle) = MuxFrameWriter::spawn(a);
        let cipher = XorCipher::new("probe");
        let mut enc = Vec::new();
        encode_mux_ws_frame(
            &mut enc,
            5,
            MuxCommand::Data,
            &[7u8; 100],
            &cipher,
            true,
            false,
        )
        .unwrap();
        writer.send_mux(5, MuxCommand::Data, enc).await.unwrap();
        drop(writer);
        let mut buf = Vec::new();
        let res = tokio::time::timeout(
            std::time::Duration::from_secs(5),
            read_frame(
                &mut b,
                Option::<&mut tokio::io::DuplexStream>::None,
                &mut buf,
            ),
        )
        .await
        .expect("timed out waiting for send_mux frame - TASK STUCK");
        let (opcode, mut payload) = res.unwrap().unwrap();
        assert_eq!(opcode, 2);
        cipher.apply(&mut payload);
        let frame = MuxFrame::decode(&payload).unwrap();
        assert_eq!(frame.stream_id, 5);
        assert_eq!(frame.payload, vec![7u8; 100]);
        handle.await.unwrap();
    }

    #[test]
    fn drr_deficit_accumulates_across_rounds() {
        // A 200KB frame needs 4 quanta (64KB x 3 = 192KB < 200KB): streams
        // 2 and 3 emit first while stream 1 accrues 64/128/192KB over
        // "empty" rounds, then 256KB (capped) lets it through.
        let mut sched = Scheduler::new();
        sched.push(drr_frame(1, false, 200 * 1024, 1));
        sched.push(drr_frame(2, false, 65536, 2));
        sched.push(drr_frame(3, false, 65536, 3));
        let mut order = Vec::new();
        while !sched.is_empty() {
            if let Some(frame) = sched.next() {
                order.push(frame[0]);
            }
        }
        assert_eq!(order, vec![2, 3, 1]);
    }
}
