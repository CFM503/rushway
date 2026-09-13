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

### Current main / verified optimization baseline
- Core benchmark commit: `7a5d4f2f72c021e6a9cb9aab5e7236e61718d5a2`.
- Latest documentation/relay commit: `37ae3396a7c66eef0d0b9ec2277173eeea2c05b8`.
- Rust toolchain: `1.82.0`.
- Windows x64 GNU build is green in the latest verified CI.

## Latest CI evidence

### Run 74 — workflow `34752159078`
Head: `fc4b876d168459e45cf779e57c15946a5d39d493`
- Linux tests: **success**
- Linux release build: **success**
- MUX benchmark: **success**
- End-to-end proxy benchmark: **success**
- Windows x64 GNU build: **success**

Run 74 e2e output:
`payload_mib=4 single_mib_s=75.28 concurrency=8 concurrent_total_mib_s=199.99`

This run is retained as historical evidence only because the older benchmark counted per-flow `payload.to_vec()` setup inside the timed region.

### Run 76 — workflow `34752632479`
Head: `7a5d4f2f72c021e6a9cb9aab5e7236e61718d5a2`
- Linux tests: **success**
- Linux release build: **success**
- MUX benchmark: **success**
- End-to-end proxy benchmark: **success**
- Windows x64 GNU build: **success**

Current formal RushWay local e2e benchmark output:
`rushway_e2e payload_mib=4 roundtrip_echo=1 c1_mib_s=43.69 c8_mib_s=219.81 c32_mib_s=340.16`

Interpretation:
- This is a local SOCKS5 -> WebSocket/MUX -> RushWay server -> local TCP echo round-trip benchmark.
- It is not one-way Internet throughput.
- The timer starts immediately before concurrent proxy flows are spawned, so the result includes SOCKS5 setup and upstream WebSocket handshake cost.
- Payload storage is shared via `Arc<[u8]>`, so setup cloning is not included in the timed region.

## MUX microbenchmark evidence

Run 76 executed:
`cargo run --release --bin mux_bench`

Measured output:
- copied decode: `12488.78 ns/op`
- owned/reused decode: `1.74 ns/op`
- reported owned-vs-copy speedup: `7169.12x`

This is directional microbenchmark evidence only. It must not be presented as end-to-end proxy speedup.

## Current runtime performance state

### MUX / WebSocket ownership work completed
- [x] `OwnedMuxFrame` exists and retains the original frame storage.
- [x] MUX DATA receive path uses owned storage on the server path.
- [x] WSS downstream receive path uses owned MUX frames.
- [x] Plain WebSocket downstream path uses the owned frame path.
- [x] DATA payload is forwarded without cloning the MUX payload.
- [x] WebSocket binary data-frame receive path transfers ownership instead of cloning payload data.
- [x] CI tests verify the ownership behavior.

### Remaining measurable performance candidates
- [ ] Outbound MUX send path still builds a frame `Vec` per write.
- [ ] `src/ws.rs` still uses `std::mem::take` for returned data buffers; successive-frame capacity reuse is not guaranteed.
- [ ] Shared WebSocket writer lock contention has not yet been profiled against GoWay.
- [ ] Session pooling/shared physical MUX session behavior has not yet been implemented or benchmarked in RushWay.
- [ ] Retry/dead-IP/connection-pool behavior is not yet implemented.

Do not change these simply because they look optimizable. Use the formal e2e benchmark and fair GoWay comparison to identify the actual bottleneck first.

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

Latest CI is green, but compiler warnings remain. They are not current correctness failures.

Examples recorded in Run 76:
- unused `OwnedMuxFrame` fields/helpers in benchmark-only compilation contexts;
- unreachable `Result::<()>::Ok(())` expressions following intentional infinite UDP loops;
- several protocol/parser helpers currently unused by the runtime path;
- unused helper methods/constants in crypto/proxy modules.

Clean these incrementally without weakening `-D warnings` policy or changing functional behavior solely for aesthetics.

## GoWay comparison baseline

Reference repository: `CFM503/way/goway`
Reference commit: `538dbee86b9fbf248a68c8c6d8eee5d6f8bdb0dc`

GoWay v1.8.4 contains performance mechanisms that RushWay should be compared against rather than assumed equivalent, including:
- pooled WebSocket buffers;
- MUX DATA buffer ownership / zero-copy style paths;
- physical MUX session pooling;
- active-stream-aware MUX session scheduling;
- byte-level backpressure;
- connection-pool prewarming and refill;
- optimized WebSocket masking path.

A fair GoWay-vs-RushWay benchmark must use the same runner class, payload size, flow counts, echo target, transport mode and measurement definition.

## Immediate continuous sequence

1. Build a dedicated GoWay v1.8.4 e2e benchmark matching the RushWay benchmark definition.
2. Execute GoWay at concurrency 1/8/32 and capture exact throughput.
3. Put GoWay and RushWay results into one comparable benchmark table.
4. Identify the slower direction/path and profile that path before modifying RushWay.
5. Only then optimize outbound MUX allocation, WebSocket receive-buffer reuse, writer contention, or session pooling according to evidence.
6. Add steady-state transfer benchmarking separately from connection-establishment-inclusive benchmarking.
7. Continue WSS, QUIC, non-MUX, pool/retry/dead-IP and release work only after core TCP/WS/MUX behavior is benchmarked and stable.
8. Add Debian 12 x64 and KWRT/OpenWrt ARMv7 build jobs and artifact packaging.
9. Only after required interoperability/platform evidence is green, create v0.0.1 release.

## AI relay rule

**Use both relay documents, but keep their roles distinct:**

- `PROGRESS.md` — canonical project roadmap, stage status, verified CI/performance baseline, current blockers, and next sequence.
- `AI_HANDOFF.md` — detailed chronological AI-to-AI handoff log, exact commit history, incidents, benchmark caveats and operational context.

Any future/relay AI must:
1. Read `PROGRESS.md`, `AI_HANDOFF.md` and `SPEC.md` before changing code.
2. Start from the latest verified commit; do not infer repository state from old notes.
3. Check the latest GitHub Actions run before trusting any performance or compatibility claim.
4. Make one logical change at a time, execute CI, then record exact commit/run/evidence.
5. Never mark GoWay interoperability, WSS UDP, QUIC, non-MUX, production readiness or release readiness complete without execution evidence.
6. Preserve both relay documents and keep them synchronized after every verified milestone.
