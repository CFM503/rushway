# RushWay v0.0.1 — Continuous Handoff Progress

> Compatibility baseline: GoWay v1.8.4, stable commit `538dbee86b9fbf248a68c8c6d8eee5d6f8bdb0dc` from `CFM503/way/goway`.
> Goal: tested, protocol-compatible Rust replacement with standalone Windows x64, Debian 12 x64 and KWRT/OpenWrt ARMv7 executables.

## Current checkpoint — 2026-09-13

**Overall engineering completion: ~65% estimate.** This is migration progress, not production-readiness evidence.

- Stage 1 bootstrap: `[~]`
- Stage 2 v1.8.4 extraction: `[~]`
- Stage 3 Rust implementation: `[~]`
- Stage 4 executable compatibility tests: `[ ]`
- Stage 5 release builds: `[~]`
- Stage 6 GitHub Actions: `[~]` — latest known jobs fail before executable workflow steps start
- Stage 7 v0.0.1 release: `[ ]`

## Current head and completed implementation

Latest code head for the WSS pooling milestone: `3de81c63fb06062183004c75e5928f0cfb851319`.

### Client front-end

- [x] SOCKS5 no-auth TCP CONNECT front-end.
- [x] HTTP CONNECT front-end.
- [x] TCP target dialing with timeout and TCP_NODELAY on the current runtime path.
- [x] `--mux` / `--no-mux` CLI switches parsed.
- [x] `--mux-sessions` CLI option, mapped to `RUSHWAY_MUX_SESSIONS`, clamped 1..64, default 4.
- [ ] Full non-MUX runtime parity with GoWay.

### Plain WebSocket / MUX

- [x] Functional plain `ws://` client/server handshake.
- [x] GoWay-compatible XOR MUX hello/OK boundary.
- [x] MUX SYN/DATA/FIN/RST flow.
- [x] uint16 DATA chunking.
- [x] Bounded per-stream queue/backpressure.
- [x] Target EOF -> FIN.
- [x] Target read error -> RST.
- [x] Local EOF -> FIN.
- [x] Remote FIN -> local half-close.
- [x] Remote RST -> local termination.

### Physical MUX pooling

- [x] Plain `ws://` physical MUX session reuse.
- [x] Least-active physical-session selection.
- [x] Logical stream multiplexing over shared sessions.
- [x] Physical session retirement on reader termination.
- [x] Configurable physical session count.
- [x] 256 logical streams/session.
- [x] Active-count lifecycle hardening.
- [x] TCP MUX serialization buffer reuse.
- [ ] Atomic acquire/stream-count reservation under heavy concurrent bursts.
- [ ] Current-head pooled throughput benchmark.

### UDP

- [x] Plain `ws://` SOCKS5 UDP relay slice.
- [x] Separate raw `UDP\n` WebSocket handshake/session.
- [x] UDP payloads are raw WebSocket binary frames, not TCP MUX DATA.
- [ ] WSS UDP end-to-end implementation/validation.
- [ ] Full GoWay UDP error/FRAG/reply lifecycle parity evidence.

### WSS / TLS

- [x] WSS client TCP forwarding path.
- [x] Verified rustls `ClientConfig` cached with `OnceLock`.
- [x] Insecure rustls `ClientConfig` cached separately with `OnceLock`.
- [x] TLS verification behavior and HTTP/1.1 ALPN preserved.
- [x] Unit pointer-reuse check for cached TLS configs.
- [x] WSS physical-session pooling implementation.
- [ ] WSS pooled TCP executable/functional validation.
- [ ] WSS pooled throughput benchmark.
- [ ] WSS UDP.
- [ ] Server-side WSS runtime.

## WebSocket codec status

- [x] RFC6455 data/control frame support for current non-fragmented path.
- [x] 64 MiB maximum frame size.
- [x] 8192-byte HTTP header hard limit.
- [x] Client masking / server unmasked frames.
- [x] Ping -> Pong, Pong discard, Close -> EOF.
- [x] Thread-local xorshift-style mask generation.
- [ ] Reuse outbound large-frame masking buffer.
- [ ] Reuse receive-buffer capacity.
- [ ] Reduce shared writer-lock contention.

A proposed large-frame masking-reuse rewrite was deliberately not committed because it could not be safely validated. Preserve the current `src/ws.rs` implementation until a clean, testable patch exists.

## Functional runtime boundary

- [x] Plain `ws://` MUX TCP forwarding.
- [x] Plain `ws://` SOCKS5 UDP slice.
- [x] WSS TCP forwarding.
- [x] WSS physical-session pooling implementation.
- [ ] WSS pooled TCP executable validation.
- [ ] WSS UDP.
- [ ] Runtime QUIC (`quic://`, `quic+tls://`).
- [ ] Full retry/dead-IP/connection-pool parity.
- [ ] Runtime non-MUX 1:1 pooling parity.
- [ ] Exact DNS/runtime compatibility.
- [ ] Full GoWay <-> RushWay executable interoperability.

## Compatibility extraction remaining

`SPEC.md` is the authority for observed GoWay behavior. Still to reconcile before marking compatibility complete:

- [ ] Every SOCKS5 TCP/UDP REP/error branch.
- [ ] UDP FRAG behavior and relay reply/close lifecycle.
- [ ] HTTP CONNECT dial/error/close behavior.
- [ ] DNS timeout/cache/fallback behavior.
- [ ] socket-buffer and keepalive flag semantics.
- [ ] block-local behavior.
- [ ] max-conn behavior.
- [ ] browser-profile HTTP/TLS fingerprint parity where required.
- [ ] Full QUIC config/TLS/stream bootstrap/close/retry behavior.
- [ ] Non-MUX 1:1 connection pool semantics.

## Required interoperability evidence

The final compatibility pass must execute, not merely compile, at least:

1. GoWay client -> RushWay server.
2. RushWay client -> GoWay server.
3. WS authenticated.
4. WS open mode where supported.
5. MUX enabled.
6. MUX disabled / 1:1.
7. WSS.
8. QUIC / QUIC+TLS.
9. SOCKS5 TCP.
10. HTTP CONNECT.
11. SOCKS5 UDP.
12. EOF / FIN.
13. RST.
14. disconnect/reconnect.
15. 1 / 100 / 500 / 1000 logical streams.
16. large sustained payloads.
17. mixed slow and fast streams.
18. invalid configuration and argument handling.

## Performance evidence

The last fully verified RushWay-only benchmark is Run 95 (`34755206730`). Historical results:

| Workload | c1 | c8 | c32 |
|---|---:|---:|---:|
| Setup-inclusive median, 5 samples | 43.31 | 170.17 | 345.55 MiB/s |
| Steady-state median, 5 samples | 43.06 | 205.04 | 349.28 MiB/s |

Historical formal e2e from that run:
`rushway_e2e payload_mib=4 roundtrip_echo=1 c1_mib_s=43.06 c8_mib_s=166.93 c32_mib_s=383.52`

Historical MUX ownership microbenchmark:
- copied decode: `11456.35 ns/op`
- owned/reused decode: `1.54 ns/op`
- reported directional speedup: `7431.47x`

These are historical only. Do not use them as current-head speed or GoWay comparison claims.

### Performance backlog

1. Current-head pooled MUX c1/c8/c32 setup-inclusive and steady-state medians.
2. Current-head WSS pooled TCP c1/c8/c32 setup-inclusive and steady-state medians.
3. Large outbound WebSocket masking allocation reuse.
4. Receive buffer capacity reuse.
5. Shared writer-lock contention.
6. Stream/session lifecycle stress.
7. Re-benchmark every transport optimization after correctness evidence.

## CI / build evidence

Latest known evidence before the WSS pooling commits:

- RushWay CI run #136, id `34761469476`, triggered from docs commit `a3e8f36836793635a967c17b1319d860e186ac16`: failed at the job level before executable workflow steps; `test` and `goway-comparison` failed, platform jobs were skipped.
- RushWay Build Smoke run #17, id `34761469458`, same trigger: failed at the job level before executable workflow steps.
- No compiler/test logs were produced, so these are runner/infrastructure failures rather than compile failures.
- Relay environment could not resolve `github.com`; current-head local clone/build was therefore unavailable.

The WSS pooling commits `5f171ec` and `3de81c6` therefore still require fresh executable build/test evidence. Do not claim current-head compile/test success until a new workflow or local execution produces logs.

## Platform / release

- [x] Prior Windows x64 build evidence exists.
- [x] Prior Debian 12 x64 release build evidence exists.
- [ ] Re-run Windows x64 build on actual release head.
- [ ] Re-run Debian 12 x64 build on actual release head.
- [ ] Verify ARMv7 with intended Rust toolchain.
- [ ] Package KWRT/OpenWrt ARMv7 artifact.
- [ ] Final artifact smoke test.
- [ ] v0.0.1 release/tag.

## Accelerated implementation sequence

1. Obtain executable CI/build evidence for the current WSS pooling head.
2. Fix compile/test failures, if any.
3. Run current-head plain MUX and WSS pooled TCP benchmarks at c1/c8/c32 and compare with the historical Run 95 baseline.
4. Implement WSS UDP with raw WebSocket UDP framing.
5. Complete GoWay compatibility extraction for SOCKS5/HTTP/DNS/options/limits.
6. Implement runtime non-MUX behavior.
7. Implement QUIC and `quic+tls` from the source-derived contract.
8. Execute the full GoWay <-> RushWay interop matrix.
9. Stress 1/100/500/1000 streams, large payloads, slow/fast concurrency, EOF/RST and reconnect.
10. Validate release artifacts on Windows x64, Debian 12 x64 and ARMv7/OpenWrt.
11. Cut v0.0.1 only after execution evidence and smoke tests are green.

## AI relay rules

Every future AI must:

1. Read `AI_HANDOFF.md`, `PROGRESS.md` and `SPEC.md` before changing code.
2. Start from the latest repository head, not from chat assumptions.
3. Check the latest Actions runs before trusting old build/test statements.
4. Make one logical change at a time and record the exact commit.
5. Record actual verification evidence: commands, test output, benchmark IDs or Actions run IDs.
6. Never mark interop, WSS UDP, QUIC, non-MUX, production readiness or release readiness complete without execution evidence.
7. Never route UDP payloads through TCP MUX DATA framing.
8. Do not weaken TLS verification or compatibility behavior to obtain a benchmark result.
9. Preserve both `AI_HANDOFF.md` and `PROGRESS.md` after every verified milestone.
10. Treat `SPEC.md` as protocol authority when implementation and assumptions conflict.
