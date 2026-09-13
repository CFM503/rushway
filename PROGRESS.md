# RushWay v0.0.1 — Continuous Handoff Progress

> Compatibility baseline: GoWay v1.8.4, stable commit `538dbee86b9fbf248a68c8c6d8eee5d6f8bdb0dc` from `CFM503/way/goway`.
> Goal: tested, protocol-compatible Rust replacement with standalone Windows x64, Debian 12 x64 and KWRT/OpenWrt ARMv7 executables.

## Current verified checkpoint — 2026-09-13

**Overall engineering completion: ~50% (estimate).** This is migration progress, not a claim of production readiness.

- Stage 1 bootstrap: `[~]`
- Stage 2 v1.8.4 extraction: `[~]`
- Stage 3 Rust implementation: `[~]`
- Stage 4 compatibility tests: `[ ]`
- Stage 5 release builds: `[~]`
- Stage 6 GitHub Actions: `[x]`
- Stage 7 v0.0.1 release: `[ ]`

### Current main / optimization infrastructure
- Formal RushWay benchmark baseline: `7a5d4f2f72c021e6a9cb9aab5e7236e61718d5a2`.
- Cross-proxy runner introduced: `e10020e7437bf575b6f674591b2ca5c146233fe1`.
- Current repeated-sample wrapper: `872ca45b800ab06c8594e4317b91d2ed1128030b`.
- Current CI median stage: `1b1a5cc01d6b2e0d5dfb3b707e0d6733ad12725c`.
- GoWay address-format compatibility fix: `9d3bc56358218aec673af589334027d823a900a0`.
- Optional pinned GoWay comparison CI: `a0e787a6642401003f34e941d09d31501dbb1422`.
- Rust toolchain: `1.82.0`.

## Latest CI evidence

### Run 89 — workflow `34754682561`
Head: `a0e787a6642401003f34e941d09d31501dbb1422`
- `goway-comparison`: **success**, but all comparison steps were skipped because the optional `WAY_READ_TOKEN` secret is not configured for the private sibling repository `CFM503/way`.
- `test`: **in progress** at the latest check; do not count it as verified until completion.

This means the comparison plumbing is present, but there is still **no GoWay throughput measurement**.

### Run 83 — workflow `34753910015`
Head: `f40cf667360adfad4ad824d0dbac6f079026cff7`
- Linux tests/release/MUX/e2e/cross-proxy self-check: **success**.
- Windows x64 GNU build: **success**.

Observed same-run benchmark outputs:
- formal `e2e_bench`: `c1=76.42 c8=161.85 c32=314.20 MiB/s`
- common `proxy_bench`: `c1=76.48 c8=274.57 c32=329.86 MiB/s`

The large 8-flow variation demonstrates that a single connection-establishment-inclusive run is noisy enough that it should not be used for performance conclusions. The repeated-sample median wrapper was added for this reason.

### Run 76 — workflow `34752632479`
Head: `7a5d4f2f72c021e6a9cb9aab5e7236e61718d5a2`
- Linux tests: **success**
- Linux release build: **success**
- MUX benchmark: **success**
- End-to-end proxy benchmark: **success**
- Windows x64 GNU build: **success**

Formal RushWay baseline:
`rushway_e2e payload_mib=4 roundtrip_echo=1 c1_mib_s=43.69 c8_mib_s=219.81 c32_mib_s=340.16`

Interpretation:
- local SOCKS5 -> WebSocket/MUX -> RushWay server -> local TCP echo;
- not one-way Internet throughput;
- timer includes SOCKS5 and upstream WebSocket establishment;
- immutable payload storage uses `Arc<[u8]>`.

## Cross-proxy benchmark infrastructure

`src/bin/proxy_bench.rs` now supports both implementation shapes:
- RushWay gets `-p <port>`;
- GoWay gets `-p :<port>` to match its listen-address CLI contract;
- both use `--up ws://127.0.0.1:<server>/` on the client side;
- same 4 MiB deterministic payload, 1/8/32 concurrency, SOCKS5 no-auth and local TCP echo.

`scripts/repeat_proxy_bench.sh` executes repeated samples and reports the median for c1/c8/c32. CI runs five RushWay samples.

The optional `goway-comparison` job pins GoWay to `538dbee86b9fbf248a68c8c6d8eee5d6f8bdb0dc` and Go 1.25.0. Because `CFM503/way` is private, the job requires a repository-read token in `WAY_READ_TOKEN`; without it, the job safely skips rather than failing the main CI.

## MUX microbenchmark evidence

Run 76:
- copied decode: `12488.78 ns/op`
- owned/reused decode: `1.74 ns/op`
- reported owned-vs-copy speedup: `7169.12x`

This is directional microbenchmark evidence only and is not an end-to-end speed claim.

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

Compiler warnings remain in the verified baseline. Examples include unused protocol helpers, dead-code benchmark fields and unreachable expressions after intentional infinite UDP loops. These are cleanup items, not current correctness failures.

## GoWay comparison baseline

Reference repository: `CFM503/way/goway`
Reference commit: `538dbee86b9fbf248a68c8c6d8eee5d6f8bdb0dc`

Documented GoWay performance mechanisms include pooled WebSocket buffers, owned MUX DATA, physical MUX session pooling, active-stream-aware scheduling, byte-level backpressure, connection-pool prewarming/refill and optimized WebSocket masking. These are comparison targets, not claims of RushWay parity.

## Immediate continuous sequence

1. Finish Run 89 and record the actual repeated RushWay median output.
2. Enable the optional private-GoWay CI comparison with `WAY_READ_TOKEN`, then capture GoWay 1/8/32 medians using the same runner.
3. Put both implementations into one comparison table before changing runtime code for speed.
4. Add a separate steady-state transfer benchmark that excludes connection setup.
5. Profile the slower path and only then optimize outbound MUX allocation, WS buffer reuse, writer contention or physical session pooling.
6. Continue WSS, QUIC, non-MUX, retry/dead-IP/pool and release work after the core benchmark/compatibility slice is stable.
7. Add Debian 12 x64 and KWRT/OpenWrt ARMv7 build jobs and artifact packaging.
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
