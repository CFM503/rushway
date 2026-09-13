# RushWay AI Relay Handoff

## 2026-09-13 verified checkpoint

Current code baseline:
- `7a5d4f2f72c021e6a9cb9aab5e7236e61718d5a2` — e2e benchmark now times the full concurrent proxy flow, including SOCKS5 setup/upstream handshake, uses shared `Arc<[u8]>` payload storage, and covers concurrency 1/8/32. **CI #76 is fully green.**
- `4bc06398d39a8da06700b4592a9378bdfd0c97f3` — first version of the fairer e2e benchmark change; superseded immediately by `7a5d4f2` because its timer started after task creation and could exclude part of connection setup.
- `fc4b876d168459e45cf779e57c15946a5d39d493` — fixed e2e benchmark payload ownership so `tokio::spawn` compiles; verified by Run 74.
- `95bfc48252790d8207bf35cc8253d3479e3aafb1` — restored the complete `src/wss_client.rs` after a transient accidental truncation during warning cleanup; the redundant `OwnedMuxFrame` import was removed as part of the restoration.
- `74bd9bbbd8fb607230e4199f49d411393c6a07b5` — fixed moved `OwnedMuxFrame` use in server MUX DATA dispatch after CI caught the ownership error.
- `b24a182c6eea268dff081fb17749ab093ec6ba7f` — introduced runtime owned MUX DATA integration for server path and WSS downstream path.
- Prior stable runtime baseline: `f36b75767531c05e83d0cdd9a336b4d1ebc50a64`.

CI evidence:
- Run 76 / workflow `34752632479` on `7a5d4f2`: **all green** — Linux tests, Linux release, MUX benchmark, e2e proxy benchmark, Windows x64 GNU.
- Run 76 e2e result: `payload_mib=4 roundtrip_echo=1 c1_mib_s=43.69 c8_mib_s=219.81 c32_mib_s=340.16`.
- Run 76 MUX benchmark: copied `12488.78 ns/op`, owned/reused `1.74 ns/op`, reported `7169.12x` speedup. Treat this as a directional microbenchmark, not an end-to-end speed claim.
- Run 74 / workflow `34752159078` on `fc4b876d`: **all green** — Linux tests, Linux release, MUX benchmark, e2e proxy benchmark, Windows x64 GNU.
- Run 74 e2e result: `payload_mib=4 single_mib_s=75.28 concurrency=8 concurrent_total_mib_s=199.99`. This remains historical only; Run 76 supersedes it because the timing and concurrency methodology was corrected.
- Run 73 / workflow `34751985733`: failed only in the benchmark itself because a borrowed payload crossed `tokio::spawn`; fixed in `fc4b876d`.
- Run 69 / workflow `34748802233` on `95bfc482`: **all green** — Linux tests, Linux release, MUX benchmark, Windows x64 GNU. This verifies the restored WSS client compiles successfully on both tested targets.
- Rust toolchain remains 1.82.0.

Benchmark interpretation:
- Run 76 is the current RushWay local baseline: SOCKS5 -> WS/MUX -> RushWay server -> local TCP echo, 4 MiB payload per flow, round-trip echo workload, with connection/setup time included in each case.
- Current measured aggregate throughput on the GitHub Ubuntu runner: **43.69 MiB/s at 1 flow, 219.81 MiB/s at 8 flows, 340.16 MiB/s at 32 flows**.
- These figures are local end-to-end round-trip echo measurements, not one-way Internet throughput and not a GoWay comparison.
- The benchmark shares immutable payload storage via `Arc<[u8]>`, so the prior artificial `payload.to_vec()` cloning cost is outside the timed workload.
- The timer starts immediately before spawning the flow tasks. It therefore includes connection establishment, SOCKS5 negotiation, upstream WebSocket handshake, data transfer, echo, and teardown for each case; it is intentionally conservative for connection/session establishment rather than a pure steady-state streaming benchmark.
- The 32-flow result shows the current architecture scales from 43.69 MiB/s single-flow to 340.16 MiB/s aggregate at 32 concurrent flows on this runner. Do not extrapolate this directly to Internet or Windows performance.

Runtime performance status:
- `OwnedMuxFrame` is used by the server MUX DATA receive path and WSS downstream receive path.
- DATA payload is written from owned storage without cloning the MUX payload.
- Plain WS downstream path uses `read_frame_owned`.
- Current outbound MUX send paths still allocate a frame Vec per write; this remains a likely optimization target only after a fair GoWay baseline is available.
- `src/ws.rs` still transfers the receive Vec with `std::mem::take`, so capacity reuse across successive frames is not yet guaranteed; investigate only after controlled comparison or allocation profiling.
- CI currently passes with several compiler warnings, including unreachable UDP loop tails, dead-code helpers, and unused protocol helpers. These are cleanup items, not current correctness failures.

Immediate next steps:
1. Build a fair GoWay-vs-RushWay local comparison under identical payload, echo target, concurrency, runner, and measurement rules.
2. Prefer the same 1/8/32 cases and 4 MiB round-trip workload first so the first comparison is apples-to-apples.
3. Record GoWay and RushWay results before changing the runtime again; identify whether RushWay is actually slower and where the gap appears.
4. Separately add a steady-state transfer mode that excludes connection setup, so buffer/MUX optimizations can be measured without handshake noise.
5. Then inspect WS receive-buffer reuse and outbound MUX frame allocation only if the controlled measurements show those paths matter.
6. Later evaluate shared session pooling, reconnect/retry/dead-IP handling, WSS/QUIC, and release packaging.
7. Keep Rust 1.82.0 compatibility.

Do not claim:
- GoWay/RushWay interoperability
- WSS UDP
- QUIC
- non-MUX parity
- production readiness
- release readiness
- RushWay faster than GoWay
without execution evidence.

Canonical documents to read first:
- `PROGRESS.md`
- `SPEC.md`
- this file
