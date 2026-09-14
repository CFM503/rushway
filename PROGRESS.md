# RushWay v0.0.3 — Continuous Handoff Progress

> Compatibility baseline: GoWay v1.8.4 at `538dbee86b9fbf248a68c8c6d8eee5d6f8bdb0dc`.

## Current checkpoint — 2026-09-14

**Implementation coverage: high; final release completion is not yet claimed.** The current blocker is executable high-concurrency validation: WS stress passes at 1, 100 and 500 streams but the 1000-stream burst test currently fails with `early eof`.

### Current validated revision

- Last fully validated pre-stress run: Run #320, `34823392373`, head `323e8408a942b545a75f17f6d5594d391a93ccbf`.
- 39 unit tests passed; Rust format/check/test/release passed.
- WS E2E passed: c1 `59.15`, c8 `175.87`, c32 `164.78` MiB/s.
- WSS E2E passed: c1 `35.88`, c8 `128.99`, c32 `142.14` MiB/s.
- QUIC E2E passed: c1 `153.08`, c8 `171.59`, c32 `192.27` MiB/s.
- Windows x64, Debian 12 x64 and ARMv7 Linux release artifacts passed.
- GoWay comparison remains skipped because `WAY_READ_TOKEN` is not configured.

### Latest stress result

- Run #324: `34825498345`.
- WS stress results:
  - 1 stream: passed, 42 ms
  - 100 streams: passed, 105 ms
  - 500 streams: passed, 1161 ms
  - 1000 streams: failed with generic `early eof`
- WSS and QUIC stress steps were not reached because the WS stress step failed first.
- The current harness does not identify the failing flow index or exact phase, so this result is a diagnostic blocker rather than proof of a 500-stream hard limit.

### Newly added stress coverage

- `src/bin/e2e_bench.rs` supports `RUSHWAY_E2E_STRESS=1`.
- Stress payload is 64 KiB and exact concurrency levels are `1/100/500/1000`.
- Each stream performs SOCKS5 CONNECT, sends the payload, validates the exact echo, and must complete.
- Overall stress timeout is 180 seconds; individual flows retain the 30-second timeout.
- CI runs dedicated stress coverage for WS, WSS and QUIC, although later transports are skipped when the first WS stress step fails.

### Immediate next action

1. Enhance `e2e_bench.rs` diagnostics with flow index and failure phase.
2. Add `RUSHWAY_E2E_STRESS_PATTERN=burst|staged`.
3. Run WS 1000 burst and staged modes separately.
4. After WS is understood, run WSS and QUIC staged stress.
5. Do not create the final v0.0.3 tag/release until stress, lifecycle/error, UDP, GoWay and real-device gates are satisfied.

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
- [ ] WS 1000-stream burst and staged stress diagnosis/fix.
- [ ] WSS/QUIC 1/100/500/1000 stream stress.
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
