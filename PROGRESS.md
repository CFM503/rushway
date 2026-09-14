# RushWay v0.0.3 — Continuous Handoff Progress

> Compatibility baseline: GoWay v1.8.4 at `538dbee86b9fbf248a68c8c6d8eee5d6f8bdb0dc`.

## Current checkpoint — 2026-09-14

**Implementation coverage: high; final release completion is not yet claimed.** The remaining gap is executable compatibility evidence, especially GoWay interop, lifecycle/error matrices, UDP parity, stress results, and OpenWrt/real-device smoke.

### Current validated revision

- Last fully validated pre-stress run: Run #320, `34823392373`, head `323e8408a942b545a75f17f6d5594d391a93ccbf`.
- 39 unit tests passed; Rust format/check/test/release passed.
- WS E2E passed: c1 `59.15`, c8 `175.87`, c32 `164.78` MiB/s.
- WSS E2E passed: c1 `35.88`, c8 `128.99`, c32 `142.14` MiB/s.
- QUIC E2E passed: c1 `153.08`, c8 `171.59`, c32 `192.27` MiB/s.
- MUX hot-path speedup: `7032.85x` owned-reused vs copy benchmark metric.
- Repeated setup-inclusive median: c1 `131.29`, c8 `144.91`, c32 `166.62` MiB/s.
- Repeated steady-state median: c1 `58.78`, c8 `153.81`, c32 `168.70` MiB/s.
- Windows x64, Debian 12 x64 and ARMv7 Linux release artifacts passed.
- GoWay comparison remains skipped because `WAY_READ_TOKEN` is not configured.

### Newly added stress coverage

- `src/bin/e2e_bench.rs` now supports `RUSHWAY_E2E_STRESS=1`.
- Stress payload is 64 KiB to keep the test focused on stream lifecycle/concurrency rather than bulk bandwidth.
- Exact concurrency levels are `1/100/500/1000`.
- Every stream performs SOCKS5 CONNECT, sends the full payload, validates the exact echo, and must complete.
- Overall stress timeout is 180 seconds; individual flows retain the 30-second timeout.
- CI now runs the same stress matrix separately for WS, WSS and QUIC.

### Transport status

| Area | Status |
|---|---|
| Plain WS handshake/auth | Executably validated |
| Plain WS MUX | Executably validated |
| Plain WS non-MUX | Implemented; lifecycle/error matrix still pending |
| Plain WS UDP | Implemented; cross-transport executable matrix pending |
| WSS MUX/non-MUX | WSS standalone TCP path executably validated |
| WSS UDP | Implemented; executable UDP matrix pending |
| QUIC/QUIC+TLS TCP | Standalone TCP path executably validated |
| QUIC UDP | Implemented; executable interoperability matrix pending |
| SOCKS5 TCP | Basic end-to-end path validated; lifecycle/error matrix pending |
| SOCKS5 UDP | Implemented; executable matrix pending |
| HTTP CONNECT | Implemented; malformed/failure lifecycle matrix pending |

### Remaining verification gates

- [x] Rust fmt/check/test/release on validated head.
- [x] Standalone WS/WSS/QUIC TCP E2E.
- [ ] First completed CI run for WS/WSS/QUIC `1/100/500/1000` stream stress.
- [ ] SOCKS5/HTTP lifecycle, malformed input, target rejection and shutdown/error matrix.
- [ ] WS/WSS/QUIC TCP+UDP interoperability matrix.
- [ ] Additional repeated benchmark evidence across current head/stress revisions.
- [x] Windows x64 / Debian 12 x64 / ARMv7 Linux release builds.
- [ ] OpenWrt or real-device smoke.
- [ ] Actual cloud GoWay v1.8.4 bidirectional WS/WSS/QUIC TCP+UDP interoperability.
- [ ] Final v0.0.3 tag/release verification.

### AI relay rule

Every new AI must begin by reading `AI_HANDOFF.md`, `PROGRESS.md`, and `SPEC.md`, then inspect the newest CI run before modifying protocol behavior. Record exact run IDs, commit SHAs, test output and blockers. A skipped GoWay job must never be reported as successful interoperability.

### Three-file relay contract

1. `AI_HANDOFF.md` — chronological engineering decisions and next action.
2. `PROGRESS.md` — compact current-state dashboard.
3. `SPEC.md` — source-derived GoWay compatibility contract.
