# RushWay AI Relay Handoff

> Chronological AI-to-AI engineering handoff. Read this together with `PROGRESS.md` and `SPEC.md` before changing code.

## 2026-09-13 — v0.0.2 formal-release checkpoint

### Target

- Repository: `CFM503/rushway`
- GoWay baseline: v1.8.4, commit `538dbee86b9fbf248a68c8c6d8eee5d6f8bdb0dc`
- Formal release target: `v0.0.2`
- Targets: Windows x64, Debian 12 x64, KWRT/OpenWrt ARMv7
- Branch: `main`

### Newly completed in this continuation

- `44054a3ea2282c0974a539d586522edcaa924a71` — remote DNS transaction-ID validation and focused mismatch test (source-side).
- `9ff943e5cecea61e72e2653987cff8957e8d060b` — added a reusable RFC1928 SOCKS5 General Failure response primitive and test.
- `7fa79e38fb629776c138510539eb3ed93d4e8ed3` — made the shared TCP socket policy reusable outside the runtime server path.
- `325c1f09bd099ee249d54eff8dee345ad914ba65` — plain WS MUX now waits for the first upstream response, maps server-side `RST` to local SOCKS5 failure / HTTP 502, and applies socket policy to pooled upstream TCP sessions.
- `24728d10801439b733b45af2a9316b160b93ca8c` / `156785a8e550d967d2109990ac28e960443d436b` — WSS upstream sockets now reuse the same NODELAY/keepalive/buffer policy, WSS non-MUX target rejection maps to local failure, and WSS MUX CONNECT waits for the first upstream response without double-decrementing terminal stream state.
- `75610b2123433a2b034eba577b61ddc161ac159f` — QUIC target-dial failure maps to local SOCKS5/HTTP failure and server target TCP sockets receive the shared socket policy; existing client pool retries once after failed `open_bi`.
- `ce9e0cf4013c615c22f89b21e01389c741e09a39` — release dashboard updated for the hardened source state.

### Important interoperability findings

GoWay's own integration tests use forms such as `-up ...`, `-log ERROR`, `-mux=true`, and `-block-local=false`; RushWay's parser normalizes these into Clap-compatible arguments.

The shared DNS resolver now covers server target resolution plus plain WS MUX, plain WS non-MUX, WSS and QUIC hostname dialing/target resolution. IP literals bypass DNS, WSS/QUIC preserve the original logical hostname for TLS/HTTP identity, and remote DNS replies are transaction-ID checked.

The local CONNECT paths for plain-WS MUX and WSS MUX no longer announce success merely because a logical stream was created. They consume the first upstream frame: `RST` is reported as SOCKS5 General Failure / HTTP 502; `DATA` is forwarded after success; `FIN` produces clean EOF after success.

QUIC client connection reuse already clears the cached connection and retries when opening a bidirectional stream fails. QUIC target-dial failure now returns a local SOCKS5/HTTP failure instead of silently closing. The QUIC server preserves the resolved host name for TLS server-name selection and now applies the common TCP socket policy to the actual target socket after connect.

### Current verification boundary

GitHub Actions is still **inconclusive rather than a compiler result**. HEAD `75610b2123433a2b034eba577b61ddc161ac159f` triggered `RushWay CI` run `34764563674` and `RushWay Build Smoke` run `34764563695`; both failed before exposing executable job steps, and the job-log endpoint returns `BlobNotFound`. This means no current-head compile/test/build result is honestly claimable from Actions yet.

A prior same-day run (`34756277983`, commit `994a52c7797f408654057d881effebb9e4af07f2`) completed successfully, but it predates the final transport hardening commits and cannot certify the current HEAD.

### Remaining work before the 100% gate

1. Finish exact SOCKS5 error/FRAG/close and HTTP CONNECT malformed/error lifecycle parity across every transport.
2. Execute QUIC dead-IP/connection-state/TLS-SNI interoperability checks against GoWay.
3. Add focused integration tests for MUX/WSS first-response RST, transport-specific target failures, and socket-option propagation.
4. Execute DNS transaction-ID, cache and truncated-response fallback tests in a usable Rust 1.82 environment.
5. Run `cargo fmt -- --check`, `cargo check --all-targets`, `cargo test --all-targets --all-features`, and `cargo build --release`; fix actual compiler/test failures rather than inferring them.
6. Run GoWay -> RushWay and RushWay -> GoWay TCP/UDP interop, 1/100/500/1000-stream stress, large payload and mixed slow/fast tests.
7. Run c1/c8/c32 benchmarks and Windows x64/Debian 12/ARMv7 artifact smoke tests.
8. Create and smoke-test `v0.0.2` only after executable evidence is complete.

### Release/tag constraint

`Cargo.toml` is `0.0.2`. The current GitHub connector exposes branch/commit writes but no tag/ref creation write operation, and the tag lookup currently does not show a `v0.0.2` reference. The tag must point to the final verified release commit once tag creation is available.

### Three-file relay contract

Only these three files are canonical AI handoff state:

1. `AI_HANDOFF.md` — decisions, commits, blockers, next step.
2. `PROGRESS.md` — compact progress dashboard.
3. `SPEC.md` — source-derived GoWay compatibility contract.

Never call the project 100% complete merely because the source paths exist. The 100% gate requires executable evidence and release validation.