# RushWay AI Relay Handoff

> Chronological AI-to-AI engineering handoff. Read this together with `PROGRESS.md` and `SPEC.md` before changing code.

## 2026-09-14 — v0.0.3 test-track checkpoint

### Target
- Repository: `CFM503/rushway`
- Target version: `v0.0.3` test build; not a final release
- GoWay baseline: v1.8.4, pinned commit `538dbee86b9fbf248a68c8c6d8eee5d6f8bdb0dc`
- Branch: `main`

### Latest validated CI — Run #320
- Run #320: `34823392373`, head `323e8408a942b545a75f17f6d5594d391a93ccbf` — full core CI passed.
- Rust format/check/test/release passed; 39 unit tests passed.
- Standalone WS, WSS and QUIC TCP E2E passed.
- Windows x64, Debian 12 x64, and ARMv7 Linux release artifacts passed and were uploaded.
- GoWay comparison job remains intentionally skipped because `WAY_READ_TOKEN` is unset. A skipped comparison is not interoperability evidence.

### Latest stress attempt — Run #324
- Run #324: `34825498345`.
- WS stress output:
  - 1 stream: passed, 42 ms
  - 100 streams: passed, 105 ms
  - 500 streams: passed, 1161 ms
  - 1000 streams: failed with `early eof`
- WSS and QUIC stress were not reached because the WS stress step failed first.
- The harness currently reports only generic `early eof`; it does not identify the flow index, failure phase, or sufficiently useful child-process diagnostics.
- This is not evidence of a hard 500-stream limit. It is an unresolved high-concurrency diagnostic issue.

### Important protocol fixes already proven
1. `916a38b4279da84a6192aef0be4661981e1d351a` — MUX SYN completion now sends an encrypted zero-length `MuxCommand::Data`, fixing the deterministic CONNECT deadlock found in Run #292.
2. `302be63036fe4435d99efbc5bdf890c22ace3f43` — benchmark child startup gets fixed auth and `--no-block-local`; this fixed the Run #300 benchmark self-check failure without changing proxy protocol behavior.
3. ARMv7 SHA-256 path now uses `ring`; `sha1` remains direct for WebSocket handshake compatibility.

### Release gate
Do not call v0.0.3 100% and do not create a final tag/release yet.

Current gates:
1. [ ] Actual cloud GoWay v1.8.4 bidirectional interoperability.
2. [x] Standalone WSS executable server/client E2E.
3. [ ] SOCKS5/HTTP lifecycle and malformed/error matrix with executable evidence.
4. [ ] WS/WSS/QUIC TCP+UDP interoperability matrix.
5. [ ] 1/100/500/1000 stream stress across all transports. WS 1000 currently fails with generic early EOF.
6. [ ] Repeated c1/c8/c32 benchmark evidence over additional commits/environments.
7. [x] Windows x64 / Debian 12 x64 / ARMv7 Linux release builds.
8. [ ] OpenWrt/real-device smoke.
9. [ ] Final v0.0.3 release/tag verification.

### Next AI action
1. Enhance `src/bin/e2e_bench.rs` with flow index and failure-phase diagnostics.
2. Add `RUSHWAY_E2E_STRESS_PATTERN=burst|staged`; staged mode should launch batches while retaining all task handles so final concurrency remains 1000.
3. Run WS 1000 burst and staged modes separately.
4. After WS is understood, run WSS and QUIC staged stress.
5. Then add executable SOCKS5/HTTP lifecycle/error-matrix coverage and TCP+UDP interoperability tests.

### Relay discipline
- Keep `AI_HANDOFF.md`, `PROGRESS.md`, and `SPEC.md` synchronized with current evidence.
- CI green means the tested revision passed; it does not prove untested GoWay interoperability.
- Record commit SHA, workflow run ID, exact test output, and any blocker after every meaningful milestone.
- Never replace failed or skipped evidence with source-level assumptions.

## Three-file relay contract
1. `AI_HANDOFF.md` — decisions, commits, blockers, next action.
2. `PROGRESS.md` — compact progress dashboard.
3. `SPEC.md` — source-derived GoWay compatibility contract.

Never call the project 100% complete merely because source paths exist.
