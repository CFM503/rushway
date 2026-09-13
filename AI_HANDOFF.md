# RushWay AI Relay Handoff

## 2026-09-13 accelerated implementation checkpoint

Latest code head: `d2dabf16e0abffe635058e79a348956585b48335`.

### Current implementation

- Client `ws://` path uses physical MUX session pooling with 4 prewarmed sessions and logical stream reuse.
- Stream lifecycle hardening prevents duplicate active-count decrements and avoids holding the shared writer lock during network reads.
- Existing server-side MUX forwarding, SOCKS5 TCP/UDP parsing, HTTP CONNECT, plain WebSocket and WSS client path remain present.
- CI is configured to publish Linux x64, Windows x64, Debian 12 x64 and ARMv7 artifacts when the runner executes normally.
- Tagged/manual release workflow is present.
- `scripts/build-local.ps1` was added so Windows users can build a release EXE locally without depending on GitHub Actions.

### Delivery priority

Do not repeat historical audit rounds. Continue implementation directly. The immediate delivery goal is a runnable Windows x64 build followed by the remaining transport/runtime compatibility layers.

### CI status

Recent runs are still failing before normal workflow steps execute (`steps: null`). Treat this only as a delivery-channel problem; it must not block implementation progress.

### Verified performance baseline

Run 95 remains the last fully verified RushWay-only baseline:
- setup-inclusive median: c1 `43.31`, c8 `170.17`, c32 `345.55` MiB/s
- steady-state median: c1 `43.06`, c8 `205.04`, c32 `349.28` MiB/s
- MUX owned/reused decode: `1.54 ns/op`

These are not GoWay comparison results.

### Remaining implementation work

Priority order:
1. Finish non-MUX 1:1 transport path and matching server-side handshake.
2. Add configurable MUX session count and full CLI/config parity.
3. Complete WSS UDP and WSS session reuse.
4. Implement QUIC runtime, pool/retry/dead-IP behavior.
5. Complete remote DNS/cache behavior and connection limits/keepalive options.
6. Add executable GoWay <-> RushWay interoperability coverage.
7. Produce final Windows x64, Debian 12 x64 and ARMv7 release packages.

Do not mark 100% compatibility or release-ready until the above implementation work has real execution evidence.

## Relay files

- `PROGRESS.md` — canonical roadmap and verified status.
- `AI_HANDOFF.md` — chronological AI relay log.
- `SPEC.md` — compatibility and implementation specification.
