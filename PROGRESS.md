# RushWay v0.0.2 — Continuous Handoff Progress

> Compatibility baseline: GoWay v1.8.4 at `538dbee86b9fbf248a68c8c6d8eee5d6f8bdb0dc`.

## Current checkpoint — 2026-09-13

**Implementation coverage: ~99% estimate. Overall project/release completion: ~85% estimate.** These are engineering estimates, not test scores.

### Newly completed source work in this continuation

- [x] MUX CONNECT now waits for the first upstream logical-stream response; server-side `RST` becomes SOCKS5 General Failure / HTTP 502 instead of premature CONNECT success.
- [x] Shared TCP socket policy is reusable by pooled plain-WS MUX and WSS upstream sockets, covering NODELAY, keepalive and optional send/receive buffer sizing.
- [x] WSS non-MUX target rejection now returns the same local failure semantics instead of silently closing.
- [x] WSS MUX first-response handling now mirrors the plain-WS MUX path.
- [x] QUIC pooled connection already retries once after `open_bi` failure; target-dial rejection now maps to local SOCKS5 failure / HTTP 502.
- [x] QUIC server target TCP sockets now receive the shared socket policy after successful connect.
- [x] `proxy.rs` now exposes and tests an RFC1928 General Failure response primitive.

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
| Plain WS MUX | `src/mux_pool.rs`, `src/runtime.rs` | Implemented; executable evidence pending |
| Plain WS non-MUX | `src/nonmux.rs` | Implemented; executable evidence pending |
| Plain WS UDP | runtime/mux client paths | Implemented; executable evidence pending |
| WSS MUX/non-MUX/UDP | `src/wss_client.rs` | Implemented; executable evidence pending |
| QUIC/QUIC+TLS TCP/UDP | `src/quic.rs` | Implemented; executable evidence pending |
| SOCKS5 TCP/UDP | `src/proxy.rs` + transport frontends | Implemented; cross-transport edge parity pending |
| HTTP CONNECT | `src/proxy.rs` + transport frontends | Implemented; malformed/error lifecycle parity pending |

### DNS coverage

- [x] Remote DNS configuration.
- [x] UDP query.
- [x] TCP retry for truncated UDP response.
- [x] System DNS fallback.
- [x] Positive cache.
- [x] Transaction-ID validation.
- [x] Server target.
- [x] Plain WS MUX upstream.
- [x] Plain WS non-MUX upstream/target.
- [x] WSS upstream.
- [x] QUIC upstream.
- [ ] Executable cache/fallback/transaction-ID tests on the final release commit.

### Remaining code work

1. Audit SOCKS5 error/FRAG/close parity and HTTP CONNECT malformed/error lifecycle across every transport, not just the primary MUX paths.
2. Finish exact QUIC dead-IP/connection-state/TLS-SNI parity against GoWay with executable interop evidence.
3. Remove or formally justify the remaining compatibility stubs (`-tui`, file logging and CPU profiling) against the release scope.
4. Add/execute focused integration tests for target-dial failure, first-response RST, and cross-path socket-policy behavior.

### Verification gate

**Current-head executable verification is still not passed.** HEAD is `75610b2123433a2b034eba577b61ddc161ac159f`. Both the `RushWay CI` run `34764563674` and `RushWay Build Smoke` run `34764563695` failed at the workflow-job level before exposing any steps; the `test`/smoke logs endpoint currently returns `BlobNotFound`. This is inconclusive GitHub Actions infrastructure/setup behavior, not evidence that the code compiled or failed to compile.

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

The package version is `0.0.2`, but `v0.0.2` is **not claimed as an existing Git tag**. The current GitHub connector exposes commit/branch writes but no tag/ref creation write operation. Tagging must be done against the final verified release commit once the required write capability is available.

### Three-file relay contract

Only these files are canonical AI handoff state:

1. `AI_HANDOFF.md`
2. `PROGRESS.md`
3. `SPEC.md`
