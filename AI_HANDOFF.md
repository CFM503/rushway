# RushWay AI Relay Handoff

## 2026-09-13 verified checkpoint

Latest verified head: `4ee6ef18697bb7fcc4549f7e49e8d1260a3b5a47`

Workflow `34755206730` (Run 95) is fully green:
- Rust 1.82 tests: success
- release build: success
- MUX benchmark: success
- formal e2e benchmark: success
- common proxy benchmark self-check: success
- repeated setup-inclusive median: success
- repeated steady-state median: success
- Windows x64 GNU build: success

The optional GoWay comparison job completed successfully but did not execute its comparison steps, so there is still no measured GoWay throughput result.

## Run 95 benchmark evidence

MUX microbenchmark:
- copied decode: `11456.35 ns/op`
- owned/reused decode: `1.54 ns/op`
- reported owned-vs-copy speedup: `7431.47x`

Formal e2e:
`rushway_e2e payload_mib=4 roundtrip_echo=1 c1_mib_s=43.06 c8_mib_s=166.93 c32_mib_s=383.52`

Common cross-proxy five-sample medians:

| Mode | c1 | c8 | c32 |
|---|---:|---:|---:|
| Setup-inclusive | 43.31 | 170.17 | 345.55 MiB/s |
| Steady-state | 43.06 | 205.04 | 349.28 MiB/s |

Both modes use a 4 MiB deterministic payload, concurrency 1/8/32, local TCP echo, local SOCKS5 no-auth and WebSocket upstream. Setup-inclusive includes initial upstream setup; steady-state performs one warm-up flow before timed cases.

These are RushWay-only measurements. They are not evidence that RushWay is faster than GoWay.

## Benchmark infrastructure history

- `4ee6ef18697bb7fcc4549f7e49e8d1260a3b5a47` — current verified head; steady-state runner formatting fixed.
- `c985438d7412c71521abbca235da41c27a080354` — steady-state benchmark CI introduced before the formatting fix.
- `a0e787a6642401003f34e941d09d31501dbb1422` — optional pinned GoWay comparison job.
- `1b1a5cc01d6b2e0d5dfb3b707e0d6733ad12725c` — repeated five-sample median stage.
- `872ca45b800ab06c8594e4317b91d2ed1128030b` — repeated benchmark wrapper.
- `e10020e7437bf575b6f674591b2ca5c146233fe1` — common proxy benchmark runner.
- `7a5d4f2f72c021e6a9cb9aab5e7236e61718d5a2` — formal RushWay e2e baseline.

## Current runtime performance status

Completed ownership work:
- `OwnedMuxFrame` retains original frame storage.
- Server MUX DATA receive and WSS downstream use owned frames.
- Plain WebSocket downstream uses the owned frame path.
- DATA payload is forwarded without cloning.
- WebSocket binary ownership transfer is covered by tests.

Potential hotspots still requiring evidence:
- outbound MUX frame allocation
- WebSocket receive-buffer capacity reuse
- shared WebSocket writer-lock contention
- physical MUX session pooling / scheduling

Do not optimize those areas blindly. First compare GoWay under the identical runner or collect allocation/CPU profile evidence.

## Functional gaps still open

- WSS UDP
- runtime QUIC
- runtime connection pool/reuse/retry/dead-IP
- runtime non-MUX mode
- true GoWay <-> RushWay interoperability evidence
- Debian 12 x64 build/artifact packaging
- KWRT/OpenWrt ARMv7 build/artifact packaging
- v0.0.1 release

Compiler warnings remain but are not current CI failures.

## Immediate relay sequence

1. Obtain the required access for the private GoWay benchmark and run the pinned GoWay v1.8.4 through both benchmark modes.
2. Produce one RushWay vs GoWay c1/c8/c32 comparison table.
3. Profile the slower/hotter path and optimize only evidenced hotspots.
4. Continue WSS UDP, QUIC, non-MUX, pool/retry/dead-IP, interoperability and platform build work.
5. Synchronize `PROGRESS.md` and `AI_HANDOFF.md` after each verified milestone.

## Do not claim without execution evidence

- GoWay/RushWay interoperability
- RushWay faster than GoWay
- WSS UDP complete
- QUIC runtime complete
- non-MUX parity
- production readiness
- v0.0.1 release readiness

## Canonical relay files

- `PROGRESS.md` — roadmap, stage status, verified evidence and next sequence.
- `AI_HANDOFF.md` — detailed chronological handoff, incidents and benchmark caveats.
- `SPEC.md` — implementation/compatibility specification.
