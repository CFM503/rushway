# RushWay v0.0.1 — Continuous Handoff Progress

> Compatibility baseline: GoWay v1.8.4 at `538dbee86b9fbf248a68c8c6d8eee5d6f8bdb0dc`.

## Current checkpoint — 2026-09-13

**Implementation coverage: ~98% estimate. Overall project/release completion: ~80% estimate.** This is an engineering estimate, not a test score.

### Completed compatibility work

- [x] GoWay single-hyphen multi-character CLI forms and boolean `=true/=false` forms.
- [x] `-log`, `-log-file`, `-tui`, `-version`, `-cpuprofile`, `-cpuprofile-duration` accepted; non-core dashboard/profile outputs remain explicit stubs.
- [x] CLI/JSON propagation for buffer size, socket buffer, TCP_NODELAY, TCP keepalive, max connections, block-local, timeout and DNS selection.
- [x] Central DNS resolver: 5s timeout, UDP, TCP retry on truncation, system fallback, 5-minute positive cache, IP-literal bypass.
- [x] DNS integrated into server target dialing, plain WS MUX/non-MUX, WSS and QUIC hostname dialing/target resolution.
- [x] MUX proxy success now waits for physical logical-stream establishment instead of returning premature success.

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

1. Apply socket buffer/keepalive/NODELAY uniformly to pooled MUX and WSS upstream TCP sockets.
2. Complete SOCKS5 target-failure REP/error/FRAG/close parity and HTTP CONNECT malformed/error lifecycle parity.
3. Audit QUIC retry/dead-IP/pool/TLS-SNI behavior against GoWay.

### Verification gate

Current-head executable verification is **not passed**. Earlier same-day CI run `34756277983` succeeded, but continuation runs terminate after roughly four seconds with `failure` and the connector exposes neither useful steps nor logs. Treat those runs as inconclusive infrastructure/setup failures.

Still required:

- `cargo fmt -- --check`
- `cargo check --all-targets`
- `cargo test --all-targets --all-features`
- `cargo build --release`
- GoWay ↔ RushWay bidirectional WS/WSS/QUIC TCP+UDP interop
- 1/100/500/1000 stream stress and mixed slow/fast workloads
- c1/c8/c32 benchmarks
- Windows x64 / Debian 12 x64 / ARMv7 smoke artifacts
- `v0.0.1` release smoke test

### Three-file relay contract

Only these files are canonical AI handoff state:

1. `AI_HANDOFF.md`
2. `PROGRESS.md`
3. `SPEC.md`
