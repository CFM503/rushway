# RushWay AI Relay Handoff

## 2026-09-13 accelerated implementation checkpoint

Latest implementation commit: `d3432d5371f8a16cee11a75cedf63d8d7b8daa5a`.

### Just completed

- Re-audited `src/mux_pool.rs` before release work.
- Fixed the UDP regression: after the raw `UDP\n` WebSocket handshake, UDP payloads are sent as raw WebSocket binary frames again; they are no longer incorrectly wrapped in TCP MUX `DATA` frames.
- Preserved TCP MUX pooling and the reused MUX serialization buffer on the TCP hot path.

### Current implementation

- Client `ws://` path uses physical MUX session pooling with logical stream reuse.
- `RUSHWAY_MUX_SESSIONS` controls the pool, clamped to 1..64; default is 4 physical sessions.
- Per-session logical stream capacity is 256.
- SOCKS5 TCP/UDP and HTTP CONNECT client handling are present.
- Server-side MUX forwarding, plain WebSocket and WSS foundations remain present.
- Release profile is optimized for deployment.

### CI / delivery reality

Commit `d3432d5` immediately triggered RushWay CI run #127 and Build Smoke run #8. At the current check both are still queued, with jobs showing no assigned runner yet. Therefore there is still no new compile/test evidence; do not call the build green until the jobs actually execute.

### Verified historical performance baseline

- setup-inclusive median: c1 `43.31`, c8 `170.17`, c32 `345.55` MiB/s
- steady-state median: c1 `43.06`, c8 `205.04`, c32 `349.28` MiB/s
- MUX owned/reused decode: `1.54 ns/op`

These numbers are historical baseline measurements, not a claim that the current commit is faster.

### Fast-track release order

1. Get current CI/build evidence.
2. Run/verify TCP MUX benchmark against the historical baseline.
3. Fix any compile/test failures immediately.
4. Audit and optimize remaining WebSocket masking allocation without changing protocol behavior.
5. Add WSS physical-session reuse.
6. Finish WSS UDP.
7. Validate GoWay interoperability.
8. Produce Windows x64 / Debian 12 x64 / ARMv7 artifacts.
9. Cut v0.0.1 only after execution evidence and smoke tests.

### Explicit unfinished items

- WSS pooled transport
- WSS UDP
- non-MUX runtime
- full GoWay interoperability validation
- QUIC runtime
- release packaging and v0.0.1
- current-commit benchmark evidence

### Operational rules

Do not claim speedup without a current benchmark. Do not claim compile/test success without workflow or local execution evidence. Do not force UDP into TCP MUX framing. Do not mark production/release ready before smoke tests.

## Relay files

- `PROGRESS.md` — canonical roadmap and verified status.
- `AI_HANDOFF.md` — chronological AI relay log.
- `SPEC.md` — compatibility and implementation specification.
