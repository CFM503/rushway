# RushWay v0.0.1 — Continuous Handoff Progress

> Compatibility baseline: GoWay v1.8.4, stable commit `538dbee86b9fbf248a68c8c6d8eee5d6f8bdb0dc` from `CFM503/way/goway`.
> Goal: tested, protocol-compatible Rust replacement with standalone Windows x64, Debian 12 x64 and KWRT/OpenWrt ARMv7 executables.

## Current status

**Overall engineering completion: ~35% (estimate).** This is migration progress, not a claim of production readiness.

- Stage 1 bootstrap: `[~]`
- Stage 2 v1.8.4 extraction: `[~]`
- Stage 3 Rust implementation: `[~]`
- Stage 4 compatibility tests: `[ ]`
- Stage 5 release builds: `[ ]`
- Stage 6 GitHub Actions: `[~]`
- Stage 7 v0.0.1 release: `[ ]`

## Latest continuous-pass work

### CI dependency/toolchain compatibility
- [x] Rust 1.82 dependency compatibility fixed by pinning `clap = 4.5.20` and `tempfile = 3.13.0`.
- [x] Linux test and release build passed in GitHub Actions run `34700901555`.
- [x] Windows x64 GNU build passed in the same run after installing `gcc-mingw-w64-x86-64` in commit `95ac981973d0ae15f0717ccf1cdd9a7af8322d2b`.
- [x] Run `34700901555` jobs `test` (`103572372719`) and `windows-x64` (`103572613046`) both completed with conclusion `success`.

### XOR compatibility correction
- [x] Previous Rust XOR implementation was audited against the exact GoWay v1.8.4 `Crypto.TransformInPlace` implementation.
- [x] Corrected an important semantic mismatch in commit `0af63b233812d810ab06c0cf3b3ddee4fad61b58`: every transform call now starts at offset zero, matching GoWay.
- [x] SHA-256(key) is expanded by repetition to 256 KiB; empty key remains a no-op.
- [x] Added a regression test proving separate calls do not carry offset state between buffers.
- [ ] XOR is not yet wired into the WebSocket/MUX transport; do not mark runtime crypto complete until that is implemented and tested.

## Runtime slice currently present

- [x] Local SOCKS5 no-auth TCP CONNECT front-end.
- [x] Local HTTP CONNECT front-end.
- [x] SOCKS5 static success response.
- [x] TCP target dial with timeout and TCP_NODELAY.
- [x] WebSocket client/server handshake integration.
- [x] MUX SYN/DATA/FIN/RST runtime path.
- [x] Server-side bounded per-stream command queue.
- [x] Target-to-MUX DATA chunking at uint16 payload boundary.
- [x] Target EOF -> MUX FIN.
- [x] Target read error -> MUX RST.
- [x] Local EOF -> MUX FIN.
- [x] Remote MUX FIN -> local half-close.
- [x] Remote MUX RST -> local connection termination.
- [ ] Runtime XOR crypto transport integration.
- [ ] Runtime TLS/WSS.
- [ ] Runtime QUIC.
- [ ] Runtime connection pool/reuse/retry/dead-IP.
- [ ] Runtime SOCKS5 UDP relay.
- [ ] Runtime non-MUX mode.

## Stage 2 — GoWay v1.8.4 extraction `[~]`
- [x] Stable compatibility commit identified.
- [x] Main CLI options and client/server selection extracted.
- [x] MUX wire format and lifecycle semantics extracted.
- [x] WebSocket handshake/framing behavior substantially extracted.
- [x] SOCKS5/HTTP front-end wire shapes and several exact error/control-flow branches extracted.
- [x] QUIC listener/client/ALPN/timeout/pool observations extracted.
- [ ] Finish exact SOCKS5 TCP/UDP lifecycle, REP mappings, UDP reply relay and FRAG behavior.
- [ ] Finish exact HTTP CONNECT target/error/close lifecycle.
- [ ] Finish exact QUIC config/TLS/bootstrap/close semantics.
- [ ] Extract exact pool/retry/dead-IP algorithm.
- [ ] Extract exact JSON/config schema.
- [ ] Map all remaining GoWay v1.8.4 tests.
- [ ] Reconcile all findings against `SPEC.md`.

## Stage 3 — Rust implementation `[~]`
- [x] `src/protocol.rs` MUX codec and tests written.
- [x] `src/ws.rs` RFC6455 codec, handshake primitives and header limit.
- [x] `src/proxy.rs` SOCKS5/UDP/HTTP parser primitives.
- [x] `src/runtime.rs` first WS+MUX+TCP forwarding slice.
- [x] Fixed HTTP CONNECT runtime protocol detection.
- [x] Rust unit tests and Linux/Windows CI builds verified by run `34700901555`.
- [x] XOR primitive corrected to match GoWay call semantics.
- [ ] Wire XOR into actual transport frames.
- [ ] Full CLI/config parity.
- [ ] WSS/TLS/SNI/FakeHost.
- [ ] SOCKS5 UDP runtime relay.
- [ ] MUX state/lifecycle hardening for high stream counts.
- [ ] QUIC.
- [ ] DNS resolver/cache.
- [ ] Pool/reconnect/retry/dead-IP.
- [ ] Statistics/logging/TUI compatibility where required.

## Stage 4 — Compatibility/tests `[ ]`
- [x] Rust unit tests executed in CI.
- [ ] GoWay Client -> RushWay Server.
- [ ] RushWay Client -> GoWay Server.
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

## Stage 5 — Release builds `[ ]`
- [ ] Windows x86_64 standalone executable.
- [ ] Debian 12 x86_64 standalone executable.
- [ ] KWRT/OpenWrt ARMv7 standalone executable.
- [ ] `--version` and `--help` verification.
- [ ] Artifact packaging verification.

## Stage 6 — GitHub Actions `[~]`
- [x] CI workflow definition exists for Rust 1.82 tests/release build.
- [x] Windows x64 GNU build job definition exists.
- [x] Linux Rust test + release build completed successfully in run `34700901555`.
- [x] Windows x64 GNU build completed successfully in run `34700901555`.
- [ ] Debian 12 build/package job.
- [ ] KWRT/OpenWrt ARMv7 build/package job.
- [ ] Release workflow.

## Stage 7 — v0.0.1 release `[ ]`
- [ ] Tag `v0.0.1`.
- [ ] Successful required Actions runs.
- [ ] Verified artifacts.
- [ ] Release notes.
- [ ] Final bidirectional GoWay/RushWay interoperability.

## Important compatibility findings

- MUX header: 7 bytes, uint32 BE stream ID + command + uint16 BE payload length.
- Commands: SYN `0x01`, DATA `0x02`, FIN `0x03`, RST `0x04`.
- SYN: uint16 target length + target + optional initial data.
- DATA must be chunked at the uint16 payload limit.
- FIN is half-close/EOF; RST is abrupt reset/error.
- GoWay WebSocket frame limit is 64 MiB; control frames are <=125 bytes; fragmentation is rejected; Ping gets Pong; Pong is discarded; Close is EOF.
- HTTP header hard limit is 8192 bytes and header acquisition must handle TCP segmentation.
- SOCKS5 uses no-auth greeting and supports IPv4/domain/IPv6 address forms; UDP uses the RFC1928 envelope.
- UDP ASSOCIATE uses an ephemeral local relay port and local bind failure maps to REP `0x01`.
- QUIC uses ALPN `goway-quic` and `h3`, idle timeout 60s and keepalive 15s; exact remaining config/bootstrap behavior is still pending.
- XOR compatibility: SHA-256(key), repeated to 256 KiB, and each TransformInPlace invocation starts at offset zero. Runtime transport integration remains pending.

## Verification policy

No build, test, interoperability, benchmark, platform artifact or Release is marked complete without execution evidence. Repository writes alone are not verification evidence.

## Exact verification status

- Repository writes: confirmed.
- Current main tip: `0af63b233812d810ab06c0cf3b3ddee4fad61b58`.
- GitHub Actions run `34700901555`: Linux Test **success**, Linux Release build **success**, Windows x64 GNU build **success**.
- XOR semantic correction commit: `0af63b233812d810ab06c0cf3b3ddee4fad61b58`.
- New CI run for the XOR correction: `34701378943`, currently **in progress** at the time this progress entry is written; its result must be checked before claiming the correction builds/tests successfully.
- Local `cargo test`: not run; no local Rust toolchain execution used.
- Local `cargo build --release`: not run.

## Next continuous sequence

1. Verify run `34701378943` and fix any compiler/test failure.
2. Wire the corrected XOR transform into the exact transport boundary used by GoWay v1.8.4 and add cross-frame tests.
3. Finish exact SOCKS5 UDP relay/error lifecycle and HTTP CONNECT lifecycle.
4. Implement WSS/TLS/SNI/FakeHost.
5. Implement QUIC and pool/retry/dead-IP behavior.
6. Add non-MUX and remaining CLI/config/DNS behavior.
7. Add bidirectional GoWay/RushWay interoperability and high-concurrency tests.
8. Add Debian/KWRT builds, package standalone artifacts, and only then release v0.0.1.

## Handoff rule

Any future/relay AI **must read `PROGRESS.md` and `SPEC.md` first**, inspect the current `main` branch, continue from the first unchecked item, update this file after every meaningful step, record exact evidence, and never claim completion without execution evidence.
