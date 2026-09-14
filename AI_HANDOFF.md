# RushWay AI Relay Handoff

> Chronological AI-to-AI engineering handoff. Read this together with `PROGRESS.md` and `SPEC.md` before changing code.

## 2026-09-14 — v0.0.3 test-track checkpoint

### Target
- Repository: `CFM503/rushway`
- Target version: `v0.0.3` test build; not a final release
- GoWay baseline: v1.8.4, pinned commit `538dbee86b9fbf248a68c8c6d8eee5d6f8bdb0dc`
- Branch: `main`

### Locally validated source
Validated source commit: `400c3de7267966f457e4437f9d57bd1dd00cd0b1`
- `cargo fmt -- --check` PASS
- `cargo check --all-targets` PASS
- `cargo test --all-targets --all-features` PASS — 38 passed, 0 failed
- `cargo build --release` PASS
- `target/release/rushway.exe --help` PASS

Direct source repairs in 400c3de:
- `src/mux_pool.rs`: `MuxSessionPool::new()` constructor tail semicolon removed.
- `src/wss_client.rs`: `WssSessionPool::new()` constructor tail semicolon removed.
- `src/quic.rs`: both receive-window `VarInt::from_u64` calls use checked `expect(...)`.
- `src/quic.rs`: `QuicClientPool::new()` constructor tail semicolon removed.

Packaging:
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
- `main` was restored to validated 400c3de source plus hardened CI in `38d64a2b3283344aec48f1536a21ac9ed18faa49`.
- Latest cleanup commit removing the obsolete repair script is `cf32df03c7579109c15c97bc4745498a07ec31a0`.

The current RushWay CI run for cleanup is pending. The obsolete repair workflows are no longer part of the current tree.

The GoWay comparison job still skips actual GoWay v1.8.4 execution because `WAY_READ_TOKEN` is not configured. Therefore cloud CI is not yet a real RushWay ↔ GoWay interoperability test.

### Release gate
Do not call v0.0.3 100% and do not create a final tag/release yet.
Remaining proof:
1. Clean cloud CI.
2. Actual cloud GoWay v1.8.4 interoperability.
3. SOCKS5/HTTP lifecycle and error matrix.
4. WS/WSS/QUIC TCP+UDP interoperability.
5. 1/100/500/1000 stream stress and mixed workloads.
6. c1/c8/c32 benchmark evidence.
7. Windows x64, Debian 12 x64 and ARMv7/OpenWrt smoke.
8. Final v0.0.3 release/tag verification.

### Next action
Read the current CI result from the cleanup commit. Once clean, design/enable a true GoWay v1.8.4 interoperability job; do not use the missing secret as a false-positive pass. Then continue the runtime SOCKS/MUX investigation before final release.

## Three-file relay contract
1. `AI_HANDOFF.md` — decisions, commits, blockers, next step.
2. `PROGRESS.md` — compact progress dashboard.
3. `SPEC.md` — source-derived GoWay compatibility contract.

Never call the project 100% complete merely because source paths exist.
