# RushWay v0.0.1 — Continuous Handoff Progress

> Compatibility baseline: GoWay v1.8.4 at `538dbee86b9fbf248a68c8c6d8eee5d6f8bdb0dc`.

## Current checkpoint — 2026-09-13

**Implementation coverage: ~90% estimate. Overall project/release completion: ~70% estimate.**

The implementation surface is now substantially complete across WS, WSS, non-MUX and QUIC. The remaining gap is primarily executable verification, exact edge-case parity, cross-platform release validation and the final v0.0.1 release gate.

### Latest code head

`97718e3bd6f3433714fb8d16ea3e8e639d2913fe`

Recent work includes:

- `924d3bf6aad9bd54f34153f54b3c7bc5814883bb` — added `socket2` for real TCP socket policy.
- `3ca2e0fc012b3936edfafe1d669dd6e3ce2f46f5` — wired NODELAY, keepalive, socket buffer and target policy into runtime.
- `97718e3bd6f3433714fb8d16ea3e8e639d2913fe` — hardened QUIC pooling, QUIC UDP authentication/XOR framing, stream lifecycle and IPv6 target formatting.

## Transport status

### Plain WS

- [x] WebSocket handshake.
- [x] Auth/XOR boundary.
- [x] MUX SYN/DATA/FIN/RST.
- [x] uint16 DATA chunking.
- [x] Per-stream backpressure.
- [x] Physical MUX pooling, least-active selection and session retirement.
- [x] SOCKS5 UDP separate `UDP\n` WebSocket transport.
- [ ] Current-head executable interop/benchmark evidence.

### Non-MUX

- [x] Plain WS 1:1 TCP client/server path.
- [x] HTTP CONNECT and SOCKS5 CONNECT front-end.
- [x] GoWay-style `host:port\n` bootstrap and XOR handling.
- [ ] Exact 1:1 pooling/lifecycle parity evidence.
- [ ] Executable GoWay interoperability evidence.

### WSS

- [x] TLS 1.2/1.3 client path.
- [x] HTTP/1.1 ALPN and functional WS handshake.
- [x] WSS physical MUX session pooling.
- [x] WSS non-MUX client path.
- [x] WSS UDP ASSOCIATE client path.
- [ ] WSS pooled TCP executable validation.
- [ ] WSS non-MUX executable validation.
- [ ] WSS UDP executable validation.
- [ ] Browser-profile fingerprint parity.

### QUIC / QUIC+TLS

- [x] QUIC and QUIC+TLS URL recognition.
- [x] GoWay ALPN `goway-quic` / `h3`.
- [x] 60s idle / 15s keepalive.
- [x] GoWay-derived receive-window configuration.
- [x] TCP stream bootstrap and `OK\n` / `ERR: DIAL_FAILED\n` semantics.
- [x] QUIC physical connection reuse with single-flight dialing.
- [x] QUIC UDP 2-byte big-endian datagram framing.
- [x] QUIC UDP keyed authentication and XOR payload handling.
- [ ] Exact GoWay retry/dead-IP behavior evidence.
- [ ] Executable QUIC TCP/UDP interop evidence.

## Compatibility layer

- [x] `max-conn` runtime limit groundwork.
- [x] block-local target policy groundwork.
- [x] TCP_NODELAY runtime policy.
- [x] socket send/receive buffer policy through `socket2`.
- [x] TCP keepalive policy through `socket2`.
- [ ] Exact remote DNS server/cache/fallback behavior.
- [ ] Every SOCKS5 REP/error/FRAG branch verified against GoWay.
- [ ] HTTP CONNECT error/close parity verified.
- [ ] Full browser/TLS profile parity.

## Verification gate — not yet executed on current head

The current execution cloud has no usable `cargo` or `rustc`, and GitHub dependency retrieval is unavailable. Therefore these remain **not verified**:

- `cargo fmt -- --check`
- `cargo check --all-targets`
- `cargo test --all-targets --all-features`
- `cargo build --release`
- authenticated GoWay ↔ RushWay TCP interoperability
- WS/WSS/QUIC UDP round trips
- 1/100/500/1000 stream stress
- current-head c1/c8/c32 benchmarks
- Windows x64 / Debian 12 x64 / ARMv7 current-head binaries
- v0.0.1 smoke test and release

## Final sequence

1. Run current-head compile/test/release build in a real Rust environment.
2. Fix all compiler and test failures.
3. Execute WS MUX/non-MUX TCP and UDP.
4. Execute WSS MUX/non-MUX TCP and UDP.
5. Execute QUIC TCP/UDP.
6. Run GoWay v1.8.4 -> RushWay and RushWay -> GoWay interop.
7. Stress 1/100/500/1000 logical streams, sustained payloads and mixed slow/fast streams.
8. Benchmark current head c1/c8/c32.
9. Build/test Windows x64, Debian 12 x64, ARMv7/OpenWrt.
10. Tag and smoke-test v0.0.1.

Never turn source inspection into a false runtime-pass claim.
