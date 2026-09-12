# RushWay v0.0.1 — Continuous Handoff Progress

> Purpose: allow any AI/developer to resume work without repeating completed stages.
> Compatibility baseline: GoWay v1.8.4, stable source commit `538dbee86b9fbf248a68c8c6d8eee5d6f8bdb0dc` from `CFM503/way/goway`.
> Release target: **v0.0.1**.
> Final runtime artifact: one standalone executable per target platform.

## Current overall status

**Stage 2 — GoWay v1.8.4 specification extraction: `[~]`**

A verified intermediate specification has been added as `SPEC.md`.

## Completed project work

### Stage 1 — Repository/bootstrap `[~]`
- [x] Confirm `CFM503/rushway` exists and is writable.
- [x] Initialize Rust/Cargo project.
- [x] Set package version to `0.0.1`.
- [x] Add initial CLI/config skeleton.
- [x] Add progress/handoff document.
- [ ] Final README and compatibility documentation.
- [ ] LICENSE if required.

### Stage 2 — GoWay v1.8.4 specification extraction `[~]`
- [x] Identify stable compatibility commit: `538dbee86b9fbf248a68c8c6d8eee5d6f8bdb0dc`.
- [x] Extract complete CLI option list visible in v1.8.4 main path.
- [x] Extract core Config fields and derived runtime resources.
- [x] Confirm client/server selection: `-up` present = Client; omitted = Server.
- [x] Confirm upstream schemes: `ws`, `wss`, `quic`, `quic+tls`.
- [x] Confirm MUX command values and 7-byte frame header layout.
- [x] Confirm MUX SYN/DATA/FIN/RST semantics at the frame level.
- [x] Confirm 8192-byte HTTP header limit and 64 MiB WebSocket frame limit.
- [x] Confirm XOR compatibility primitive: SHA-256-derived repeating expanded key.
- [x] Confirm pooled buffering/backpressure/lifecycle design at a high level.
- [x] Map v1.8.4 regression tests for header limit, PRNG lifecycle, target lifecycle, slow/fast/RST, 1000 streams and log-ring concurrency.
- [x] Add `SPEC.md` with verified intermediate migration contract.
- [ ] Read all remaining sections of `goway.go` without truncation.
- [ ] Extract exact WebSocket handshake/header validation behavior.
- [ ] Extract exact SOCKS5 TCP/UDP behavior.
- [ ] Extract exact HTTP CONNECT behavior.
- [ ] Extract exact QUIC listener/client/session/stream behavior.
- [ ] Extract exact connection-pool algorithms and retry/dead-IP behavior.
- [ ] Extract exact JSON/config-file schema and behavior.
- [ ] Extract all remaining tests and map them to Rust tests.
- [ ] Reconcile every extracted behavior against `SPEC.md`.

### Stage 3 — Rust core implementation `[ ]`
- [ ] CLI/config compatibility.
- [ ] Protocol frame encoding/decoding.
- [ ] Crypto compatibility.
- [ ] WebSocket transport.
- [ ] SOCKS5 + HTTP CONNECT front-end.
- [ ] TCP forwarding.
- [ ] MUX sessions/streams/backpressure.
- [ ] TLS/SNI/fakehost behavior.
- [ ] QUIC.
- [ ] DNS resolver/cache.
- [ ] Connection pooling/reuse/reconnect.
- [ ] Cancellation/EOF/FIN/RST/error handling.
- [ ] Statistics/logging/TUI where appropriate.

### Stage 4 — Compatibility and tests `[ ]`
- [ ] Rust unit tests.
- [ ] Rust integration tests.
- [ ] GoWay Client ↔ RushWay Server.
- [ ] RushWay Client ↔ GoWay Server.
- [ ] 1/100/500/1000 streams.
- [ ] Large payload/file.
- [ ] Slow/fast streams.
- [ ] EOF/FIN/RST.
- [ ] Disconnect/reconnect.
- [ ] TLS/WSS.
- [ ] QUIC.
- [ ] SOCKS5 TCP/UDP.
- [ ] HTTP CONNECT.
- [ ] Invalid arguments/config.

### Stage 5 — Single-file release builds `[ ]`
- [ ] Windows x86_64.
- [ ] Debian 12 x86_64.
- [ ] KWRT/OpenWrt ARMv7.
- [ ] Standalone executable verification.
- [ ] `--version` / `--help` verification.

### Stage 6 — GitHub Actions `[ ]`
- [ ] CI workflow.
- [ ] Three target builds.
- [ ] Release workflow.
- [ ] Artifact packaging and naming.
- [ ] Tests before publish.

### Stage 7 — v0.0.1 release `[ ]`
- [ ] Tag `v0.0.1`.
- [ ] Successful required Actions runs.
- [ ] Verified downloadable artifacts.
- [ ] Release notes.
- [ ] Final end-to-end verification.

## Important compatibility findings

- MUX header is 7 bytes: 4-byte big-endian stream ID + 1-byte command + 2-byte big-endian payload length.
- SYN payload starts with a 2-byte target-address length, followed by target address and optional initial data.
- DATA is chunked because payload length is uint16.
- FIN is the stream half-close/EOF signal; RST is abrupt reset/error.
- Client MUX buffering is bounded; server stream delivery also uses bounded queues/backpressure.
- A slow stream must not stall unrelated streams.
- Close/RST must release resources and unblock pending operations.
- Server mode without key requires explicit open-proxy permission.
- `-max-conn` default is 1000 and validated to 1..1,000,000.
- `-W` default is 128 KiB; runtime buffer has a lower bound around 64 KiB and an upper bound around 12 MiB plus framing overhead.
- Remote DNS falls back to system DNS on failure and caches successful results.
- `-fakehost` affects CDN/reverse-proxy Host/SNI behavior and requires exact source-level treatment.

## Verification policy

No cross-platform build, interoperability result, benchmark result, or Release is marked complete unless there is actual execution evidence. The current connector can write/read repository files but does not itself constitute evidence that Rust binaries compile or interoperate.

## Current commit

Latest RushWay commit after this documentation step: `600829f0e6f12b16b8fd84e99c799635330238d6` (`SPEC.md`).

Previous project commits remain in history, including the initial Cargo/CLI skeleton and progress tracker.

## Exact commands actually run in this work session

No local Rust/Cargo build was executed in this session. No GitHub Actions build was executed in this session. Repository changes were written through GitHub's repository API.

## Outstanding failures / limitations

- Full `goway.go` extraction is not finished because the source is large and tool responses can be truncated.
- Exact QUIC, pool, proxy parser, config-file and handshake details remain to be extracted.
- RushWay core is still only the initial skeleton; it is not a functional GoWay replacement yet.

## Single recommended next step

**Continue Stage 2:** extract the remaining GoWay v1.8.4 source in smaller verified sections, especially QUIC, WebSocket handshake, SOCKS5/HTTP, pools, config-file parsing and lifecycle, then reconcile the complete specification before substantial Rust implementation.

## Future target

The eventual target is a tested, protocol-compatible **RushWay v0.0.1** that can replace GoWay v1.8.4 in real deployments, interoperate in both directions, handle high stream counts reliably, and publish standalone Windows x64, Debian 12 x64 and KWRT/OpenWrt ARMv7 executables through GitHub Actions.

## Handoff instruction

Any future AI must read `PROGRESS.md` and `SPEC.md` first, inspect the current repository, continue from the first unchecked item, update this document after each meaningful step, record exact evidence, and never claim completion without verification.