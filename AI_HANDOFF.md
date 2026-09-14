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
- MUX hot-path benchmark: copy `2550.87 MB/s`, owned-reused `17939922.79 MB/s`, speedup `7032.85x`.
- Standalone WS E2E: c1 `59.15`, c8 `175.87`, c32 `164.78` MiB/s.
- Standalone WSS E2E: c1 `35.88`, c8 `128.99`, c32 `142.14` MiB/s. This is executable two-process server/client evidence, not merely a compile test.
- Standalone QUIC E2E: c1 `153.08`, c8 `171.59`, c32 `192.27` MiB/s.
- Repeated setup-inclusive RushWay median: c1 `131.29`, c8 `144.91`, c32 `166.62` MiB/s.
- Repeated steady-state RushWay median: c1 `58.78`, c8 `153.81`, c32 `168.70` MiB/s.
- Windows x64, Debian 12 x64, and ARMv7 Linux release artifacts all passed and were uploaded.
- GoWay comparison job remains intentionally skipped because `WAY_READ_TOKEN` is unset. A skipped comparison is not interoperability evidence.

### WSS implementation and proof
- `src/main.rs` has standalone WSS server mode via `--wss-server`.
- The WSS server terminates TLS, validates the existing RFC6455 handshake, connects to a private loopback plain-WS runtime, performs the internal WS handshake, then bridges already-framed WebSocket bytes with `copy_bidirectional`.
- `src/tls.rs` provides a standalone self-signed `localhost` acceptor using rcgen/rustls with HTTP/1.1 ALPN.
- The design deliberately reuses the existing WS/MUX protocol path instead of maintaining a second TLS-specific MUX implementation.

### E2E harness state
- `src/bin/e2e_bench.rs` runs real separate RushWay server/client processes against an independent local echo target.
- Normal E2E covers c1/c8/c32 with 4 MiB payloads and a 30s per-flow timeout.
- WSS and QUIC are selected by `RUSHWAY_E2E_TRANSPORT`.
- Commit `98369a709095c25fc80b666c2f792cc3d9e6124b` adds stress mode through `RUSHWAY_E2E_STRESS` using a 64 KiB payload and exact stream counts `1/100/500/1000`.
- Stress mode reuses the same SOCKS5 CONNECT + echo validation, requires every stream to complete, and has a 180s overall timeout.
- Commit `dd7aebf22fa17cd337102e581d277dc43a7fa4b7` wires dedicated WS, WSS, and QUIC 1/100/500/1000-stream CI steps.

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
5. [ ] 1/100/500/1000 stream stress across all transports — implementation and CI wiring are committed; current head still needs the first completed stress run.
6. [ ] Repeated c1/c8/c32 benchmark evidence over additional commits/environments.
7. [x] Windows x64 / Debian 12 x64 / ARMv7 Linux release builds.
8. [ ] OpenWrt/real-device smoke.
9. [ ] Final v0.0.3 release/tag verification.

### Next AI action
The next AI must first inspect the newest CI run triggered by the stress commits and record the WS/WSS/QUIC `1/100/500/1000` completion results. If stress is green, immediately add executable SOCKS5/HTTP lifecycle/error-matrix coverage, then extend the same matrix to TCP+UDP. Do not claim GoWay compatibility until the optional `WAY_READ_TOKEN` is configured and the pinned private GoWay v1.8.4 job actually executes.

### Relay discipline
- Keep `AI_HANDOFF.md`, `PROGRESS.md`, and `SPEC.md` synchronized with current evidence.
- CI green means the tested revision passed; it does not prove untested GoWay interoperability.
- Record commit SHA, workflow run ID, exact test output, and any blocker after every meaningful milestone.
- Never replace failed or skipped evidence with source-level assumptions.

## Three-file relay contract
1. `AI_HANDOFF.md` — decisions, commits, blockers, next step.
2. `PROGRESS.md` — compact progress dashboard.
3. `SPEC.md` — source-derived GoWay compatibility contract.

Never call the project 100% complete merely because source paths exist.
