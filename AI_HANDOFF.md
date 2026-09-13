# RushWay AI Relay Handoff

> This file is the chronological AI-to-AI engineering handoff log. A fresh AI that clones the repository must read this file together with `PROGRESS.md` and `SPEC.md` before changing code.

## 2026-09-13 — Transport expansion checkpoint

### Starting point

- Repository: `CFM503/rushway`
- Compatibility target: GoWay v1.8.4
- GoWay compatibility baseline commit: `538dbee86b9fbf248a68c8c6d8eee5d6f8bdb0dc`
- Latest RushWay head recorded by this checkpoint: `bc3d1e6d34af7d3f58ba8edf36b6e2bc5dc47bab`
- Intended release: `v0.0.1`
- Target artifacts: Windows x64, Debian 12 x64, KWRT/OpenWrt ARMv7
- This checkpoint is implementation progress, not release-readiness evidence.

### Current completion assessment

- **Implementation coverage: ~80% estimate.** Major transport families and proxy front-ends are now represented in code.
- **Executable verification coverage: ~0% for the newest transport expansion.** This environment does not have a usable Rust/Cargo toolchain and cannot resolve GitHub for dependency fetching, so current-head `cargo test`/`cargo build` has not been run locally.
- **Overall project/release completion: ~70% estimate.** Remaining work is dominated by executable validation, exact compatibility cleanup, stress/interop evidence, platform builds and release packaging.

Do not interpret the percentage as a test score.

### What was implemented in this relay

#### Non-MUX / 1:1

- Added `src/nonmux.rs` for GoWay-style plain WS non-MUX TCP forwarding.
- Non-MUX protocol shape follows the v1.8.4 source: first binary payload is `host:port\n`, optional XOR transformation is applied, server responds with `OK\n`, then raw TCP bytes are carried in WebSocket binary frames.
- HTTP CONNECT and SOCKS5 CONNECT front-ends are accepted.
- Non-MUX server target dialing is timeout bounded.
- Non-MUX client and server lifecycle handling is implemented in code.
- Main entry now selects `nonmux::run_client` / `nonmux::run_server` when `--no-mux` is active.

Key commits:
- `7f45e8f04c4422fa190d2ebe28ab9a1c5cef2198` — initial non-MUX module implementation.
- `fb770d66596409aa3a3f10295f2f3683256cfc58` — main runtime wiring for non-MUX.
- `74d32a802ccddaba25b2d7147e64afdbb9995e53` — WSS non-MUX entry-point support.

#### WSS non-MUX

- Added WSS 1:1 forwarding path in `src/wss_client.rs`.
- Reuses existing TLS/WebSocket handshake logic.
- Supports SOCKS5 CONNECT / HTTP CONNECT and WSS UDP ASSOCIATE path.
- `--no-mux` on a `wss://` upstream now selects the WSS non-MUX runtime instead of rejecting immediately.

#### QUIC / QUIC+TLS

- Added `src/quic.rs`.
- ALPN compatibility follows GoWay: `goway-quic` and `h3`.
- Transport timing/window values were taken from GoWay v1.8.4 source: 60s idle timeout, 15s keepalive, 2/8 MiB stream receive windows and 4/16 MiB connection receive windows.
- QUIC TCP client path now accepts local SOCKS5/HTTP CONNECT and opens one QUIC bidirectional stream per local TCP connection.
- QUIC authentication header follows GoWay: `<key> <target>\n` when a key is configured, otherwise `<target>\n`.
- QUIC server replies `OK\n` after successful target dialing and `ERR: DIAL_FAILED\n` on target failure.
- `quic://` and `quic+tls://` are recognized by `src/main.rs`.
- Server mode currently starts the normal TCP/WebSocket runtime and QUIC runtime together.

Key commits:
- `31a1a552f96386eef8d2e25d500b990355a597ec` — QUIC transport module scaffold/configuration.
- `66275e792dac8471f6b943a5bc12c8e88be5fc1e` — QUIC TCP client/server relay path.
- `67d0ac95acc43c06476a7b9cacd390f0e6c371be` — QUIC local proxy request integration and ownership corrections.
- `f3d70b2fa174a58e32e9d9f537d4e4363cd340e2` — QUIC runtime cleanup/refinement.
- `29c0b7c0b1c47bd510c619954527fb8cb8535313` — QUIC stream/server error handling refinement.
- `626030f93629f0d41b9108433da7277e75d61082` and `bc3d1e6d34af7d3f58ba8edf36b6e2bc5dc47bab` — main entry and configuration integration updates.

#### QUIC UDP

- Implemented GoWay-derived QUIC UDP framing from the v1.8.4 source inspection.
- UDP transport starts with the `UDP\n` control line.
- Each UDP datagram is length-prefixed with a 2-byte big-endian payload length on the QUIC stream.
- Return packets carry the same 2-byte length prefix and a SOCKS5 UDP envelope.
- This work was added without routing UDP packets through TCP MUX DATA frames.
- End-to-end executable validation is still pending.

#### Runtime option / compatibility groundwork

- Added runtime fields and wiring for compatibility-oriented options including `max-conn`, block-local policy and TCP_NODELAY-related behavior.
- Main configuration parsing now has a larger compatibility surface and continues to use JSON/CLI overrides.
- Exact socket-buffer, keepalive, DNS resolver, every SOCKS5 REP branch and every GoWay option are still not fully validated.

#### MUX XOR correctness audit

A protocol-level audit against GoWay v1.8.4 identified an important compatibility rule: GoWay applies its XOR transform to the full encoded MUX frame, not only the MUX handshake. RushWay was corrected to move toward full-frame XOR handling on the MUX paths.

Key commits:
- `4cbeef7a1035cac0a545e2b20bb9cbc8f520078f` — client MUX full-frame XOR direction.
- `5a49877185a9287db4958496259f256e13a67655` — follow-up correction after static review.
- `242a8dd074176d121f1ae1fffa7ab3abe1a0c73d` — plain runtime/server MUX full-frame XOR direction.
- `7b690ed23a0a387fa6f954a6d0688ce946781f33` — WSS-side XOR/framing alignment.

This is **not yet execution-verified** against GoWay with an authenticated transfer.

### Previously implemented and retained

- SOCKS5 no-auth TCP CONNECT front-end.
- HTTP CONNECT front-end.
- Plain WS client/server handshake.
- Plain WS MUX SYN/DATA/FIN/RST.
- uint16 DATA chunking.
- Per-stream bounded queues.
- Plain WS physical MUX pooling, least-active selection and session retirement.
- WSS TLS client path, cached verified/insecure rustls configurations and WSS physical MUX pooling.
- Plain WS UDP `UDP\n` transport kept separate from TCP MUX.
- SOCKS5 UDP ASSOCIATE `0.0.0.0:0` acceptance fix.

### Known blockers / not yet complete

- **No current-head executable build/test evidence.** This is the biggest blocker and must be the first post-implementation activity in a runnable Rust environment.
- WSS pooled TCP functional validation.
- WSS UDP end-to-end validation, including reply/source-address semantics.
- Full plain WS and WSS non-MUX compatibility validation.
- QUIC TCP and QUIC UDP interoperability against GoWay v1.8.4.
- QUIC physical connection reuse / pooling and exact retry/dead-IP semantics are not yet at GoWay parity.
- Exact DNS behavior and resolver/cache/fallback semantics.
- Complete SOCKS5 REP/error/FRAG/lifecycle parity.
- Complete HTTP CONNECT error/close parity.
- Exact `socket-buffer`, TCP keepalive and other low-level option behavior.
- Full browser profile / TLS fingerprint parity where compatibility requires it.
- GoWay <-> RushWay bidirectional executable interop matrix.
- Stress at 1/100/500/1000 logical streams.
- Current-head throughput benchmarks for plain MUX and WSS pooling.
- Windows x64, Debian 12 x64, ARMv7/OpenWrt release artifacts on the current head.
- v0.0.1 packaging, smoke tests and release/tag.

### Verification status

#### Verified by source inspection / static reasoning in this relay

- GoWay v1.8.4 non-MUX wire shape was extracted from baseline source.
- GoWay v1.8.4 QUIC ALPN, timing/window configuration, TCP stream bootstrap and UDP framing were extracted from baseline source.
- MUX full-frame XOR requirement was identified from baseline implementation.
- Main entry selection for WS/WSS/QUIC/non-MUX paths was inspected after wiring.

#### Not verified by execution

- `cargo build`
- `cargo test`
- authenticated GoWay <-> RushWay TCP transfer
- SOCKS5 UDP packet round-trip
- WSS packet round-trip
- QUIC TCP/UDP packet round-trip
- 1/100/500/1000 stream stress
- throughput/latency benchmark on current head
- release binaries

### Next highest-priority action for the next AI

**Run current-head compile/test first.** Do not add another large feature until compiler output is available. Fix every compile error and regression introduced by the transport expansion, then execute a minimal authenticated loopback/interoperability matrix before optimizing or packaging.

### Required next sequence after first successful build

1. `cargo check` / `cargo test` / release build.
2. Plain WS authenticated MUX TCP round-trip.
3. Plain WS authenticated non-MUX TCP round-trip.
4. WSS authenticated MUX and non-MUX TCP round-trip.
5. Plain WS UDP round-trip.
6. WSS UDP round-trip.
7. QUIC TCP and QUIC UDP round-trip.
8. GoWay v1.8.4 -> RushWay and RushWay -> GoWay interop.
9. 1/100/500/1000 stream stress and large-payload tests.
10. Current-head c1/c8/c32 benchmarks.
11. Windows/Debian/ARMv7 builds.
12. v0.0.1 release.

### Three-file relay contract

There are intentionally **three** important handoff documents; do not create a fourth competing status/log file:

1. `AI_HANDOFF.md` — chronological history, exact commits/incidents, current boundaries and the next engineering sequence.
2. `PROGRESS.md` — compact canonical current-state dashboard, stage checklist and release gaps.
3. `SPEC.md` — compatibility contract and source-derived GoWay behavior.

Every future AI must append a dated checkpoint with exact commits and exact verification evidence. Never label code-only implementation as runtime verified.
