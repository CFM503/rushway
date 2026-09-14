# RushWay AI Relay Handoff

> Chronological AI-to-AI engineering handoff. Read this together with `PROGRESS.md` and `SPEC.md` before changing code.

## 2026-09-14 — v0.0.3 test-track checkpoint

### Target
- Repository: `CFM503/rushway`
- Target version: `v0.0.3` test build; not a final release
- GoWay baseline: v1.8.4, pinned commit `538dbee86b9fbf248a68c8c6d8eee5d6f8bdb0dc`
- Branch: `main`

### Latest validated CI
- Run #313: `34821431764`, head `ae24c2a4a22d9c51eb256ec3d652e7392300657d` — full core validation passed before the WSS server feature work.
- Rust format/check/test/release passed; 38 unit tests passed.
- MUX hot-path benchmark passed; owned-vs-copy speedup `580.86x` in Run #313.
- Standalone WS E2E passed: c1 `170.57`, c8 `175.28`, c32 `212.82` MiB/s.
- Standalone QUIC E2E passed: c1 `222.99`, c8 `187.92`, c32 `291.62` MiB/s.
- Repeated setup-inclusive RushWay median: c1 `189.61`, c8 `190.32`, c32 `220.37` MiB/s.
- Repeated steady-state RushWay median: c1 `207.60`, c8 `189.00`, c32 `218.76` MiB/s.
- Windows x64, Debian 12 x64, and ARMv7 Linux release builds all passed.
- GoWay comparison job is still skipped because `WAY_READ_TOKEN` is unset; a green skipped job is not interoperability evidence.

### Standalone server/client validation
- Confirmed from `src/main.rs`: when `--up` is absent, RushWay enters server mode and requires `-k` unless `--allow-open`; when `--up` is present, RushWay client mode selects WS/WSS/QUIC according to the upstream URL.
- `src/bin/e2e_bench.rs` launches two separate RushWay child processes from the same release binary and validates a separate local SOCKS5 listener against an independent local echo target.
- WS+MUX standalone path is proven from Run #305.
- QUIC standalone path is proven from Run #313, including a real two-process server/client round trip.
- WSS server mode is now implemented in `src/main.rs` with `--wss-server`, using TLS termination plus a private loopback WS runtime; `src/tls.rs` now provides a standalone self-signed WSS acceptor. A dedicated WSS E2E mode is wired into CI, but the first feature run was blocked only by rustfmt and has not yet produced WSS executable evidence.

### WSS implementation design
- External listener accepts TLS and RFC6455 WebSocket handshake.
- After the external 101 response, the process connects to an ephemeral loopback plain-WS runtime running the existing `runtime::run_server` implementation.
- The WSS side performs an internal WS client handshake and then transparently bridges already-framed RFC6455 bytes with `copy_bidirectional`.
- This preserves the existing WS/MUX protocol implementation rather than duplicating it in a new TLS-specific transport.
- Default WSS server certificate is self-signed for `localhost`; the existing WSS client defaults to insecure verification unless `--verify-ssl` is supplied.

### E2E harness fixes
1. Local echo target requires `--no-block-local` in both e2e RushWay children.
2. Each e2e flow has a 30-second timeout.
3. Child diagnostic handling was hardened so stderr collection cannot become a second deadlock.
4. `RUSHWAY_E2E_TRANSPORT` now supports `ws`, `wss`, and `quic` modes.

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
2. WSS executable server/client E2E result after the current WSS implementation passes CI.
3. SOCKS5/HTTP lifecycle and error matrix.
4. WS/WSS/QUIC TCP+UDP interoperability.
5. 1/100/500/1000 stream stress and mixed workloads.
6. Repeated c1/c8/c32 benchmark evidence beyond current local RushWay-vs-RushWay samples.
7. Windows x64, Debian 12 x64, ARMv7/OpenWrt smoke; current CI covers the first three, OpenWrt still pending.
8. Final v0.0.3 release/tag verification.

### Next action
Complete the current WSS CI cycle. Once WSS E2E is green, move directly to SOCKS5/HTTP lifecycle plus 1/100/500/1000 stream stress, then enable the real GoWay v1.8.4 comparison using the optional repository-read credential. Never treat the skipped GoWay comparison as proof.

## Three-file relay contract
1. `AI_HANDOFF.md` — decisions, commits, blockers, next step.
2. `PROGRESS.md` — compact progress dashboard.
3. `SPEC.md` — source-derived GoWay compatibility contract.

Never call the project 100% complete merely because source paths exist.
