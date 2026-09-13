# RushWay AI Relay Handoff

## 2026-09-13 verified checkpoint

Current code baseline:
- `7a5d4f2f72c021e6a9cb9aab5e7236e61718d5a2` — e2e benchmark now times the full concurrent proxy flow, including SOCKS5 setup/upstream handshake, uses shared `Arc<[u8]>` payload storage, and covers concurrency 1/8/32. CI #76 is pending at the time of this note.
- `4bc06398d39a8da06700b4592a9378bdfd0c97f3` — first version of the fairer e2e benchmark change; superseded immediately by `7a5d4f2` because its timer started after task creation and could exclude part of connection setup.
- `fc4b876d168459e45cf779e57c15946a5d39d493` — fixed e2e benchmark payload ownership so `tokio::spawn` compiles; verified by Run 74.
- `95bfc48252790d8207bf35cc8253d3479e3aafb1` — restored the complete `src/wss_client.rs` after a transient accidental truncation during warning cleanup; the redundant `OwnedMuxFrame` import was removed as part of the restoration.
- `74bd9bbbd8fb607230e4199f49d411393c6a07b5` — fixed moved `OwnedMuxFrame` use in server MUX DATA dispatch after CI caught the ownership error.
- `b24a182c6eea268dff081fb17749ab093ec6ba7f` — introduced runtime owned MUX DATA integration for server path and WSS downstream path.
- Prior stable runtime baseline: `f36b75767531c05e83d0cdd9a336b4d1ebc50a64`.

CI evidence:
- Run 74 / workflow `34752159078` on `fc4b876d`: **all green** — Linux tests, Linux release, MUX benchmark, e2e proxy benchmark, Windows x64 GNU.
- Run 74 e2e result: `payload_mib=4 single_mib_s=75.28 concurrency=8 concurrent_total_mib_s=199.99`.
- Run 74 MUX benchmark: copied `12305.74 ns/op`, owned/reused `2.55 ns/op`, reported `4819.02x` speedup. Treat this as a directional microbenchmark, not an end-to-end speed claim.
- Run 73 / workflow `34751985733`: failed only in the benchmark itself because a borrowed payload crossed `tokio::spawn`; fixed in `fc4b876d`.
- Run 69 / workflow `34748802233` on `95bfc482`: **all green** — Linux tests, Linux release, MUX benchmark, Windows x64 GNU. This verifies the restored WSS client compiles successfully on both tested targets.
- Rust toolchain remains 1.82.0.
- Run 75 / workflow `34752620277` corresponds to the superseded `4bc06398` benchmark commit and is not the target baseline because `7a5d4f2` corrected its timer placement immediately afterward.
- Run 76 / workflow `34752632479` is the current target CI for `7a5d4f2`; its test job is still in progress as of this note.

Benchmark interpretation:
- Run 74 proves the current RushWay local SOCKS5 -> WS/MUX -> server -> local TCP echo path can sustain 75.28 MiB/s for a single 4 MiB round-trip flow and 199.99 MiB/s aggregate across 8 concurrent 4 MiB flows on that GitHub runner.
- These figures are local end-to-end round-trip echo measurements, not one-way Internet throughput and not a GoWay comparison.
- The old e2e benchmark counted `payload.to_vec()` cloning inside the timed region. The current `7a5d4f2` benchmark removes that artificial clone cost by sharing immutable payload storage via `Arc<[u8]>`.
- The current timer begins immediately before spawning the concurrent proxy flows, so it intentionally includes SOCKS5 connection setup and upstream WebSocket handshake cost for each case. This makes the result conservative for real proxy-session establishment rather than a pure steady-state transfer benchmark.
- The benchmark now covers concurrency 1, 8, and 32. The new results are not valid until Run 76 passes and its e2e output is captured.

Runtime performance status:
- `OwnedMuxFrame` is used by the server MUX DATA receive path and WSS downstream receive path.
- DATA payload is written from owned storage without cloning the MUX payload.
- Plain WS downstream path uses `read_frame_owned`.
- Current outbound MUX send paths still allocate a frame Vec per write; this remains a likely optimization target only after the new e2e baseline is captured.
- `src/ws.rs` still transfers the receive Vec with `std::mem::take`, so capacity reuse across successive frames is not yet guaranteed; investigate only after fair e2e baseline and allocation evidence.
- CI currently passes with several compiler warnings, including unreachable UDP loop tails, dead-code helpers, and unused protocol helpers. These are cleanup items, not current correctness failures.

Immediate next steps:
1. Finish Run 76 and record the exact c1/c8/c32 e2e throughput.
2. If the benchmark is green, add a fair GoWay-vs-RushWay comparison under identical local conditions before making any speed claim.
3. Extend the benchmark to test a larger payload and, separately, steady-state transfer without including connection setup if needed for optimization profiling.
4. Inspect WS receive-buffer reuse and outbound MUX frame allocation only after the benchmark identifies a measurable bottleneck.
5. Then evaluate shared session pooling, reconnect/retry/dead-IP handling, WSS/QUIC, and release packaging.
6. Keep Rust 1.82.0 compatibility.

Do not claim:
- GoWay/RushWay interoperability
- WSS UDP
- QUIC
- non-MUX parity
- production readiness
- release readiness
without execution evidence.

Canonical documents to read first:
- `PROGRESS.md`
- `SPEC.md`
- this file
