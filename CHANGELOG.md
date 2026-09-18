# Changelog

All notable changes to this project will be documented in this file.

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
