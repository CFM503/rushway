# RushWay v0.0.1 — Continuous Handoff Progress

> Compatibility baseline: GoWay v1.8.4 at `538dbee86b9fbf248a68c8c6d8eee5d6f8bdb0dc`.

## Current checkpoint — 2026-09-13

**Implementation coverage: ~92% estimate. Overall project/release completion: ~72% estimate.**

This is an engineering estimate, not a test score. The transport surface is substantially present across WS, WSS, non-MUX and QUIC. The active implementation gate is now CLI/config parity followed by the real compiler/test/interop gate.

### Latest handoff checkpoint

- `133d6fad53053b889c96233c122cfdfc68191290` — normalized GoWay single-dash CLI forms and propagated socket policy values.
- `1cf7e24e00896fefb7f41491ed9b2309de04bfea` — updated interruption-safe AI handoff with the new implementation boundary.
- No current-head compiler, unit-test, interoperability or release result has been claimed.

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

- [x] TLS client path.
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
- [x] socket send/receive buffer configuration propagation.
- [x] TCP keepalive configuration propagation.
- [x] GoWay single-hyphen multi-character CLI normalization (`-up`, `-fakehost`, `-mux-sessions`, etc.).
- [x] Parser tests for legacy aliases and ordinary short flags.
- [ ] Remote DNS runtime implementation: 5s remote lookup, system fallback and 5-minute positive cache.
- [ ] Remote DNS integration into both target dialing and upstream host dialing.
- [ ] Exact socket-option propagation audit across non-MUX/WSS upstream connections.
- [ ] Every SOCKS5 REP/error/FRAG branch verified against GoWay.
- [ ] HTTP CONNECT error/close parity verified.
- [ ] Full browser/TLS profile parity.

## Source-derived DNS contract

GoWay v1.8.4 uses a custom resolver with a 5-second timeout, remote DNS first, system-DNS fallback on failure and a 5-minute positive cache. The configured DNS resolver is used for server-side target resolution and client-side upstream host resolution; TLS/SNI keeps the original hostname while transport dialing may use the resolved address.

## Verification gate — not yet executed on current head

The current execution environment has no usable `cargo` or `rustc`; the GitHub Actions connector also reports no workflow run associated with commit `133d6fad53053b889c96233c122cfdfc68191290`. Therefore these remain **not verified**:

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

## Mandatory resume order

1. Implement the exact GoWay remote-DNS resolver contract.
2. Audit non-MUX/WSS upstream socket policy propagation.
3. Compile current head in a real Rust environment and fix every compiler failure.
4. Run all unit tests and fix every failure.
5. Execute WS MUX/non-MUX TCP + UDP.
6. Execute WSS MUX/non-MUX TCP + UDP.
7. Execute QUIC TCP + UDP.
8. Run GoWay -> RushWay and RushWay -> GoWay interoperability.
9. Stress 1/100/500/1000 streams, sustained large payloads and mixed slow/fast streams.
10. Benchmark c1/c8/c32, build Windows/Debian/ARMv7 artifacts, smoke-test and tag v0.0.1.

Never turn source inspection into a false runtime-pass claim.

## Three-file relay contract

Only these files are canonical handoff state:

1. `AI_HANDOFF.md` — chronological decisions/blockers/next step.
2. `PROGRESS.md` — compact project dashboard.
3. `SPEC.md` — source-derived GoWay compatibility contract.
