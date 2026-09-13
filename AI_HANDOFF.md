# RushWay AI Relay Handoff

## 2026-09-13 continuous checkpoint

Latest code head: `f14f99c5b3295b3166cb95c98d9c3c6e54f9892e`.

### Current implementation

- Client `ws://` path now uses a physical MUX session pool with 4 prewarmed sessions and logical stream reuse.
- Stream lifecycle hardening prevents duplicate active-count decrements and avoids holding the shared writer lock during network reads.
- Existing server-side MUX forwarding, SOCKS5 TCP/UDP parsing, HTTP CONNECT, plain WebSocket and WSS client path remain present.
- CI now uploads Linux x64, Windows x64, Debian 12 x64 and ARMv7 binaries as artifacts whenever the runner executes successfully.
- `.github/workflows/release.yml` was added for tagged/manual release artifact generation.

### CI incident / build delivery status

Runs 101 through 110 have failed before normal workflow steps execute (`steps: null`). This is a GitHub Actions runner-layer failure, not compiler output. Run 95 remains the last fully verified RushWay-only execution.

Because the current environment cannot execute Rust locally (no Cargo toolchain/cache and no outbound DNS) and GitHub Actions is failing before job startup, there is not yet a newly compiled pooled-client binary that can honestly be attached from this checkpoint.

### Verified performance baseline to preserve

Run 95 remains the last fully verified RushWay-only baseline:
- setup-inclusive median: c1 `43.31`, c8 `170.17`, c32 `345.55` MiB/s
- steady-state median: c1 `43.06`, c8 `205.04`, c32 `349.28` MiB/s
- MUX owned/reused decode: `1.54 ns/op`

These are not GoWay comparison results.

### Remaining compatibility work

The project is not honestly at 100% GoWay v1.8.4 parity yet. The specification still has open implementation/evidence requirements for WSS UDP, QUIC runtime, non-MUX mode, remote DNS/cache behavior, retry/dead-IP policy, full connection-pool semantics, complete CLI parity, and executable GoWay <-> RushWay interoperability coverage.

### Continuous execution order

1. Restore a normal GitHub Actions runner execution and compile the current pooled-client head.
2. Download and manually test the Windows x64 binary first.
3. Test SOCKS5 TCP / HTTP CONNECT through a real GoWay server and compare repeated c1/c8/c32 throughput.
4. Build and test Debian 12 x64 and ARMv7 binaries.
5. Continue WSS UDP, QUIC, non-MUX and retry/dead-IP implementation using SPEC-derived behavior only.
6. Add executable interoperability tests for GoWay Client -> RushWay Server and RushWay Client -> GoWay Server.
7. Only after all required execution evidence is green, mark v0.0.1 release-ready.

Do not claim 100% compatibility, GoWay parity or performance improvement without execution evidence.

## Relay files

- `PROGRESS.md` — canonical roadmap and verified status.
- `AI_HANDOFF.md` — chronological AI relay log.
- `SPEC.md` — compatibility and implementation specification.
