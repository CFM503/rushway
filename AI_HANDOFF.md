# RushWay AI Relay Handoff

## 2026-09-13 verified checkpoint

Current optimization/benchmark infrastructure:
- `a0e787a6642401003f34e941d09d31501dbb1422` — CI adds an optional pinned GoWay v1.8.4 comparison job. It uses Go 1.25.0 and requires the optional `WAY_READ_TOKEN` secret because `CFM503/way` is private.
- `9d3bc56358218aec673af589334027d823a900a0` — cross-proxy runner accepts GoWay's `-p :port` listen-address form while retaining RushWay's port-only form.
- `1b1a5cc01d6b2e0d5dfb3b707e0d6733ad12725c` — CI adds five repeated RushWay samples and median reporting.
- `872ca45b800ab06c8594e4317b91d2ed1128030b` — added `scripts/repeat_proxy_bench.sh` for repeated samples and c1/c8/c32 medians.
- `16d92b3ee6c93404bcfc357b3701f4a3efee1d69` — CI initially added the common proxy benchmark self-check.
- `e10020e7437bf575b6f674591b2ca5c146233fe1` — added `src/bin/proxy_bench.rs`, a reusable runner that launches either proxy implementation under the same workload.
- `7a5d4f2f72c021e6a9cb9aab5e7236e61718d5a2` — formal RushWay e2e benchmark baseline, CI Run 76 green.

## Current CI state

### Run 89 — workflow `34754682561`
Head when created: `a0e787a6642401003f34e941d09d31501dbb1422`
- `goway-comparison` job `103716949326`: **success**, but intentionally skipped all comparison steps because `WAY_READ_TOKEN` is not configured.
- `test` job `103716949458`: was still **in progress** at the latest inspection. Its completion must be observed before this checkpoint is called fully verified.

No GoWay throughput number has been produced. Do not infer one from source documentation.

## Benchmark evidence

Run 76 / workflow `34752632479`:
`rushway_e2e payload_mib=4 roundtrip_echo=1 c1_mib_s=43.69 c8_mib_s=219.81 c32_mib_s=340.16`

Run 83 / workflow `34753910015` demonstrated why repeated sampling is necessary on the connection-establishment-inclusive workload:
- formal `e2e_bench`: c1 76.42, c8 161.85, c32 314.20 MiB/s
- common `proxy_bench`: c1 76.48, c8 274.57, c32 329.86 MiB/s

The large c8 difference occurred on the same runner, so a single run is not a valid optimization decision. `scripts/repeat_proxy_bench.sh` therefore collects five samples and reports medians.

## Cross-proxy definition

The common runner uses:
- 4 MiB deterministic payload per flow;
- concurrency 1/8/32;
- local TCP echo target;
- local SOCKS5 no-auth negotiation;
- WebSocket upstream;
- connection setup, SOCKS5 negotiation, WebSocket handshake, transfer and echo inside the measured interval.

The runner adapts only the listen-address syntax needed by each implementation: RushWay receives `-p <port>`, GoWay receives `-p :<port>`. This is a CLI compatibility adjustment, not a benchmark condition change.

GoWay reference remains `CFM503/way` commit `538dbee86b9fbf248a68c8c6d8eee5d6f8bdb0dc`. Its source documents four default physical MUX sessions and pooled/owned data paths, but those are reference characteristics rather than measured parity.

## Runtime performance status

- `OwnedMuxFrame` is used by server MUX DATA receive and WSS downstream paths.
- DATA payload is forwarded from owned storage without payload cloning.
- Plain WS downstream uses `read_frame_owned`.
- Outbound MUX frame allocation remains a possible optimization target.
- WebSocket receive buffer capacity reuse remains unproven.
- Shared writer-lock contention and physical session pooling have not been profiled against GoWay.

Do not optimize those paths merely because they look expensive. First use the repeated controlled comparison or allocation/profile evidence.

## Immediate relay sequence

1. Finish and inspect Run 89 test/Windows results.
2. Record the five-sample RushWay median from CI.
3. Enable `WAY_READ_TOKEN` only when the private GoWay comparison is ready; then run the pinned GoWay v1.8.4 binary through the identical runner.
4. Produce a single GoWay/RushWay c1/c8/c32 comparison table before runtime performance changes.
5. Add a steady-state transfer benchmark that excludes connection setup.
6. Profile the slower path and optimize only evidence-backed hotspots.
7. Later continue WSS UDP, QUIC, non-MUX, connection pool/retry/dead-IP, Debian/KWRT builds and release packaging.

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
