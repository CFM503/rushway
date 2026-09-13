# RushWay v0.0.1 — Continuous Handoff Progress

> Compatibility baseline: GoWay v1.8.4 at `538dbee86b9fbf248a68c8c6d8eee5d6f8bdb0dc`.

## Current checkpoint — 2026-09-13

**Implementation coverage: ~90% estimate. Overall project/release completion: ~70% estimate.**

The implementation surface is substantially present across WS, WSS, non-MUX and QUIC. The next gate is exact CLI/config compatibility, then executable compile/test/interop evidence, stress, benchmarks and release artifacts.

### Latest handoff checkpoint

- `d8b37416b66e0778fad0baee1afbd7277b0c48da` — interruption-safe AI handoff checkpoint.
- Code baseline to resume from: `97718e3bd6f3433714fb8d16ea3e8e639d2913fe`.
- No current-head runtime test result has been claimed.

## Transport status

### Plain WS

- [x] WebSocket handshake.
- [x] Auth/XOR boundary.
- [x] MUX SYN/DATA/FIN/RST.
- [x] uint16 DATA chunking.
- [x] Per-stream backpressure.
- [x] Physical MUX pooling, least-active selection and session retirement.
- [x] SOCKS5 UDP separate `UDP\\n` WebSocket transport.
- [ ] Current-head executable interop/benchmark evidence.

### Non-MUX

- [x] Plain WS 1:1 TCP client/server path.
- [x] HTTP CONNECT and SOCKS5 CONNECT front-end.
- [x] GoWay-style `host:port\\n` bootstrap and XOR handling.
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
- [x] TCP stream bootstrap and `OK\\n` / `ERR: DIAL_FAILED\\n` semantics.
- [x] QUIC physical connection reuse with single-flight dialing.
- [x] QUIC UDP 2-byte big-endian datagram framing.
- [x] QUIC UDP keyed authentication and XOR payload handling.
- [ ] Exact GoWay retry/dead-IP behavior evidence.
- [ ] Executable QUIC TCP/UDP interop evidence.

## Compatibility layer

- [x] `max-conn` runtime limit groundwork.
- [x] block-local target policy groundwork.
- [x] TCP_NODELAY runtime policy.
- [x] socket send/receive buffer implementation groundwork.
- [x] TCP keepalive implementation groundwork.
- [ ] Exact GoWay single-hyphen multi-character CLI forms (`-up`, `-fakehost`, `-mux-sessions`, etc.).
- [ ] Full propagation of `socket-buffer`, `dns`, `no-tcp-keepalive` from CLI/JSON into runtime behavior.
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

## Mandatory resume order for the next AI

1. Implement exact GoWay single-hyphen multi-character CLI normalization without changing ordinary `-p`, `-k`, `-W` behavior.
2. Wire parsed socket-buffer, keepalive and DNS values into `RuntimeConfig`; remove throwaway locals.
3. Add/adjust parser unit tests for GoWay-style and GNU-style aliases.
4. Run `cargo fmt -- --check`.
5. Run `cargo check --all-targets` and fix every compiler error.
6. Run `cargo test --all-targets --all-features` and fix every failing test.
7. Build release and start the WS/WSS/QUIC interop matrix.
8. Stress 1/100/500/1000 streams, benchmark c1/c8/c32, build target platforms, smoke-test and tag v0.0.1.

Never turn source inspection into a false runtime-pass claim.

## Three-file relay contract

Only these files are canonical handoff state:

1. `AI_HANDOFF.md` — chronological decisions/commits/blockers/next step.
2. `PROGRESS.md` — compact project dashboard.
3. `SPEC.md` — source-derived GoWay compatibility contract.
