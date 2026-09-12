# RushWay v0.0.1 — Continuous Handoff Progress

> Compatibility baseline: GoWay v1.8.4, stable commit `538dbee86b9fbf248a68c8c6d8eee5d6f8bdb0dc` from `CFM503/way/goway`.
> Goal: tested, protocol-compatible Rust replacement with standalone Windows x64, Debian 12 x64 and KWRT/OpenWrt ARMv7 executables.

## Current status

**Overall engineering completion: ~30% (estimate).** This is migration progress, not a claim of production readiness.

- Stage 1 bootstrap: `[~]`
- Stage 2 v1.8.4 extraction: `[~]`
- Stage 3 Rust implementation: `[~]`
- Stage 4 compatibility tests: `[ ]`
- Stage 5 release builds: `[ ]`
- Stage 6 GitHub Actions: `[~]`
- Stage 7 v0.0.1 release: `[ ]`

## Newly completed in the current continuous pass

### Runtime slice
- [x] Added `src/runtime.rs`.
- [x] Local SOCKS5 no-auth TCP CONNECT front-end.
- [x] Local HTTP CONNECT front-end.
- [x] SOCKS5 static success response.
- [x] TCP target dial with timeout and TCP_NODELAY.
- [x] WebSocket client handshake integration.
- [x] WebSocket server handshake integration.
- [x] MUX SYN/DATA/FIN/RST runtime path.
- [x] Server-side per-stream bounded command queue.
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

### CLI/config
- [x] Wire runtime into `main.rs`.
- [x] `-p`, `-up`, `-k`, `--fakehost`, `--mux`, `--no-mux`, `--allow-open`, `-W`, `--connection-timeout` basic arguments.
- [x] Basic JSON config loading with snake_case and selected camelCase aliases.
- [ ] Full GoWay v1.8.4 CLI/config parity.

### CI
- [x] Added `.github/workflows/ci.yml` for Rust 1.82 tests/release build.
- [x] Added Windows x64 GNU build job definition.
- [ ] CI execution evidence. The workflow has been pushed, but no completed workflow run is recorded yet.
- [ ] Debian 12 native artifact packaging.
- [ ] KWRT/OpenWrt ARMv7 build.
- [ ] Release packaging/tag workflow.

## Existing verified implementation

### Protocol
- [x] MUX 7-byte header: uint32 BE stream ID + command + uint16 BE payload length.
- [x] SYN/DATA/FIN/RST values and SYN target-length framing.
- [x] Unit tests written; execution still pending.

### WebSocket
- [x] RFC6455 accept key.
- [x] Frame lengths through 64-bit length field.
- [x] 64 MiB frame limit.
- [x] Control frame 125-byte limit.
- [x] Mask/unmask.
- [x] Ping -> Pong, Pong discard, Close -> EOF.
- [x] Server/client handshake primitives.
- [x] HTTP header hard limit 8192 bytes.
- [x] Header reader accepts CRLFCRLF and LF LF.
- [ ] Browser-profile/TLS fingerprint parity.
- [ ] WSS runtime.

### Proxy
- [x] SOCKS5 greeting parser.
- [x] SOCKS5 CONNECT parser for IPv4/domain/IPv6.
- [x] SOCKS5 UDP envelope parser.
- [x] HTTP CONNECT parser.
- [x] Exact high-level v1.8.4 findings for UDP ASSOCIATE ephemeral bind, truncated-read protection and HTTP header acquisition.
- [ ] Exact UDP relay lifecycle/FRAG/error mapping implementation.

### DNS / QUIC / pools
- [x] v1.8.4 high-level remote-DNS fallback/cache behavior documented in `SPEC.md`.
- [x] Initial QUIC listener/dial/ALPN/timeout/pool observations documented in `SPEC.md`.
- [ ] Full QUIC config/TLS/bootstrap/close implementation.
- [ ] Full pool algorithm and retry/dead-IP implementation.

## Compatibility extraction still required

- [ ] Finish exact SOCKS5 TCP/UDP target dial, all REP mappings, UDP reply relay, FRAG behavior and close lifecycle.
- [ ] Finish exact HTTP CONNECT target dial/error/close behavior.
- [ ] Finish exact QUIC config/TLS/stream bootstrap/close behavior.
- [ ] Extract exact connection pool algorithms and retry/dead-IP behavior.
- [ ] Extract exact JSON/config schema.
- [ ] Extract/map all remaining GoWay v1.8.4 tests.
- [ ] Reconcile all findings against `SPEC.md`.

## Verification policy

No build, interoperability, benchmark, platform artifact or Release is marked complete without execution evidence.

### Actual execution evidence in this session

- GitHub repository writes: **confirmed**.
- Local `cargo test`: **not run** (no local Rust toolchain used).
- Local `cargo build --release`: **not run**.
- GitHub Actions: workflow definition pushed, but `fetch_commit_workflow_runs` currently reports no run for commit `a582dde4f00424fc0d8426a6e857a232a19b0698`.
- Therefore runtime code is **not yet declared build-verified**.

## Important caution

The newly added runtime is a first functional transport slice, not yet a GoWay v1.8.4 replacement. It currently implements only `SOCKS5/HTTP CONNECT -> WS -> MUX -> TCP target` and deliberately does not claim WSS, QUIC, XOR, UDP relay, pooling, retry, or full configuration compatibility.

## Next continuous sequence

1. Correct/verify the runtime with CI compilation.
2. Finish SOCKS5 UDP relay and exact HTTP/SOCKS error lifecycle.
3. Implement XOR compatibility and config parity.
4. Implement WSS/TLS/SNI/FakeHost.
5. Implement QUIC and pooling/retry/dead-IP.
6. Add Rust integration/interoperability tests against GoWay v1.8.4.
7. Add 100/500/1000-stream, large-payload, slow/fast, FIN/RST and reconnect tests.
8. Add Debian x64 + KWRT ARMv7 builds and packaging.
9. Only after successful evidence, tag and publish `v0.0.1`.

## Handoff rule

Any future/relay AI must read `PROGRESS.md` and `SPEC.md`, inspect the current repository, continue from the first unchecked item, update this file after every meaningful step, and never claim completion without actual evidence.
