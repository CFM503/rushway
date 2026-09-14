# RushWay AI Relay Handoff

> Chronological AI-to-AI engineering handoff. Read this together with `PROGRESS.md` and `SPEC.md` before changing code.

## 2026-09-14 — clean-source recovery checkpoint

### Target

- Repository: `CFM503/rushway`
- GoWay baseline: v1.8.4, commit `538dbee86b9fbf248a68c8c6d8eee5d6f8bdb0dc`
- Formal release target: `v0.0.2`
- Targets: Windows x64, Debian 12 x64, KWRT/OpenWrt ARMv7
- Branch: `main`

### Recovery completed

- `main` was explicitly reset to `07675eb094f40a78655aa33ad30fc087547a013f` to discard the later malformed compiler-repair commits.
- `src/mux_pool.rs` is now back on the requested clean-source checkpoint before the accidental follow-up constructor rewrite.
- The temporary compiler diagnostic and source-repair files are intentionally still present at this checkpoint; they will be removed only after the real source compiles cleanly.

### Exact remaining source repairs

The remaining known compiler blockers are limited to five edits:

1. `src/mux_pool.rs` — `MuxSessionPool::new`: remove the stray semicolon after the `Arc::new(Self { ... })` tail expression.
2. `src/wss_client.rs` — `WssSessionPool::new`: remove the stray semicolon after the `Arc::new(Self { ... })` tail expression.
3. `src/quic.rs` — `QuicClientPool::new`: remove the stray semicolon after the `Arc::new(Self { ... })` tail expression.
4. `src/quic.rs` — `TransportConfig::stream_receive_window`: unwrap `VarInt::from_u64(8 * 1024 * 1024)` with `expect("8MiB fits QUIC VarInt")`.
5. `src/quic.rs` — `TransportConfig::receive_window`: unwrap `VarInt::from_u64(16 * 1024 * 1024)` with `expect("16MiB fits QUIC VarInt")`.

Do not run or restore `scripts/repair_compiler_issues.py` as part of the formal build. Do not treat a temporary repair workflow as permanent build logic.

### Mandatory next gate

After those five source edits:

1. Remove `.github/workflows/compiler-diagnostic.yml`.
2. Remove `.github/workflows/source-repair-once.yml`.
3. Remove `scripts/repair_compiler_issues.py`.
4. Run clean `fmt -> cargo check --all-targets --all-features -> cargo test --all-targets --all-features -> cargo build --release`.
5. Record actual executable results in this file and `PROGRESS.md`.

### Release work after the compiler gate

- c1/c8/c32 benchmark measurements.
- Windows x64, Debian 12 x64 and ARMv7/OpenWrt smoke artifacts.
- GoWay <-> RushWay bidirectional WS/WSS/QUIC TCP+UDP interoperability.
- dead-IP, connection-state and TLS-SNI interoperability cases.
- 1/100/500/1000-stream stress plus large-payload and mixed slow/fast workloads.
- Final `v0.0.2` tag and GitHub Release only after executable evidence is green.

### Release rule

`Cargo.toml` may be `0.0.2`, but RushWay is not 100% complete and `v0.0.2` is not released until the executable gate and release smoke validation are green.

### Three-file relay contract

1. `AI_HANDOFF.md` — decisions, commits, blockers, next step.
2. `PROGRESS.md` — compact progress dashboard.
3. `SPEC.md` — source-derived GoWay compatibility contract.

Never call the project 100% complete merely because source paths exist.
