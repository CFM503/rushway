# RushWay v0.0.1 — Continuous Handoff Progress

> Compatibility baseline: GoWay v1.8.4, stable commit `538dbee86b9fbf248a68c8c6d8eee5d6f8bdb0dc` from `CFM503/way/goway`.
> Goal: tested, protocol-compatible Rust replacement with standalone Windows x64, Debian 12 x64 and KWRT/OpenWrt ARMv7 executables.

## Current status

**Overall engineering completion: ~31% (estimate).** This is migration progress, not a claim of production readiness.

- Stage 1 bootstrap: `[~]`
- Stage 2 v1.8.4 extraction: `[~]`
- Stage 3 Rust implementation: `[~]`
- Stage 4 compatibility tests: `[ ]`
- Stage 5 release builds: `[ ]`
- Stage 6 GitHub Actions: `[~]`
- Stage 7 v0.0.1 release: `[ ]`

## Latest continuous-pass work

### Runtime correctness fix
- [x] Audited the current `src/runtime.rs` path before extending it.
- [x] Found and fixed a concrete HTTP CONNECT dispatch bug: the runtime had checked for `G` even though an HTTP CONNECT request begins with `C`.
- [x] Runtime now routes `C...` to the HTTP header reader/parser while preserving SOCKS5 `0x05` detection.
- [ ] Runtime still needs actual compilation/test execution; this environment has no local Rust toolchain execution evidence.

### Runtime slice currently present
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
- [ ] Runtime TLS/WSS.
- [ ] Runtime QUIC.
- [ ] Runtime XOR crypto.
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
- [ ] Execute Rust unit tests.
- [ ] Execute release build.
- [ ] Full CLI/config parity.
- [ ] XOR compatibility implementation and transport integration.
- [ ] WSS/TLS/SNI/FakeHost.
- [ ] SOCKS5 UDP runtime relay.
- [ ] MUX state/lifecycle hardening for high stream counts.
- [ ] QUIC.
- [ ] DNS resolver/cache.
- [ ] Pool/reconnect/retry/dead-IP.
- [ ] Statistics/logging/TUI compatibility where required.

## Stage 4 — Compatibility/tests `[ ]`
- [ ] Rust unit/integration tests executed.
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
- [ ] Completed CI run evidence.
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
- XOR compatibility is SHA-256-derived repeating expanded key; runtime application remains pending.

## Verification policy

No build, test, interoperability, benchmark, platform artifact or Release is marked complete without execution evidence. Repository writes alone are not verification evidence.

## Exact verification status

- Repository writes: confirmed.
- Current main tip before this progress update: `0f0c6eba1292a185a60773e774f94f4ab07289ab` (HTTP CONNECT runtime dispatch fix).
- Local `cargo test`: not run; no local Rust toolchain execution used.
- Local `cargo build --release`: not run.
- GitHub Actions: workflow definition exists, but no completed workflow evidence has been observed through the available status/run inspection.

## Git continuity note

During the latest audit, several pre-existing work branches/refs were found. No force-update of `main` was performed. The canonical continuation branch is **`main`**, and this document is updated on `main` after each meaningful step. Future AIs must start from the current `main` tip rather than assuming an older branch is authoritative.

## Next continuous sequence

1. Use the repository's CI-capable environment to run `cargo test` and `cargo build --release`; fix every compiler/test failure found.
2. Finish exact SOCKS5 UDP relay/error lifecycle and HTTP CONNECT lifecycle against GoWay v1.8.4.
3. Implement XOR compatibility.
4. Implement WSS/TLS/SNI/FakeHost.
5. Implement QUIC and pool/retry/dead-IP behavior.
6. Add bidirectional GoWay/RushWay interoperability and high-concurrency tests.
7. Add Windows/Debian/KWRT builds and only then release v0.0.1.

## Handoff rule

Any future/relay AI **must read `PROGRESS.md` and `SPEC.md` first**, inspect the current `main` branch, continue from the first unchecked item, update this file after every meaningful step, record exact evidence, and never claim completion without execution evidence.
