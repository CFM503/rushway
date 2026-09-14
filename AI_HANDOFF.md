# RushWay AI Relay Handoff

> Chronological AI-to-AI engineering handoff. Read this together with `PROGRESS.md` and `SPEC.md` before changing code.

## 2026-09-14 — v0.0.3 test-track checkpoint

### Target
- Repository: `CFM503/rushway`
- Target version: `v0.0.3` test build; not a final release
- GoWay baseline: v1.8.4, pinned commit `538dbee86b9fbf248a68c8c6d8eee5d6f8bdb0dc`
- Branch: `main`

### Latest validated CI
- Run #305: `34818400219`, head `27c44a68036e99fda07eecea74f688276517de03` — full core validation passed.
- Rust format/check/test/release passed; 38 unit tests passed.
- MUX hot-path benchmark passed: owned-vs-copy speedup `7047.11x`.
- E2E proxy benchmark passed: c1 `119.64`, c8 `172.55`, c32 `170.66` MiB/s.
- Repeated setup-inclusive RushWay median (5 samples): c1 `58.16`, c8 `153.60`, c32 `173.25` MiB/s.
- Repeated steady-state RushWay median (5 samples): c1 `130.39`, c8 `170.01`, c32 `171.87` MiB/s.
- Windows x64, Debian 12 x64, and ARMv7 Linux release builds all passed.
- GoWay comparison job is still skipped because `WAY_READ_TOKEN` is unset; a green skipped job is not interoperability evidence.

### Standalone server/client validation
- Confirmed from `src/main.rs`: when `--up` is absent, RushWay enters server mode and requires `-k` unless `--allow-open`; when `--up` is present, RushWay enters client mode and selects WS/WSS/QUIC according to the upstream URL.
- `src/bin/e2e_bench.rs` launches two separate RushWay child processes from the same release binary: one server with no `--up`, one client with `--up ws://127.0.0.1:<server_port>/`.
- The client exposes a separate local SOCKS5 port, connects through the server process to an independent local echo target, and round-trips a 4 MiB payload at c1/c8/c32.
- Run #305 passed this complete subprocess path. Therefore RushWay can independently run as a server process and as a client process for the WS+MUX path. This proves RushWay↔RushWay standalone operation; it is not proof of GoWay interoperability.

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

### Proxy benchmark harness finding and fix
Run #300 failed only at `Cross-proxy benchmark runner self-check` because temporary RushWay server children were started without authentication. The benchmark also needed `--no-block-local` for the localhost echo target.

Fix: commit `302be63036fe4435d99efbc5bdf890c22ace3f43` (`test: fix proxy benchmark server startup policy`).
- Temporary RushWay children receive a fixed test key.
- The benchmark passes `--no-block-local` for the local echo target.
- Logging is restricted to `ERROR` during benchmark startup.
- This is benchmark-harness-only; no proxy protocol change was made.

### ARMv7 compatibility fix
- Run #302 exposed an ARMv7 build failure from direct `cpufeatures 0.2.17` use.
- RushWay SHA-256 in `src/crypto.rs` now uses `ring`, which supports the supported architecture set while preserving the existing SHA-256 digest semantics.
- `sha1` remains a direct dependency because `src/ws.rs` requires it for WebSocket handshake calculation.
- Run #305 confirms ARMv7 Linux release build passes with this arrangement.

### Release gate
Do not call v0.0.3 100% and do not create a final tag/release yet.
Remaining proof:
1. Actual cloud GoWay v1.8.4 interoperability.
2. SOCKS5/HTTP lifecycle and error matrix.
3. WS/WSS/QUIC TCP+UDP interoperability.
4. 1/100/500/1000 stream stress and mixed workloads.
5. Repeated c1/c8/c32 benchmark evidence beyond the current local RushWay-vs-RushWay samples.
6. Windows x64, Debian 12 x64, ARMv7/OpenWrt smoke; current CI covers the first three, OpenWrt still pending.
7. Final v0.0.3 release/tag verification.

### Next action
Preserve the proven standalone server/client path, then execute the remaining compatibility gates in order. Enable the real GoWay v1.8.4 comparison using the optional repository-read credential. Never treat the skipped GoWay comparison as proof.

## Three-file relay contract
1. `AI_HANDOFF.md` — decisions, commits, blockers, next step.
2. `PROGRESS.md` — compact progress dashboard.
3. `SPEC.md` — source-derived GoWay compatibility contract.

Never call the project 100% complete merely because source paths exist.
