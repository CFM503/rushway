# RushWay AI Relay Handoff

## 2026-09-13 verified checkpoint

Current verified code baseline:
- `f36b75767531c05e83d0cdd9a336b4d1ebc50a64` — `perf: make default WebSocket receive path ownership based`
- Latest documentation/relay record commit: `d76e279a4b7f809f48ab371efbacba2c6e046f07`

Latest verified CI:
- Workflow run `34746698462` / Run 56
- Linux tests: 6/6 mux_bench protocol tests + 28/28 main tests passed
- Linux release build: passed
- Windows x64 GNU build: passed
- Rust: 1.82.0

Latest benchmark:
- `rushway_mux_decode_owned_reused`
- 200,000 iterations
- 32 KiB payload
- `1.55 ns/op`
- `20,206,592.20 MB/s` reported by the benchmark

Important interpretation:
- The MUX header parsing/owned decode micro-path is already extremely cheap.
- The current remaining MUX performance bottleneck is not `MuxHeader::parse` itself.
- The runtime still uses `MuxFrame::decode(&payload)` in the server and client MUX DATA paths, so the runtime currently performs an additional payload allocation/copy.

Immediate next task:
1. Change server `handle_mux_parts` to use `MuxFrame::decode_owned(payload)`.
2. Carry `OwnedMuxFrame` through the per-stream command channel for DATA frames.
3. In `server_stream_task`, write `frame.payload()` directly to the target socket.
4. Change the client reverse path to use `decode_owned(payload)` and write `frame.payload()` directly to the local socket.
5. Preserve SYN parsing semantics; only DATA fast paths need ownership transfer first.
6. Run GitHub Actions immediately after the change.
7. If CI fails, fix the smallest ownership/lifetime issue and rerun.
8. Record exact commit/run evidence in `PROGRESS.md`.

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

The project must remain compatible with Rust 1.82.0 and the GoWay v1.8.4 wire contract.