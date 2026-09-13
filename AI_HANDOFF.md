# RushWay AI Relay Handoff

## 2026-09-13 accelerated implementation checkpoint

Latest code head: `1c16c2195d4a01a2f1b901bdee73763719665134`.

### Current implementation

- Client `ws://` path uses physical MUX session pooling with logical stream reuse.
- MUX session pool now honors the user-facing `--mux-sessions` setting through the `RUSHWAY_MUX_SESSIONS` runtime value, clamped to 1..64.
- Per-session logical stream capacity remains 256.
- Stream lifecycle hardening prevents duplicate active-count decrements and avoids holding the shared writer lock during network reads.
- Existing server-side MUX forwarding, SOCKS5 TCP/UDP parsing, HTTP CONNECT, plain WebSocket and WSS client path remain present.
- `scripts/build-local.ps1` provides a direct Windows release build path.
- A minimal `.github/workflows/build-smoke.yml` now builds/tests Linux x64 and Windows x64 and uploads both artifacts when Actions runners execute normally.

### Delivery priority

Historical benchmark/audit reruns are not the active work queue. Continue implementation directly, then use CI for packaging and execution evidence.

### CI / delivery

The new Build Smoke workflow and the existing CI both triggered on the latest push, but the runner still failed immediately with `steps: null`. Therefore no new binary artifact has been produced yet. This remains a runner/infrastructure execution problem rather than compiler evidence.

### Verified performance baseline

Run 95 remains the last fully verified RushWay-only baseline:
- setup-inclusive median: c1 `43.31`, c8 `170.17`, c32 `345.55` MiB/s
- steady-state median: c1 `43.06`, c8 `205.04`, c32 `349.28` MiB/s
- MUX owned/reused decode: `1.54 ns/op`

These are not GoWay comparison results.

### Remaining implementation work

1. Finish non-MUX 1:1 transport path and matching server-side handshake.
2. Complete WSS UDP and WSS session reuse.
3. Implement QUIC runtime, retry/dead-IP and richer connection-pool behavior.
4. Complete remote DNS/cache behavior and socket/keepalive options.
5. Complete GoWay <-> RushWay interoperability coverage.
6. Produce final Windows x64, Debian 12 x64 and ARMv7 release packages from successful runner execution.

Do not mark 100% compatibility or release-ready until implementation and final execution evidence are complete.

## Relay files

- `PROGRESS.md` — canonical roadmap and verified status.
- `AI_HANDOFF.md` — chronological AI relay log.
- `SPEC.md` — compatibility and implementation specification.
