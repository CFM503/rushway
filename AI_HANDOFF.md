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

- `133d6fad53053b889c96233c122cfdfc68191290` — GoWay single-hyphen multi-letter CLI normalization and socket-option propagation.
- `c09a22be2780a68023526ec86f4a19edc9b957a8` — centralized remote DNS resolver: 5s timeout, UDP, TCP fallback for truncated replies, system fallback, 5-minute positive cache.
- `051463bc552d402db61355effce6d79b2370c530` / `8c54f465e0fc11f005596138be21336fadf89510` — CLI/JSON DNS configuration and server target integration.
- `dd54bd4f02a89129e8c88c59fb19211b4e130991` — plain non-MUX DNS/socket-policy integration.
- `da7c6bf6e4f39e19441976d7b465667cbf6f278c` — GoWay diagnostic/profiling flag acceptance.
- `3cd5d6af6b28885cc6e4922d8296a2567ff2ea18` — removed invalid Clap version-field usage.
- `cb24a792a072bd3f16f598e27cd80bdbe5de9f61` — GoWay boolean `=true/=false` option compatibility.
- `57031ac11159183696d4fe9c5d7d24804124f009` — pooled plain-WS MUX upstream DNS integration.
- `b8618409d9ab3360c581b563e443e9b3c7c15222` — WSS upstream DNS integration while preserving TLS/Host identity.
- `4ca5b78734f62056a309c762cafa8cc100d2a875` — QUIC upstream/server target DNS integration while preserving QUIC server name.
- `9ab22dead3e778027ad770017261967cc67ace8e` — MUX proxy success response deferred until the upstream stream is actually established.
- `3d23529bc08d09e7f0539b711b9a15c89af76296` — package version advanced to `0.0.2` for the formal release line.
- `44054a3ea2282c0974a539d586522edcaa924a71` — remote DNS transaction-ID validation and focused mismatch test.

### Important interoperability findings

GoWay's own integration tests use forms such as `-up ...`, `-log ERROR`, `-mux=true`, and `-block-local=false`; RushWay's parser now normalizes these into Clap-compatible arguments.

The shared DNS resolver now covers server target resolution plus plain WS MUX, plain WS non-MUX, WSS and QUIC upstream/target hostname dialing. IP literals bypass DNS, WSS/QUIC preserve the original logical hostname for TLS/HTTP identity, and remote DNS replies are transaction-ID checked.

The MUX local proxy path now waits until the physical/logical stream is acquired before returning local CONNECT success. The remaining refinement is to consume the first upstream MUX response so a server-side RST can be converted into the exact local SOCKS5/HTTP failure response.

### Current verification boundary

GitHub Actions remains **inconclusive** rather than a compiler result: the newest `v0.0.2` CI run (`34764182241`, head `3d23529bc08d09e7f0539b711b9a15c89af76296`) again marked the `test` job failed without exposing any job steps/logs; dependent artifact jobs were skipped. A same-day earlier run (`34756277983`, commit `994a52c7797f408654057d881effebb9e4af07f2`) completed successfully. No current-head `cargo fmt`, `cargo check`, `cargo test`, release build, binary build, or runtime interoperability result is claimed as passed.

### Remaining work before the 100% gate

1. Finish the MUX first-response/RST local failure mapping so target dial failures do not appear as successful CONNECTs.
2. Cross-path TCP socket-policy audit for pooled MUX/WSS upstream connections (NODELAY, keepalive, send/recv buffer).
3. SOCKS5 target-failure/error/FRAG/close parity and HTTP CONNECT malformed-request/status parity.
4. QUIC retry/dead-IP/pool semantics and exact TLS/SNI audit.
5. Execute DNS/cache/fallback tests after obtaining usable Rust 1.82 execution evidence.
6. Run `cargo fmt -- --check`, `cargo check --all-targets`, `cargo test --all-targets --all-features`, and `cargo build --release`; fix actual failures.
7. Run GoWay -> RushWay and RushWay -> GoWay TCP/UDP interoperability, 1/100/500/1000-stream stress, large payload and mixed slow/fast tests.
8. Run c1/c8/c32 benchmarks and Windows/Debian/ARMv7 artifact smoke tests.
9. Create and smoke-test `v0.0.2` only after executable evidence is complete.

### Release/tag constraint

`Cargo.toml` is now `0.0.2`. The current GitHub connector exposes branch/commit writes but no tag/ref-creation write operation, so `v0.0.2` has **not** been falsely claimed as tagged. The tag must point to the final verified release commit once tag creation is available.

### Three-file relay contract

Only these three files are canonical AI handoff state:

1. `AI_HANDOFF.md` — decisions, commits, blockers, next step.
2. `PROGRESS.md` — compact progress dashboard.
3. `SPEC.md` — source-derived GoWay compatibility contract.

Never call the project 100% complete merely because the source paths exist. The 100% gate requires executable evidence and release validation.