# RushWay AI Relay Handoff

## 2026-09-13 accelerated implementation checkpoint

Latest implementation commit: `f20c19924e70b843338da2c245bb8668f7125bb3`.

### Just completed

- Re-audited `src/mux_pool.rs` before release work.
- Fixed the UDP regression: after the raw `UDP\n` WebSocket handshake, UDP payloads are sent as raw WebSocket binary frames again; they are no longer incorrectly wrapped in TCP MUX `DATA` frames.
- Preserved TCP MUX pooling and the reused MUX serialization buffer on the TCP hot path.
- Optimized WSS TLS setup by caching the verified and insecure `rustls::ClientConfig` instances with `OnceLock`; TLS verification policy and ALPN behavior are unchanged.
- Added a unit-level pointer-reuse check for both cached TLS configurations.

### Current implementation

- Client `ws://` path uses physical MUX session pooling with logical stream reuse.
- `RUSHWAY_MUX_SESSIONS` controls the pool, clamped to 1..64; default is 4 physical sessions.
- Per-session logical stream capacity is 256.
- SOCKS5 TCP/UDP and HTTP CONNECT client handling are present.
- Server-side MUX forwarding, plain WebSocket and WSS foundations remain present.
- WSS client currently establishes one upstream physical connection per local proxy connection; WSS physical-session pooling is still unfinished.
- Release profile is optimized for deployment.

### CI / delivery reality

RushWay CI run #128 and Build Smoke run #9 for documentation commit `9668fad1...` both failed before running workflow steps; the jobs report no runner execution and no usable job logs. A retry was attempted and again did not produce executable steps. This is not compile/test evidence.

The current `f20c199` commit has not yet obtained workflow execution evidence. Local container execution was also blocked because the environment could not resolve `github.com`, so do not claim a local build/test pass.

### Verified historical performance baseline

- setup-inclusive median: c1 `43.31`, c8 `170.17`, c32 `345.55` MiB/s
- steady-state median: c1 `43.06`, c8 `205.04`, c32 `349.28` MiB/s
- MUX owned/reused decode: `1.54 ns/op`

These numbers are historical baseline measurements, not a claim that the current commit is faster.

### Fast-track release order

1. Obtain executable CI/build evidence on a commit containing the current WSS TLS optimization.
2. Run/verify TCP MUX benchmark against the historical baseline.
3. Fix any compile/test failures immediately.
4. Audit and optimize remaining WebSocket masking allocation without changing protocol behavior.
5. Add WSS physical-session reuse.
6. Finish WSS UDP.
7. Validate GoWay interoperability.
8. Produce Windows x64 / Debian 12 x64 / ARMv7 artifacts.
9. Cut v0.0.1 only after execution evidence and smoke tests.

### Explicit unfinished items

- current-commit compile/test/benchmark evidence
- WSS pooled transport
- WSS UDP
- non-MUX runtime
- full GoWay interoperability validation
- QUIC runtime
- release packaging and v0.0.1

### Operational rules

Do not claim speedup without a current benchmark. Do not claim compile/test success without workflow or local execution evidence. Do not force UDP into TCP MUX framing. Do not mark production/release ready before smoke tests.

## Relay files

- `PROGRESS.md` — canonical roadmap and verified status.
- `AI_HANDOFF.md` — chronological AI relay log.
- `SPEC.md` — compatibility and implementation specification.
