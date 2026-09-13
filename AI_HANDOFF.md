# RushWay AI Relay Handoff

## 2026-09-13 accelerated implementation checkpoint

Latest implementation head before documentation sync: `1c16c2195d4a01a2f1b901bdee73763719665134`.
Documentation sync commit: `89c5f706dde9db52cb30dabfcb7d4272b179eca3`.

### Current implementation

- Client `ws://` path uses physical MUX session pooling with logical stream reuse.
- MUX session pool honors the user-facing `--mux-sessions` setting through `RUSHWAY_MUX_SESSIONS`, clamped to 1..64.
- Per-session logical stream capacity remains 256.
- Stream lifecycle hardening prevents duplicate active-count decrements and avoids holding the shared writer lock during network reads.
- Existing server-side MUX forwarding, SOCKS5 TCP/UDP parsing, HTTP CONNECT, plain WebSocket and WSS client path remain present.
- `scripts/build-local.ps1` provides a direct Windows release build path.
- A minimal `.github/workflows/build-smoke.yml` builds/tests Linux x64 and Windows x64 and uploads both artifacts when Actions runners execute normally.

### Current CI / delivery reality

The latest push triggered both the main CI and Build Smoke workflows, but the runner failed before executing steps (`steps: []` / no runner assigned). Downstream platform jobs were skipped. Treat this as a GitHub Actions runner/infrastructure problem, not as compile/test evidence.

The last fully verified RushWay-only performance baseline remains Run 95. No GoWay throughput comparison has been established yet.

### Verified performance baseline

- setup-inclusive median: c1 `43.31`, c8 `170.17`, c32 `345.55` MiB/s
- steady-state median: c1 `43.06`, c8 `205.04`, c32 `349.28` MiB/s
- MUX owned/reused decode: `1.54 ns/op`

### Remaining implementation work — accelerated priority

1. Restore a normal Actions execution path and obtain actual build/test logs.
2. Benchmark the new physical MUX pooling against the Run 95 baseline.
3. Profile and remove remaining MUX DATA hot-path allocation/lock overhead.
4. Add WSS physical-session reuse while preserving TLS behavior.
5. Complete WSS UDP.
6. Implement non-MUX transport mode and matching server handshake.
7. Add retry/dead-IP and connection-pool parity.
8. Complete true GoWay <-> RushWay interoperability coverage.
9. Implement/validate QUIC runtime.
10. Produce Windows x64, Debian 12 x64 and ARMv7/OpenWrt release packages and cut v0.0.1 only after execution evidence.

### Operational rule

Do not mark GoWay interoperability, WSS UDP, QUIC, non-MUX, production readiness or release readiness complete without execution evidence.

## Relay files

- `PROGRESS.md` — canonical roadmap and verified status.
- `AI_HANDOFF.md` — chronological AI relay log.
- `SPEC.md` — compatibility and implementation specification.
