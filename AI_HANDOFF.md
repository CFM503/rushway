# RushWay AI Relay Handoff

## 2026-09-13 verified checkpoint

Current code baseline:
- `74bd9bbbd8fb607230e4199f49d411393c6a07b5` — fixed moved `OwnedMuxFrame` use in the server MUX DATA dispatch after CI caught the ownership error.
- `b24a182c6eea268dff081fb17749ab093ec6ba7f` — introduced runtime owned MUX DATA integration for the server path and WSS downstream path.
- Prior stable runtime baseline: `f36b75767531c05e83d0cdd9a336b4d1ebc50a64`.

CI evidence:
- Run 60 / workflow `34747612337`: Linux tests passed, Linux release build passed, MUX benchmark passed, Windows x64 GNU build passed.
- Run 65 / workflow `34748092662`: failed at compile with `E0382` because `OwnedMuxFrame` was moved into `StreamCommand::Data` and then its stream ID was referenced. This is fixed in `74bd9bb` by saving `stream_id` before the move.
- Run 66 / workflow `34748327454` is currently running against `74bd9bb`.
- Rust toolchain remains 1.82.0.

Benchmark finding:
- The corrected benchmark in `24d36bc` applies `black_box` to the copied decode input.
- The corrected 32 KiB comparison showed the copied path around 12.5 microseconds/op versus owned around 1.7 nanoseconds/op in the benchmark run, demonstrating that the previous 0.62 ns/op copied result was invalid and that the corrected benchmark exposes the clone cost.
- Treat microbenchmarks as directional evidence only; end-to-end proxy throughput still needs measurement.

Runtime performance status:
- `OwnedMuxFrame` is now used by the server MUX DATA receive path and WSS downstream receive path.
- DATA payload is written from owned storage without cloning the MUX payload.
- SYN parsing remains unchanged.
- The client plain-WS downstream path already uses `read_frame_owned`.
- WSS import cleanup may still be needed if CI leaves only an unused `OwnedMuxFrame` import warning.

Immediate next steps:
1. Wait for Run 66 and inspect exact test/release/benchmark/Windows results.
2. If Run 66 passes, clean only the now-redundant WSS `OwnedMuxFrame` import/warnings, then CI again.
3. Add a real end-to-end local proxy throughput benchmark before further speculative allocation changes.
4. Inspect shared WebSocket writer lock contention and queue/backpressure behavior against GoWay's documented limits.
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
