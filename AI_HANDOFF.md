# RushWay AI Relay Handoff

## 2026-09-13 continuous engineering checkpoint

Latest code head: `590a818c843af81043277f107eaae2fc8bd375c8`.

### CI / platform status

- Run 100 (`34756739632`) proved the Rust 1.82 main test/release path and Windows/Debian paths; its real ARM failure was a transitive Edition 2024 dependency requiring a newer Cargo.
- ARMv7 CI was then pinned to Rust 1.86.0 in `.github/workflows/ci.yml` while the main compatibility/test job remains Rust 1.82.0.
- Runs 101-105 repeatedly failed at the GitHub Actions run/job layer with jobs reported as completed + failure but `steps: null`; no compile or test logs were produced. Treat these as CI infrastructure failures, not code evidence.

### New runtime optimization work

Commits:
- `f23ab32d8b5ef8343a81b9bfa4665da945d06e5b` — add `src/mux_pool.rs` with client-side physical MUX session pooling.
- `590a818c843af81043277f107eaae2fc8bd375c8` — route normal `ws://` client mode through the pooled MUX client.

The new client path changes the connection model:
- prewarm up to 4 physical WebSocket/MUX sessions;
- local TCP proxy connections become logical MUX streams;
- choose the least-active reusable physical session;
- maintain a per-session stream dispatch map and one reader task;
- reuse the same physical WebSocket across repeated local TCP connections;
- retain the existing WSS client path separately;
- retain SOCKS5 UDP handling in the pooled client module.

This directly targets the largest currently visible client-side setup overhead: every local TCP connection previously created a fresh upstream WebSocket + MUX physical connection.

### Current performance interpretation

This pooling change is an architectural optimization, not yet a measured speed claim. Existing RushWay-only benchmark evidence remains:

| Mode | c1 | c8 | c32 |
|---|---:|---:|---:|
| Setup-inclusive median, Run 95 | 43.31 | 170.17 | 345.55 MiB/s |
| Steady-state median, Run 95 | 43.06 | 205.04 | 349.28 MiB/s |

Existing MUX ownership microbenchmark evidence remains directional only:
- copied decode: `11456.35 ns/op`
- owned/reused decode: `1.54 ns/op`
- reported owned-vs-copy speedup: `7431.47x`

No RushWay-vs-GoWay speed conclusion is permitted until the optional private GoWay comparison executes successfully.

### Static audit items before trusting the pooled client

The new pool should be compiled and exercised before further optimization. Pay particular attention to:
- stream active-counter lifecycle on terminal FIN/RST;
- reader-loop WebSocket ping/pong handling without holding the shared writer lock across a network read;
- session replacement after physical connection failure;
- prewarm behavior when the upstream is unavailable;
- repeated c1/c8/c32 setup-inclusive and steady-state medians versus the pre-pool baseline.

### Functional gaps still open

- WSS UDP
- runtime QUIC
- runtime connection pool/reuse/retry/dead-IP beyond the new TCP MUX physical-session reuse
- runtime non-MUX mode
- true GoWay <-> RushWay interoperability evidence
- Debian 12 x64 artifact packaging
- KWRT/OpenWrt ARMv7 artifact packaging
- v0.0.1 release

Compiler warnings remain but are not current correctness evidence.

## Relay operating rule

1. Read `PROGRESS.md`, `AI_HANDOFF.md` and `SPEC.md` before changing code.
2. Treat the latest repository head and latest Actions run as authoritative; old benchmark notes are historical.
3. Make one logical runtime change at a time, validate it, then record exact commit/run/evidence.
4. Never convert a microbenchmark, RushWay-only benchmark or documentation claim into a GoWay comparison claim.
5. Keep `PROGRESS.md` as the canonical status table and this file as the chronological handoff log.
