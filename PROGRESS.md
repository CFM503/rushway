# RushWay v0.0.1 — Continuous Handoff Progress

> Compatibility baseline: GoWay v1.8.4, stable commit `538dbee86b9fbf248a68c8c6d8eee5d6f8bdb0dc` from `CFM503/way/goway`.
> Goal: tested, protocol-compatible Rust replacement with standalone Windows x64, Debian 12 x64 and KWRT/OpenWrt ARMv7 executables.

## Current verified checkpoint — 2026-09-13

**Overall engineering completion: ~55% (estimate).** This is migration progress, not a claim of production readiness.

- Stage 1 bootstrap: `[~]`
- Stage 2 v1.8.4 extraction: `[~]`
- Stage 3 Rust implementation: `[~]`
- Stage 4 compatibility tests: `[ ]`
- Stage 5 release builds: `[~]`
- Stage 6 GitHub Actions: `[~]` — run layer currently unstable
- Stage 7 v0.0.1 release: `[ ]`

### Current head / optimization work
- Current code head: `590a818c843af81043277f107eaae2fc8bd375c8`.
- Added client-side physical MUX session pool in `f23ab32d8b5ef8343a81b9bfa4665da945d06e5b`.
- Routed normal `ws://` client mode through the pool in `590a818c843af81043277f107eaae2fc8bd375c8`.
- Debian 12 CI fix: `994a52c7797f408654057d881effebb9e4af07f2`.
- ARMv7 is now pinned to Rust 1.86.0 because fresh target dependency resolution can require Edition 2024 support; the main test job remains Rust 1.82.0.

## Latest CI status

### Run 100 — workflow `34756739632`
- Main test/release path passed.
- Windows x64 and Debian 12 x64 passed.
- ARMv7 failed for a real dependency/toolchain reason: a transitive package selected Edition 2024, unavailable to Cargo 1.82.0.

### Runs 101–105 — workflow layer failure
Runs `34756815694`, `34758696973`, `34758732812`, `34759343302`, and the later run on head `590a818c...` repeatedly show jobs as failed with `steps: null` and no compile/test log output; downstream jobs are skipped. These runs are not usable as code correctness evidence.

The workflow itself still contains the intended ARMv7 build using Rust 1.86.0. Do not mark ARM complete until a normal run executes actual build steps and succeeds.

## New client-side MUX pooling architecture

`src/mux_pool.rs` now provides a normal `ws://` client path that:
- prewarms up to 4 physical WebSocket/MUX sessions;
- maps each local TCP proxy connection onto a logical MUX stream;
- selects the least-active reusable physical session;
- maintains per-session stream dispatch via an async channel map;
- keeps the physical WebSocket alive across many local TCP connections;
- replaces a dead physical session instead of forcing every local stream to create one;
- keeps WSS handling in the existing dedicated client path;
- retains a SOCKS5 UDP path in the pooled client module.

This specifically removes repeated upstream TCP + WebSocket + MUX handshake cost from sequential local TCP connections. It is an architectural optimization only; no throughput improvement is claimed before benchmark execution.

## Previous benchmark evidence — RushWay only

Run 95 (`34755206730`) was fully green before the ARM toolchain change:

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

`scripts/repeat_proxy_bench.sh` executes repeated samples and reports the median for c1/c8/c32. CI runs five samples.

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

### Newly implemented
- [x] Client-side physical MUX session reuse for normal `ws://` client traffic.
- [x] Least-active physical session selection.
- [x] Logical stream dispatch from one physical WebSocket to multiple local TCP connections.
- [x] Basic session retirement on physical reader termination.

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
- [x] SOCKS5 UDP relay implementation slice.
- [x] TLS/WSS client TCP forwarding path compiles on prior verified Linux/Windows CI.
- [ ] WSS UDP.
- [ ] Runtime QUIC.
- [ ] Retry/dead-IP/connection-pool parity beyond the new physical MUX reuse.
- [ ] Runtime non-MUX mode.
- [ ] True GoWay <-> RushWay interoperability evidence.

## Platform / release gaps

- [x] Windows x64 build evidence on prior green run.
- [x] Debian 12 x64 release build evidence on prior green run.
- [ ] ARMv7 verified build after moving target job to Rust 1.86.
- [ ] KWRT/OpenWrt ARMv7 release artifact packaging.
- [ ] v0.0.1 release.

## Immediate continuous sequence

1. Get one normal GitHub Actions execution with actual steps; verify Rust 1.86 ARMv7 build and compile the new MUX pool.
2. Fix any real compiler/test findings from that execution immediately.
3. Run setup-inclusive and steady-state c1/c8/c32 medians on the pooled client and compare with Run 95.
4. Execute the pinned GoWay comparison when the optional private repository-read credential is available.
5. Profile any remaining throughput/CPU/allocation hotspot before further optimization.
6. Continue WSS UDP, QUIC, non-MUX, retry/dead-IP, interoperability and release artifact work.
7. Synchronize `PROGRESS.md` and `AI_HANDOFF.md` after every verified milestone.

## AI relay rule

**Use both relay documents, but keep their roles distinct:**

- `PROGRESS.md` — canonical roadmap, stage status, verified CI/performance baseline, current blockers and next sequence.
- `AI_HANDOFF.md` — chronological AI-to-AI handoff log, exact commit history, incidents, benchmark caveats and operational context.

Any future/relay AI must:
1. Read `PROGRESS.md`, `AI_HANDOFF.md` and `SPEC.md` before changing code.
2. Start from the latest repository head; do not infer state from old notes.
3. Check the latest Actions run before trusting performance or compatibility claims.
4. Make one logical change at a time, validate it, then record exact commit/run/evidence.
5. Never mark GoWay interoperability, WSS UDP, QUIC, non-MUX, production readiness or release readiness complete without execution evidence.
6. Preserve both relay documents and synchronize them after every verified milestone.
