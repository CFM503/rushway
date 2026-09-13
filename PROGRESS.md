# RushWay v0.0.2 — Continuous Handoff Progress

> Compatibility baseline: GoWay v1.8.4 at `538dbee86b9fbf248a68c8c6d8eee5d6f8bdb0dc`.

## Current checkpoint — 2026-09-13

**Implementation coverage: ~98% estimate. Overall project/release completion: ~80% estimate.** This is an engineering estimate, not a test score.

### Completed compatibility work

- [x] GoWay single-hyphen multi-character CLI forms and boolean `=true/=false` forms.
- [x] `-log`, `-log-file`, `-tui`, `-version`, `-cpuprofile`, `-cpuprofile-duration` accepted; non-core dashboard/profile outputs remain explicit stubs.
- [x] CLI/JSON propagation for buffer size, socket buffer, TCP_NODELAY, TCP keepalive, max connections, block-local, timeout and DNS selection.
- [x] Central DNS resolver: 5s timeout, UDP, TCP retry on truncation, system fallback, 5-minute positive cache, IP-literal bypass.
- [x] DNS integrated into server target dialing, plain WS MUX/non-MUX, WSS and QUIC hostname dialing/target resolution.
- [x] MUX proxy success now waits for logical-stream acquisition instead of returning immediate premature success.
- [x] `Cargo.toml` package version is now `0.0.2`.

### Transport status

| Area | Source path | Current status |
|---|---|---|
| Plain WS handshake/auth | `src/ws.rs` | Implemented |
| Plain WS MUX | `src/mux_pool.rs`, `src/runtime.rs` | Implemented; executable evidence pending |
| Plain WS non-MUX | `src/nonmux.rs` | Implemented; executable evidence pending |
| Plain WS UDP | runtime/mux client paths | Implemented; executable evidence pending |
| WSS MUX/non-MUX/UDP | `src/wss_client.rs` | Implemented; executable evidence pending |
| QUIC/QUIC+TLS TCP/UDP | `src/quic.rs` | Implemented; executable evidence pending |
| SOCKS5 TCP/UDP | `src/proxy.rs` + transport frontends | Implemented; edge parity pending |
| HTTP CONNECT | `src/proxy.rs` + transport frontends | Implemented; edge parity pending |

### DNS coverage

- [x] Remote DNS configuration.
- [x] UDP query.
- [x] TCP retry for truncated UDP response.
- [x] System DNS fallback.
- [x] Positive cache.
- [x] Server target.
- [x] Plain WS MUX upstream.
- [x] Plain WS non-MUX upstream/target.
- [x] WSS upstream.
- [x] QUIC upstream.
- [ ] Transaction-ID validation and dedicated cache/fallback executable tests.

### Remaining code work

1. Finish MUX first-response/RST handling so target dial failures map to local SOCKS5/HTTP failure instead of success.
2. Apply socket buffer/keepalive/NODELAY uniformly to pooled MUX and WSS upstream TCP sockets.
3. Complete SOCKS5 target-failure REP/error/FRAG/close parity and HTTP CONNECT malformed/error lifecycle parity.
4. Audit QUIC retry/dead-IP/pool/TLS-SNI behavior against GoWay.
5. Add DNS transaction-ID validation and focused cache/fallback tests.

### Verification gate

Current-head executable verification is **not passed**. The latest `v0.0.2` push (`3d23529bc08d09e7f0539b711b9a15c89af76296`) triggered run `34764182241`; the `test` job failed without exposed steps/logs and downstream artifact jobs were skipped. This is treated as inconclusive Actions infrastructure/setup behavior, not as a proven compiler/test result.

Still required before release:

- `cargo fmt -- --check`
- `cargo check --all-targets`
- `cargo test --all-targets --all-features`
- `cargo build --release`
- GoWay ↔ RushWay bidirectional WS/WSS/QUIC TCP+UDP interop
- 1/100/500/1000 stream stress and mixed slow/fast workloads
- c1/c8/c32 benchmarks
- Windows x64 / Debian 12 x64 / ARMv7 smoke artifacts
- `v0.0.2` release smoke test
- `v0.0.2` tag creation and verification

### Three-file relay contract

Only these files are canonical AI handoff state:

1. `AI_HANDOFF.md`
2. `PROGRESS.md`
3. `SPEC.md`
