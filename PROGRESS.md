# RushWay v0.0.1 — Continuous Handoff Progress

> Compatibility baseline: GoWay v1.8.4, stable commit `538dbee86b9fbf248a68c8c6d8eee5d6f8bdb0dc` from `CFM503/way/goway`.
> Goal: tested, protocol-compatible Rust replacement with standalone Windows x64, Debian 12 x64 and KWRT/OpenWrt ARMv7 executables.

## Current status

**Overall engineering completion: ~42% (estimate).** This is migration progress, not a claim of production readiness.

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
- [x] Windows x64 GNU build passed in the same run.
- [x] Corrected-XOR CI run `34701378943` completed successfully: Linux test/release and Windows x64 GNU all succeeded.

### XOR compatibility and runtime integration
- [x] Exact GoWay v1.8.4 `Crypto.TransformInPlace` semantics audited.
- [x] Commit `0af63b233812d810ab06c0cf3b3ddee4fad61b58` corrected the per-call offset-reset behavior.
- [x] SHA-256(key) repeated to 256 KiB; empty key is a no-op.
- [x] Runtime uses XOR only at the standalone `MUX\n` / `OK\n` handshake boundary, matching GoWay v1.8.4 source evidence.
- [x] Commit `1650c3e02de30b19e5bdc4d4915c0dbad58422a4` introduced the MUX hello/OK transport boundary.
- [ ] The later UDP/runtime rewrite still needs GitHub Actions verification before it is marked verified.

### SOCKS5 UDP compatibility work
- [x] GoWay v1.8.4 UDP path was re-inspected: client uses a local ephemeral UDP relay socket; transport starts with standalone `UDP\n` / `OK\n`; UDP datagrams use RFC1928 framing; server relays to a UDP socket and returns source-address envelopes.
- [x] RushWay runtime now recognizes SOCKS5 `UDP ASSOCIATE` instead of rejecting it.
- [x] RushWay client allocates an ephemeral local UDP relay port and returns a SOCKS5 success response with the bound port.
- [x] RushWay client sends `UDP\n`, waits for `OK\n`, forwards RFC1928 datagrams as binary WebSocket payloads, and maps returned source envelopes back to the latest local UDP peer.
- [x] RushWay server recognizes `UDP\n`, returns `OK\n`, resolves IPv4/domain/IPv6 targets, relays payloads over UDP, and wraps replies with the actual UDP source address.
- [x] UDP payloads are XOR-transformed per datagram when `-k` is configured, matching the existing GoWay crypto call semantics.
- [ ] Actual GoWay↔RushWay UDP echo interoperability test is still pending.
- [ ] FRAG behavior and exact UDP error/close lifecycle still need final regression coverage.

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
- [~] SOCKS5 UDP runtime relay implemented; CI + real interop still pending.
- [ ] Runtime TLS/WSS.
- [ ] Runtime QUIC.
- [ ] Runtime connection pool/reuse/retry/dead-IP.
- [ ] Runtime non-MUX mode.

## Stage 2 — GoWay v1.8.4 extraction `[~]`
- [x] Stable compatibility commit identified.
- [x] Main CLI options and client/server selection extracted.
- [x] MUX wire format and lifecycle semantics extracted.
- [x] WebSocket handshake/framing behavior substantially extracted.
- [x] SOCKS5/HTTP front-end wire shapes and exact UDP envelope structure extracted.
- [x] QUIC listener/client/ALPN/timeout/pool observations extracted.
- [x] Exact XOR handshake placement confirmed.
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
- [x] XOR primitive corrected to match GoWay call semantics.
- [x] XOR MUX hello/OK runtime boundary implemented.
- [x] UDP runtime relay path implemented in commit `7bb82a08049a4259e329c878ce74c49fd13df273`.
- [ ] Full CLI/config parity.
- [ ] WSS/TLS/SNI/FakeHost.
- [ ] MUX state/lifecycle hardening for high stream counts.
- [ ] QUIC.
- [ ] DNS resolver/cache.
- [ ] Pool/reconnect/retry/dead-IP.
- [ ] Statistics/logging/TUI compatibility where required.

## Stage 4 — Compatibility/tests `[ ]`
- [x] Rust unit tests and Linux/Windows builds have passed for earlier runtime commits.
- [ ] GitHub Actions verification of commit `7bb82a08049a4259e329c878ce74c49fd13df273`.
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
- [x] Corrected-XOR run `34701378943` completed successfully.
- [ ] Current UDP/runtime commit CI run `34702137868` is still in progress at this documentation point.
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
- GoWay UDP transport is selected by standalone `UDP\n` WebSocket binary hello and acknowledged by standalone `OK\n`; the per-datagram XOR boundary is the UDP payload envelope, not the WebSocket framing bytes.
- QUIC uses ALPN `goway-quic` and `h3`, idle timeout 60s and keepalive 15s; exact remaining config/bootstrap behavior is still pending.
- XOR compatibility: SHA-256(key), repeated to 256 KiB, each transform call starts at offset zero.

## Verification policy

No build, test, interoperability, benchmark, platform artifact or Release is marked complete without execution evidence. Repository writes alone are not verification evidence.

## Exact verification status

- Latest main code commit: `7bb82a08049a4259e329c878ce74c49fd13df273`.
- Current CI run: `34702137868`, status **in progress** at the time this document was written.
- Previous runtime commit CI run `34701689043`: Linux test/release and Windows x64 GNU all **success**.
- Corrected-XOR run `34701378943`: Linux test/release and Windows x64 GNU all **success**.
- Local `cargo test`: not run; no local Rust toolchain execution used.
- Local `cargo build --release`: not run.

## Next continuous sequence

1. Verify CI run `34702137868`; fix any compiler/test failure immediately and update this file.
2. If green, run/construct real GoWay v1.8.4 ↔ RushWay TCP and UDP echo interoperability tests; record exact evidence.
3. Finish exact SOCKS5 UDP error/FRAG lifecycle and HTTP CONNECT lifecycle.
4. Implement WSS/TLS/SNI/FakeHost.
5. Implement QUIC and pool/retry/dead-IP behavior.
6. Add non-MUX and remaining CLI/config/DNS behavior.
7. Add high-concurrency tests, Debian/KWRT builds, standalone artifacts, and only then release v0.0.1.

## Handoff rule

Any future/relay AI **must read `PROGRESS.md` and `SPEC.md` first**, inspect the current `main` branch, continue from the first unchecked item, update this file after every meaningful step, record exact evidence, and never claim completion without execution evidence.
