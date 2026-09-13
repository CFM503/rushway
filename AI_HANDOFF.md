# RushWay AI Relay Handoff

## 2026-09-13 accelerated implementation checkpoint

Latest code head: `b2623a9af779100b2bdb29d9a7122f1b1f17aee1`.

### Current implementation

- Client `ws://` path uses physical MUX session pooling with 4 prewarmed sessions and logical stream reuse.
- Stream lifecycle hardening prevents duplicate active-count decrements and avoids holding the shared writer lock during network reads.
- Existing server-side MUX forwarding, SOCKS5 TCP/UDP parsing, HTTP CONNECT, plain WebSocket and WSS client path remain present.
- CI is configured to publish Linux x64, Windows x64, Debian 12 x64 and ARMv7 artifacts when the runner executes normally.
- Tagged/manual release workflow is present.
- `scripts/build-local.ps1` provides a direct Windows release build path independent of the GitHub Actions runner.

### Delivery priority

Historical audits and benchmark reruns are not the current work queue. Continue implementation directly, then use CI only for final packaging and evidence.

### CI / delivery

The latest Actions runs have still been failing before normal workflow steps execute (`steps: null`), so they have not produced a new binary. The source tree remains the authoritative deliverable until a normal runner executes.

### Verified performance baseline

Run 95 remains the last fully verified RushWay-only baseline:
- setup-inclusive median: c1 `43.31`, c8 `170.17`, c32 `345.55` MiB/s
- steady-state median: c1 `43.06`, c8 `205.04`, c32 `349.28` MiB/s
- MUX owned/reused decode: `1.54 ns/op`

These are not GoWay comparison results.

### Remaining implementation work

1. Finish non-MUX 1:1 transport path and matching server-side handshake.
2. Make MUX session count configurable and wire the setting through both CLI/config and pool.
3. Complete WSS UDP and WSS session reuse.
4. Implement QUIC runtime, pool/retry/dead-IP behavior.
5. Complete remote DNS/cache behavior and connection/keepalive/socket options.
6. Complete GoWay <-> RushWay interoperability coverage.
7. Produce final Windows x64, Debian 12 x64 and ARMv7 release packages.

Do not mark 100% compatibility or release-ready until implementation and final execution evidence are complete.

## Relay files

- `PROGRESS.md` — canonical roadmap and verified status.
- `AI_HANDOFF.md` — chronological AI relay log.
- `SPEC.md` — compatibility and implementation specification.
