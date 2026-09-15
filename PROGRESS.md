# RushWay v0.0.3 — Continuous Handoff Progress

> Compatibility baseline: GoWay v1.8.4 at `538dbee86b9fbf248a68c8c6d8eee5d6f8bdb0dc`.

## Current checkpoint — 2026-09-15

**Implementation coverage: high; final release completion is not yet claimed.** The 1000-stream diagnostic results now separate transports: WS and QUIC pass at 1000 with 64 physical MUX sessions, while WSS still fails at flow 406 with `early eof`. The 1000 diagnostics remain non-blocking evidence gathering.

### Native Session Manager alignment in progress

- Branch: `astra/go-style-mux-session-manager`.
- Plain-WS pooled MUX now binds the local proxy before upstream physical-session replenishment instead of synchronously prewarming the full configured pool.
- Physical session creation remains serialized by `session_creation`; closed sessions are pruned during maintenance and acquisition.
- Session selection snapshots active counts before choosing the least-loaded session.
- No MUX wire-format, target-policy, or stream-limit change.
- This is implemented but not yet accepted; executable CI evidence is required.

### Current validated revision

- Latest merged revision: `89615e5039207d2e51628e968e383677f6a11a8b` (PR #11 merge).
- PR #11 adds non-blocking WSS and QUIC 1000-stream diagnostics using staged stress, 64 physical MUX sessions, and `ulimit -n 8192`.
- PR #11 pre-merge CI Run #383: `34907505435` passed the full test job and all build jobs.
- 39 unit tests passed; Rust format/check/test/release passed.
- Standalone WS/WSS/QUIC TCP E2E passed.
- Windows x64, Debian 12 x64 and ARMv7 Linux release artifacts passed.
- GoWay comparison remains skipped because `WAY_READ_TOKEN` is not configured.

### Latest 1000-stream diagnostic evidence

- WS 1000: **passed** with 64 MUX sessions, staged pattern, 1000/1000 completed.
- WSS 1000: **failed** at flow 406 with `SOCKS5 stage connect_reply_read timed out after 10s`, despite 64 MUX sessions and `ulimit -n 8192`.
- QUIC 1000: **passed** with 64 MUX sessions, staged pattern, 1000/1000 completed in 571 ms.
- The same Run #383 also passed WS/WSS/QUIC 1/100/500 blocking stress.
- This is strong evidence that the 1000-stream issue is transport-specific for WSS rather than a universal MUX stream-capacity failure. It is not yet proof that WS/WSS/QUIC 1000 are release-ready because WS/QUIC need repeated evidence and WSS still fails.

### Latest main CI

- Run #384: `34910162991`, merge commit `89615e5039207d2e51628e968e383677f6a11a8b`.
- Its earlier core stages passed; the GoWay comparison is skipped when `WAY_READ_TOKEN` is absent.
- Do not treat a skipped comparison as interoperability evidence.

### Transport status

| Area | Status |
|---|---|
| Plain WS handshake/auth | Executably validated |
| Plain WS MUX | Executably validated |
| Plain WS 1000 diagnostic | Passed once at 64 MUX sessions; repeated acceptance pending |
| Plain WS non-MUX | Implemented; lifecycle/error matrix still pending |
| Plain WS UDP | Implemented; cross-transport executable matrix pending |
| WSS MUX/non-MUX | WSS standalone TCP path executably validated |
| WSS 1000 diagnostic | **Failing** at flow 406 under 64-session staged diagnostic on baseline; PR #18 startup fix has full CI success but repeatable 1000 evidence pending |
| WSS UDP | Implemented; executable UDP matrix pending |
| QUIC/QUIC+TLS TCP | Standalone TCP path executably validated |
| QUIC 1000 diagnostic | Passed once at 64 MUX sessions; repeated acceptance pending |
| QUIC UDP | Implemented; executable interoperability matrix pending |
| SOCKS5 TCP | Basic end-to-end path validated; lifecycle/error matrix pending |
| SOCKS5 UDP | Implemented; executable matrix pending |
| HTTP CONNECT | Implemented; malformed/failure lifecycle matrix pending |

### Remaining verification gates

- [x] Rust fmt/check/test/release on validated revision.
- [x] Standalone WS/WSS/QUIC TCP E2E.
- [ ] WS 1000 repeated acceptance stress.
- [ ] WSS 1000 diagnosis/fix and repeated acceptance stress.
- [ ] QUIC 1000 repeated acceptance stress.
- [ ] SOCKS5/HTTP lifecycle, malformed input, target rejection and shutdown/error matrix.
- [ ] WS/WSS/QUIC TCP+UDP interoperability matrix.
- [ ] Additional repeated benchmark evidence across current head/stress revisions.
- [x] Windows x64 / Debian 12 x64 / ARMv7 Linux release builds.
- [ ] OpenWrt or real-device smoke.
- [ ] Actual cloud GoWay v1.8.4 bidirectional WS/WSS/QUIC TCP+UDP interoperability.
- [ ] Final v0.0.3 release/tag verification.

### Known CI warnings / cleanup backlog

Run #383 is green but reports existing compiler warnings, including unused imports/functions and unreachable expressions in `runtime.rs`, `mux_pool.rs`, `udp_relay.rs`, `quic.rs`, and `wss_client.rs`. These are cleanup items, not currently accepted as functional blockers; avoid broad cleanup until the transport/lifecycle gates are stabilized.

### AI relay rule

Every new AI must begin by reading `AI_HANDOFF.md`, `PROGRESS.md`, and `SPEC.md`, then inspect the newest CI run before modifying protocol behavior. Record exact run IDs, commit SHAs, test output and blockers. A skipped GoWay job must never be reported as successful interoperability.

### Three-file relay contract

1. `AI_HANDOFF.md` — chronological engineering decisions and next action.
2. `PROGRESS.md` — compact current-state dashboard.
3. `SPEC.md` — source-derived GoWay compatibility contract.
