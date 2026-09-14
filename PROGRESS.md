# RushWay v0.0.2 — Continuous Handoff Progress

> Compatibility baseline: GoWay v1.8.4 at `538dbee86b9fbf248a68c8c6d8eee5d6f8bdb0dc`.

## Current checkpoint — 2026-09-14

**Implementation coverage: ~99% estimate. Overall project/release completion: ~85% estimate.** These are engineering estimates, not test scores.

### Newly completed source work in this continuation

- [x] MUX CONNECT now waits for the first upstream logical-stream response; server-side `RST` becomes SOCKS5 General Failure / HTTP 502 instead of premature CONNECT success.
- [x] Shared TCP socket policy is reusable by pooled plain-WS MUX and WSS upstream sockets, covering NODELAY, keepalive and optional send/receive buffer sizing.
- [x] WSS non-MUX target rejection now returns the same local failure semantics instead of silently closing.
- [x] WSS MUX first-response handling now mirrors the plain-WS MUX path.
- [x] QUIC pooled connection already retries once after `open_bi` failure; target-dial rejection now maps to local SOCKS5 failure / HTTP 502.
- [x] QUIC server target TCP sockets now receive the shared socket policy after successful connect.
- [x] `proxy.rs` now exposes and tests an RFC1928 General Failure response primitive.
- [x] Dependency stack is pinned for Rust 1.85-era toolchains: Quinn 0.11.9, ring-backed TLS, time 0.3.44 and cpufeatures 0.2.17.
- [x] Rustfmt now passes on the repaired source tree in Actions.

### Completed compatibility work

- [x] GoWay single-hyphen multi-character CLI forms and boolean `=true/=false` forms.
- [x] `-log`, `-log-file`, `-tui`, `-version`, `-cpuprofile`, `-cpuprofile-duration` accepted; non-core dashboard/profile outputs remain explicit stubs.
- [x] CLI/JSON propagation for buffer size, socket buffer, TCP_NODELAY, TCP keepalive, max connections, block-local, timeout and DNS selection.
- [x] Central DNS resolver: 5s timeout, UDP, TCP retry on truncation, system fallback, 5-minute positive cache, IP-literal bypass and transaction-ID validation.
- [x] DNS integrated into server target dialing, plain WS MUX/non-MUX, WSS and QUIC hostname dialing/target resolution.
- [x] `Cargo.toml` package version is now `0.0.2`.

### Transport status

| Area | Source path | Current status |
|---|---|---|
| Plain WS handshake/auth | `src/ws.rs` | Implemented |
| Plain WS MUX | `src/mux_pool.rs`, `src/runtime.rs` | Implemented; executable compile currently being repaired |
| Plain WS non-MUX | `src/nonmux.rs` | Implemented; executable compile currently being repaired |
| Plain WS UDP | runtime/mux client paths | Implemented; executable compile currently being repaired |
| WSS MUX/non-MUX/UDP | `src/wss_client.rs` | Implemented; executable compile currently being repaired |
| QUIC/QUIC+TLS TCP/UDP | `src/quic.rs` | Implemented; executable compile currently being repaired |
| SOCKS5 TCP/UDP | `src/proxy.rs` + transport frontends | Implemented; cross-transport edge parity pending |
| HTTP CONNECT | `src/proxy.rs` + transport frontends | Implemented; malformed/error lifecycle parity pending |

### Current compiler blocker

Actions now exposes full compiler diagnostics through a temporary artifact workflow. The active source-repair pass is correcting 31 mechanical Rust errors across MUX pool lifecycles, QUIC API names, socket keepalive ownership, WSS state derives, runtime IP matching and Clap range parsers. `cargo fmt --check` is green; `cargo check --all-targets --all-features` is the current gate.

### Verification gate

**Current-head executable verification is still not passed.** The clean formal CI has reached the real `cargo check` stage; the GoWay comparison job is skipped only because `CFM503/way` is private and the optional `WAY_READ_TOKEN` is not configured.

The final 100% gate still requires successful executable evidence for:

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

### Release/tag constraint

The package version is `0.0.2`, but `v0.0.2` is **not claimed as an existing Git tag**. Tagging must be done against the final verified release commit once the required write capability is available.

### Three-file relay contract

Only these files are canonical AI handoff state:

1. `AI_HANDOFF.md`
2. `PROGRESS.md`
3. `SPEC.md`
