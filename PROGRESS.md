# RushWay v0.0.1 — Continuous Handoff Progress

> Compatibility baseline: GoWay v1.8.4 at `538dbee86b9fbf248a68c8c6d8eee5d6f8bdb0dc`.

## Current checkpoint — 2026-09-13

**Implementation coverage: ~94% estimate. Overall project/release completion: ~74% estimate.**

This is an engineering estimate, not a test score. The core transport surface is present; the active implementation gate is now full resolver integration, edge-case parity, then executable compile/test/interop/stress/release evidence.

### Latest implementation commits

- `133d6fad53053b889c96233c122cfdfc68191290` — GoWay single-hyphen multi-character CLI normalization and socket-policy propagation.
- `c09a22be2780a68023526ec86f4a19edc9b957a8` — centralized GoWay-style remote DNS resolver.
- `051463bc552d402db61355effce6d79b2370c530` — CLI/JSON DNS configuration.
- `8c54f465e0fc11f005596138be21336fadf89510` — runtime server DNS integration.
- `dd54bd4f02a89129e8c88c59fb19211b4e130991` — non-MUX DNS and socket-policy integration.
- `30cc175008364f60597f8a797ddda3eee3d69579` — current AI handoff synchronization.

## Compatibility status

### CLI

- [x] GoWay single-hyphen multi-character options normalized before Clap.
- [x] Existing short forms `-p`, `-k`, `-u`, `-W` preserved.
- [x] Parser tests for legacy and short forms.
- [ ] Remaining diagnostic/profiling flags (`-log`, `-log-file`, `-tui`, `-version`, profile options) audited for exact compatibility.

### DNS

- [x] Central resolver.
- [x] 5-second timeout.
- [x] Remote UDP query.
- [x] TCP retry on truncated UDP response.
- [x] System DNS fallback.
- [x] 5-minute positive cache.
- [x] Server target resolution.
- [x] Non-MUX upstream/target resolution.
- [ ] Plain MUX upstream resolution.
- [ ] WSS upstream resolution.
- [ ] QUIC upstream resolution.
- [ ] Executable DNS behavior test.

### Socket policy

- [x] Runtime NODELAY.
- [x] Runtime keepalive.
- [x] Runtime socket-buffer sizing.
- [x] Server target dial policy.
- [x] Non-MUX upstream/target policy.
- [ ] MUX/WSS/QUIC path audit and runtime evidence.

### Transports

- [x] Plain WS handshake/auth/XOR.
- [x] Plain WS MUX SYN/DATA/FIN/RST and pooling.
- [x] Plain WS non-MUX.
- [x] Plain WS UDP transport.
- [x] WSS MUX/non-MUX/UDP client code paths.
- [x] QUIC/QUIC+TLS TCP/UDP code paths.
- [ ] Current-head executable evidence for all transport families.

## Verification gate

Current GitHub Actions behavior is **not yet conclusive**. Earlier same-day CI run `34756277983` on commit `994a52c7797f408654057d881effebb9e4af07f2` completed successfully. New runs after the continuation commits fail in roughly four seconds, before usable job logs are exposed through the connector. Therefore the red status is recorded as an infrastructure/setup signal, not as a proven compiler failure.

Still not verified on the current head:

- `cargo fmt -- --check`
- `cargo check --all-targets`
- `cargo test --all-targets --all-features`
- `cargo build --release`
- GoWay ↔ RushWay authenticated WS/MUX/non-MUX interoperability
- WSS TCP/UDP interop
- QUIC TCP/UDP interop
- 1/100/500/1000 stream stress
- c1/c8/c32 benchmarks
- Windows x64 / Debian 12 x64 / ARMv7 current binaries
- v0.0.1 release smoke

## Mandatory next sequence

1. Integrate DNS into MUX, WSS and QUIC upstream dialing.
2. Finish socket-policy audit across those paths.
3. Audit exact SOCKS5/HTTP edge-case parity.
4. Audit remaining CLI flags and QUIC retry/dead-IP/TLS semantics.
5. Run real Rust fmt/check/test/release and fix every issue.
6. Execute full GoWay interop and stress matrix.
7. Build/smoke-test Windows, Debian 12 and ARMv7/OpenWrt artifacts.
8. Tag and smoke-test `v0.0.1`.

Never turn source inspection or a source-only CI API status into a false runtime-pass claim.

## Three-file relay contract

Only these files are canonical handoff state:

1. `AI_HANDOFF.md` — chronological decisions/blockers/next step.
2. `PROGRESS.md` — compact project dashboard.
3. `SPEC.md` — source-derived GoWay compatibility contract.
