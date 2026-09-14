# RushWay AI Relay Handoff

> Chronological AI-to-AI engineering handoff. Read this together with `PROGRESS.md` and `SPEC.md` before changing code.

## 2026-09-14 — v0.0.2 formal-release checkpoint

### Target

- Repository: `CFM503/rushway`
- GoWay baseline: v1.8.4, commit `538dbee86b9fbf248a68c8c6d8eee5d6f8bdb0dc`
- Formal release target: `v0.0.2`
- Targets: Windows x64, Debian 12 x64, KWRT/OpenWrt ARMv7
- Branch: `main`

### CI recovery and parser repair

- `32cc8df6236055e2b85f70b34c7326baf2e941bb` — consolidated CI, removed redundant `build-smoke.yml`, pinned runner/toolchain gates and enabled concurrency cancellation.
- `5b2f1f937999eb4f004d4cfede5d4776a3920f19` — removed the final extra `}` from `src/runtime.rs` after formal CI parser validation.
- `86db1c8d00e50867e667dcf08e29fbc3072d171f` — repaired `src/udp_relay.rs` missing `await?;` syntax and retained the full UDP relay implementation.
- `e22b17a9ce84be363bbd664b17aa0ea8e17f9ac3` — GitHub Actions successfully ran the one-time Rustfmt repair, repaired residual compressed UDP syntax in `wss_client.rs`, formatted the full Rust tree, and removed the temporary formatter workflow.

### Verified Actions findings

- Ubuntu 22.04 runner is healthy.
- Rust 1.82.0 plus `rustfmt` installs successfully.
- `cargo fmt --all` completed successfully in the one-time repair workflow.
- The formal CI run before the formatter commit failed only because formatting could not complete while `udp_relay.rs` and `wss_client.rs` still contained compressed parser errors.
- The formatter-generated commit is driven by `GITHUB_TOKEN`, so it does not itself create another push-triggered workflow run; a normal repository commit is required to re-enter the formal CI gate.

### Current source state

- Core runtime, plain WS, WSS and QUIC hardening remains in place.
- Shared TCP socket policy, DNS transaction-ID validation, SOCKS5 General Failure mapping and MUX first-response handling are retained.
- Temporary repair workflows have been removed; do not recreate them after the next clean formal CI unless a narrowly scoped emergency repair is required.

### Remaining work before the 100% gate

1. Trigger and pass the clean formal `cargo fmt --check` gate on the post-format tree.
2. Continue through `cargo check --all-targets --all-features`, all-features tests and release build; fix real compiler/test failures.
3. Execute c1/c8/c32 benchmarks and record reproducible results.
4. Execute GoWay ↔ RushWay bidirectional TCP/UDP interoperability for WS/WSS/QUIC, including dead-IP, connection-state and TLS-SNI cases.
5. Execute 1/100/500/1000-stream stress plus large-payload and mixed slow/fast workloads.
6. Produce and smoke-test Windows x64, Debian 12 x64 and ARMv7 artifacts.
7. Create and verify `v0.0.2` tag/release only after all executable evidence is green.

### Release constraint

`Cargo.toml` is `0.0.2`. `v0.0.2` must not be called released until the executable gate is green and the Git tag/release is verified.

### Three-file relay contract

1. `AI_HANDOFF.md` — decisions, commits, blockers, next step.
2. `PROGRESS.md` — compact progress dashboard.
3. `SPEC.md` — source-derived GoWay compatibility contract.

Never call the project 100% complete merely because source paths exist. The 100% gate requires executable evidence and release validation.
