# RushWay AI Relay Handoff

## 2026-09-13 continuous checkpoint

Latest code head: `7be62764ff49bcf06eae74c43b6396d07701bcad`.

### Current status

The client now has physical MUX session reuse for normal `ws://` traffic. Up to 4 physical WebSocket/MUX sessions are reused, local TCP connections become logical MUX streams, and the least-active session is selected.

Latest hardening in `7be62764ff49bcf06eae74c43b6396d07701bcad`:
- terminal FIN/RST removes a stream at most once;
- active stream counts cannot underflow from duplicate cleanup;
- the reader task does not hold the shared writer lock while waiting for network input;
- dead physical sessions are retired and their stream map is cleared.

### CI status

Runs 101 through the latest pooled-client run are still failing before any job step starts (`steps: null`). Therefore the pooled client has not yet received compiler/test execution evidence from GitHub Actions.

ARMv7 remains pinned to Rust 1.86.0; the main test path remains Rust 1.82.0.

### Benchmark baseline to preserve

Run 95 remains the last fully verified RushWay-only baseline:
- setup-inclusive median: c1 `43.31`, c8 `170.17`, c32 `345.55` MiB/s
- steady-state median: c1 `43.06`, c8 `205.04`, c32 `349.28` MiB/s

These are not GoWay comparison results.

### Next sequence

1. Obtain a normal Actions execution and compile/test the pooled client.
2. Run pooled-client c1/c8/c32 setup-inclusive and steady-state medians.
3. Compare with the Run 95 baseline.
4. Run the pinned GoWay comparison when its benchmark path is available.
5. Continue the remaining WSS UDP, QUIC, non-MUX, retry/dead-IP, interoperability and release work.

Do not claim performance improvement or GoWay parity without execution evidence.

## Relay files

- `PROGRESS.md` — canonical roadmap and verified status.
- `AI_HANDOFF.md` — chronological AI relay log.
- `SPEC.md` — compatibility and implementation specification.
