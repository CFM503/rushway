# RushWay v0.0.1 — Continuous Handoff Progress

> Compatibility baseline: GoWay v1.8.4, stable commit `538dbee86b9fbf248a68c8c6d8eee5d6f8bdb0dc` from `CFM503/way/goway`.
> Goal: tested, protocol-compatible Rust replacement with standalone Windows x64, Debian 12 x64 and KWRT/OpenWrt ARMv7 executables.

## Current checkpoint — 2026-09-13

**Overall project/release completion: ~70% estimate.**

This is an engineering estimate, not a test score. Implementation coverage is roughly 80%; executable verification of the newest transport expansion is still pending.

- Stage 1 bootstrap: `[~]`
- Stage 2 v1.8.4 extraction: `[~]`
- Stage 3 Rust implementation: `[~]`
- Stage 4 executable compatibility tests: `[ ]`
- Stage 5 release builds: `[~]`
- Stage 6 GitHub Actions: `[~]` — old known runs failed at job/infrastructure level before executable steps
- Stage 7 v0.0.1 release: `[ ]`

## Current head

Latest recorded head: `b6b3ab6c8b7d3bf1d4a1179433c0305a9e7475ab`.

Recent implementation commits include:
- `8096a7a410c5c5f0cf9a8b3d947114059111e68d` — WSS UDP ASSOCIATE raw transport.
- `a03e0e40fc336ad18ec877223b913b1be26545df` — SOCKS5 UDP ASSOCIATE zero-port bind hint.
- `7f45e8f04c4422fa190d2ebe28ab9a1c5cef2198` — plain non-MUX module.
- `74d32a802ccddaba25b2d7147e64afdbb9995e53` — WSS non-MUX entry path.
- `31a1a552f96386eef8d2e25d500b990355a597ec` through `29c0b7c0b1c47bd510c619954527fb8cb8535313` — QUIC TCP/UDP implementation sequence.
- `4cbeef7a1035cac0a545e2b20bb9cbc8f520078f` through `7b690ed23a0a387fa6f954a6d0688ce946781f33` — MUX/WSS XOR framing corrections.
- `c67bd7379d99d6f628befd820a8ff893ce5c5772` — runtime compatibility option groundwork.
- `b6b3ab6c8b7d3bf1d4a1179433c0305a9e7475ab` — this relay checkpoint.

## Transport implementation status

### Client/proxy front-end

- [x] SOCKS5 no-auth TCP CONNECT.
- [x] HTTP CONNECT.
- [x] TCP target timeout handling.
- [x] `--mux` / `--no-mux`.
- [x] `--mux-sessions` 1..64, default 4.
- [x] Non-MUX client/server code path exists.
- [ ] Exact non-MUX pooling and lifecycle parity.

### Plain WebSocket / MUX

- [x] Functional `ws://` handshake.
- [x] XOR MUX hello/OK boundary.
- [x] SYN/DATA/FIN/RST.
- [x] uint16 DATA chunking.
- [x] Bounded stream queues.
- [x] EOF/FIN/RST mapping.
- [x] Physical MUX pooling and least-active selection.
- [x] 256 logical streams/session.
- [x] Session retirement.
- [x] MUX serialization buffer reuse.
- [ ] Heavy concurrent reservation stress proof.
- [ ] Current-head throughput benchmark.
- [ ] Authenticated GoWay interop.

### UDP

- [x] Plain WS SOCKS5 UDP slice.
- [x] Separate raw `UDP\n` WS session.
- [x] No TCP MUX framing for UDP.
- [x] WSS UDP ASSOCIATE code path exists.
- [x] QUIC UDP length-prefixed stream framing code exists.
- [ ] Plain WS UDP executable round-trip.
- [ ] WSS UDP executable round-trip.
- [ ] QUIC UDP executable round-trip.
- [ ] Full FRAG/error/reply lifecycle parity.

### WSS / TLS

- [x] WSS TCP forwarding.
- [x] Cached verified/insecure rustls configurations.
- [x] HTTP/1.1 ALPN.
- [x] WSS physical-session pooling.
- [x] WSS non-MUX client path.
- [x] WSS UDP ASSOCIATE client path.
- [ ] WSS pooled TCP executable validation.
- [ ] WSS non-MUX executable validation.
- [ ] WSS UDP executable validation.
- [ ] Server-side WSS runtime.

### QUIC / QUIC+TLS

- [x] `quic://` / `quic+tls://` main-entry recognition.
- [x] QUIC TLS with GoWay ALPN `goway-quic` / `h3`.
- [x] 60s idle timeout and 15s keepalive configuration.
- [x] GoWay-derived stream/connection receive windows.
- [x] TCP stream bootstrap and `OK\n` / `ERR: DIAL_FAILED\n` semantics.
- [x] UDP `UDP` control path and 2-byte big-endian packet framing.
- [ ] QUIC physical connection pooling parity.
- [ ] QUIC retry/dead-IP semantics.
- [ ] QUIC TCP executable validation.
- [ ] QUIC UDP executable validation.
- [ ] GoWay QUIC interop.

## Compatibility option status

- [x] `max-conn` runtime groundwork.
- [x] block-local policy groundwork.
- [x] TCP_NODELAY policy groundwork.
- [ ] Exact socket-buffer semantics.
- [ ] Exact TCP keepalive semantics.
- [ ] Exact DNS/cache/fallback behavior.
- [ ] Every SOCKS5 REP/error branch.
- [ ] HTTP CONNECT error/close parity.
- [ ] Browser profile/TLS fingerprint parity.

## WebSocket codec

- [x] RFC6455 non-fragmented data/control path.
- [x] 64 MiB frame limit.
- [x] 8192-byte HTTP header limit.
- [x] Client masking/server unmasked.
- [x] Ping/Pong/Close.
- [x] Thread-local xorshift-style masking PRNG.
- [ ] Large outbound masking buffer reuse.
- [ ] Receive-buffer reuse.
- [ ] Writer contention reduction.

## Verification status

### Verified by source/static inspection in this relay

- GoWay v1.8.4 non-MUX wire shape.
- GoWay v1.8.4 QUIC ALPN/timing/window values.
- GoWay v1.8.4 QUIC TCP bootstrap and UDP 2-byte framing.
- GoWay v1.8.4 MUX full-frame XOR requirement.
- Main routing of WS/WSS/QUIC/non-MUX paths.

### Not executable-verified yet

- `cargo check` / `cargo build` / `cargo test` on current head.
- Authenticated GoWay ↔ RushWay TCP interop.
- WS/WSS/QUIC UDP round-trip.
- 1/100/500/1000 stream stress.
- Current-head c1/c8/c32 throughput.
- Windows x64 / Debian 12 / ARMv7 current-head release builds.
- v0.0.1 release smoke tests.

The relay environment currently lacks a usable Rust/Cargo toolchain and cannot resolve GitHub for dependency retrieval. Do not convert code inspection into a false “passed” claim.

## Final verification sequence

1. Compile/test current head and fix every compiler/test failure.
2. Plain WS authenticated MUX TCP.
3. Plain WS authenticated non-MUX TCP.
4. WSS MUX/non-MUX TCP.
5. Plain WS UDP.
6. WSS UDP.
7. QUIC TCP/UDP.
8. Bidirectional GoWay v1.8.4 interop.
9. 1/100/500/1000 stream stress + large payloads + mixed slow/fast.
10. Current-head c1/c8/c32 benchmarks.
11. Windows/Debian/ARMv7 release artifacts.
12. v0.0.1 smoke test and tag.

## AI relay rules

- Read `AI_HANDOFF.md`, `PROGRESS.md`, `SPEC.md` first.
- Start from latest `main`.
- One logical change per commit.
- Record exact verification evidence.
- Never claim runtime interoperability or release readiness without actual execution.
- Never route UDP packets through TCP MUX DATA.
- Preserve TLS verification and protocol semantics while optimizing.
