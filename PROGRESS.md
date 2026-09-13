# RushWay v0.0.1 — Continuous Handoff Progress

> Compatibility baseline: GoWay v1.8.4, stable commit `538dbee86b9fbf248a68c8c6d8eee5d6f8bdb0dc` from `CFM503/way/goway`.
> Goal: tested, protocol-compatible Rust replacement with standalone Windows x64, Debian 12 x64 and KWRT/OpenWrt ARMv7 executables.

## Current verified checkpoint — 2026-09-13

**Overall engineering completion: ~52% (estimate).** This is migration progress, not a claim of production readiness.

- Stage 1 bootstrap: `[~]`
- Stage 2 v1.8.4 extraction: `[~]`
- Stage 3 Rust implementation: `[~]`
- Stage 4 compatibility tests: `[ ]`
- Stage 5 release builds: `[~]`
- Stage 6 GitHub Actions: `[x]`
- Stage 7 v0.0.1 release: `[ ]`

### Current verified head / optimization infrastructure
- Current head: `3bfad37bee0a7c783bef35d2365888e292182a80`.
- Cross-proxy runner: `e10020e7437bf575b6f674591b2ca5c146233fe1`.
- Repeated-sample wrapper: `872ca45b800ab06c8594e4317b91d2ed1128030b`.
- CI median stage: `1b1a5cc01d6b2e0d5dfb3b707e0d6733ad12725c`.
- GoWay address-format compatibility fix: `9d3bc56358218aec673af589334027d823a900a0`.
- Optional pinned GoWay comparison CI: `a0e787a6642401003f34e941d09d31501dbb1422`.
- Steady-state benchmark support: `4ee6ef18697bb7fcc4549f7e49e8d1260a3b5a47`.
- Debian 12 CI fix: `994a52c7797f408654057d881effebb9e4af07f2`.
- ARMv7 CI validation: `3bfad37bee0a7c783bef35d2365888e292182a80`.
- Rust toolchain: `1.82.0`.

## Latest CI evidence

### Run 99 — workflow `34756277983` — **fully green**
Head: `994a52c7797f408654057d881effebb9e4af07f2`

- `test`: **success** — Rust 1.82 tests, release build, MUX benchmark, formal e2e, common proxy runner, repeated setup-inclusive median and repeated steady-state median all passed.
- `windows-x64`: **success**.
- `debian-12-x64`: **success** — release binary built successfully inside `rust:1.82-bookworm`.
- `goway-comparison`: **success**, but comparison steps were skipped because the optional private GoWay repository-read credential is not configured.

This is the first verified CI run with Debian 12 x64 release-build coverage after fixing the container shell/PATH issue.

### Run 100 — workflow `34756739632` — **in progress**
Head: `3bfad37bee0a7c783bef35d2365888e292182a80`

Added `armv7-unknown-linux-gnueabihf` cross-build validation using `gcc-arm-linux-gnueabihf`. ARMv7 is **not marked complete until this run finishes successfully**.

### Run 95 — workflow `34755206730` — **fully green**
Head: `4ee6ef18697bb7fcc4549f7e49e8d1260a3b5a47`

- `test`: **success** — Rust 1.82 tests, release build, MUX benchmark, formal e2e, common proxy runner, repeated setup-inclusive median and repeated steady-state median all passed.
- `windows-x64`: **success**.
- `goway-comparison`: **success**, but comparison steps were skipped because the optional private GoWay repository-read credential is not configured.

The RushWay-only benchmark evidence from this verified run is:

| Workload | c1 | c8 | c32 |
|---|---:|---:|---:|
| Setup-inclusive median, 5 samples | 43.31 | 170.17 | 345.55 MiB/s |
| Steady-state median, 5 samples | 43.06 | 205.04 | 349.28 MiB/s |

Both are local end-to-end proxy benchmarks using the same runner conditions; neither is a one-way Internet throughput claim.

### Run 95 MUX microbenchmark
- copied decode: `11456.35 ns/op`
- owned/reused decode: `1.54 ns/op`
- reported owned-vs-copy speedup: `7431.47x`

This is directional microbenchmark evidence only and is not an end-to-end speed claim.

### Run 95 formal e2e benchmark
`rushway_e2e payload_mib=4 roundtrip_echo=1 c1_mib_s=43.06 c8_mib_s=166.93 c32_mib_s=383.52`

The common cross-proxy runner remains the preferred comparison metric because it uses the same launch and protocol workload shape for both implementations.

### Earlier evidence
Run 83 / workflow `34753910015` showed large same-run c8 variance between the formal and common runners. This is why five-sample medians are now used instead of single-run performance conclusions.

Run 76 / workflow `34752632479` formal baseline:
`rushway_e2e payload_mib=4 roundtrip_echo=1 c1_mib_s=43.69 c8_mib_s=219.81 c32_mib_s=340.16`

## Cross-proxy benchmark infrastructure

`src/bin/proxy_bench.rs` supports both implementation shapes:
- RushWay gets `-p <port>`;
- GoWay gets `-p :<port>` to match its listen-address CLI contract;
- both use `--up ws://127.0.0.1:<server>/` on the client side;
- same 4 MiB deterministic payload, 1/8/32 concurrency, SOCKS5 no-auth and local TCP echo.

`scripts/repeat_proxy_bench.sh` executes repeated samples and reports the median for c1/c8/c32. CI runs five samples.

The benchmark has two modes:
- `setup_inclusive`: timer includes local SOCKS5 negotiation, upstream WebSocket setup, transfer and echo;
- `steady_state`: a warm-up flow establishes the physical upstream WebSocket before the timed flows, so the timed interval focuses on new proxy streams plus data transfer.

The optional `goway-comparison` job pins GoWay to `538dbee86b9fbf248a68c8c6d8eee5d6f8bdb0dc` and Go 1.25.0. Because `CFM503/way` is private, the job requires a repository-read credential; without it, the job safely skips rather than failing the main CI.

## Current runtime performance state

### MUX / WebSocket ownership work completed
- [x] `OwnedMuxFrame` exists and retains the original frame storage.
- [x] MUX DATA receive path uses owned storage on the server path.
- [x] WSS downstream receive path uses owned MUX frames.
- [x] Plain WebSocket downstream path uses the owned frame path.
- [x] DATA payload is forwarded without cloning the MUX payload.
- [x] WebSocket binary data-frame receive path transfers ownership instead of cloning payload data.
- [x] CI tests verify ownership behavior.

### Remaining measurable performance candidates
- [ ] Outbound MUX send path still builds a frame `Vec` per write.
- [ ] `src/ws.rs` still uses `std::mem::take`; successive-frame capacity reuse is not guaranteed.
- [ ] Shared WebSocket writer lock contention has not yet been profiled against GoWay.
- [ ] Session pooling/shared physical MUX sessions are not yet implemented or benchmarked in RushWay.
- [ ] Retry/dead-IP/connection-pool behavior is not yet implemented.

Do not optimize these blindly. First obtain controlled RushWay vs GoWay evidence or allocation/profile evidence.

## Functional runtime slice currently present

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
- [x] TLS/WSS client TCP forwarding path compiles and passes Linux/Windows CI.
- [ ] WSS UDP.
- [ ] Runtime QUIC.
- [ ] Runtime connection pool/reuse/retry/dead-IP.
- [ ] Runtime non-MUX mode.
- [ ] True GoWay <-> RushWay interoperability evidence.

## Known cleanup backlog

Compiler warnings remain in the verified baseline. Run 95 still reports unused protocol helpers, dead-code fields/methods and unreachable expressions after intentional infinite UDP loops. These are cleanup items, not current correctness failures.

## GoWay comparison baseline

Reference repository: `CFM503/way/goway`
Reference commit: `538dbee86b9fbf248a68c8c6d8eee5d6f8bdb0dc`

Documented GoWay performance mechanisms include pooled WebSocket buffers, owned MUX DATA, physical MUX session pooling, active-stream-aware scheduling, byte-level backpressure, connection-pool prewarming/refill and optimized WebSocket masking. These are comparison targets, not claims of RushWay parity.

There is still **no measured GoWay throughput number** because the optional private sibling-repository credential is not configured.

## Immediate continuous sequence

1. Finish ARMv7 CI validation and fix any cross-compilation errors without weakening the target build.
2. Enable the optional private-GoWay CI comparison with the repository-read credential, then capture GoWay setup-inclusive and steady-state c1/c8/c32 medians using the identical runner.
3. Put RushWay and GoWay results into one comparison table before changing runtime code for speed.
4. Profile the slower/hotter path and only then optimize outbound MUX allocation, WS buffer reuse, writer contention or physical session pooling.
5. Continue WSS UDP, QUIC, non-MUX, retry/dead-IP/pool and broader compatibility work.
6. Package verified Windows x64 and Debian 12 x64 binaries; after ARMv7 passes, add the KWRT/OpenWrt release artifact path.
7. Add true GoWay <-> RushWay interoperability tests.
8. Only after required evidence is green, create v0.0.1 release.

## AI relay rule

**Use both relay documents, but keep their roles distinct:**

- `PROGRESS.md` — canonical roadmap, stage status, verified CI/performance baseline, current blockers and next sequence.
- `AI_HANDOFF.md` — chronological AI-to-AI handoff log, exact commit history, incidents, benchmark caveats and operational context.

Any future/relay AI must:
1. Read `PROGRESS.md`, `AI_HANDOFF.md` and `SPEC.md` before changing code.
2. Start from the latest verified commit; do not infer state from old notes.
3. Check the latest Actions run before trusting performance or compatibility claims.
4. Make one logical change at a time, execute CI, then record exact commit/run/evidence.
5. Never mark GoWay interoperability, WSS UDP, QUIC, non-MUX, production readiness or release readiness complete without execution evidence.
6. Preserve both relay documents and synchronize them after every verified milestone.
