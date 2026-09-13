# RushWay v0.0.1 — Continuous Handoff Progress

> Compatibility baseline: GoWay v1.8.4 at `538dbee86b9fbf248a68c8c6d8eee5d6f8bdb0dc`.

## Current checkpoint — 2026-09-13

**Implementation coverage: ~95% estimate. Overall project/release completion: ~75% estimate.**

This is an engineering estimate, not a test score. Core transport paths and most CLI compatibility are present; the remaining work is increasingly about exact cross-path integration and executable evidence.

### Latest code/handoff checkpoint

- `dd54bd4f02a89129e8c88c59fb19211b4e130991` — non-MUX DNS/socket policy integration.
- `da7c6bf6e4f39e19441976d7b465667cbf6f278c` — GoWay logging/profiling option acceptance.
- `3cd5d6af6b28885cc6e4922d8296a2567ff2ea18` — fixed invalid Clap version reference.
- `cb24a792a072bd3f16f598e27cd80bdbe5de9f61` — GoWay boolean `=true/=false` CLI normalization.
- `71bada7b8601e1764d288796ee04469ce4f16f1b` — current AI handoff synchronization.

## CLI compatibility

- [x] GoWay single-hyphen multi-character forms.
- [x] `-up`, `-fakehost`, `-mux`, `-no-mux`, `-mux-sessions`.
- [x] `-mux=true/false`, `-block-local=true/false`, `-no-tcp-* =true/false` semantics.
- [x] `-log`, `-log-file`, `-tui`, `-version`, `-cpuprofile`, `-cpuprofile-duration` accepted.
- [x] Existing `-p`, `-k`, `-u`, `-W` short forms preserved.
- [ ] File logging, TUI and CPU profiling are still compatibility stubs, not full feature parity.

## DNS

- [x] Central resolver.
- [x] 5-second timeout.
- [x] UDP lookup.
- [x] TCP retry on truncated UDP response.
- [x] System-DNS fallback.
- [x] 5-minute positive cache.
- [x] Server target resolution.
- [x] Non-MUX upstream/target resolution.
- [ ] Plain MUX upstream resolution.
- [ ] WSS upstream resolution.
- [ ] QUIC upstream resolution.
- [ ] Executable DNS/fallback/cache test.

## Socket policy

- [x] Server NODELAY / keepalive / buffer policy.
- [x] Non-MUX upstream/target policy.
- [ ] MUX upstream audit.
- [ ] WSS upstream audit.
- [ ] QUIC/TCP path audit where applicable.

## Transport status

- [x] Plain WS handshake/auth/XOR.
- [x] Plain WS MUX SYN/DATA/FIN/RST and physical pooling.
- [x] Plain WS non-MUX.
- [x] Plain WS UDP transport.
- [x] WSS MUX/non-MUX/UDP code paths.
- [x] QUIC/QUIC+TLS TCP/UDP code paths.
- [ ] Current-head executable evidence for all transport families.

## Verification gate

GitHub Actions remains inconclusive: earlier same-day run `34756277983` was successful, while continuation runs terminate in roughly four seconds and expose no job logs through the available connector. This must not be reported as a compiler/test pass or failure without actual logs.

Still not verified on current head:

- `cargo fmt -- --check`
- `cargo check --all-targets`
- `cargo test --all-targets --all-features`
- `cargo build --release`
- authenticated GoWay ↔ RushWay WS/MUX/non-MUX interop
- WSS TCP/UDP interop
- QUIC TCP/UDP interop
- 1/100/500/1000 stream stress
- c1/c8/c32 benchmarks
- Windows x64 / Debian 12 x64 / ARMv7 current-head binaries
- v0.0.1 smoke/release

## Mandatory next sequence

1. Integrate DNS into MUX/WSS/QUIC upstream dialing.
2. Complete cross-path socket policy audit.
3. Finish SOCKS5 and HTTP CONNECT edge-case parity.
4. Finish QUIC retry/dead-IP/TLS/SNI audit.
5. Run real Rust fmt/check/test/release and fix actual failures.
6. Execute full GoWay interop/stress/benchmark matrix.
7. Build and smoke-test Windows x64, Debian 12 x64 and ARMv7/OpenWrt.
8. Tag and smoke-test `v0.0.1`.

## Three-file relay contract

Only these files are canonical handoff state:

1. `AI_HANDOFF.md` — chronological decisions/blockers/next step.
2. `PROGRESS.md` — compact project dashboard.
3. `SPEC.md` — source-derived GoWay compatibility contract.
