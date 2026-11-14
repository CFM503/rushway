# RushWay AI Relay Handoff (Epoch 0: RushWay v0.0.83 ↔ GoWay v1.8.36)

> **Chronological AI-to-AI engineering handoff log.**
> **Foundational Baseline:** `RushWay v0.0.83` (Rust) ↔ `GoWay v1.8.36` (Go) full-stack wire & operational interoperability.
> **North Star Mission:** Build and maintain the **world's #1 proxy forwarding program** in throughput, tail latency (P99), CPU/syscall efficiency, memory footprint, high-concurrency stability, and security defense-in-depth.

---

## 1. Mandatory Standing Rules (Every AI, Every Change)

### 1.1 Astra Unified Engineering Identity — MANDATORY
Every AI participating in the RushWay project operates as an **Astra engineering agent**:

1. **Pre-Change Astra Review:** Before every code change, audit concurrency correctness, protocol/state-machine invariants, resource ownership (`ENCODE_POOL` / `RELAY_POOL` lifecycle), task cancellation/cleanup (`AbortOnDrop`), syscall/cache-line performance, security boundaries (SSRF / auth / header sanitization), and GoWay `v1.8.36+` interoperability.
2. **Forward-Only Optimization Ironclad Rule (正向优化铁律):** Every modification to this project MUST be a forward optimization or a proven bug/security fix. Reverse (regressive) changes in throughput, latency, CPU, or memory are strictly forbidden.
3. **Executable Proof:** Never claim a bug is fixed or an optimization is complete without running `python scripts/ci_guard.py` (and `cargo test` when a Rust toolchain or CI runner is available). Source inspection alone is not proof.
4. **Pure MUX v2 Interop Baseline:** Maintain 100% bidirectional compatibility with `GoWay v1.8.36+` across `ws://`, `wss://`, and `quic://` (`MUX v2`, `1:1 Non-MUX`, `SOCKS5 TCP/UDP`, and `HTTP Proxy`). Legacy `MUX v1` (`peer_version` fallback) and `-obfs` padding are permanently retired.

### 1.2 Mandatory Automatic AI Handoff Logging — EVERY BUG FIX & RELEASE
Every time an AI fixes a bug or ships an optimization batch, it **MUST update `AI_HANDOFF.md` in the same engineering cycle** following this exact order:

```text
Astra review
   ↓
implement change in src/main.rs
   ↓
run validation (python scripts/ci_guard.py + unit tests)
   ↓
record entry in AI_HANDOFF.md & CHANGELOG.md
   ↓
declare completion
```

Each handoff entry must contain:
- **Bug / Objective:** exact symptom, bottleneck, or capability and affected path.
- **Root cause:** evidence-based technical cause.
- **Astra review:** concurrency, lifecycle, protocol, security, and performance invariants verified.
- **Change:** exact modules/functions and behavioral change.
- **Validation:** `python scripts/ci_guard.py` output, unit test counts, and CI/benchmark results.
- **Status:** fixed / validated / released.
- **Remaining risk:** known untested paths or follow-up items.
- **Next action:** the single most important next engineering step.

---

## 2. Foundational Baseline Architecture (`RushWay v0.0.83` ↔ `GoWay v1.8.36`)

### 2.1 Transport & Proxy Matrix
| Layer | Supported Modes & Interop Guarantees (`RushWay v0.0.83` ↔ `GoWay v1.8.36`) |
| :--- | :--- |
| **Inbound Local Proxy** | Single-port auto-detection of **SOCKS5** (`CONNECT` + `UDP ASSOCIATE`), **HTTP `CONNECT`**, and **HTTP Origin-Form Proxy** (RFC 7230 origin-form rewrite with `Proxy-Authorization` & `Proxy-Connection` stripped). |
| **Upstream Transports** | `ws://` (Plain WebSocket), `wss://` (TLS 1.2/1.3 WebSocket with 7-browser fingerprint rotation & `-fakehost` CDN IP fallback), `quic://` / `quic+tls://` (RFC 9000 Quinn with multi-connection pooling, `webpki-roots` / `--no-verify-ssl` TLS verification, and `SHA-256(key)` / legacy plaintext stream auth). |
| **Tunnel Handshake** | First WebSocket binary frame carries XOR-encrypted `"MUX\n"`, `"UDP\n"`, or `"host:port\n"` (prefixed with `<key> ` when `-k` is set); server replies with XOR-encrypted `"OK\n"`. All handshakes use 16 KiB `BufReader` (`read_http_headers_timeout_buf`) for single-syscall header parsing. |
| **Multiplexing (`-mux`)** | **Pure MUX v2** (`MUX_PROTO_VERSION = 2`), default `8` parallel sessions (`-mux-sessions 1..64`), congestion-aware session selection (`active * 4 + writer_queued`). |
| **Non-MUX (`-no-mux`)** | Pre-warmed 1:1 connection pool (`PlainNonMuxPool` / `NonMuxPool`), XOR-encrypted handshake followed by unencrypted data frames (GoWay `handleServer` / `handleClient` parity) with half-close `shutdown(Write)` support. |

### 2.2 Pure MUX v2 Wire Protocol & Flow-Control Invariants
1. **7-Byte MUX Header (`MUX_HEADER_LEN = 7`):**
   - Layout: `[stream_id: u32 BE (0..4)][cmd_or_jumbo: u8 (4)][len_lo: u16 BE (5..7)]`.
   - Standard frames (`cmd_or_jumbo & 0x80 == 0`): `Syn = 0x01`, `Data = 0x02`, `Fin = 0x03`, `Rst = 0x04`, `Version = 0x05`, `Window = 0x06`, payload length `0..=65,535 B` (`MAX_MUX_PAYLOAD`).
   - **23-Bit Jumbo `Data` frames (`cmd_or_jumbo & 0x80 != 0`):** byte 4 stores `0x80 | ((len >> 16) as u8)` and bytes 5..7 store `(len & 0xFFFF) as u16 BE`, encoding `Data` payloads from `65,536 B` up to `MAX_MUX_V2_PAYLOAD = 512 KiB`.
   - **0-RTT Immediate Jumbo Chunking:** Both client (`relay_client_mux_stream`) and server (`target_to_mux`) unconditionally chunk `Data` reads at `MAX_MUX_V2_CHUNK = 129,600 B` from byte 0 without waiting for `VERSION` negotiation.
   - **Coalesced Multi-MUX Super-Packets:** `MuxFrame::decode_v2_coalesced` (matching GoWay `isValidCoalescedMuxTail`) unpacks multiple concatenated MUX frames from a single WebSocket binary frame, fast-pathing single frames with 0 allocations and transferring buffer ownership in-place on the final frame.
2. **Session & Per-Stream Flow Control (`CreditGate`):**
   - Upon session creation, both client and server send a coalesced flight containing `MuxCommand::Version` (`stream_id = 0`, `[2, win16_hi, win16_lo]`) and, when `mux_initial_window_kib > 65535`, `MuxCommand::Window` (`stream_id = 0`, 4-byte `u32` KiB up to `1 GiB = 1,048,576 KiB`).
   - `CreditGate::enable` and `CreditGate::upgrade_window` monotonically enable/expand live stream windows lock-free.
   - Receiving `MuxCommand::Rst` immediately calls `gate.close()` and `writer.drop_stream(stream_id)`.
3. **Dual-Channel DRR Outbound Scheduler (`MuxFrameWriter` & `Scheduler`):**
   - `DRR_QUANTUM = 128 KiB` (`131,072 B`, matching GoWay `v1.8.36` `muxDRRQuantum = 128 KiB`), `DRR_MAX_DEFICIT = 512 KiB`. Because `MAX_MUX_V2_CHUNK (129,600 B) + 7 B (MUX) + 14 B (WS) = 129,621 B <= 131,072 B` (and even with GoWay's `+28 B` AEAD tag `= 129,635 B <= 131,072 B`), each `128 KiB` DRR round dispatches exactly 1 full Jumbo frame with only `1,451 B` (`1.1%`) residual deficit, preventing multi-frame burst head-of-line jitter.
   - `ctrl_tx` carries `Syn`, `Rst`, `Window`, `Version`, WebSocket `Ping`/`Pong`, and `drop_stream` signals; `data_tx` carries `Data` and `Fin`.
   - **Ordering Invariant 1 (`SYN` before `DATA`):** `writer_loop` drains `ctrl_rx` unconditionally both before AND immediately after draining `data_rx`, so `DATA` can never overtake `SYN` across channels.
   - **Ordering Invariant 2 (`DATA` before `FIN`):** `Fin` is routed through `data_tx` into the stream's FIFO `StreamQueue`, so `FIN` never overtakes prior `DATA`.
   - **Ordering Invariant 3 (`RST` / `drop_stream` Purge):** `Rst` and `drop_stream(stream_id)` purge both `Scheduler::streams[sid]` and any in-flight `data_rx` frames for `sid` in the same `writer_loop` turn, recycling `ENCODE_POOL` buffers immediately and guaranteeing no `DATA` is emitted after `RST`.

### 2.3 Adaptive Memory Tiers & CLI Parity
- **Memory Detection:** Reads `RUSHWAY_MEM_MIB` / `GOWAY_MEM_MIB`, Linux `/proc/meminfo` (`MemTotal`), and cgroup v1/v2 limits (`memory.max` / `memory.limit_in_bytes`), or Windows `GlobalMemoryStatusEx`.
  - `Low-RAM (<= 640 MiB)`: `4 MiB` window, `512 KiB` refresh, `768` stream queue cap.
  - `Balanced (641..=2048 MiB)`: `16 MiB` window, `2 MiB` refresh, `1536` stream queue cap.
  - `High-BDP (> 2048 MiB)`: `64 MiB` (`65,535 KiB`) window, `8 MiB` refresh, `3072` stream queue cap.
- **Window Override:** `-mux-window <KiB>` (`64..=1048576`, or `0` = auto) and `RUSHWAY_WINDOW_KIB` / `GOWAY_WINDOW_KIB` compute `(win, refresh)` via `compute_window_override`, clamping `refresh <= win_bytes / 2` to prevent credit starvation on small windows.
- **GoWay v1.8.36 CLI Flag Parity:** Supports both `-flag` and `--flag` forms for `-p`, `-l`, `-up`, `-k`, `-fakehost`, `-mux`, `-no-mux`, `-mux-sessions`, `-mux-window`, `-quic-conns`, `-W`, `-socket-buffer`, `-no-tcp-nodelay`, `-no-tcp-keepalive`, `-dns`, `-block-local`, `-no-block-local`, `-server-block-local`, `-no-server-block-local`, `-allow-open`, `-verify-ssl`, `-no-verify-ssl`, `-max-conn`, `-connection-timeout`, `-log`, `-log-file`, `-tui`, `-cpuprofile`, `-cpuprofile-duration`.

### 2.4 Data-Plane Zero-Allocation & Syscall-Reduction Engine
- **Fused SIMD Crypto (`mod crypto`, `mod ws`):** Single-pass `apply_fused_xor` combining 256 KiB SHA-256 keystream XOR + 4-byte WebSocket masking via runtime-dispatched `AVX2` (128 B/iter), `SSE2` (64 B/iter), `ARM64 NEON` (64 B/iter), and 64-bit word fallback.
- **Closed-Loop Buffer Recycling (`ENCODE_POOL` & `RELAY_POOL`):** 8-shard thread-local `ENCODE_POOL` (`32..144 KiB`) and `RELAY_POOL` with RAII guards (`OwnedMuxFrame`, `RelayBufGuard`) and explicit recycling on all `timeout(...)` and error branches.
- **Adaptive UDP Batching (`mod udp_batch`):** Linux `recvmmsg` (`UdpBatchReader`) and `sendmmsg` (`UdpBatchWriter`, up to 8 datagrams/syscall) with opportunistic immediate flush (`rd.buffer().len() < 2` or `stash.len() < 2`), `2 MiB` OS UDP socket buffers (`bind_local_udp_socket`), and coalesced multi-packet writes in QUIC UDP streams.
- **Cache-Line Isolated Telemetry (`mod stats`):** `active_conns`, `bytes_up`, and `bytes_down` each reside on a dedicated 64-byte cache line (`#[repr(align(64))] struct AlignedAtomicI64`), fed by 64 KiB `StatsBatcher` accumulators across all relay loops.

### 2.5 Security & Protocol Compliance Boundaries
- **SSRF Defense-in-Depth (`is_blocked_local_ip`):** Rejects loopback, RFC1918 private, link-local, broadcast, unspecified (`0.0.0.0/8`), multicast, IPv4 Class E (`240.0.0.0/4`), IPv6 Unique Local (`fc00::/7`), NAT64 Well-Known Prefix (`64:ff9b::/96`), 6to4 (`2002::/16`), and `localhost` / `.localhost` domains across TCP and UDP (client pre-check + server post-DNS check to defeat DNS Rebinding). RFC 6598 CGNAT (`100.64.0.0/10`) and `198.18.0.0/15` are intentionally exempted for GoWay parity.
- **UDP Return-Traffic Pinning:** `record_known_udp_target` normalizes dual-stack IPv4-mapped IPv6 addresses (`normalize_socket_addr`) and caches `last_allowed_src` so only authorized destination endpoints can send UDP packets back into the tunnel.

---

## 3. Chronological Relay Log

### Release: v0.0.85 (2026-11-14) — UTF-8 Proxy Safety, Fixed DNS SWR Cap, MUX/QUIC Teardown & Diagnostics

- **Objective:** Address all verified Round 2 audit findings without altering GoWay wire compatibility, hot-path throughput, or local socket semantics.
- **Change:**
  - `src/main.rs`:
    - `proxy`: Replaced byte-indexed `&uri[..7]` / `&uri[..8]` in `parse_http_proxy_request` with `.as_bytes().get(...)`, surfaced `ProxyParseError::HeaderTooLarge` in `read_client_proxy_request` on `>= 8192`-byte non-EOF headers without a terminator, and added unit tests `http_proxy_handles_multibyte_utf8_uri_without_panic` and `read_client_proxy_request_reports_oversized_header_too_large`.
    - `common`: Invalidated `last_stream` in `mux_client_reader_loop` when fast-path channel delivery returns `Some(Err(_))`.
    - `dns`: Introduced `CachedDnsEntry<T>` (`expires` + immutable `stale_deadline`), `bounded_insert`, and `bump_stale_cooldown` across `CACHE` and `ALL_V4_CACHE` so failed stale refreshes cannot extend cache lifetime indefinitely; added `stale_dns_cooldown_caps_at_and_expires_after_stale_deadline`.
    - `mux_writer`: Added `push_ctrl_frame` helper, drained `rst_streams` after the post-`data_rx` `ctrl_rx` drain in `writer_loop`, and returned early in `Scheduler::remove_stream` on `None`.
    - `quic` & `main`: Sent `quic_reject_local` on header write failure in `relay_quic`, reset QUIC `send` stream with `0u32` on target read error in `handle_server_stream`, and moved the non-TTY `-tui` warning after subscriber/log-file initialization.
  - `Cargo.toml`, `Cargo.lock`, `CHANGELOG.md`, `AI_HANDOFF.md`: Synchronized to `v0.0.85`.
- **Status:** Committed and pending CI + tag `v0.0.85` release verification.

### Release: v0.0.84 (2026-11-14) — UDP Epoll Readiness, Pool/QUIC Lifecycle & Boundary Hardening

- **Objective:** Apply high-certainty bugfixes from the full-codebase audit without regressing GoWay wire compatibility, MUX 0-RTT latency, or hot-path throughput.
- **Change:**
  - `src/main.rs`:
    - `udp_batch`: Wrapped `recv_batch_linux` / `send_batch_linux` in `sock.try_io(Interest::READABLE / WRITABLE, ...)`, added bounded `ConnectionReset` handling on non-Linux `try_recv_batch`, and guarded `UdpBatchWriter::push` / `flush` count invariants.
    - `mux_pool` & `wss_client`: Added `capacity_notify.notified()` + 50ms sleep backoff on saturated-pool dial failures, validated WS/WSS paths against CR/LF/whitespace, filtered empty `fakehost`, and propagated `profile_idx` into `dial_cloudflare_fallback`.
    - `runtime`, `mux_writer`, `tui`: Cleared `last_stream` on `handle_mux_parts` stream eviction, wrapped `writer_task`/`heartbeat_handle` in `AbortOnDrop`, fixed unbounded post-download read in `handle_server_tcp_parts`, capped `writer_loop` `w.shutdown()` to 2s, reserved undersized pooled encode buffers in `acquire_encode_buf_sized`, and released `save_log_file` lock before logging.
    - `quic`: Evicted connections on `open_bi` error/timeout and called `endpoint.close` before `drain_join_set` in `run_server`.
    - `proxy` & `main`: Normalized empty `-k`/`fakehost` via `sanitize_runtime_config` (plus CR/LF rejection), used earliest HTTP header terminator (`find_http_header_end`), rejected trailing garbage after bracketed IPv6 authorities, and added 3 regression unit tests.
  - `.github/workflows/release.yml`: Added `cargo test --all-targets --all-features` to the release compilation gate.
  - `Cargo.toml`, `Cargo.lock`, `CHANGELOG.md`, `AI_HANDOFF.md`: Synchronized to `v0.0.84` and corrected QUIC auth & CGNAT exemption documentation.
- **Status:** Released (`v0.0.84`, superseded by `v0.0.85`).

### Release: v0.0.83 (2026-11-09) — Epoch 0 Baseline (`RushWay v0.0.83` ↔ `GoWay v1.8.36`)

- **Bug / Objective:**
  Establish `RushWay v0.0.83` ↔ `GoWay v1.8.36` as the unified pure-MUX-v2 foundational baseline, eliminate all cross-channel ordering races and resource leaks found in full-stack code audit, optimize handshake/UDP/QUIC/stats hot paths, harden HTTP/SOCKS5/SSRF compliance, and reset `AI_HANDOFF.md` from scratch.
- **Root cause:**
  1. *MUX Ordering & Teardown:* `mux_writer::writer_loop` previously drained `ctrl_rx` only before `data_rx` (and gated `ctrl_rx` behind `WRITER_QUEUE`), allowing `DATA` to overtake `SYN` or be emitted after `RST` across channels; incoming `MuxCommand::Rst` did not call `gate.close()` on the client or purge queued outbound `DATA` frames in `Scheduler` (`drop_stream`); `Fin` slow-path task reset `pending_ctrl` to `false` after delivery.
  2. *Flow Control & CLI Parity:* Small `-mux-window` / `RUSHWAY_WINDOW_KIB` values (`64..=512 KiB`) left `mux_window_refresh` larger than `window_bytes / 2`, stalling credit refunds; GoWay `v1.8.36` flags (`-mux-window`, `-quic-conns`, `-server-block-local`, `GOWAY_MEM_MIB`) were missing in `Args`.
  3. *Syscall & Buffer Efficiency:* Client `open_ws_upstream` and `connect_wss_upstream` read HTTP upgrade responses 1 byte per syscall on unbuffered streams; `MuxClientSession::spawn_from_parts` double-wrapped `BufReader`; `timeout(..., read_frame(..., &mut frame_buf))` leaked pooled buffers on timeout; `UdpBatchWriter::flush` left stale packets queued on non-`WouldBlock` errors; `QuicClientPool` was limited to a single connection.
  4. *Protocol & SSRF Compliance:* HTTP proxy forwarded absolute-URI request lines and `Proxy-Authorization`/`Proxy-Connection` headers to origin servers; SOCKS5 `UDP ASSOCIATE` replied `0x00` before verifying upstream tunnel readiness; `is_blocked_local_ip` missed multicast, IPv4 Class E, NAT64 (`64:ff9b::/96`), and 6to4 (`2002::/16`).
- **Astra review:**
  - Verified pure MUX v2 0-RTT `MAX_MUX_V2_CHUNK` (`129,600 B`) alignment with `GoWay v1.8.36` (zero `peer_version` atomic overhead, coalesced `VERSION + WINDOW(0)` handshake flight, `DRR_QUANTUM = 128 KiB`).
  - Verified strict cross-channel `SYN -> DATA -> FIN` ordering and immediate `RST` / `drop_stream` queue purging in `writer_loop`.
  - Verified zero byte-stealing on 16 KiB `BufReader` WS/WSS handshakes, lossless `StatsBatcher::drop` flushing, and non-blocking coalesced `-log-file` background flushing.
- **Change:**
  - `src/main.rs`:
    - `adaptive`: Added `compute_window_override`, `set_window_override`, `GOWAY_MEM_MIB` / `GOWAY_WINDOW_KIB` support, and unit test `window_override_clamps_refresh_below_half_window`.
    - `common` & `runtime`: Pure MUX v2 `MAX_MUX_V2_CHUNK` (`129,600 B`) 0-RTT execution, coalesced client `VERSION + WINDOW(0)` flight, `last_stream` server cache, immediate `gate.close()` + `writer.drop_stream(id)` on `MuxCommand::Rst`, latched `pending_ctrl` on `Fin`, 16 KiB `BufReader` in `open_ws_upstream` and `handle_server_udp_parts`, `compare_exchange_weak` in `dec_active` / `try_reserve`, and `frame_buf` recycling on all timeout paths.
    - `mux_writer`: Locked `DRR_QUANTUM` at `128 KiB` (`131,072 B`, matching GoWay `v1.8.36`), added `MuxFrameWriter::drop_stream`, drained `ctrl_rx` unconditionally before and after `data_rx`, purged `rst_streams` after `data_rx` drain, and added unit tests `mux_writer_syn_precedes_data_under_concurrent_enqueue` and `mux_writer_rst_purges_queued_and_inflight_data`.
    - `quic`: Multi-connection `QuicClientPool` (`1..=16` connections with round-robin selection), `StatsBatcher` integration, deferred SOCKS5 `UDP ASSOCIATE` reply, coalesced multi-packet QUIC UDP writes, and graceful `drain_join_set` before `endpoint.close`.
    - `proxy` & `runtime`: HTTP origin-form rewrite + `Proxy-Authorization`/`Proxy-Connection` stripping, strict `is_valid_target_host` & `InvalidPort` propagation, extended `is_blocked_local_ip` (multicast, Class E, NAT64, 6to4), normalized `record_known_udp_target`, and unit tests.
    - `stats`, `udp_batch`, `udp_relay`, `wss_client`, `tui`, `tls`, `profile`, `main`: `#[repr(align(64))] AlignedAtomicI64`, `bind_local_udp_socket` (`2 MiB`), adaptive `batch_writer.push` + conditional `flush`, `UdpBatchWriter` error reset, async `spawn_blocking` log flushing, `AbortOnDrop` pool maintainers, and full GoWay `v1.8.36` CLI flag parity (`-mux-window`, `-quic-conns`, `-server-block-local`).
  - `scripts/ci_guard.py`: Added `check_src_tree_hygiene()` (`src/` must contain only `.rs` files), `crate::<mod>::<sym>` cross-module symbol verification, and tightened static unused-import analysis.
  - `Cargo.toml`, `Cargo.lock`, `CHANGELOG.md`, `AI_HANDOFF.md`: Synchronized to `v0.0.83` and rewrote `AI_HANDOFF.md` from scratch on the `RushWay v0.0.83` ↔ `GoWay v1.8.36` baseline.
- **Validation:**
  - `python scripts/ci_guard.py` passed 100% (`src/` hygiene, `v0.0.83` version sync, UTF-8 integrity, lexical delimiter balance, 20-module static semantic analysis, 92 `crate::` cross-module symbol checks, `git diff --check`).
  - 6 new regression unit tests added in `src/main.rs` and verified via GitHub Actions `RushWay CI`.
- **Status:** Fixed and validated (`v0.0.83` released).
- **Remaining risk:** None identified.
- **Next action:** Maintain `RushWay v0.0.83` ↔ `GoWay v1.8.36` interoperability and run head-to-head throughput benchmarks when requested.
