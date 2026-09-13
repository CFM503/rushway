# RushWay v0.0.1 — Continuous Handoff Progress

> Compatibility baseline: GoWay v1.8.4, stable commit `538dbee86b9fbf248a68c8c6d8eee5d6f8bdb0dc` from `CFM503/way/goway`.
> Goal: tested, protocol-compatible Rust replacement with standalone Windows x64, Debian 12 x64 and KWRT/OpenWrt ARMv7 executables.

## Current verified checkpoint — 2026-09-13

**Overall engineering completion: ~60% (estimate).** This is migration progress, not a claim of production readiness.

- Stage 1 bootstrap: `[~]`
- Stage 2 v1.8.4 extraction: `[~]`
- Stage 3 Rust implementation: `[~]`
- Stage 4 compatibility tests: `[ ]`
- Stage 5 release builds: `[~]`
- Stage 6 GitHub Actions: `[~]` — runner execution is currently failing before steps start
- Stage 7 v0.0.1 release: `[ ]`

### Current head / optimization work
- Current code head: `1c16c2195d4a01a2f1b901bdee73763719665134`.
- Client-side physical MUX session pooling is implemented for normal `ws://` traffic.
- `--mux-sessions` now controls the physical pool through `RUSHWAY_MUX_SESSIONS`, clamped to 1..64.
- Per-session logical stream capacity is 256.
- Stream lifecycle handling was hardened to avoid duplicate active-count decrements and to avoid holding the shared writer lock during network reads.
- Build-smoke workflow is present for Linux x64 and Windows x64.

## Latest CI status

Latest push head `f2b15f6b66e008bc305555cfadb8200fd23929b1` triggered both the main CI and Build Smoke workflows. Both failed immediately with jobs reporting `steps: []` / no runner execution; dependent platform jobs were skipped. This is runner/infrastructure evidence, not compiler/test evidence. Do not infer code failure from these runs.

The last fully verified RushWay-only baseline remains Run 95 (`34755206730`).

## Verified performance baseline — RushWay only

| Workload | c1 | c8 | c32 |
|---|---:|---:|---:|
| Setup-inclusive median, 5 samples | 43.31 | 170.17 | 345.55 MiB/s |
| Steady-state median, 5 samples | 43.06 | 205.04 | 349.28 MiB/s |

Formal e2e from the same verified run:
`rushway_e2e payload_mib=4 roundtrip_echo=1 c1_mib_s=43.06 c8_mib_s=166.93 c32_mib_s=383.52`

MUX ownership microbenchmark:
- copied decode: `11456.35 ns/op`
- owned/reused decode: `1.54 ns/op`
- reported directional speedup: `7431.47x`

These are RushWay-only/local measurements and are not GoWay comparison evidence.

## Cross-proxy benchmark infrastructure

`src/bin/proxy_bench.rs` supports both implementation shapes:
- RushWay gets `-p <port>`;
- GoWay gets `-p :<port>`;
- both use `--up ws://127.0.0.1:<server>/` on the client side;
- same 4 MiB deterministic payload, concurrency 1/8/32, SOCKS5 no-auth and local TCP echo.

`scripts/repeat_proxy_bench.sh` executes repeated samples and reports the median for c1/c8/c32.

Modes:
- `setup_inclusive`: local SOCKS5 negotiation + upstream WebSocket setup + transfer + echo;
- `steady_state`: warm-up establishes the physical upstream WebSocket before timed flows.

The optional pinned GoWay comparison job remains disabled when the private sibling-repository read credential is unavailable. There is still no measured GoWay throughput result.

## Runtime performance state

### Completed ownership work
- [x] `OwnedMuxFrame` retains the original frame storage.
- [x] Server MUX DATA receive path uses owned storage.
- [x] WSS downstream receive path uses owned MUX frames.
- [x] Plain WebSocket downstream path uses the owned frame path.
- [x] DATA payload is forwarded without cloning.
- [x] WebSocket binary data-frame receive transfers ownership.

### Completed pooling work
- [x] Client-side physical MUX session reuse for normal `ws://` client traffic.
- [x] Least-active physical session selection.
- [x] Logical stream dispatch from one physical WebSocket to multiple local TCP connections.
- [x] Basic session retirement on physical reader termination.
- [x] User-configurable physical session count via `--mux-sessions`.
- [x] Per-session logical stream limit of 256.
- [x] Stream lifecycle active-count hardening.

### Still requiring profiling / validation
- [ ] Outbound MUX frame allocation per DATA write.
- [ ] WebSocket receive-buffer capacity reuse.
- [ ] Shared WebSocket writer-lock contention.
- [ ] Robust stream/session lifecycle stress testing.
- [ ] Retry/dead-IP/connection-pool policy parity with GoWay.
- [ ] WSS physical-session pooling.

## Functional runtime slice

- [x] Local SOCKS5 no-auth TCP CONNECT front-end.
- [x] Local HTTP CONNECT front-end.
- [x] TCP target dial with timeout and TCP_NODELAY.
- [x] Plain WebSocket client/server handshake integration.
- [x] GoWay-compatible XOR MUX hello/OK authentication boundary.
- [x] MUX SYN/DATA/FIN/RST runtime path.
- [x] Server-side bounded per-stream command queue.
- [x] Target-to-MUX DATA chunking at uint16 payload boundary.
- [x] Target EOF -> MUX FIN.
- [x] Target read error -> MUX RST.
- [x] Local EOF -> MUX FIN.
- [x] Remote MUX FIN -> local half-close.
- [x] Remote MUX RST -> local connection termination.
- [x] SOCKS5 UDP relay implementation slice for plain `ws://`.
- [x] TLS/WSS client TCP forwarding path.
- [ ] WSS UDP.
- [ ] Runtime QUIC.
- [ ] Retry/dead-IP/connection-pool parity beyond physical MUX reuse.
- [ ] Runtime non-MUX mode.
- [ ] True GoWay <-> RushWay interoperability evidence.

## Platform / release gaps

- [x] Windows x64 build evidence on prior green run.
- [x] Debian 12 x64 release build evidence on prior green run.
- [ ] ARMv7 verified build after moving target job to Rust 1.86.
- [ ] KWRT/OpenWrt ARMv7 release artifact packaging.
- [ ] v0.0.1 release.

## Immediate accelerated sequence

1. Restore a normal Actions execution path and obtain real compile/test logs; do not treat runner failures as code evidence.
2. Run the pooled-client setup-inclusive and steady-state c1/c8/c32 medians and compare against Run 95.
3. Remove remaining hot-path allocation/lock overhead in MUX DATA forwarding based on profiling, not guesswork.
4. Implement WSS physical-session reuse without weakening TLS verification or compatibility behavior.
5. Implement WSS UDP relay and validate packet framing/end-to-end behavior.
6. Implement non-MUX transport mode and matching server handshake.
7. Add retry/dead-IP and connection-pool parity, then execute true GoWay interoperability tests.
8. Complete QUIC runtime and release artifact packaging for Windows x64, Debian 12 x64 and ARMv7/OpenWrt.
9. Synchronize `PROGRESS.md` and `AI_HANDOFF.md` after every verified milestone.
10. Cut v0.0.1 only after implementation and execution evidence are complete.

## AI relay rule

**Use both relay documents, but keep their roles distinct:**

- `PROGRESS.md` — canonical roadmap, verified CI/performance baseline, current blockers and next sequence.
- `AI_HANDOFF.md` — chronological AI-to-AI handoff log, exact commit history, incidents, benchmark caveats and operational context.

Any future/relay AI must:
1. Read `PROGRESS.md`, `AI_HANDOFF.md` and `SPEC.md` before changing code.
2. Start from the latest repository head; do not infer state from old notes.
3. Check the latest Actions run before trusting performance or compatibility claims.
4. Make one logical change at a time, validate it, then record exact commit/run/evidence.
5. Never mark GoWay interoperability, WSS UDP, QUIC, non-MUX, production readiness or release readiness complete without execution evidence.
6. Preserve both relay documents and synchronize them after every verified milestone.
