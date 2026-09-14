# RushWay AI Relay Handoff

> Chronological AI-to-AI engineering handoff. Read this together with `PROGRESS.md` and `SPEC.md` before changing code.

## 2026-09-14 — v0.0.3 test-track checkpoint

### Target
- Repository: `CFM503/rushway`
- Target version: `v0.0.3` test build; not a final release
- GoWay baseline: v1.8.4, pinned commit `538dbee86b9fbf248a68c8c6d8eee5d6f8bdb0dc`
- Branch: `main`

### Current cloud state
Latest relevant CI run before this fix: Run #289, ID `34807474279`, commit `1dc812eb3d76c477dda76281eecd6f28b700f3be`.
- Rust format check: PASS
- Rust check: PASS
- Tests: PASS
- Release build: PASS
- MUX hot-path benchmark: PASS
- End-to-end proxy benchmark: STUCK in progress for 30+ minutes
- GoWay comparison: job reported success only because `WAY_READ_TOKEN` was unset and the comparison was skipped; this is not GoWay interoperability evidence.

### E2E hang root cause
`src/bin/e2e_bench.rs` used `127.0.0.1` as the echo target while the RushWay client retained the default `block_local=true` policy. `mux_pool::handle_tcp_proxy()` calls `pool.acquire(&target)`, which enforces the local-target policy before producing a SOCKS5 success/failure response. The client-side e2e flow then waited indefinitely in `read_exact()` for a SOCKS5 reply.

This was a test-harness bug / missing timeout, not evidence that proxy throughput itself needed 30+ minutes.

### E2E fix committed
Commit: `fca8ec2e628e7e8dd03deb41ee1dde64b296e475`

Changes in `src/bin/e2e_bench.rs`:
- Added `--no-block-local` to both e2e RushWay child processes because the echo target is intentionally local.
- Added a 30-second timeout around every individual proxy flow.
- Timeout now reports `e2e flow timed out after 30s` instead of allowing an indefinite CI hang.
- Existing child stderr diagnostics and early-exit detection remain enabled.

A push of this commit should cancel the stuck run under the workflow concurrency rule and start a fresh CI run.

### Locally validated source baseline
Validated source commit before the e2e harness fix: `400c3de7267966f457e4437f9d57bd1dd00cd0b1`.
- `cargo fmt -- --check` PASS
- `cargo check --all-targets` PASS
- `cargo test --all-targets --all-features` PASS — 38 passed, 0 failed
- `cargo build --release` PASS

Direct source repairs in that baseline:
- `src/mux_pool.rs`: `MuxSessionPool::new()` constructor tail semicolon removed.
- `src/wss_client.rs`: `WssSessionPool::new()` constructor tail semicolon removed.
- `src/quic.rs`: receive-window `VarInt::from_u64` calls use checked `expect(...)`.
- `src/quic.rs`: `QuicClientPool::new()` constructor tail semicolon removed.

### Packaging
- `Cargo.toml` version `0.0.3`.
- `Cargo.lock` added.
- `.gitignore` added for `target/` and `*.pdb`.

### Local GoWay interoperability
GoWay: `D:\SOFT\ROUTER\goflyway_windows_386\goway.exe`, `GOWAY v1.8.4`.

Topology:
- GoWay server `127.0.0.1:18880`, key `test123`.
- RushWay client `127.0.0.1:11080`, upstream `ws://127.0.0.1:18880`.

Results:
- 4 physical MUX sessions active: PASS.
- SOCKS5 to public HTTPS: FAIL during TLS (`curl` error 35).
- SOCKS5 to local HTTP with `--no-block-local`: hung / later closed.
- No GoWay business connection observed.

Runtime SOCKS → MUX stream → GoWay business forwarding is therefore not proven.

### Cloud CI incident
The old CI contained a source-mutating repair step. On the `400c3de` run it reintroduced three constructor semicolons and pushed `5d553ab455f5fe8a827255fb5a2771de88131a25` (`fix: repair compiler blockers`). The cloud `cargo check` then failed on those three constructors.

This was a CI automation defect, not a failure of 400c3de.

### Cloud CI hardening and cleanup
- `ci.yml` no longer mutates source or pushes code.
- `ci.yml` uses `contents: read`.
- `.github/workflows/compiler-diagnostic.yml` removed.
- `.github/workflows/source-repair-once.yml` removed.
- `scripts/repair_compiler_issues.py` removed.
- `main` was restored to validated 400c3de source plus hardened CI.

The GoWay comparison job still skips actual GoWay v1.8.4 execution because `WAY_READ_TOKEN` is not configured. Therefore cloud CI is not yet a real RushWay ↔ GoWay interoperability test.

### Release gate
Do not call v0.0.3 100% and do not create a final tag/release yet.
Remaining proof:
1. Clean cloud CI after the e2e harness fix.
2. Actual cloud GoWay v1.8.4 interoperability.
3. SOCKS5/HTTP lifecycle and error matrix.
4. WS/WSS/QUIC TCP+UDP interoperability.
5. 1/100/500/1000 stream stress and mixed workloads.
6. c1/c8/c32 benchmark evidence.
7. Windows x64, Debian 12 x64 and ARMv7/OpenWrt smoke.
8. Final v0.0.3 release/tag verification.

### Next action
Monitor the fresh CI run triggered by `fca8ec2e628e7e8dd03deb41ee1dde64b296e475`. If e2e now completes, inspect its throughput and continue to the remaining proxy benchmarks. Then enable real GoWay v1.8.4 comparison using the optional repository-read credential; never treat the skipped job as interoperability proof.

## Three-file relay contract
1. `AI_HANDOFF.md` — decisions, commits, blockers, next step.
2. `PROGRESS.md` — compact progress dashboard.
3. `SPEC.md` — source-derived GoWay compatibility contract.

Never call the project 100% complete merely because source paths exist.
