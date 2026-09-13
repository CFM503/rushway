# RushWay AI Relay Handoff

## 2026-09-13 verified checkpoint

Current code baseline:
- `24d36bc2fed4c85518ccdd104da23a56e5d86138` — corrected MUX benchmark to prevent compiler elimination of the copied decode input.
- Prior stable runtime baseline: `f36b75767531c05e83d0cdd9a336b4d1ebc50a64`.

Latest verified CI before benchmark correction:
- Run 59 / workflow `34747270574`: Linux tests passed, Linux release build passed, Windows x64 GNU passed.
- Rust toolchain: 1.82.0.

Important benchmark finding:
- Run 59 originally reported `decode_copy = 0.62 ns/op` and `decode_owned_reused = 1.65 ns/op` for 32 KiB payloads.
- That result is NOT trusted because the copied decode input was not passed through `black_box`; compiler elimination may have made the copy path unrealistically cheap.
- Benchmark has now been corrected in `24d36bc` by changing the copied path to `MuxFrame::decode(black_box(encoded.as_slice()))`.
- Do not use the old 0.62 ns/op result for performance decisions.

Runtime performance status:
- `ws.rs` now has an owned frame receive helper, but the main runtime still uses the established `read_frame`/`MuxFrame::decode` path.
- `OwnedMuxFrame` exists and has tests proving the original payload storage is retained without copying.
- Do not wire OwnedMuxFrame into runtime until a fair benchmark demonstrates an actual benefit and CI validates ownership/lifetime correctness.

Immediate next steps:
1. Wait for CI on `24d36bc` and record the corrected copy-vs-owned benchmark result.
2. Only if owned decode is measurably beneficial, implement the smallest runtime integration: server MUX DATA first, then client reverse DATA path.
3. Preserve SYN parsing as-is; optimize DATA fast paths first.
4. Record every meaningful commit and exact Actions run evidence in `PROGRESS.md`.
5. Keep Rust 1.82.0 compatibility.

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
