# RushWay AI Relay Handoff

> Chronological AI-to-AI engineering handoff. Read this together with `PROGRESS.md` and `SPEC.md` before changing code.

## 2026-09-14 — v0.0.3 test-track checkpoint

### Target
- Repository: `CFM503/rushway`
- Target version: `v0.0.3` test build; not a final release
- GoWay baseline: v1.8.4, pinned commit `538dbee86b9fbf248a68c8c6d8eee5d6f8bdb0dc`
- Branch: `main`

### Cloud CI history and current state
- Run #292: `34811581004`, commit `10098cd5...` — Rust format/check/test/release/MUX gates passed; e2e failed deterministically after 30 seconds with `e2e flow timed out after 30s`.
- Run #300: `34813126116`, commit `1f519e10...` — Rust format/check/test/release passed; MUX hot-path benchmark passed; e2e proxy benchmark passed; only the `proxy_bench` self-check failed because its temporary RushWay server was launched without authentication flags.
- Run #300 e2e result: `rushway_e2e payload_mib=4 roundtrip_echo=1 c1_mib_s=57.18 c8_mib_s=192.89 c32_mib_s=181.44`.
- Run #300 MUX decode benchmark: copy `2855.35 MB/s`, owned-reused `20272198.87 MB/s`, speedup `7099.72x`.
- Run #301: `34814373714`, head `302be63036fe4435d99efbc5bdf890c22ace3f43`, currently queued at last check.
- GoWay comparison remains skipped because `WAY_READ_TOKEN` is unset; a green skipped job is not interoperability evidence.

### E2E harness fixes
1. Local echo target requires `--no-block-local` in both e2e RushWay children.
2. Each e2e flow has a 30-second timeout.
3. Child diagnostic handling was hardened so stderr collection cannot become a second deadlock.

### Real MUX protocol finding and fix
Run #292 exposed a real CONNECT deadlock:
- `mux_pool::SessionState::open_stream()` sends `MuxCommand::Syn` and waits for a first `Data/Fin/Rst` frame before replying success to the local SOCKS5 client.
- `runtime::handle_mux_parts()` successfully dials the target and registers the stream, but previously sent no success frame when `Syn` completed.
- The local SOCKS5 client therefore waited for a CONNECT response, while the server-side target waited for application data.

Fix: commit `916a38b4279da84a6192aef0be4661981e1d351a` (`fix: acknowledge successful mux stream open`).
- After successful target connection and stream registration, the server sends an encrypted zero-length `MuxCommand::Data` frame.
- The existing client logic already interprets the first `Data` frame as successful CONNECT, so no new frame type was introduced.
- Run #300 e2e passing confirms this deadlock fix works in the internal RushWay client/server path.

### Proxy benchmark harness finding and fix
Run #300 failed only at `Cross-proxy benchmark runner self-check` with:
`port 33299 did not open`.

Inspection showed `src/bin/proxy_bench.rs` started RushWay children with only `-p`. `src/main.rs` rejects server mode without `-k` unless `--allow-open` is present, so the benchmark server exited before listening. The benchmark also uses a localhost echo target, so the client needed `--no-block-local`.

Fix: commit `302be63036fe4435d99efbc5bdf890c22ace3f43` (`test: fix proxy benchmark server startup policy`).
- `proxy_bench` now gives both temporary RushWay children a fixed test key.
- It passes `--no-block-local` for the local echo target.
- Logging is restricted to `ERROR` during benchmark startup.
- This is benchmark-harness-only; no proxy protocol change was made.

### Release gate
Do not call v0.0.3 100% and do not create a final tag/release yet.
Remaining proof:
1. Run #301 clean CI after the benchmark harness fix.
2. Actual cloud GoWay v1.8.4 interoperability.
3. SOCKS5/HTTP lifecycle and error matrix.
4. WS/WSS/QUIC TCP+UDP interoperability.
5. 1/100/500/1000 stream stress and mixed workloads.
6. Repeated c1/c8/c32 benchmark evidence.
7. Windows x64, Debian 12 x64 and ARMv7/OpenWrt smoke.
8. Final v0.0.3 release/tag verification.

### Next action
Monitor Run #301. If the proxy benchmark self-check passes, inspect the repeated cross-proxy and steady-state medians and cross-platform jobs. Then move to real GoWay v1.8.4 interoperability using the optional repository-read credential. Never treat the skipped GoWay comparison as proof.

## Three-file relay contract
1. `AI_HANDOFF.md` — decisions, commits, blockers, next step.
2. `PROGRESS.md` — compact progress dashboard.
3. `SPEC.md` — source-derived GoWay compatibility contract.

Never call the project 100% complete merely because source paths exist.
