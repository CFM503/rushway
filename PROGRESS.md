# RushWay v0.0.1 — Continuous Handoff Progress

> Compatibility baseline: GoWay v1.8.4, stable commit `538dbee86b9fbf248a68c8c6d8eee5d6f8bdb0dc` from `CFM503/way/goway`.
> Goal: tested, protocol-compatible Rust replacement with standalone Windows x64, Debian 12 x64 and KWRT/OpenWrt ARMv7 executables.

## Current status

**Overall engineering completion: ~47% (estimate).** This is migration progress, not a claim of production readiness.

- Stage 1 bootstrap: `[~]`
- Stage 2 v1.8.4 extraction: `[~]`
- Stage 3 Rust implementation: `[~]`
- Stage 4 compatibility tests: `[ ]`
- Stage 5 release builds: `[ ]`
- Stage 6 GitHub Actions: `[~]`
- Stage 7 v0.0.1 release: `[ ]`

## Latest continuous-pass work

### CI verification
- [x] Linux `cargo test --all-targets` passed for UDP transport commit `7bb82a08049a4259e329c878ce74c49fd13df273` in Actions run `34702137868`.
- [x] Linux release build passed in the same run.
- [x] Windows x64 GNU build passed in the same run.
- [x] Corrected-XOR CI run `34701378943` previously passed test, release and Windows jobs.
- [x] WSS dependency compatibility was corrected for Rust 1.82: `zeroize = 1.7.0`, `jobserver = 0.1.32`.
- [x] Rustls `ServerName` import was corrected to `rustls::pki_types::ServerName` in commit `c66ed87b3cc3030ea8eaa72655413dc9f77f50c3`.
- [x] Full WSS implementation CI run `34702979646` passed Linux tests, Linux release build, and Windows x64 GNU build.

### XOR compatibility and runtime integration
- [x] Previous Rust XOR implementation was audited against exact GoWay v1.8.4 `Crypto.TransformInPlace` semantics.
- [x] Corrected per-call offset reset in commit `0af63b233812d810ab06c0cf3b3ddee4fad61b58`.
- [x] SHA-256(key) is repeated to 256 KiB; empty key is a no-op.
- [x] Runtime uses XOR at the GoWay authentication boundary: standalone binary `MUX\\n` client probe and `OK\\n` server response.
- [x] Ordinary MUX stream frames remain untransformed.
- [x] Runtime XOR integration CI passed through run `34702137868`.

### SOCKS5 UDP transport
- [x] Added RFC1928 UDP datagram parsing to runtime path.
- [x] Added local UDP ASSOCIATE relay port allocation.
- [x] Added UDP-to-WebSocket and WebSocket-to-UDP transport ownership.
- [x] Added UDP handshake/response flow and proxy success response handling.
- [x] Added XOR handling at the UDP transport boundary where required by the current Rust protocol path.
- [x] Linux tests/release and Windows build passed after UDP implementation.
- [ ] Real GoWay v1.8.4 ↔ RushWay UDP echo interoperability test.
- [ ] Exact GoWay FRAG/error/close behavior still needs final reconciliation.

### WSS/TLS
- [x] Audited GoWay v1.8.4 WSS policy: TLS 1.2-1.3, `http/1.1` ALPN, `-verify-ssl` controls certificate verification and defaults to insecure verification.
- [x] Added `rustls`, `tokio-rustls` and `webpki-roots` dependencies.
- [x] Added `src/tls.rs` with strict WebPKI and GoWay-compatible insecure verification modes.
- [x] Added `src/wss_client.rs` with actual WSS client forwarding: TLS -> WebSocket -> XOR MUX hello/OK -> MUX SYN/DATA/FIN/RST -> local SOCKS5/HTTP CONNECT.
- [x] Added `--verify-ssl` CLI flag and WSS dispatch in `main.rs`.
- [x] WSS path preserves plain `ws://` runtime as a separate path.
- [x] Rust 1.82 WSS build/test/release verification is green in Actions run `34702979646`.
- [ ] WSS UDP path.
- [ ] Real WSS interoperability test.
- [ ] Final SNI/FakeHost compatibility verification against GoWay.

## Runtime slice currently present

- [x] Local SOCKS5 no-auth TCP CONNECT front-end.
- [x] Local HTTP CONNECT front-end.
- [x] SOCKS5 static success response.
- [x] TCP target dial with timeout and TCP_NODELAY.
- [x] Plain WebSocket client/server handshake integration.
- [x] GoWay-compatible XOR MUX hello/OK authentication boundary.
- [x] MUX SYN/DATA/FIN/RST runtime path.
- [x] Server-side bounded per-stream command queue.
- [x] Target-to-MUX DATA chunking at uint16 payload boundary.
- [x] Target EOF -> MUX FIN.
- [x] Target read error -> MUX RST.
- [x] Local EOF -> MUX FIN.
- [x] Remote MUX FIN -> local half-close.
- [x] Remote MUX RST -> local connection termination.
- [x] SOCKS5 UDP relay implementation slice.
- [x] TLS/WSS client TCP forwarding path compiles and passes Linux/Windows CI.
- [ ] WSS UDP.
- [ ] Runtime QUIC.
- [ ] Runtime connection pool/reuse/retry/dead-IP.
- [ ] Runtime non-MUX mode.

## Stage 2 — GoWay v1.8.4 extraction `[~]`
- [x] Stable compatibility commit identified.
- [x] Main CLI options and client/server selection extracted.
- [x] MUX wire format and lifecycle semantics extracted.
- [x] WebSocket handshake/framing behavior substantially extracted.
- [x] SOCKS5/HTTP front-end wire shapes and several exact error/control-flow branches extracted.
- [x] QUIC listener/client/ALPN/timeout/pool observations extracted.
- [x] Exact XOR handshake placement confirmed.
- [x] UDP ASSOCIATE local relay and RFC1928 envelope behavior extracted to implementation level.
- [x] WSS certificate policy and TLS version/ALPN policy extracted from v1.8.4 source/README evidence.
- [ ] Finish exact SOCKS5 TCP/UDP lifecycle, REP mappings, UDP reply relay and FRAG behavior.
- [ ] Finish exact HTTP CONNECT target/error/close lifecycle.
- [ ] Finish exact QUIC config/TLS/bootstrap/close semantics.
- [ ] Extract exact pool/retry/dead-IP algorithm.
- [ ] Extract exact JSON/config schema.
- [ ] Map all remaining GoWay v1.8.4 tests.
- [ ] Reconcile all findings against `SPEC.md`.

## Stage 3 — Rust implementation `[~]`
- [x] `src/protocol.rs` MUX codec and tests.
- [x] `src/ws.rs` RFC6455 codec, handshake primitives and header limit.
- [x] `src/proxy.rs` SOCKS5/UDP/HTTP parser primitives.
- [x] `src/runtime.rs` first WS+MUX+TCP forwarding slice.
- [x] Fixed HTTP CONNECT runtime protocol detection.
- [x] XOR primitive corrected to GoWay semantics.
- [x] XOR MUX hello/OK runtime boundary implemented and CI-verified.
- [x] SOCKS5 UDP relay implementation slice added and CI-verified for compilation/tests/builds.
- [x] Rustls WSS client foundation added.
- [x] WSS client TCP forwarding implementation added.
- [x] WSS CI verification completed successfully in run `34702979646`.
- [ ] Full CLI/config parity.
- [ ] WSS/SNI/FakeHost final verification.
- [ ] MUX state/lifecycle hardening for high stream counts.
- [ ] QUIC.
- [ ] DNS resolver/cache.
- [ ] Pool/reconnect/retry/dead-IP.
- [ ] Statistics/logging/TUI compatibility where required.

## Stage 4 — Compatibility/tests `[ ]`
- [x] Rust unit tests executed in CI for current WSS implementation.
- [x] Current Linux release build and Windows x64 build verified by run `34702979646`.
- [x] WSS compilation/build verification.
- [ ] GoWay Client -> RushWay Server.
- [ ] RushWay Client -> GoWay Server.
- [ ] GoWay/RushWay SOCKS5 TCP echo.
- [ ] GoWay/RushWay SOCKS5 UDP echo.
- [ ] 1/100/500/1000 streams.
- [ ] Large payload/file.
- [ ] Slow/fast streams.
- [ ] EOF/FIN/RST.
- [ ] Disconnect/reconnect.
- [ ] Real TLS/WSS interoperability.
- [ ] QUIC.
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
- [x] Linux Rust test + release build completed successfully in run `34702979646`.
- [x] Windows x64 GNU build completed successfully in run `34702979646`.
- [x] WSS dependency/API compatibility fixes are CI-green.
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
- GoWay WSS uses TLS 1.2-1.3 and ALPN `http/1.1`; strict certificate verification is opt-in via `-verify-ssl`, default is insecure.
- QUIC uses ALPN `goway-quic` and `h3`, idle timeout 60s and keepalive 15s; exact remaining config/bootstrap behavior is still pending.
- XOR compatibility: SHA-256(key), repeated to 256 KiB, each transform invocation starts at offset zero. In v1.8.4 runtime, the XOR boundary is the standalone binary `MUX\\n` / `OK\\n` handshake, not ordinary MUX stream frames.

## Verification policy

No build, test, interoperability, benchmark, platform artifact or Release is marked complete without execution evidence. Repository writes alone are not verification evidence.

## Exact verification status

- Latest fully green implementation verification: Actions run `34702979646` — Linux test **success**, Linux release build **success**, Windows x64 GNU build **success**.
- Previous WSS compile failure run `34702806288`: Rust 1.82 rejected `rustls::ServerName`; corrected in `c66ed87b3cc3030ea8eaa72655413dc9f77f50c3` using `rustls::pki_types::ServerName`.
- The corrective WSS run `34702979646` is fully green.
- Local `cargo test`: not run; no local Rust toolchain execution used.
- Local `cargo build --release`: not run.
- Actual GoWay ↔ RushWay interoperability: **not yet executed**, therefore not claimed.

## Next continuous sequence

1. Audit the now-CI-green WSS client against GoWay v1.8.4 for TLS SNI, Host/FakeHost, WebSocket headers and handshake response behavior.
2. Add targeted WSS unit/integration coverage for certificate modes and handshake framing.
3. Add WSS UDP only after WSS TCP behavior is reconciled.
4. Reconcile UDP FRAG/error/close behavior against GoWay v1.8.4.
5. Implement QUIC and exact pool/retry/dead-IP behavior.
6. Add non-MUX and remaining CLI/config/DNS behavior.
7. Build real bidirectional GoWay/RushWay interop and high-concurrency tests.
8. Add Debian/KWRT builds and standalone packaging.
9. Only after all required evidence is green, create v0.0.1 release.

## Handoff rule

Any future/relay AI **must read `PROGRESS.md` and `SPEC.md` first**, inspect current `main`, continue from the first unchecked item, update this file after every meaningful step, record exact evidence, and never claim completion without execution evidence.
