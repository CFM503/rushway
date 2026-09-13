# RushWay AI Relay Handoff

> Chronological AI-to-AI engineering handoff. Read this together with `PROGRESS.md` and `SPEC.md` before changing code.

## 2026-09-13 — Final implementation expansion checkpoint

### Repository / target

- Repository: `CFM503/rushway`
- Compatibility baseline: GoWay v1.8.4 at `538dbee86b9fbf248a68c8c6d8eee5d6f8bdb0dc`
- Intended release: `v0.0.1`
- Target artifacts: Windows x64, Debian 12 x64, KWRT/OpenWrt ARMv7

### This relay's implementation work

The implementation was expanded substantially beyond the earlier WSS pooling checkpoint:

- Plain WS non-MUX TCP path added in `src/nonmux.rs` and wired from `src/main.rs`.
- WSS non-MUX TCP path added and wired.
- QUIC / QUIC+TLS module added with GoWay-derived ALPN, timeout/window values, TCP stream bootstrap and UDP framing.
- QUIC client physical-connection reuse with single-flight dialing barrier is implemented.
- QUIC UDP framing uses a 2-byte big-endian length prefix and applies the configured XOR transform to payloads.
- QUIC UDP authentication now follows GoWay's `<key> UDP\n` form when a key is configured.
- MUX XOR compatibility audit identified that GoWay transforms the complete encoded 7-byte MUX frame plus payload; RushWay plain runtime and WSS/MUX paths were moved toward full-frame XOR handling.
- Runtime now has `max_connections`, `block_local`, `tcp_nodelay`, `tcp_keepalive` and `socket_buffer` configuration fields.
- TCP socket policy wiring was added through `socket2` for NODELAY, send/receive buffer sizing and keepalive behavior on the server runtime.
- QUIC IPv6 target formatting uses `[addr]:port`.

### Key commits in this relay

- `7f45e8f04c4422fa190d2ebe28ab9a1c5cef2198` — initial non-MUX implementation.
- `fb770d66596409aa3a3f10295f2f3683256cfc58` — non-MUX main wiring.
- `74d32a802ccddaba25b2d7147e64afdbb9995e53` — WSS non-MUX wiring.
- `31a1a552f96386eef8d2e25d500b990355a597ec` — QUIC module scaffold/config.
- `66275e792dac8471f6b943a5bc12c8e88be5fc1e` — QUIC TCP path.
- `67d0ac95acc43c06476a7b9cacd390f0e6c371be` — QUIC local request integration.
- `f3d70b2fa174a58e32e9d9f537d4e4363cd340e2` — QUIC cleanup.
- `29c0b7c0b1c47bd510c619954527fb8cb8535313` — QUIC error/stream refinement.
- `4cbeef7a1035cac0a545e2b20bb9cbc8f520078f` / `5a49877185a9287db4958496259f256e13a67655` — client MUX full-frame XOR direction and correction.
- `242a8dd074176d121f1ae1fffa7ab3abe1a0c73d` — plain runtime/server MUX full-frame XOR direction.
- `7b690ed23a0a387fa6f954a6d0688ce946781f33` — WSS XOR/framing alignment.
- `924d3bf6aad9bd54f34153f54b3c7bc5814883bb` — add `socket2` for real low-level socket policy.
- `3ca2e0fc012b3936edfafe1d669dd6e3ce2f46f5` — wire NODELAY, keepalive, socket buffer and runtime target policy into the server path.
- `97718e3bd6f3433714fb8d16ea3e8e639d2913fe` — harden QUIC pooling, QUIC UDP authentication/XOR framing, QUIC stream lifecycle and IPv6 target formatting.

### Current implementation assessment

- Major transport families represented in code: plain WS, WSS, non-MUX, QUIC/QUIC+TLS.
- Major proxy front-ends represented: SOCKS5 CONNECT, HTTP CONNECT, SOCKS5 UDP ASSOCIATE.
- MUX physical pooling exists for plain WS and WSS; QUIC has physical connection reuse with single-flight dialing.
- Low-level runtime option support is substantially wired.
- Implementation coverage is roughly **90% by code surface**, but this is an engineering estimate, not test coverage.

### Important unverified boundary

The current execution environment still has **no usable `cargo` or `rustc`** and cannot fetch the repository/dependencies directly. Therefore none of the newest transport-expansion commits have current-head compiler/test evidence here.

Do NOT claim any of the following as passed yet:

- `cargo check`
- `cargo test`
- authenticated GoWay <-> RushWay transfer
- WSS TCP/UDP end-to-end
- QUIC TCP/UDP end-to-end
- 1/100/500/1000 stream stress
- current-head c1/c8/c32 benchmarks
- current-head Windows/Debian/ARMv7 artifacts
- v0.0.1 release smoke

### Known remaining implementation/parity gaps

- Main CLI still has a compatibility cleanup pending for exact GoWay option handling, especially legacy multi-letter forms and fully applying every parsed low-level option.
- Exact remote DNS server/cache/fallback behavior is not yet reproduced; `--dns` remains a compatibility item.
- Complete SOCKS5 REP/error/FRAG/reply/close parity still needs executable evidence.
- HTTP CONNECT error/close parity still needs executable evidence.
- Browser TLS/HTTP fingerprint parity is not complete.
- Full GoWay dead-IP/retry/pool semantics still need executable validation.
- WSS server-side mode is not claimed complete unless the GoWay baseline actually exposes that listener role; current implementation primarily targets GoWay-compatible WSS client/upstream behavior.
- Release artifact validation remains outstanding.

### Next action

In a runnable Rust environment, execute the implementation-first gate:

1. `cargo fmt -- --check`
2. `cargo check --all-targets`
3. `cargo test --all-targets --all-features`
4. `cargo build --release`
5. authenticated plain WS MUX/non-MUX TCP
6. plain WS UDP
7. WSS MUX/non-MUX TCP + UDP
8. QUIC TCP + UDP
9. GoWay -> RushWay and RushWay -> GoWay interoperability
10. 1/100/500/1000 stream stress and large/slow-fast mixed workloads
11. current-head c1/c8/c32 benchmarks
12. Windows/Debian/ARMv7 release artifacts and smoke tests
13. v0.0.1 tag/release

Never call the implementation 100% complete merely because the source paths exist. The 100% gate is executable evidence plus release validation.

### Three-file relay contract

Only these three files are canonical handoff state:

1. `AI_HANDOFF.md` — chronological decisions/commits/blockers/next step.
2. `PROGRESS.md` — compact project dashboard.
3. `SPEC.md` — source-derived GoWay compatibility contract.
