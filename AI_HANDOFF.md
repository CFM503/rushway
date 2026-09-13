# RushWay AI Relay Handoff

## 2026-09-13 accelerated implementation checkpoint

Latest code commit: `f20c19924e70b843338da2c245bb8668f7125bb3`.

### Completed and reusable

- Client `ws://` path uses physical MUX session pooling with logical stream reuse.
- `RUSHWAY_MUX_SESSIONS` controls the pool, clamped to 1..64; default is 4 physical sessions.
- Per-session logical stream capacity is 256.
- SOCKS5 TCP/UDP and HTTP CONNECT client handling are present.
- TCP MUX serialization buffer reuse is present on the client hot path.
- The UDP regression was fixed: after the raw `UDP\n` WebSocket handshake, UDP payloads are sent as raw WebSocket binary frames rather than TCP MUX `DATA` frames.
- WSS TLS setup now caches the verified and insecure `rustls::ClientConfig` instances with `OnceLock`; TLS verification policy and ALPN behavior are unchanged.
- A unit-level pointer-reuse check was added for the cached TLS configurations.

### Current implementation boundary

- Plain `ws://` MUX is the strongest current release candidate path.
- WSS client TCP forwarding exists, but each local proxy connection still creates its own upstream TLS + WebSocket + MUX connection.
- WSS physical-session pooling is not yet implemented.
- WSS UDP is not yet implemented/validated end-to-end.
- Non-MUX runtime, QUIC runtime and true GoWay interoperability evidence remain unfinished.

### CI / delivery reality

The runner problem persists. RushWay CI run #131 and Build Smoke run #12, triggered by documentation commit `c90d8ca70bd7c9c33c472e1b46d2e60af1c43bc4`, both failed before executable workflow steps. No useful compiler/test logs were produced. This is runner/infrastructure evidence, not compile/test evidence.

The current `f20c199` code commit has not obtained executable CI evidence. The relay environment also could not resolve `github.com`, so no local clone/build was possible. Never claim current-head build/test success without actual execution evidence.

### Historical performance baseline

- setup-inclusive median: c1 `43.31`, c8 `170.17`, c32 `345.55` MiB/s
- steady-state median: c1 `43.06`, c8 `205.04`, c32 `349.28` MiB/s
- MUX owned/reused decode: `1.54 ns/op`

These are historical measurements, not current-head speed claims.

### Current sprint decision

A WebSocket large-frame masking-reuse optimization was reviewed, but the proposed replacement was not committed because the complete file rewrite could not be validated safely in this environment. Do not treat that optimization as complete. Preserve the existing working `src/ws.rs` until a clean, testable change can be made.

### Fast-track release order

1. Obtain executable CI/build evidence on a commit containing the current WSS TLS optimization.
2. Run/verify the pooled TCP MUX benchmark against the historical baseline.
3. Fix any compile/test failures immediately.
4. Implement and validate WSS physical-session reuse without weakening TLS verification or compatibility.
5. Implement and validate WSS UDP.
6. Validate GoWay client/server interoperability for TCP and UDP.
7. Produce Windows x64, Debian 12 x64 and ARMv7/OpenWrt artifacts.
8. Cut v0.0.1 only after execution evidence and smoke tests.

### Operational rules

Do not claim speedup without a current benchmark. Do not claim compile/test success without workflow or local execution evidence. Do not force UDP into TCP MUX framing. Do not mark production/release ready before smoke tests. Prefer small, independently verifiable commits over large protocol rewrites.

## Relay files

- `PROGRESS.md` — canonical roadmap, verified CI/performance baseline, current blockers and next sequence.
- `AI_HANDOFF.md` — chronological AI-to-AI handoff log, exact commits, incidents and caveats.
- `SPEC.md` — GoWay v1.8.4 compatibility contract and extracted protocol behavior.
