# RushWay v0.0.1 — Continuous Handoff Progress

> Purpose: allow any AI/developer to resume work without repeating completed stages.
> Compatibility baseline: GoWay v1.8.4, stable source commit `538dbee86b9fbf248a68c8c6d8eee5d6f8bdb0dc` from `CFM503/way/goway`.
> Release target: **v0.0.1**.
> Final runtime artifact: one standalone executable per target platform.

## Current overall status

**Stage 2 — GoWay v1.8.4 specification extraction: `[~]`**

**Stage 3 — Rust core implementation: `[~]`**

The verified MUX primitive and an initial RFC6455 WebSocket frame codec are now in the repository. Neither is marked fully complete until compilation/tests and interoperability evidence exist.

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
- [x] Extract main CLI options visible in v1.8.4 main path.
- [x] Extract core Config fields and derived runtime resources.
- [x] Confirm client/server selection: `-up` present = Client; omitted = Server.
- [x] Confirm upstream schemes: `ws`, `wss`, `quic`, `quic+tls`.
- [x] Confirm MUX command values and 7-byte frame header layout.
- [x] Confirm MUX SYN/DATA/FIN/RST frame semantics.
- [x] Confirm 8192-byte HTTP header limit and 64 MiB WebSocket frame limit.
- [x] Confirm XOR compatibility primitive: SHA-256-derived repeating expanded key.
- [x] Confirm pooled buffering/backpressure/lifecycle design at a high level.
- [x] Map v1.8.4 regression tests for header limit, PRNG lifecycle, target lifecycle, slow/fast/RST, 1000 streams and log-ring concurrency.
- [x] Add `SPEC.md` with verified intermediate migration contract.
- [~] Extract WebSocket framing behavior: complete non-fragmented data frames, 64 MiB limit, RFC6455 control-frame limits, masking/unmasking, Ping/Pong/Close handling, RFC6455 accept-key vector. **Handshake HTTP validation is still pending.**
- [ ] Read all remaining sections of `goway.go` without truncation.
- [ ] Extract exact WebSocket handshake/header validation behavior.
- [ ] Extract exact SOCKS5 TCP/UDP behavior.
- [ ] Extract exact HTTP CONNECT behavior.
- [ ] Extract exact QUIC listener/client/session/stream behavior.
- [ ] Extract exact connection-pool algorithms and retry/dead-IP behavior.
- [ ] Extract exact JSON/config-file schema and behavior.
- [ ] Extract all remaining tests and map them to Rust tests.
- [ ] Reconcile every extracted behavior against `SPEC.md`.

### Stage 3 — Rust core implementation `[~]`
- [~] Add MUX frame codec in `src/protocol.rs` using the verified 7-byte big-endian header.
- [~] Add MUX command enum: SYN/DATA/FIN/RST.
- [~] Add SYN payload codec: uint16 target length + target + optional initial data.
- [~] Add protocol unit tests for round-trip and malformed frames. **Tests written; not executed yet.**
- [~] Add RFC6455 frame codec in `src/ws.rs`.
- [~] Add WebSocket accept-key calculation and RFC6455 test vector.
- [~] Add WebSocket frame length encoding for 0..125, 126..65535 and 64-bit lengths.
- [~] Add masking/unmasking and control-frame validation.
- [~] Add Ping/Pong/Close handling to the frame reader.
- [ ] Execute and pass Rust unit tests before marking these codecs complete.
- [ ] CLI/config compatibility.
- [ ] Crypto compatibility implementation.
- [ ] WebSocket HTTP handshake/client/server transport.
- [ ] SOCKS5 + HTTP CONNECT front-end.
- [ ] TCP forwarding.
- [ ] MUX session/stream state machine, backpressure and cancellation.
- [ ] TLS/SNI/fakehost behavior.
- [ ] QUIC.
- [ ] DNS resolver/cache.
- [ ] Connection pooling/reuse/reconnect.
- [ ] Statistics/logging/TUI where appropriate.

### Stage 4 — Compatibility and tests `[ ]`
- [ ] Rust unit tests executed successfully.
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

- MUX header is exactly 7 bytes: 4-byte big-endian stream ID + 1-byte command + 2-byte big-endian payload length.
- Valid MUX commands are SYN `0x01`, DATA `0x02`, FIN `0x03`, RST `0x04`.
- SYN payload starts with a 2-byte big-endian target-address length, followed by target address and optional initial data.
- DATA is chunked because payload length is uint16.
- FIN is the stream half-close/EOF signal; RST is abrupt reset/error.
- Client MUX buffering is bounded; server stream delivery also uses bounded queues/backpressure.
- A slow stream must not stall unrelated streams.
- Close/RST must release resources and unblock pending operations.
- Server mode without key requires explicit open-proxy permission.
- `-max-conn` default is 1000 and validated to 1..1,000,000.
- `-W` default is 128 KiB; runtime buffer has a lower bound around 64 KiB and an upper bound around 12 MiB plus framing overhead.
- Remote DNS falls back to system DNS on failure and caches successful results.
- GoWay WebSocket framing rejects fragmented data/control frames, caps frames at 64 MiB, caps control payloads at 125 bytes, unmasks masked frames and treats Close as EOF; Ping is answered with Pong and Pong is discarded.
- GoWay uses the RFC6455 accept-key formula: SHA-1(client key + RFC6455 GUID), Base64 encoded.
- Exact WebSocket HTTP handshake/header behavior is not yet fully reconciled.

## Verification policy

No cross-platform build, interoperability result, benchmark result, or Release is marked complete unless there is actual execution evidence. Repository writes are not build/test evidence.

## Current commit

`a462460a2557593ad59d884b214785672437273f` — WebSocket codec implementation/test cleanup.

## Exact verification commands actually run in this work session

No local Rust/Cargo build was executed. No local Rust/Cargo test was executed. No GitHub Actions build/test was executed. Repository changes were written through GitHub's repository API.

The next verification command that must be run by a runtime-capable environment is:

```text
cargo test
```

Then, if successful:

```text
cargo build --release
```

## Outstanding failures / limitations

- Full `goway.go` extraction is not finished because the source is large and connector responses can be truncated.
- Exact QUIC, pool, proxy parser, config-file and handshake details remain to be extracted.
- RushWay core is still not a functional GoWay replacement.
- The new MUX and WebSocket codecs have not yet been compiled or tested in this environment.

## Single recommended next step

**Continue Stage 2 extraction:** finish exact WebSocket handshake validation and then extract SOCKS5 TCP/UDP + HTTP CONNECT behavior from the GoWay v1.8.4 baseline before building the proxy front-end.

## Future target

The eventual target is a tested, protocol-compatible **RushWay v0.0.1** that can replace GoWay v1.8.4 in real deployments, interoperate in both directions, handle high stream counts reliably, and publish standalone Windows x64, Debian 12 x64 and KWRT/OpenWrt ARMv7 executables through GitHub Actions.

## Handoff instruction

Any future AI must read `PROGRESS.md` and `SPEC.md` first, inspect the current repository, continue from the first unchecked item, update this document after each meaningful step, record exact evidence, and never claim completion without verification.
