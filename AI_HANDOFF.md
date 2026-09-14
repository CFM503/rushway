# RushWay AI Relay Handoff

> Chronological AI-to-AI engineering handoff. Read this together with `PROGRESS.md` and `SPEC.md` before changing code.

## 2026-09-14 — v0.0.3 test-track checkpoint

### Target
- Repository: `CFM503/rushway`
- GoWay baseline: v1.8.4, commit `538dbee86b9fbf248a68c8c6d8eee5d6f8bdb0dc`
- Target version: `v0.0.3` test build; not yet a final release
- Targets: Windows x64, Debian 12 x64, KWRT/OpenWrt ARMv7
- Branch: `main`

### Local validated build
Local commit `400c3de7267966f457e4437f9d57bd1dd00cd0b1` passed:
- `cargo fmt -- --check`
- `cargo check --all-targets`
- `cargo test --all-targets --all-features` — 38 passed, 0 failed
- `cargo build --release`
- `target/release/rushway.exe --help`

### Direct source repairs in 400c3de
1. `src/mux_pool.rs` — remove the stray semicolon after `Arc::new(Self { ... })` in `MuxSessionPool::new`.
2. `src/wss_client.rs` — remove the stray semicolon after `Arc::new(Self { ... })` in `WssSessionPool::new`.
3. `src/quic.rs` — wrap both QUIC receive-window `VarInt::from_u64` calls with `expect(...)`.
4. `src/quic.rs` — remove the stray semicolon after `Arc::new(Self { ... })` in `QuicClientPool::new`.

### Packaging
- `Cargo.toml` version is `0.0.3`.
- `Cargo.lock` added.
- `.gitignore` added with `target/` and `*.pdb`.

### Local GoWay interoperability
GoWay: `D:\SOFT\ROUTER\goflyway_windows_386\goway.exe`, `GOWAY v1.8.4`.

Topology:
- GoWay: `127.0.0.1:18880`, key `test123`.
- RushWay: `127.0.0.1:11080`, upstream `ws://127.0.0.1:18880`.

Observed:
- 4 physical MUX sessions became active on GoWay.
- WebSocket/MUX physical establishment PASS.
- SOCKS5 to public HTTPS FAIL at TLS (`curl` error 35).
- SOCKS5 to local HTTP with `--no-block-local` hung / later closed.
- No GoWay business connection was observed.

Conclusion: runtime SOCKS → MUX stream → GoWay business forwarding is not proven.

### Cloud CI incident — 2026-09-14
The `400c3de` v0.0.3 test commit was pushed to `main`.
The old CI contained a source-mutating repair step that ran `scripts/repair_compiler_issues.py` and could commit/push to `main`.
That step reintroduced three constructor semicolons and pushed:
`5d553ab455f5fe8a827255fb5a2771de88131a25` — `fix: repair compiler blockers`.

The resulting cloud build failed `cargo check` on the same three constructor return types.
This is a CI automation defect, not a failure of the locally validated 400c3de source.

### CI hardening
The CI workflow has been changed to:
- remove the source-mutating repair step;
- use `contents: read` permission.

The current GoWay comparison job is still skipped because `WAY_READ_TOKEN` is not configured, so it is not yet a real cloud RushWay ↔ GoWay interoperability test.

### Release gate
Do not call v0.0.3 100% and do not create a final tag/release yet.
Remaining proof includes clean cloud CI, actual GoWay v1.8.4 interoperability, SOCKS5/HTTP lifecycle/error matrix, WS/WSS/QUIC TCP+UDP interoperability, 1/100/500/1000 stream tests, benchmark evidence, Windows/Debian/ARMv7 smoke, and final release verification.

### Next action
Restore `main` to the validated 400c3de source while retaining the hardened CI workflow; then remove obsolete source-repair workflow/script, run clean cloud CI, and establish a real GoWay interoperability job when the private-repo credential is available.

## Three-file relay contract
1. `AI_HANDOFF.md` — decisions, commits, blockers, next step.
2. `PROGRESS.md` — compact progress dashboard.
3. `SPEC.md` — source-derived GoWay compatibility contract.

Never call the project 100% complete merely because source paths exist.
