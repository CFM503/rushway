# RushWay AI Relay Handoff

## 2026-09-13 verified checkpoint

Current code baseline:
- `95bfc48252790d8207bf35cc8253d3479e3aafb1` — restored the complete `src/wss_client.rs` after a transient accidental truncation during warning cleanup; the redundant `OwnedMuxFrame` import was removed as part of the restoration.
- `40f36c608c4633f8aaee1b87c412f23aedf6661b` — intended WSS import cleanup commit; its contents were preserved in the restored file, but the repository state is now anchored at `95bfc482`.
- `74bd9bbbd8fb607230e4199f49d411393c6a07b5` — fixed moved `OwnedMuxFrame` use in server MUX DATA dispatch after CI caught the ownership error.
- `b24a182c6eea268dff081fb17749ab093ec6ba7f` — introduced runtime owned MUX DATA integration for server path and WSS downstream path.
- Prior stable runtime baseline: `f36b75767531c05e83d0cdd9a336b4d1ebc50a64`.

CI evidence:
- Run 60 / workflow `34747612337`: Linux tests, Linux release, MUX benchmark, Windows x64 GNU all passed.
- Run 65 / workflow `34748092662`: failed at compile with `E0382` because `OwnedMuxFrame` was moved into `StreamCommand::Data` and its stream ID was then referenced. Fixed in `74bd9bb` by saving `stream_id` before the move.
- Run 66 / workflow `34748327454`: **all green** — Linux tests, Linux release, MUX benchmark, Windows x64 GNU.
- Run 67 / workflow `34748365855`: **all green**.
- Run 69 / workflow `34748802233` on `95bfc482`: **all green** — Linux tests, Linux release, MUX benchmark, Windows x64 GNU. This also verifies the restored WSS client compiles successfully on both tested targets.
- Rust toolchain remains 1.82.0.

Benchmark finding:
- The corrected benchmark in `24d36bc` applies `black_box` to the copied decode input.
- The corrected 32 KiB comparison showed the copied path around 12.5 microseconds/op versus owned around 1.7 nanoseconds/op in the benchmark run, demonstrating that the earlier 0.62 ns/op copied result was invalid and that the corrected benchmark exposes clone cost.
- Treat microbenchmarks as directional evidence only; end-to-end proxy throughput still needs measurement.

Runtime performance status:
- `OwnedMuxFrame` is used by server MUX DATA receive path and WSS downstream receive path.
- DATA payload is written from owned storage without cloning the MUX payload.
- SYN parsing remains unchanged.
- Plain WS downstream path uses `read_frame_owned`.
- WSS client file is complete and CI-verified after the transient truncation incident.
- Current outbound MUX send paths still allocate a frame Vec per write; this is the next meaningful performance area to measure rather than assuming another optimization is beneficial.

Immediate next steps:
1. Build a real end-to-end local proxy throughput benchmark for RushWay, preferably exercising MUX DATA in both directions.
2. Add concurrency cases (single flow, 8 flows, 32 flows) and payload sizes that represent real proxy traffic.
3. Inspect shared WebSocket writer lock contention and queue/backpressure behavior against GoWay's documented limits.
4. Compare GoWay and RushWay under identical local conditions before claiming a speed advantage.
5. Only after evidence, consider buffer pooling, session pooling, retry/dead-IP handling, WSS/QUIC expansion, and release packaging.
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
