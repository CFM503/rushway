# RushWay v0.0.1 — Continuous Handoff Progress

> Compatibility baseline: GoWay v1.8.4, stable commit `538dbee86b9fbf248a68c8c6d8eee5d6f8bdb0dc` from `CFM503/way/goway`.
> Goal: tested, protocol-compatible Rust replacement with standalone Windows x64, Debian 12 x64 and KWRT/OpenWrt ARMv7 executables.

## Current checkpoint — 2026-09-13

**Overall engineering completion: ~60% (estimate).** This is migration progress, not production-readiness evidence.

- Stage 1 bootstrap: `[~]`
- Stage 2 v1.8.4 extraction: `[~]`
- Stage 3 Rust implementation: `[~]`
- Stage 4 compatibility tests: `[ ]`
- Stage 5 release builds: `[~]`
- Stage 6 GitHub Actions: `[~]` — jobs continue to fail before executable workflow steps start
- Stage 7 v0.0.1 release: `[ ]`

### Current head / optimization work

- Latest code optimization head remains `f20c19924e70b843338da2c245bb8668f7125bb3`.
- Client-side physical MUX session pooling is implemented for normal `ws://` traffic.
- `--mux-sessions` controls the physical pool through `RUSHWAY_MUX_SESSIONS`, clamped to 1..64.
- Per-session logical stream capacity is 256.
- TCP MUX serialization buffer reuse is implemented on the client hot path.
- The UDP regression was corrected so raw `UDP\n` WebSocket sessions use raw binary UDP payload frames instead of TCP MUX framing.
- WSS TLS `rustls::ClientConfig` construction is cached separately for verified and insecure modes with `OnceLock`.
- Existing WSS client TCP forwarding remains one upstream physical connection per local proxy connection; pooling is not yet implemented.

## Latest CI / environment evidence

RushWay CI run #131 and Build Smoke run #12, triggered by documentation commit `c90d8ca70bd7c9c33c472e1b46d2e60af1c43bc4`, failed before any workflow step executed. No useful compiler/test logs were produced. A retry of the earlier failure also did not produce executable workflow steps. Treat this as runner/infrastructure evidence, not compiler evidence.

The relay environment could not resolve `github.com`, so no local clone/build was possible. Do not claim current-head build/test success without actual execution evidence.

The last fully verified RushWay-only performance baseline remains Run 95 (`34755206730`).

## Verified historical performance baseline — RushWay only

| Workload | c1 | c8 | c32 |
|---|---:|---:|---:|
| Setup-inclusive median, 5 samples | 43.31 | 170.17 | 345.55 MiB/s |
| Steady-state median, 5 samples | 43.06 | 205.04 | 349.28 MiB/s |

Formal e2e from that verified run:
`rushway_e2e payload_mib=4 roundtrip_echo=1 c1_mib_s=43.06 c8_mib_s=166.93 c32_mib_s=383.52`

MUX ownership microbenchmark:
- copied decode: `11456.35 ns/op`
- owned/reused decode: `1.54 ns/op`
- reported directional speedup: `7431.47x`

These are historical local measurements, not current-head speed claims and not GoWay comparison evidence.

## Completed pooling / reuse work

- [x] Client-side physical MUX session reuse for normal `ws://` client traffic.
- [x] Least-active physical session selection.
- [x] Logical stream dispatch from one physical WebSocket to multiple local TCP connections.
- [x] Basic session retirement on physical reader termination.
- [x] User-configurable physical session count via `--mux-sessions`.
- [x] Per-session logical stream limit of 256.
- [x] Stream lifecycle active-count hardening.
- [x] TCP hot-path MUX serialization buffer reuse.
- [x] WSS TLS client configuration reuse through `OnceLock`.

## Functional runtime slice

- [x] Local SOCKS5 no-auth TCP CONNECT front-end.
- [x] Local HTTP CONNECT front-end.
- [x] TCP target dial with timeout and TCP_NODELAY.
- [x] Plain WebSocket client/server handshake integration.
- [x] GoWay-compatible XOR MUX hello/OK authentication boundary.
- [x] MUX SYN/DATA/FIN/RST runtime path.
- [x] Server-side bounded per-stream command queue.
- [x] Target-to-MUX DATA chunking at uint16 payload boundary.
- [x] Target EOF -> MUX FIN.
- [x] Target read error -> MUX RST.
- [x] Local EOF -> MUX FIN.
- [x] Remote MUX FIN -> local half-close.
- [x] Remote MUX RST -> local connection termination.
- [x] SOCKS5 UDP relay implementation slice for plain `ws://`.
- [x] TLS/WSS client TCP forwarding path.
- [ ] WSS physical-session pooling.
- [ ] WSS UDP.
- [ ] Runtime QUIC.
- [ ] Retry/dead-IP/connection-pool parity beyond physical MUX reuse.
- [ ] Runtime non-MUX mode.
- [ ] True GoWay <-> RushWay interoperability evidence.

## Performance work remaining

- [ ] Outbound WebSocket masking allocation per large DATA frame.
- [ ] WebSocket receive-buffer capacity reuse.
- [ ] Shared WebSocket writer-lock contention.
- [ ] Robust stream/session lifecycle stress testing.
- [ ] Current-head pooled MUX benchmark.

A proposed large-frame masking-reuse rewrite was deliberately not committed because it could not be safely validated in this relay environment. The existing WebSocket codec remains the known implementation and must be preserved until a clean, testable change is available.

## Platform / release gaps

- [x] Windows x64 build evidence on a prior green run.
- [x] Debian 12 x64 release build evidence on a prior green run.
- [ ] ARMv7 verified build after moving target job to Rust 1.86.
- [ ] KWRT/OpenWrt ARMv7 release artifact packaging.
- [ ] v0.0.1 release.

## Immediate accelerated sequence

1. Obtain executable CI/build evidence on a commit containing the current WSS TLS optimization.
2. Run the pooled TCP MUX setup-inclusive and steady-state c1/c8/c32 medians and compare against Run 95.
3. Fix any compile/test failures immediately.
4. Implement WSS physical-session reuse without weakening TLS verification or compatibility behavior.
5. Implement WSS UDP relay and validate packet framing/end-to-end behavior.
6. Validate GoWay client/server interoperability for TCP and UDP.
7. Produce Windows x64, Debian 12 x64 and ARMv7/OpenWrt artifacts.
8. Cut v0.0.1 only after implementation and execution evidence are complete.

## AI relay rule

Future/relay AI must:
1. Read `PROGRESS.md`, `AI_HANDOFF.md` and `SPEC.md` before changing code.
2. Start from the latest repository head; do not infer state from old notes.
3. Check the latest Actions run before trusting performance or compatibility claims.
4. Make one logical change at a time, validate it, then record exact commit/run/evidence.
5. Never mark GoWay interoperability, WSS UDP, QUIC, non-MUX, production readiness or release readiness complete without execution evidence.
6. Preserve both relay documents and synchronize them after every verified milestone.
