# RushWay v0.0.1 — Continuous Handoff Progress

> Compatibility baseline: GoWay v1.8.4, stable commit `538dbee86b9fbf248a68c8c6d8eee5d6f8bdb0dc` from `CFM503/way/goway`.
> Goal: tested, protocol-compatible Rust replacement with standalone Windows x64, Debian 12 x64 and KWRT/OpenWrt ARMv7 executables.

## Current status

**Overall engineering completion: ~38% (estimate).** This is migration progress, not a claim of production readiness.

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
- [x] Corrected-XOR CI run `34701378943` completed successfully: test job `103573640597` and Windows job `103573877936` both `success`.

### XOR compatibility and runtime integration
- [x] Previous Rust XOR implementation was audited against the exact GoWay v1.8.4 `Crypto.TransformInPlace` implementation.
- [x] Corrected semantic mismatch in commit `0af63b233812d810ab06c0cf3b3ddee4fad61b58`: every transform call starts at offset zero.
- [x] SHA-256(key) is expanded by repetition to 256 KiB; empty key remains a no-op.
- [x] Added regression coverage for per-call offset reset.
- [x] Runtime transport now uses the corrected XOR at the GoWay v1.8.4 authentication boundary in commit `1650c3e02de30b19e5bdc4d4915c0dbad58422a4`.
- [x] Client sends one binary `MUX\n` WebSocket message, XOR-transformed when a key is configured, and waits for `OK\n` before the first SYN.
- [x] Server validates/decrypts the standalone `MUX\n` message and returns transformed `OK\n` before accepting MUX frames.
- [x] MUX stream frames themselves remain untransformed, matching the exact GoWay source evidence found during audit.
- [ ] The new runtime integration still needs its own GitHub Actions build/test result before being called verified.

## Runtime slice currently present

- [x] Local SOCKS5 no-auth TCP CONNECT front-end.
- [x] Local HTTP CONNECT front-end.
- [x] SOCKS5 static success response.
- [x] TCP target dial with timeout and TCP_NODELAY.
- [x] WebSocket client/server handshake integration.
- [x] GoWay-compatible XOR MUX hello/OK authentication boundary implemented.
- [x] MUX SYN/DATA/FIN/RST runtime path.
- [x] Server-side bounded per-stream command queue.
- [x] Target-to-MUX DATA chunking at uint16 payload boundary.
- [x] Target EOF -> MUX FIN.
- [x] Target read error -> MUX RST.
- [x] Local EOF -> MUX FIN.
- [x] Remote MUX FIN -> local half-close.
- [x] Remote MUX RST -> local connection termination.
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
- [x] Exact XOR handshake placement confirmed: standalone `MUX\n` client probe and standalone `OK\n` server response; XOR is applied to those payloads only.
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
- [x] XOR MUX hello/OK runtime boundary implemented.
- [ ] Full CLI/config parity.
- [ ] WSS/TLS/SNI/FakeHost.
- [ ] SOCKS5 UDP runtime relay.
- [ ] MUX state/lifecycle hardening for high stream counts.
- [ ] QUIC.
- [ ] DNS resolver/cache.
- [ ] Pool/reconnect/retry/dead-IP.
- [ ] Statistics/logging/TUI compatibility where required.

## Stage 4 — Compatibility/tests `[ ]`
- [x] Rust unit tests executed in CI before latest runtime integration.
- [ ] GitHub Actions verification of commit `1650c3e02de30b19e5bdc4d4915c0dbad58422a4`.
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
- [x] Corrected-XOR run `34701378943` test + release + Windows jobs completed successfully.
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
- XOR compatibility: SHA-256(key), repeated to 256 KiB, each TransformInPlace invocation starts at offset zero. In v1.8.4 runtime, the XOR boundary is the standalone binary `MUX\n` / `OK\n` handshake, not ordinary MUX stream frames.

## Verification policy

No build, test, interoperability, benchmark, platform artifact or Release is marked complete without execution evidence. Repository writes alone are not verification evidence.

## Exact verification status

- Repository writes: confirmed.
- Latest code commit: `1650c3e02de30b19e5bdc4d4915c0dbad58422a4`.
- Corrected-XOR CI run `34701378943`: Linux test **success**, Linux release build **success**, Windows x64 GNU build **success**.
- New runtime XOR integration commit `1650c3e02de30b19e5bdc4d4915c0dbad58422a4`: CI result pending at this progress update.
- Local `cargo test`: not run; no local Rust toolchain execution used.
- Local `cargo build --release`: not run.

## Next continuous sequence

1. Verify the new runtime XOR integration commit in GitHub Actions; fix any compiler/test failure.
2. Finish exact SOCKS5 UDP relay/error lifecycle and HTTP CONNECT lifecycle.
3. Implement WSS/TLS/SNI/FakeHost.
4. Implement QUIC and pool/retry/dead-IP behavior.
5. Add non-MUX and remaining CLI/config/DNS behavior.
6. Add bidirectional GoWay/RushWay interoperability and high-concurrency tests.
7. Add Debian/KWRT builds, package standalone artifacts, and only then release v0.0.1.

## Handoff rule

Any future/relay AI **must read `PROGRESS.md` and `SPEC.md` first**, inspect the current `main` branch, continue from the first unchecked item, update this file after every meaningful step, record exact evidence, and never claim completion without execution evidence.
