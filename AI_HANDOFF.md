# RushWay AI Relay Handoff

> This file is the chronological AI-to-AI engineering handoff log. A fresh AI that clones the repository must read this file together with `PROGRESS.md` and `SPEC.md` before changing code.

## 2026-09-13 — Full relay checkpoint

### Starting point

- Repository: `CFM503/rushway`
- Compatibility target: GoWay v1.8.4
- GoWay compatibility baseline commit: `538dbee86b9fbf248a68c8c6d8eee5d6f8bdb0dc`
- Current RushWay code head: `f20c19924e70b843338da2c245bb8668f7125bb3`
- Intended release: `v0.0.1`
- Target artifacts: Windows x64, Debian 12 x64, KWRT/OpenWrt ARMv7
- This checkpoint is engineering progress, not a release-readiness claim.

### What is actually complete in the current code

#### Client/proxy front-end

- SOCKS5 no-auth TCP CONNECT path exists.
- HTTP CONNECT path exists.
- TCP target dialing includes connection timeout handling and TCP_NODELAY in the current runtime path.
- `--mux` / `--no-mux` CLI switches exist, but runtime non-MUX parity is not complete.
- `--mux-sessions` exists and maps to `RUSHWAY_MUX_SESSIONS`, clamped to 1..64, default 4.

#### Plain WebSocket + MUX

- Functional plain `ws://` client/server WebSocket handshake path exists.
- GoWay-compatible XOR MUX hello/OK authentication boundary exists.
- MUX frame format uses the 7-byte header: StreamID + command + uint16 payload length.
- SYN / DATA / FIN / RST runtime flow exists.
- TCP DATA is chunked at the uint16 payload boundary.
- Server side uses bounded per-stream queues for backpressure.
- Target EOF maps to FIN.
- Target read error maps to RST.
- Local EOF maps to FIN.
- Remote FIN maps to local half-close.
- Remote RST terminates the local stream.

#### Physical MUX pooling / reuse

- Plain `ws://` client traffic uses physical MUX session pooling.
- Multiple logical TCP streams can share one physical WebSocket/MUX session.
- Pool selection uses least-active available sessions.
- Physical sessions are retired when the reader terminates.
- Per-session logical stream capacity is 256.
- Physical session count is configurable through `--mux-sessions` / `RUSHWAY_MUX_SESSIONS`.
- TCP MUX serialization buffer reuse is implemented on the client hot path.
- Stream active-count lifecycle hardening is present.

#### UDP

- The plain `ws://` SOCKS5 UDP relay slice exists.
- UDP deliberately uses a separate raw `UDP\n` WebSocket session rather than TCP MUX framing.
- A previous optimization regression that incorrectly pushed UDP payloads through MUX DATA was fixed.
- After the raw `UDP\n` handshake, UDP payloads are raw WebSocket binary payloads.
- Do not regress this by routing UDP packets through TCP MUX DATA frames.
- WSS UDP is still not implemented/validated end-to-end.

#### WSS / TLS

- WSS client TCP forwarding path exists.
- WSS client establishes TCP + TLS + WebSocket + MUX for local proxy connections.
- Verified TLS `rustls::ClientConfig` is cached with `OnceLock`.
- Insecure TLS `rustls::ClientConfig` is cached separately with `OnceLock`.
- TLS verification policy and HTTP/1.1 ALPN behavior were preserved.
- A unit-level pointer-reuse test exists for cached TLS configurations.
- Current WSS client still creates a separate physical upstream TLS+WebSocket+MUX connection for each local proxy connection.
- WSS physical-session pooling is not implemented yet.
- WSS UDP is not implemented/validated end-to-end.
- Server-side WSS integration is not currently claimed complete.

#### WebSocket codec

- RFC6455 non-fragmented data/control frame handling exists.
- Maximum frame size is 64 MiB.
- HTTP header hard limit is 8192 bytes.
- Client masking and server unmasked frames are supported.
- Ping -> Pong, Pong discard and Close -> EOF behavior exists.
- Current codec uses a thread-local xorshift-style mask generator.
- Large outbound masked frames currently allocate a complete frame buffer before masking.
- Receive-path buffer capacity reuse is still an optimization opportunity.
- A large-frame masking-reuse rewrite was reviewed but deliberately not committed because it could not be safely validated. Preserve `src/ws.rs` until a clean, testable change is available.

#### Code quality / architecture status

- Rust implementation is structured into crypto, MUX pool, protocol, proxy, runtime, TLS, WebSocket and WSS client modules.
- Release profile is configured with thin LTO, one codegen unit, stripped binary and abort-on-panic.
- Current code is a migration implementation, not yet a protocol-complete GoWay replacement.

### What is NOT complete yet

- WSS physical-session pooling.
- WSS UDP relay and end-to-end packet framing validation.
- Full non-MUX runtime parity / 1:1 pool behavior.
- QUIC runtime (`quic://` and `quic+tls://`).
- Exact GoWay DNS/resolution behavior in the complete runtime.
- Full retry/dead-IP/connection-pool parity.
- Browser TLS/HTTP fingerprint parity beyond functional WebSocket/TLS behavior.
- Complete SOCKS5/HTTP error and lifecycle parity against every GoWay branch.
- True GoWay v1.8.4 <-> RushWay executable interoperability matrix.
- Large stream-count stress validation (1/100/500/1000) on current head.
- Sustained large-payload and mixed slow/fast stream validation on current head.
- Current-head pooled throughput benchmark.
- Windows x64, Debian 12 x64 and ARMv7/OpenWrt final release artifact validation on the current head.
- v0.0.1 release and release smoke validation.

### Performance history and evidence

The strongest verified RushWay-only historical benchmark is Run 95 (`34755206730`):

| Metric | c1 | c8 | c32 |
|---|---:|---:|---:|
| Setup-inclusive median | 43.31 | 170.17 | 345.55 MiB/s |
| Steady-state median | 43.06 | 205.04 | 349.28 MiB/s |

Historical MUX ownership benchmark:

- copied decode: `11456.35 ns/op`
- owned/reused decode: `1.54 ns/op`
- reported directional speedup: `7431.47x`

These are historical measurements only. They must not be presented as current-head speed claims or GoWay comparison results.

### CI and environment evidence

- RushWay CI run #131, id `34761172740`, was triggered from documentation commit `c90d8ca70bd7c9c33c472e1b46d2e60af1c43bc4` and failed before executable workflow steps.
- RushWay Build Smoke run #12, id `34761172747`, triggered by the same documentation commit, also failed before executable workflow steps.
- No compiler/test logs were produced by those failures.
- Earlier retries showed the same infrastructure-level behavior.
- The relay environment could not resolve `github.com`, preventing local clone/build execution.
- Therefore there is currently no evidence that the latest `f20c199` head compiles/tests successfully in the present relay environment.
- Future AIs must check the latest Actions status before claiming executable evidence.

### Chronological engineering history

1. **Protocol extraction / bootstrap** — extracted GoWay v1.8.4 compatibility facts into `SPEC.md` and established Rust project/module structure.
2. **Initial Rust runtime** — implemented proxy front-end, plain WebSocket handshake, crypto boundary, MUX primitives and stream lifecycle.
3. **MUX pooling** — introduced multiple physical MUX sessions, logical stream dispatch, least-active selection and configurable session count.
4. **Hot-path reuse** — reused TCP MUX serialization buffers and hardened active stream counting.
5. **UDP regression repair** — restored the intended raw `UDP\n` WebSocket UDP framing after a performance refactor accidentally routed UDP through TCP MUX DATA.
6. **TLS reuse** — `f20c199` cached verified/insecure rustls client configs with `OnceLock` without changing verification or ALPN semantics.
7. **Current boundary** — plain `ws://` MUX is the strongest candidate path; WSS works for TCP forwarding but lacks physical pooling; WSS UDP, QUIC, non-MUX and true GoWay interop remain.

### Immediate implementation order

Do these in order unless new execution evidence proves a better blocker:

1. Get executable CI/build evidence on a current-head commit containing the TLS optimization.
2. Run current-head pooled MUX throughput benchmarks at c1/c8/c32, with setup-inclusive and steady-state medians, and compare to Run 95.
3. Fix compile/test failures before doing further optimization.
4. Implement WSS physical-session pooling, preserving TLS verification, ALPN and stream lifecycle semantics.
5. Validate WSS pooled TCP with functional tests and current-head benchmarks.
6. Implement WSS UDP using the same raw-UDP framing rule as plain `ws://`; do not reuse TCP MUX DATA framing.
7. Extract/implement remaining exact GoWay compatibility behavior: SOCKS5/HTTP error branches, DNS, connection limits, keepalive/socket-buffer flags and non-MUX 1:1 pooling.
8. Implement QUIC and its TLS/pool/close/retry semantics from `SPEC.md` rather than assumptions.
9. Execute the full GoWay <-> RushWay interop matrix for TCP/UDP/WSS/QUIC/non-MUX/MUX.
10. Validate 1/100/500/1000 stream counts, large sustained transfers, slow/fast mixed streams, EOF/FIN, RST and reconnects.
11. Produce current-head Windows x64, Debian 12 x64 and ARMv7/OpenWrt artifacts.
12. Cut `v0.0.1` only after execution evidence and smoke tests are green.

### Performance backlog, in priority order

- [ ] Current-head pooled MUX benchmark.
- [ ] Large masked WebSocket DATA buffer reuse.
- [ ] Receive buffer capacity reuse.
- [ ] Shared writer-lock contention reduction.
- [ ] Session/stream lifecycle stress testing.
- [ ] Only after correctness: micro-optimizations that preserve wire compatibility.

### Platform / release backlog

- [x] Prior Windows x64 build evidence exists.
- [x] Prior Debian 12 x64 release build evidence exists.
- [ ] Verify ARMv7 with the intended Rust toolchain.
- [ ] Package KWRT/OpenWrt ARMv7 artifact.
- [ ] Re-run build matrix on the actual release head.
- [ ] v0.0.1 tag/release notes/artifacts.

### Three-file relay contract

There are intentionally **three** important handoff documents; do not create a fourth competing status/log file:

1. `AI_HANDOFF.md` — chronological history, exact commits/incidents, current boundaries and the next engineering sequence.
2. `PROGRESS.md` — compact canonical current-state dashboard, stage checklist and release gaps.
3. `SPEC.md` — compatibility contract and source-derived GoWay behavior.

When a milestone is genuinely verified, update the relevant status in all required files and record the exact commit/run/test evidence. Do not mark a feature complete because code merely exists.

### Rules for every future AI

- Clone/open the latest default branch and read all three relay files before coding.
- Treat `PROGRESS.md` as the current dashboard, `AI_HANDOFF.md` as history/decisions, and `SPEC.md` as protocol authority.
- Check latest GitHub Actions before trusting old CI claims.
- Use small, reviewable, independently verifiable commits.
- Do not claim a benchmark, build, test, interop result, or release readiness without actual evidence.
- Never route UDP payloads through TCP MUX DATA framing.
- Do not weaken TLS verification or silently change compatibility behavior for an optimization.
- Do not repeat a previously rejected/unverified large rewrite merely because it looks faster on paper.
- When a change fails, record the failure and the exact affected commit rather than hiding it.
- Preserve working paths while adding new transport functionality.

### Handoff entry format for future AIs

Append a dated section containing:

- code commit(s)
- files changed
- exactly what was implemented
- exactly what was verified
- test/benchmark/Actions run identifiers and relevant output
- known regressions or blockers
- the next single highest-priority action

This keeps future clones immediately actionable without relying on chat history.
