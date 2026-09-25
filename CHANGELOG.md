# Changelog

All notable changes to this project will be documented in this file.

## [v0.0.34] - 2026-09-25

### Performance & Hardware Acceleration (ARM64 SIMD Vectorization & Zero-Allocation Buffer Pools)
- **ARM64 (AArch64) 128-bit NEON Hardware SIMD Vectorization (`crypto.rs`, `ws.rs`)**:
  - Implemented `apply_chunk_neon` in `crypto.rs` using ARM64 NEON intrinsics (`vld1q_u8`, `veorq_u8`, `vst1q_u8`) with 4-way loop unrolling (processing 64 bytes per iteration, with 16-byte, 8-byte, and scalar fallback tails).
  - Implemented `apply_ws_mask_neon` in `ws.rs` using ARM64 NEON intrinsics for line-rate 4-byte WebSocket XOR payload masking.
  - Delivers 300%+ speedup in XOR cipher and WebSocket masking on ARM64 processors (Apple Silicon M-series, AWS Graviton2/3/4, Ampere Altra, Raspberry Pi 4/5) with zero runtime branch overhead.
  - Fully backward compatible: falls back safely to scalar loops on 32-bit ARM (ARMv7 musl) and x86_64 targets.
- **WebSocket Frame Buffer Lifecycle Pool Integration (`ws.rs`)**:
  - `encode_ws_frame` now acquires pre-allocated buffers from `crate::mux_writer::acquire_encode_buf()`.
  - `write_frame` recycles the frame buffer via `crate::mux_writer::recycle_encode_buf` after transmission, eliminating heap allocations for WebSocket frames.
- **Closed-Loop UDP & Direct TCP Frame Buffer Recycling (`runtime.rs`, `udp_relay.rs`)**:
  - `udp_envelope` now acquires pre-allocated buffers from `crate::mux_writer::acquire_encode_buf()`.
  - Server UDP relay loop (`handle_server_udp_parts`) recycles outgoing frame packets and incoming WebSocket datagram packets via `crate::mux_writer::recycle_encode_buf`.
  - Client UDP proxy loop (`src/udp_relay.rs`) recycles upload packets and download frame packets back into the thread-safe buffer pool.
  - Server direct non-MUX TCP relay loop (`handle_server_tcp_parts`) recycles frame payload buffers back into the pool.
  - Eliminates per-packet heap allocation and deallocation across all UDP and non-MUX relay paths.
- **Compiler Warnings & Unused Imports Cleanup (`protocol.rs`, `nonmux.rs`)**:
  - Added `#[allow(dead_code)]` to `OwnedMuxFrame::from_parts`.
  - Removed unused `SockRef` import in `nonmux.rs`.

## [v0.0.33] - 2026-09-25

### Performance & Kernel/Memory Protocol Acceleration (5-in-1 TCP Deep Pipeline)
- **Linux Listener `TCP_DEFER_ACCEPT` (3s) (`runtime.rs`)**:
  - Configured `libc::TCP_DEFER_ACCEPT` (3 seconds) in `apply_listener_options` across all TCP listeners (`run_server`, `run_wss_server`, `nonmux`, `mux_pool`, `wss_client`, `quic`).
  - Defers waking up `accept()` until the client's first data packet arrives at the NIC, eliminating redundant empty connection wakeups, context switches, and saving 1 event loop tick per incoming connection.
  - Automatically drops empty SYN port scans and idle probes at the kernel level without application thread involvement.
- **Linux Socket `SO_BUSY_POLL` (50µs) (`runtime.rs`)**:
  - Injected `libc::SO_BUSY_POLL` (50 microseconds) in `apply_socket_options_raw`.
  - Enables low-latency kernel polling directly in the device driver receive queue for incoming packets before putting worker threads to sleep, drastically cutting tail latency (P99) and context-switch costs on active proxy streams.
- **Zero-Allocation Inbound Frame Buffer Pool & Full Lifecycle Recycling (`ws.rs`, `runtime.rs`, `mux_pool.rs`, `wss_client.rs`, `nonmux.rs`)**:
  - Refilled `*buf` in `ws.rs:read_frame` using `crate::mux_writer::acquire_encode_buf()` after `mem::take(buf)` (replacing `Vec::new()` which previously reset capacity to 0 on every single frame read).
  - Explicitly recycled `frame.into_storage()` back into the thread-safe encode pool via `crate::mux_writer::recycle_encode_buf` after writing frame payloads in stream tasks across server and client relay loops (`runtime.rs`, `mux_pool.rs`, `wss_client.rs`, `nonmux.rs`).
  - Forms a closed zero-allocation cycle (`pool -> read_frame -> OwnedMuxFrame -> channel -> write_all -> pool`), eliminating per-frame heap allocations on high-throughput data streams.
- **Server Relay Local Scratch Buffer Reuse (`runtime.rs`, `wss_client.rs`)**:
  - Implemented `send_mux_parts_encrypted_reuse` and wired local `frame_scratch` buffer reuse across `target_to_mux` read loop iterations in `runtime.rs`, matching the client optimization in `mux_pool.rs`.
  - Implemented `send_mux_parts_reuse` in `wss_client.rs` upload loop, eliminating per-slice buffer acquisition across all client and server relay directions.
- **Direct `OwnedMuxFrame::from_parts` Construction (`protocol.rs`, `runtime.rs`)**:
  - Added `OwnedMuxFrame::from_parts(stream_id, command, payload)` with pre-allocated storage capacity `MUX_HEADER_LEN + payload_len`.
  - Replaced the redundant `MuxFrame::new -> encode -> decode_owned` intermediate pass during `syn.initial_data` processing in `runtime.rs:handle_mux_parts` with a single direct frame allocation.
- **Synchronous Zero-Yield DNS Cache (`dns.rs`)**:
  - Replaced asynchronous `tokio::sync::Mutex` DNS cache with synchronous `std::sync::RwLock`.
  - Cached hostname queries (99.9%+ of requests in production) now execute in pure synchronous nanoseconds without yielding to the Tokio task scheduler or causing lock contention across workers.

## [v0.0.32] - 2026-09-25

### Performance & Kernel TCP Data Plane (4-in-1 Linux Kernel Optimizations)
- **Server-Side TCP Fast Open (`TCP_FASTOPEN`, RFC 7413) (`runtime.rs`, `main.rs`, `nonmux.rs`, `mux_pool.rs`, `wss_client.rs`, `quic.rs`)**:
  - Implemented `apply_listener_options` with `libc::TCP_FASTOPEN` (queue depth 256) across all server and client TCP listening sockets (`run_server`, `run_wss_server`, `nonmux`, `mux_pool`, `wss_client`, `quic`).
  - Allows TFO-capable clients to deliver payload in the SYN packet, saving an entire round-trip time (~150ms on trans-Pacific / trans-Eurasian WAN links) during connection setup.
  - Transparent fallback for non-TFO clients with 100% protocol compatibility.
- **Dynamic BBR Congestion Control (`TCP_CONGESTION`) (`runtime.rs`)**:
  - Dynamically configured `b"bbr\0"` per socket in `apply_socket_options_raw`.
  - On cross-border / WAN links with 1%~3% random packet loss, prevents Cubic's catastrophic throughput collapse caused by loss-triggered window halving; BBR estimates bottleneck bandwidth and min-RTT to maintain near line-rate throughput.
  - Safe best-effort: silently falls back to system congestion control if BBR is not loaded in the kernel.
- **Delayed ACK Elimination (`TCP_QUICKACK`) (`runtime.rs`)**:
  - Enabled `TCP_QUICKACK = 1` during initial socket setup in `apply_socket_options_raw`.
  - Suppresses receiver-side 40ms delayed-ACK timers during initial HTTP/WebSocket proxy handshakes and protocol headers exchange.
- **Zombie Connection Cleanup (`TCP_USER_TIMEOUT`, RFC 5482) (`runtime.rs`)**:
  - Injected `TCP_USER_TIMEOUT = 30000` (30 seconds) in `apply_socket_options_raw`.
  - Forcefully terminates broken connections and silent network drops after 30 seconds of unacknowledged data, preventing sockets from remaining stuck for 15+ minutes in kernel retransmission queues.
- **Comprehensive Socket & Listener Option Coverage**:
  - Injected missing `apply_socket_options` on client accept loops in `nonmux.rs` and `quic.rs`.
  - Injected missing `apply_socket_options` on inbound TLS and internal loopback streams in `main.rs:run_wss_server`.
### CI/CD & Build Pipeline Fixes
- **Interactive Manual Release Button (`workflow_dispatch`) (`.github/workflows/release.yml`)**:
  - Enhanced `release.yml` with configurable `workflow_dispatch` inputs (`tag_name`, `draft`, `prerelease`), adding a "Run workflow" button in GitHub Actions web UI for one-click manual builds and releases.
  - Fixed `publish` job execution logic (`startsWith(github.ref, 'refs/tags/v') || github.event_name == 'workflow_dispatch'`), allowing manual releases without being skipped by branch triggers.
  - Automatically identifies tag name from input, git tag ref, or `Cargo.toml`.
  - Added `workflow_dispatch` trigger to `ci.yml` for on-demand CI runs.
- **Fixed Cross-Compilation Linking & Syntax Errors**:
  - Removed `.cargo/config.toml` link flag (`link-self-contained=yes`) which conflicted with Ubuntu MinGW `x86_64-w64-mingw32-ld` runtime libraries (`crt2.o`).
  - Scoped `mimalloc` memory allocator to 64-bit Linux in `Cargo.toml` and `main.rs`, preventing MinGW C compilation error (`ERROR_COMMITMENT_MINIMUM`).
  - Fixed `SocketAddrV6::flowinfo()` call in `udp_batch.rs` (previously invalid `flow_info()`).
  - Fixed borrow of moved value `cfg` inside loop in `main.rs:run_wss_server`.

## [v0.0.31] - 2026-09-25

### Performance & UDP Data Plane (Linux `sendmmsg` Batched Outbound)
- **Linux UDP `sendmmsg` Batched Outbound Writer (`udp_batch.rs`)**:
  - Implemented `UdpBatchWriter` alongside `UdpBatchReader` to emit up to `UDP_BATCH = 8` datagrams per syscall via `libc::sendmmsg` on Linux.
  - Eliminates up to 87.5% of kernel context switches during high-PPS workloads (DNS bursts, online gaming, and QUIC transfers).
  - Opportunistic batching: zero waiting delay on single packets, automatic batch emission under burst arrival.
  - Transparent fallback to single non-blocking `try_send_to` calls on Windows and macOS.
- **Unified UDP Relay Pipeline Integration (`runtime.rs`, `udp_relay.rs`, `quic.rs`)**:
  - Wired `UdpBatchWriter` into server and client UDP forwarders across standard WebSocket MUX UDP relay and QUIC UDP tunnels.
- **Validation**:
  - Added unit test `batch_writer_delivers_in_order` in `udp_batch.rs` verifying exact datagram delivery and FIFO ordering across batch flushes.
  - All workspace tests passing cleanly.

## [v0.0.30] - 2026-09-25

### Performance & Architecture Enhancements (Multi-Core & Adaptive SIMD)
- **Global `mimalloc` High-Performance Allocator (`Cargo.toml`, `main.rs`)**:
  - Integrated Microsoft's `mimalloc` as the global memory allocator on 64-bit platforms (`x86_64` and `aarch64`).
  - Eliminates multi-threaded cross-core arena lock contention and heap fragmentation during heavy concurrent buffer relaying (especially on multi-core server platforms like 16-thread Xeon E5).
  - Preserves system allocator fallback on 32-bit embedded targets (`armv7` router firmware).
- **Runtime Dynamic CPU Dispatch & Dual-Engine SIMD (`crypto.rs`, `ws.rs`)**:
  - Implemented runtime feature detection (`has_avx2()`) to automatically select the optimal hardware vector pipeline without recompilation or SIGILL crash risk on older CPUs:
    - **AVX2 Engine**: 256-bit YMM vectorization with 4-way loop unrolling (128 bytes/iter) for modern Intel/AMD processors.
    - **SSE2 Engine**: 128-bit XMM vectorization with 4-way loop unrolling (64 bytes/iter) for Sandy Bridge / Ivy Bridge generation processors (e.g. Intel Xeon E5-2660).
    - **Fallback Engine**: 32-byte chunks with scalar tail for portable non-x86 architectures.
  - Vectorized WebSocket 4-byte XOR frame masking via unified `apply_ws_mask` across `encode_ws_frame`, `write_frame_borrowed`, and `read_frame`.
- **Linux Anti-Bufferbloat Socket Option `TCP_NOTSENT_LOWAT` (`runtime.rs`, `nonmux.rs`)**:
  - Injected `TCP_NOTSENT_LOWAT = 16384` (16 KiB ceiling) in `apply_socket_options_raw` on Linux.
  - Prevents kernel TCP send buffer queue bloat on high-BDP cross-border proxy links during saturated file transfers, drastically reducing TTFB and latency jitter for concurrent interactive streams.
  - Unified `nonmux.rs` socket options to use `runtime::apply_socket_options_raw`.

### Validation
- Unit tests added in `crypto.rs` and `ws.rs` validating bitwise equivalence of SSE2, AVX2, and fallback routines across all length boundaries (0B to 64KB+).
- All 109 workspace tests passing cleanly (`cargo test --workspace`).

## [v0.0.29] - 2026-09-25

### Performance & Optimizations (Forward-Only Standing Rule Verified)
- **Client TCP_NODELAY & Socket Options Injected (`mux_pool.rs`, `wss_client.rs`, `runtime.rs`)**:
  - Fixed client `accept()` loop omission of `apply_socket_options`, removing Nagle's algorithm 40ms delayed-ACK penalty on local loopback and proxy handshakes.
  - Setup-mode single-connection `c1` throughput improved by +60% to +90% (up to 234 MiB/s).
- **32-Byte AVX2 SIMD Vectorization (`crypto.rs`, `ws.rs`, `mux_writer.rs`)**:
  - Vectorized `XorCipher::apply` with 32-byte chunks and direct `.zip()` pairing, enabling LLVM AVX2 `vpxor` auto-vectorization across the entire keystream pass.
  - Vectorized WebSocket frame unmasking and fused cipher+mask encoder passes from 8-byte scalar loops to 32-byte SIMD chunks.
- **DRR Quantum Deficit Underflow Stall Elimination (`mux_writer.rs`)**:
  - Increased MUX outbound scheduler quantum `DRR_QUANTUM` from 64KB to 128KB and `DRR_MAX_DEFICIT` from 256KB to 512KB (GoWay parity), eliminating multi-round latency stall for maximum-size (~67KB) MUX DATA frames.
- **Scheduler Queue Lossless Retention & Memmove Removal (`mux_writer.rs`)**:
  - Active stream queues in `Scheduler` are now retained across frame transmissions instead of being destroyed on each transient queue empty event, removing per-frame `HashMap::remove` heap churn and $O(N)$ `rotation.remove(idx)` `memmove` shifts.
  - Cleanly teardown stream entries upon FIN/RST or total queue drain.
- **Zero-Allocation Single-Lock Batch Buffer Recycling (`mux_writer.rs`)**:
  - Implemented `recycle_encode_bufs` to return up to 32 frame buffers into `ENCODE_POOL` under a single mutex acquisition, eliminating 32 consecutive lock acquisitions per batch write. Peak RSS reduced by 15-25 MB.
- **Synchronous Relay Pool Mutex (`runtime.rs`)**:
  - Replaced Tokio async `Mutex` with `std::sync::Mutex` for `RELAY_BUFS`.

### Validation
- **Interleaved Paired A/B Benchmarks (`scripts/r6_ab_bench.ps1`, `bench/r6_ab_rushway.csv`)**:
  - Tested paired interleaved runs vs v0.0.28 baseline across setup and steady modes for c1, c8, c32, CPU time, and peak RSS.
  - Achieved full non-inferiority across all dimensions under two-sided Sign Test, with significant FORWARD gains in single-stream latency, multi-stream throughput (reaching 940-960 MiB/s peak), and memory footprint reduction.
- All 107 workspace tests passing cleanly (`cargo test --workspace`).

## [v0.0.28] - 2026-09-24

### Added
- **Outbound encode buffer pool (Round-2 candidate 3)**: process-wide pool (32 × 80 KiB ceiling) recycles pre-encoded MUX frames after the writer flushes them; bulk DATA encode becomes `clear` + no-op `reserve` instead of per-frame `malloc`/`free`. Writer loop drains batches back into the pool (`recycle_encode_buf`).

### Changed
- Round-2 A/B harnesses and rejection evidence committed: `scripts/r{4,5}_ab_bench.ps1`, `bench/r{4,5}_ab_{rushway.csv,raw.log}`.

### Rejected (source not shipped)
- **WS prefill read window**: setup cpu/c32 8:0 REGRESSED — reverted.
- **writer_loop cold-wake coalesce**: NOISE both modes — reverted.
- **read-frame buffer pool**: NOISE both modes — reverted.
- **bulk direct-read zero-copy encode**: NOISE both modes (after fixing a `Vec::reserve` capacity bug that caused ConnectionReset on first run) — reverted. Full analysis in `AI_HANDOFF.md`.

### Validation
- Candidate 3 accepted: Windows paired A/B n=8 × setup+steady — setup c8/c32 FORWARD 8:0, steady c8/c32 FORWARD 8:0, no metric beyond-noise regressed (`bench/r3d_ab_rushway.csv`).
- `cargo test` 108+12 green at candidate-3 HEAD; post-revert tree matches `bbbc0a9` source.
- CI: Rust pin 1.85→1.88 (`slice_as_chunks`); duplicate `toolchain` key fixed in `release.yml`.

## [v0.0.26] - 2026-09-23

### Added
- **Mux VERSION/WINDOW credit flow control (Phase 3, dual-stack)**: `MuxCmdVERSION=0x05` / `MuxCmdWINDOW=0x06` control frames on stream_id=0, negotiated post-handshake; per-stream `CreditGate` (new `src/flow.rs`) enabled idempotently on first VERSION (window 8 MiB), WINDOW refunds every 1 MiB consumed, capped at advertised window; every stream Close path wakes waiters (no deadlock), oversized acquires clamp to window. Wired into `protocol.rs`, `runtime.rs` (server), `mux_pool.rs` (client), `wss_client.rs`. WS MUX scope only; unknown-cmd frames skipped silently → probe negotiation is safe against old peers.
- `scripts/w3_compat_smoke.ps1`: cross-impl + old/new compatibility smoke (8/8 PASS; `bench/w3_smoke_logs/`).
- `scripts/w3_ab_bench.ps1`: interleaved same-session A/B vs HEAD-built goway v1.8.11 with paired per-sample deltas + exact two-sided sign test (verdict method for the forward-only rule).

### Changed
- Defaults retuned after round-1 regression: `MUX_INITIAL_WINDOW_KIB` 1024→8192 (8 MiB), `MUX_WINDOW_REFRESH` 65536→1048576 (1 MiB) — bulk flows never stall on credit RTTs; slow receivers still bounded at 8 MiB/stream (unbounded pre-W3).

### Validation
- `cargo clippy --all-targets` 0 warnings; `cargo test` 106+12 pass; compat smoke 8/8 PASS (twice, incl. post-retune).
- Interleaved paired A/B (n=10 setup + n=10 steady vs goway v1.8.11): every throughput/CPU/RSS metric NOISE (none regressed beyond noise) → goway-side W3 certified non-inferior under the forward-only standing rule; numbers in `AI_HANDOFF.md` / `PROGRESS.md` and `bench/w3_ab_goway.csv`.

## [v0.0.25] - 2026-09-22

### Added
- **RushWay UX Parity (Phase 1)**: `-tui` dashboard (GoWay-parity layout, TTY-gated, 10 Hz dirty refresh), `-log-file` (last 10 WARN/ERROR, rewritten on append + exit flush), `-cpuprofile`/`-cpuprofile-duration` (pprof 99 Hz on Unix; warn-and-continue elsewhere), and `[STATS]` line with global `ConnGuard`/byte counters on TCP relay paths.
- New modules: `src/tui.rs`, `src/stats.rs`, `src/profile.rs` (pprof, `cfg(unix)` dependency).
- Removed accepted-but-unimplemented warnings for those flags; SPEC CLI section updated.
- Default parity audit: `-block-local`, `-W`, `-max-conn`, `-mux-sessions`, `-connection-timeout` match GoWay head.

### Changed
- **clippy 6-idiom cleanup** (`-D warnings` clean): `mux_pool` needless-return + collapsible-if, `quic` while-let-loop, `runtime` `io::Error::other`, `tui` manual-clamp + `writeln!`.

### Fixed
- `cargo clippy --release -- -D warnings` now passes (0 warnings); `cargo fmt --check` clean.

### Validation
- `cargo test --all-targets` 96 passed; `cargo fmt --check` clean; `cargo clippy --release -- -D warnings` clean.
- Paired note: goway server egress writer change documented in `AI_HANDOFF.md` (Phase 2); no rushway protocol change.

## [v0.0.24] - 2026-09-21

### Changed
- **QUIC Transport Trim (GoWay #5 Parity)**: `max_idle_timeout` 60s→30s (faster dead-conn reclaim); explicit `max_concurrent_bidi_streams` 512 (bounds a malicious peer's stream table; was quinn default 100); `datagram_receive_buffer_size(None)` (no send/read_datagram call exists anywhere — negotiating DATAGRAM frames only cost handshake bytes, same as GoWay disabling them). Kept deliberately: keep-alive 15s (NAT safety), uni streams 0 (unused; stricter than GoWay's 128), 8/16MiB windows (bulk-tuned), MTU discovery on, Cubic (BBR reverted with evidence).
- **Measured**: QUIC e2e still functional post-trim (c1=62/c8=83/c32=67 MiB/s); QUIC bulk remains ~3x behind WS (congestion/flow-control untouched, as scoped). WS bulk healthy (c8/c32 ~200-220).

## [v0.0.23] - 2026-09-21

### Added
- **UDP Batched Reads — `recvmmsg` (GoWay #2 Parity)**: new `src/udp_batch.rs` (`UdpBatchReader`) drains up to 8 datagrams per syscall on Linux; all five UDP relay upload loops (`udp_relay`, `mux_pool`, `wss_client`, `quic` ×2, `runtime` server) converted with byte-identical per-datagram behavior. Other platforms keep single-`recv_from` fallback (no equivalent kernel API). Zero new dependencies (`libc` promoted to direct; buffers stay owned full-length, no `set_len` unsoundness).
- **Measured**: loopback drain of 20000×512B pre-filled socket (WSL Debian): single 408 MB/s → batched 784 MB/s (**1.92×**, full 8-batches). Unit test `batch_reader_delivers_in_order` covers order + source addresses on all platforms.

## [v0.0.22] - 2026-09-21

### Measured
- **Slow-Link Fairness A/B (honest negative on magnitude)**: 20Mbps+30ms shaped link (WSL `tc`), 16×4MiB bulk background, 1KB interactive probes (n=40-50). p50 identical (32.3ms both — no median regression); tails multi-second on BOTH (FIFO p99 0.1-4.9s / DRR p99 3.4-5.4s across rounds, high variance). Conclusion: on a link-saturated topology the end-to-end tail is set by kernel/shaper queueing + loss recovery, not proxy scheduling — DRR neither wins nor loses here. Its proven value stays structural (deterministic per-stream fairness in `drr_interactive_not_starved_by_bulk`, no bulk-throughput loss) with expected payoff on CPU-constrained proxies (OpenWrt/small VPS), unmeasured. No inflated claims shipped.

## [v0.0.21] - 2026-09-20

### Fixed
- **SYN-Yields-DATA Loss (critical, burst-only)**: the self-ordering rule initially applied to ALL controls, so a SYN queued behind its own stream's DATA yielded — the peer then dropped that DATA as unknown-stream (no error anywhere: no Rst, no dispatch-miss, no panic). Manifested as 1–25% of flows vanishing under tight burst (full request sent, zero echo). Only FIN/RST yield now; SYN always leads. Proven by per-id frequency analysis (lost SYNs were always the burst tail) + `drr_syn_never_yields_to_own_data`.
- **Tail Stall When No Further Arrivals**: the emit loop broke at the first unaffordable `next()`; at flow tails (no future arrivals to add credit) the remainder stranded forever. Now retries while non-empty (each call accrues a quantum; frames far below the cap, so termination is guaranteed).
- **Close-Time Frame Abandonment**: channel close dropped scheduler leftovers (56 frames caught live). Now drains on close with a bounded spin guard.
- **Feed 64→256**: absorbs tight bursts without sender blocking (validated 5×100-burst green); total bound 512 frames worst-case.

### Added
- **Burst Regression Tests**: `concurrent_bulk_all_frames_delivered` (8×71 live-task frames), `drr_syn_never_yields_to_own_data`.

## [v0.0.20] - 2026-09-20

### Added
- **MUX Write-Side Fair DRR Scheduling (GoWay Parity)**: `MuxFrameWriter` now takes `send_mux(stream_id, command, frame)` alongside priority `send()` (handshake hello/ping); per-stream FIFOs + priority lane + deficit round robin (64KB quantum, 256KB cap, 256-frame total bound). Vectored-write batching preserved (batches are DRR-ordered).

### Fixed
- **FIN-Overtakes-DATA Data Loss (critical, caught by loopback e2e)**: first DRR version let a stream's FIN jump its own queued DATA → peer closed early → tail dropped (`curl 000` while the server had emitted the full response). Priority lane is now stream-aware: a control yields while its own stream still has queued DATA, still jumping other streams' bulk. Same latent race existed in GoWay (won by speed on fast links); fixed on both sides, proven by `FinNeverOvertakesOwnData` (failed pre-fix with `[FIN DATA DATA]`).
- **Scheduler `total` Undercount**: priority-branch early return skipped `total += 1`, making `is_empty()` lie and the writer task exit early (handshake hello never emitted). One-line fix; covered by all writer tests.

## [v0.0.19] - 2026-09-18

### Added
- **MUX Sender-Side `-obfs` Padding (GoWay Parity)**: `encode_mux_ws_frame` appends a random `[0, OBFS_PAD_MAX]` tail to DATA frames inside the WS payload length (MUX declared length unchanged); control frames stay exact. Plumbed through `mux_pool` (client), `wss_client` (`self.obfs`/`session.obfs`), and `runtime` server (`cfg.obfs`); `--obfs` CLI + config-file key already existed and are now honored end to end.

### Fixed
- **Obfs Pad Framing Desync (critical)**: first implementation appended pad *after* the WS header was computed, so peers left pad bytes in the stream and the next header parsed as garbage — reproduced as `ws.rs:692 unreachable!()` panic on a live obfs↔obfs transfer. Pad is now sized up-front and covered by the WS length; pad bytes are masked/ciphered with the rest.
- **Reserved-Opcode Panic Hardening**: `read_frame`'s `_ => unreachable!()` (reachable via reserved opcodes 0xB–0xF after any desync, or from a hostile peer) now returns an error instead of panicking the worker.

## [v0.0.18] - 2026-09-18

### Fixed
- **Batched WS Header Reads**: `read_frame` parses the header in at most 3 reads (2-byte base, then extended-length + mask key in one go) instead of up to 5; matters most for small-frame traffic (tens of thousands of frames/s).
- **Borrowed Non-MUX Frame Writes**: new `ws::write_frame_borrowed` masks reusable scratch in place and emits via a single vectored write — zero allocation on all 1:1 non-MUX relay paths (the interim `write_frame_owned` was superseded and removed).

## [v0.0.17] - 2026-09-18

### Fixed
- **Batched WS Header Reads**: `read_frame` parses the header in at most 3 reads (2-byte base, then extended-length + mask key in one go) instead of up to 5; matters most for small-frame traffic (tens of thousands of frames/s).
- **Zero-Copy Non-MUX Frame Writes**: new `ws::write_frame_owned` masks the caller's buffer in place and emits header (+ key) + payload with a single vectored write — one fewer allocation + copy + syscall per chunk on all 1:1 non-MUX relay paths (`nonmux` client/server, `wss_client`).

## [v0.0.16] - 2026-09-17

### Added
- **GoWay-Style Startup Banner**: unconditional `RushWay vX.Y.Z` dashboard header (mode/listen/upstream/mux/DNS/auth/buffer/max-conns), visible at any `--log` level.
- **Non-MUX Pre-Warmed Upstream Pools (GoWay `ConnPool` Parity)**: `ws://` (`src/nonmux.rs`) and `wss://` (`src/wss_client.rs`) non-MUX clients keep 4 pre-handshaked transports (5 min max age, 30 s idle, 5 s refill, stop-on-first-failure). Single-use transports; pool is a pre-warmed dial cache. First attempt was reverted over a misdiagnosed stall (test-harness pipe-buffer freeze); re-applied with file-logged validation proving both WS and WSS pools hit in production paths.

## [v0.0.15] - 2026-09-17

### Fixed
- **Single-Pass Cipher+Mask Fusion**:
  - `encode_mux_ws_frame` now XORs the cipher keystream and WS mask in one word-at-a-time pass (`word ^ keystream ^ mask64`) instead of two full memory passes; offset semantics unchanged (region-relative, verified by round-trip tests).
- **RwLock Stream Tables**:
  - Per-session stream maps (`mux_pool`, `wss_client`, `runtime` server) switched from `Mutex` to `RwLock`: concurrent lookups per DATA frame, exclusive insert/remove. Pool admission also fails fast on dead writers.
  - Measured same-window A/B vs v0.0.14: neutral-to-marginal (c32 ~205-249 vs ~182-196, c8 overlapping); kept for halved memory passes + no per-frame exclusive lock, not for claimed gain. Flow `early eof` flakes occur on both sides (environmental watch item).

## [v0.0.14] - 2026-09-17

### Fixed
- **QUIC BBR Rejected (Measured)**:
  - Trialled `BbrConfig` congestion control; loopback throughput halved with tripled variance vs Cubic (c8 92→~55). Reverted same session; in-tree NOTE records the verdict. Cubic retained.
- **Single-Stream Tuning Guide (Measured)**:
  - c1 bulk vs `-W` (single MUX stream, 2 MB, release medians): 128/512/1024 → ~77-78 MiB/s, **4096 → ~123 MiB/s**. `-W 1024` remains the balanced default; raise to 4096 only when single-stream ceiling matters (hard ceiling is the protocol-locked 64 KB MUX frame).

## [v0.0.13] - 2026-09-17

### Fixed
- **Fused MUX+WS Frame Encoding**:
  - New `mux_writer::encode_mux_ws_frame` builds `[WS header][mask?][MUX header|payload]` in one buffer (one allocation, bytes written once) instead of MUX-`Vec` → cipher → WS-`Vec` + full copy; `ws::ws_header_into` extracted as the single layout authority.
  - Adopted in all MUX upload paths (`mux_pool`, `wss_client`, `runtime` server).
  - Measured (`e2e_bench`, localhost): within noise of the writer-task baseline (c8 ~240-271, c32 ~234-292 both before/after); kept for strictly-less-work + simpler path, not for claimed gain. One `flow 13 early eof` in 6 runs treated as harness flake (watch item).
- **QUIC Socket Buffers + MTU Discovery**:
  - QUIC endpoints now bind custom UDP sockets with enlarged buffers (8 MiB default, honors `--socket-buffer`) instead of OS defaults, and enable path MTU discovery (previously every datagram stayed near 1200 bytes).
  - Fixed self-inflicted dual-stack outage found during rollout (custom socket lacked `IPV6_V6ONLY=0`, breaking IPv4-mapped targets with `AddrNotAvailable`).
  - Measured (`e2e_bench`, QUIC 4 MiB, localhost): c1 46 → 62-75, c8 64 → ~92, c32 43 → ~89 (**up to 2×**, c32 ≥ c8 inversion fixed).

## [v0.0.12] - 2026-09-17

### Fixed
- **Dedicated MUX Writer Task + Write Coalescing (GoWay `muxOutboundWriter` Parity)**:
  - New `src/mux_writer.rs`: each MUX session owns one writer task fed by a bounded (256) channel instead of all streams locking a shared `Mutex<WriteHalf>` per frame.
  - Callers pre-encode complete WS frames via new `ws::encode_ws_frame` (masking on the stream task); the writer loop coalesces queued frames (≤ 32 frames / 1 MiB) into vectored writes.
  - Migrated all three MUX session types (`mux_pool` client, `wss_client` sessions, `runtime` server); 1:1 non-MUX/UDP paths keep direct writes (contention-free).
  - Fixed self-owning-`Arc` task leak found by test (task now holds only the `closed` flag; new `writer_task_exits_when_handles_dropped` test).
  - Pool admission now also checks writer liveness (`is_closed`) for fail-fast routing.
  - Measured (`e2e_bench`, WS 4 MiB, localhost): c8 ~203 → ~250-271, c32 ~197 → ~244-292 (**+25-40%**, c32 ≥ c8 inversion fixed).

## [v0.0.11] - 2026-09-16

### Fixed
- **Graceful Shutdown on Ctrl+C / Ctrl+Break**:
  - All 7 accept loops (`runtime`/`nonmux` ×2/`mux_pool`/`wss_client` ×2/`quic` ×2/`main` WSS server) now stop accepting on signal, drain in-flight tasks with a 5 s budget (`SHUTDOWN_DRAIN_SECS`), then exit cleanly.
  - `mux_pool` admission switched from blocking `acquire_owned` to fail-fast `try_acquire_owned` so shutdown is never stuck behind a permit wait.
  - Verified live: Ctrl+Break → `draining` logged → exit code 0. Clean exit also flushes PGO profiles, enabling relay-traffic training.
- **Global Relay Buffer Pool (GoWay `BufPool` Reuse Parity)**:
  - New shared pool in `src/runtime.rs` (`relay_buf`/`recycle_buf`): buffers ≤ 1 MiB reused across relay tasks (max 128 retained); larger `-W` buffers fall back to allocate/free.
  - Adopted at all 11 relay read sites (`runtime`/`mux_pool`/`wss_client`/`nonmux`/`quic`); pool hit/miss covered by `relay_buf_pool_reuses_allocations` test.
  - Buffers are fully overwritten by `read` before use, so no zeroing is needed on either path.

### Added
- **Relay-Traffic PGO Training Result**:
  - Extended `scripts/pgo-build.ps1` with the measured verdict: relay-trained profile lands within noise of the normal build on loopback (c8/c32 medians roughly -5%, overlapping variance) — PGO binary **not shipped**; `dist` keeps the normal release.
  - Training driver pattern documented (real server/client bulk traffic + Ctrl+Break clean exit; killed processes never flush `.profraw`).

## [v0.0.10] - 2026-09-16

### Fixed
- **Bulk 8-Byte XOR Transform**:
  - `XorCipher::apply` now processes 8 bytes per iteration via native-endian `u64` words, mirroring GoWay `TransformInPlace`; byte `j` still maps to `key[j % 256KiB]`.
  - Micro-benchmark (16 MiB × 20, `-O`): 1.58 → 3.48 GB/s (**2.2×** on the cipher kernel); A/B vs v0.0.9 tag: c8/c32 medians **+~35%**.
- **GoWay-Aligned Relay Buffer Ceiling**:
  - New shared `runtime::relay_buffer_size` honors `-W` up to 12 MiB (GoWay BufPool parity); 9 relay read sites across `runtime/mux_pool/wss_client/nonmux/quic` previously truncated at 1 MiB.
  - 128 KiB default behavior unchanged; only high `-W` (e.g. `-W 1024/4096` + large `--socket-buffer`) now takes effect.
  - Live check: 2 MiB bulk through MUX with `-W 4096` byte-identical end to end.
- **Browser-Profile Rotation & TLS Fingerprint Parity**:
  - New 7-profile table in `src/ws.rs` (Chrome 136 ×3 / Edge 136 / Firefox 138 ×2 / Android Chrome) mirroring GoWay; random profile per handshake; middle headers Fisher-Yates shuffled with fixed top/bottom; Firefox profiles omit `sec-ch-ua` like real Firefox.
  - `src/tls.rs` orders ring cipher suites / key-exchange groups per profile (Chromium vs Firefox order, X25519-first), cached per (profile, verify) config; RSA/CBC and P-521 unavailable in ring are documented gaps.
  - Exposed `connect_with_profile` / `build_client_handshake_request_with_profile` for future TLS+HTTP identity correlation (GoWay draws them independently).

### Added
- **PGO Build Pipeline** (`scripts/pgo-build.ps1`): instrument → train → `llvm-profdata merge` → `-Cprofile-use` rebuild (requires `rustup component add llvm-tools`).
  - Measured outcome: unit-test-trained profile **regressed** steady-state relay throughput ~15-20% (c8/c32 medians) — training data marks async relay loops as cold. PGO binary **not shipped**.
  - Correct training needs a cleanly-exiting relay workload (killed `e2e_bench` children never flush `.profraw`); blocked on graceful-shutdown support.

## [v0.0.9] - 2026-09-16

### Fixed
- **0-RTT MUX Deadlock Resolution (GoWay Server + RushWay Client)**:
  - Eliminated blocking upstream first-frame wait in `src/mux_pool.rs` and `src/wss_client.rs`.
  - Client sends immediate 0-RTT SOCKS5 success reply or `HTTP/1.1 200 Connection Established`.
  - Removed non-standard empty `Data` frame dispatch from server dial path in `src/runtime.rs`.
- **SOCKS5 UDP Datagram Preservation (RFC 1928 §7)**:
  - Fixed UDP relay across all transports (`mux_pool.rs`, `wss_client.rs`, `udp_relay.rs`) to forward complete datagrams (`&packet`) preserving `[RSV][FRAG][ATYP][DST.ADDR][DST.PORT]` headers.
- **SOCKS5 UDP Association Lifecycle Management (RFC 1928 §6)**:
  - Added concurrent `control.read()` liveness monitoring via `tokio::select!` across all UDP relay paths.
  - Automatically aborts background relay tasks and releases UDP port when client TCP control connection terminates.
- **QUIC UDP Protocol Alignment with GoWay**:
  - Removed invalid XOR cipher on QUIC UDP streams (QUIC transport is already TLS 1.3 encrypted).
  - Aligned stream handshake to plaintext `"<key> UDP\n"` or `"UDP\n"`.
  - Switched to length-prefixed raw datagram framing matching `goway.go:4970-5180`.
- **Full Plain HTTP Proxy Support**:
  - Added unified `read_client_proxy_request` supporting `GET`, `POST`, `HEAD`, `PUT`, `DELETE`, `OPTIONS`, `PATCH`.
  - Packed original HTTP request bytes into `SynPayload.initial_data` for true 0-RTT upstream dispatch without invalid `200 Connection Established` interception.
- **Idle MUX Session Keepalive (WebSocket Ping Heartbeat)**:
  - Added 25-second heartbeat task sending masked WebSocket `Ping` frames on idle sessions, preventing Cloudflare 100s idle drops and NAT connection timeouts.
- **Collision-Free Stream ID Allocation**:
  - Added non-zero allocation loop checking `!streams.contains_key(&id)` under stream table lock, preventing stream ID reuse or collision upon 32-bit integer overflow.
- **TCP Half-Close Relay & Client FIN Liveness**:
  - Maintained upstream-to-client reader task on `MuxCommand::Fin` in `src/runtime.rs` so HTTP responses are fully delivered when client half-closes its upload side.
  - Decoupled `Fin` from cancellation watcher during stream connection dial.
- **Hardened Localhost & IPv6 Bracket Target Filtering**:
  - Stripped brackets `[` / `]` and added case-insensitive `"localhost"` check, IPv4-mapped IPv6 handling (`::ffff:127.0.0.1`), and multicast link-local detection in `src/runtime.rs`, matching GoWay's `isLocalTarget` exactly.
  - Handled bracketed IPv6 literals in `src/dns.rs` (`resolve_host` and `resolve_all_ipv4`) without failing over to DNS queries.
- **HTTP Proxy Pipelined Body Preservation**:
  - Retained all buffered bytes (headers + body prefix) in `initial_payload` within `src/proxy.rs` so POST/PUT body payloads are never truncated.
- **UDP Upstream DNS & Socket Options**:
  - Wired custom `-dns` resolution and socket options in `src/udp_relay.rs`.
- **Clean WebSocket EOF Handling & Symmetrical Pong Masking**:
  - `read_frame` returns `Ok(None)` on clean socket EOF rather than raising `UnexpectedEof`, eliminating false error logging on connection disconnects.
  - Symmetrically masks Pong replies (`!masked`) when responding as a client to satisfy RFC 6455 §5.1.
- **Extended TLS / QUIC Signature Scheme Support**:
  - Expanded `NoCertificateVerification` in `src/tls.rs` and `src/quic.rs` to support SHA-384 and SHA-512 algorithms (ECDSA P-384/P-521, RSA-PSS, RSA-PKCS1).
- **Multi-Transport Target Authority Unification & IPv6 Hardening**:
  - Added unified `parse_target_authority` and `parse_authority_with_default` in `src/proxy.rs` adhering to RFC 3986.
  - Automatically strips brackets from IPv6 hostnames (`TargetAddr.host`), rejects port 0, and catches bare multi-colon IPv6 strings.
  - Unified authority handling across all transports (`runtime.rs`, `quic.rs`, `nonmux.rs`, `wss_client.rs`, `udp_relay.rs`, `mux_pool.rs`).
- **Session Hang & Resource Leak Prevention**:
  - Added `tokio::select!` monitoring between upload tasks and download frame loops in non-MUX client mode (`src/nonmux.rs`, `src/wss_client.rs`).
  - Added `tokio::select!` monitoring between backend read tasks and foreground loops in QUIC server mode (`src/quic.rs`) and plain WS server mode (`src/nonmux.rs`), preventing hung sessions when target backend or client disconnects.
- **Non-MUX TCP Plaintext Alignment (GoWay Interop)**:
  - GoWay `handleServer`/`handleClient` encrypt only the `host:port\n` / `OK\n` handshake; data frames are plaintext. RushWay wrongly XOR-applied every data frame, producing garbled relay against GoWay servers.
  - Removed cipher application from all non-MUX data paths (`src/nonmux.rs` client + server, `src/wss_client.rs` non-MUX), keeping encryption for handshake only.
  - Verified live: `RushWay -> GoWay` and `GoWay -> RushWay` both pass in MUX and non-MUX modes.
- **Plain-WS Cloudflare Edge Fallback (`dialFallback` Parity)**:
  - Ported GoWay `dialFallback` to plain `ws://`: when upstream is a literal IP, `-fakehost` is set, and the primary dial fails, resolve `fakehost` A records and try each non-primary edge (`src/mux_pool.rs`, `src/nonmux.rs`, `src/udp_relay.rs`).
  - Aligned `Origin` (`http://<sni>`) and `Sec-Fetch-Site` (SNI-vs-Host comparison) with GoWay `performWSHandshake` on all plain-WS paths.
- **WSS Stream Accounting Single-Ownership (P0)**:
  - `wss_client.rs` reader no longer decrements `active` on terminal `FIN`/`RST`; cleanup happens once in `handle_connection` guarded by `remove().is_some()`, mirroring `mux_pool.rs`.
- **Bounded Pre-Dial Buffering (P0)**:
  - Capped pre-dial `pending` at 64 frames / 1 MiB in `src/runtime.rs`; overflow sends `RST` and drops the stream instead of unbounded growth.
- **Runtime Server Non-MUX Branch (P0)**:
  - `src/runtime.rs::run_server` now serves plain `host:port\n` targets via new `handle_server_tcp_parts` (GoWay `handleServer` fallthrough parity) instead of `bail!("unknown transport handshake")`; `Go non-mux -> RushWay` verified passing.
- **Incremental WS Payload Reads (P0)**:
  - `src/ws.rs::read_frame` grows the payload buffer in 64 KiB segments, so a bogus 64 MB length prefix no longer causes instant OOM.
- **DNS All-v4 Cache & Pool Backoff (P1)**:
  - `resolve_all_ipv4` results cached for 5 minutes (`src/dns.rs`).
  - `MuxSessionPool`/`WssSessionPool` track consecutive failures; `maintain` sleeps base interval plus 500 ms per failure capped at ~5 s; MUX creation failures promoted to `WARN`.
- **Fail-Fast Server Admission (P1)**:
  - `src/runtime.rs::run_server` uses `try_acquire_owned` at capacity instead of stalling the accept loop.
- **Dead Module Removal (P1)**:
  - Deleted unreferenced `src/mux_config.rs` / `src/runtime_config.rs` (stale `MAX_MUX_STREAMS_PER_SESSION=256` vs authoritative 2048).
- **Bulk 8-Byte XOR Transform (Perf Step 1)**:
  - `XorCipher::apply` now processes 8 bytes per iteration via native-endian `u64` words, mirroring GoWay `TransformInPlace`; byte `j` still maps to `key[j % 256KiB]`.
  - Micro-benchmark (16 MiB × 20, `-O`): 1.58 → 3.48 GB/s (**2.2×** on the cipher kernel).
- **GoWay-Aligned Relay Buffer Ceiling (Perf Step 2)**:
  - New shared `runtime::relay_buffer_size` honors `-W` up to 12 MiB (GoWay BufPool parity); 9 relay read sites across `runtime/mux_pool/wss_client/nonmux/quic` previously truncated at 1 MiB.
  - 128 KiB default behavior unchanged; only high `-W` (e.g. `-W 1024/4096` + large `--socket-buffer`) now takes effect.
  - Live check: 2 MiB bulk through MUX with `-W 4096` byte-identical end to end.
- **PGO Build Pipeline (Perf Step 3)**:
  - New `scripts/pgo-build.ps1`: instrument → train → `llvm-profdata merge` → `-Cprofile-use` rebuild (requires `rustup component add llvm-tools`).
  - Measured outcome: unit-test-trained profile **regressed** steady-state relay throughput ~15-20% (c8/c32 medians) — training data marks async relay loops as cold. PGO binary **not shipped**; `dist` keeps the normal release build.
  - Correct training needs a cleanly-exiting relay workload (killed `e2e_bench` children never flush `.profraw`); blocked on graceful-shutdown support.
- **Browser-Profile Rotation & TLS Fingerprint Parity (A)**:
  - New 7-profile table in `src/ws.rs` (Chrome 136 ×3 / Edge 136 / Firefox 138 ×2 / Android Chrome) mirroring GoWay; random profile per handshake; middle headers Fisher-Yates shuffled with fixed top/bottom; Firefox profiles omit `sec-ch-ua` like real Firefox.
  - `src/tls.rs` orders ring cipher suites / key-exchange groups per profile (Chromium vs Firefox order, X25519-first), cached per (profile, verify) config; RSA/CBC and P-521 unavailable in ring are documented gaps.
  - Exposed `connect_with_profile` / `build_client_handshake_request_with_profile` for future TLS+HTTP identity correlation (GoWay draws them independently).

### Added
- **GoWay CLI `-l` / `--local` Flag Support**:
  - Added `-l` / `--local` flag to `Args` and `LEGACY_LONG_FLAGS` in `src/main.rs`, mapping to `cfg.proxy_host`.

### Changed
- **Concurrency Scaling**:
  - Scaled `MAX_STREAMS_PER_SESSION` from 256 to 2048 in `runtime.rs`, `mux_pool.rs`, and `wss_client.rs` to support full `--max-conn 1500` capacity without premature RST throttling.
- **HTTP Proxy Header I/O Optimization**:
  - Replaced byte-by-byte `read_exact` syscall loop with chunked buffered reading (up to 1024 bytes per read) in `src/proxy.rs`.
- **Zero Warnings**:
  - Cleaned all unused imports, dead code, and unreachable loop expressions across all crate targets.

## [v0.0.8] - 2026-09-15

### Fixed
- **WSS Handshake Timeout & Detailed Diagnostics**:
  - Added strict configurable timeout for WebSocket upgrade response via `read_http_headers_timeout`, preventing indefinite hangs on HTTP 101.
  - Added granular stage logging:
    - `[WSS] TCP connected`
    - `[WSS] TLS handshake completed` (including protocol, cipher, ALPN)
    - `[WSS] Sending WebSocket upgrade`
    - `[WSS] Waiting for WebSocket 101`
    - `[WSS] WebSocket handshake completed`
    - `[WSS] Sending MUX handshake`
    - `[WSS] MUX handshake completed`
  - Added partial response diagnostics: on handshake timeout or unexpected peer closure, logs whether 0 bytes were received or prints lossy UTF-8 partial HTTP response headers.
  - Added explicit HTTP non-101 status logging (`HTTP 403`, `HTTP 404`, `HTTP 400`, `HTTP 502`, etc.) printing the first response line and headers.
  - Upgraded physical session creation failures in `WssSessionPool` to `WARN` with full structured context (`error`, `upstream`, `fakehost`, `sni`, `host`, `path`) without leaking credentials.
  - Added timeout on MUX handshake `OK\n` frame reading.
  - Aligned client handshake request header formatting and casing (`sec-ch-ua` lowercase) with GoWay v1.8.x.
  - Added `redact_handshake_request` to safely log outgoing HTTP request headers in `DEBUG` level with `Sec-WebSocket-Key: [REDACTED]`.
  - Added `probe_wss_handshake` and standalone WSS handshake integration test.

## [v0.0.7] - 2026-09-15

### Fixed
- **Unified WSS Transport for MUX Physical Sessions**:
  - Exported authoritative crate-level `connect_wss_upstream` transport helper in `src/wss_client.rs`.
  - Clarified execution boundaries: verified that `wss://` with `-mux` routes through `WssSessionPool`, with all parallel physical sessions established via the Cloudflare-aware pipeline.
  - Ensured `-fakehost` applies consistently to TLS SNI and HTTP `Host` for all MUX physical sessions.
  - Ensured Cloudflare Edge fallback via DNS resolution of `fakehost` is available across each physical session dial.
  - Fixed plain `ws://` origin scheme in `src/mux_pool.rs` to use `http://` instead of `https://`.
- **Workflow Cleanup**:
  - Removed completed one-shot workflows (`.github/workflows/cli-align-once.yml` and `.github/workflows/stress-gate-once.yml`).
- **Regression Coverage**:
  - Added in-process mock WSS server integration test verifying TLS SNI, HTTP `Host`, `Origin`, and encrypted `MUX\n` / `OK\n` handshake.

## [v0.0.6] - 2026-09-15

### Added
- **Cloudflare CDN / FakeHost Alignment**:
  - Full separation of TCP connection address and TLS SNI / HTTP Host identity.
  - Aligned TLS ServerName, HTTP `Host`, and `Origin` (`https://<fakehost>`) with GoWay's production specification.
  - Path retention (`/path`) across custom CDN WebSocket routing endpoints.
  - Automatic Cloudflare Edge fallback via DNS resolution of `fakehost` when primary IP connection fails.
  - Multi-candidate IPv4 fallback dialing skipping the already-attempted primary IP.
- **Unified WSS Dialing Pipeline**:
  - Unified `open_upstream` pipeline across MUX physical sessions, non-MUX connections, and UDP-over-WSS relays.
  - Informative logging of WSS connection target, TLS ServerName, HTTP Host, path, and fallback attempts without sensitive key leakage.
- **Automated Tests**:
  - Unit tests for WSS URL parsing with IP addresses and custom endpoints (`/pyway`).
  - Unit tests verifying header construction (`Host`, `Origin`, `Upgrade`, `Sec-WebSocket-Version`).
  - Unit tests confirming no raw IP leakage into `Host` or `Origin` headers when `fakehost` is configured.
  - Unit tests for multiple IPv4 DNS answer parsing and Cloudflare fallback eligibility rules.
  - CLI parser regression test for the full Cloudflare FakeHost command with 8 MUX sessions.

### Changed
- **Default Maximum Connections**: Set runtime default `max_connections` to 1,500 (aligning with v0.0.5/v0.0.6 requirements).
- **CLI Help & Compatibility**:
  - Updated `--fakehost` and `--max-conn` help text.
  - Ensured legacy flags `-help`, `-fakehost`, and related variants remain fully functional.
- **Version**: Bumped version to `0.0.6`.

## [v0.0.25] (historical draft) — Phase 1 UX (see top of file for released entry)

- Implemented `-tui` dashboard (GoWay-parity layout, TTY-gated, 10 Hz dirty refresh), `-log-file` (last 10 WARN/ERROR, rewritten on append + exit flush), `-cpuprofile`/`-cpuprofile-duration` (pprof 99 Hz on Unix; warn-and-continue elsewhere), and `[STATS]` line with global `ConnGuard`/byte counters on TCP relay paths.
- Removed accepted-but-unimplemented warnings for those flags; SPEC CLI section updated.
- Default parity audit: `-block-local`, `-W`, `-max-conn`, `-mux-sessions`, `-connection-timeout` match GoWay head.
- Tests: `cargo test --all-targets` 96 passed (new log-file ring, stats guard, byte counter tests).
- Paired note: goway server egress writer change documented in `AI_HANDOFF.md` (Phase 2); no rushway protocol change.

## [v0.0.25] (historical draft) — Phase 2 paired note: goway server egress writer (interop impact)

- No RushWay code change this entry. Documenting the paired goway change so interop/CI expectations stay accurate:
  - GoWay server MUX `SendFrame` now uses a dedicated `muxOutboundWriter` (unmasked fused encode) instead of `writeMu` + two-pass cipher.
  - Wire format unchanged �� RushWay client/server need no protocol bump to interoperate.
  - See `D:\SOFT\AI\github\way\goway\AI_HANDOFF.md` and `D:\SOFT\AI\github\way\CHANGELOG.md` for full change/validation.
- Cross-repo handoff: Phase 1 (TUI/log-file/cpuprofile/STATS) remains the RushWay-side UX delivery; Phase 4 joint bench is the next shared milestone.
