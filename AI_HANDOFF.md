# RushWay AI Relay Handoff

> Chronological AI-to-AI engineering handoff. Read this together with `PROGRESS.md` and `SPEC.md` before changing code.

## 2026-09-14 — v0.0.3 test-track checkpoint

### Target
- Repository: `CFM503/rushway`
- Target version: `v0.0.3` test build; not a final release
- GoWay baseline: v1.8.4, pinned commit `538dbee86b9fbf248a68c8c6d8eee5d6f8bdb0dc`
- Branch: `main`

### Current cloud state
- Run #289: `34807474279`, commit `1dc812eb...` — Rust gates passed; e2e hung for 30+ minutes.
- Run #291: `34810149855`, commit `795fe17...` — e2e still hung because timeout diagnostics could block.
- Run #292: `34811581004`, commit `10098cd5...` — Rust format/check/test/release/MUX gates passed; e2e failed deterministically after 30 seconds with `e2e flow timed out after 30s`.
- Current fix commit: `916a38b4279da84a6192aef0be4661981e1d351a` (`fix: acknowledge successful mux stream open`).
- Fresh CI run #299: `34813099975` is pending at last check.
- GoWay comparison remains skipped because `WAY_READ_TOKEN` is unset; that is not interoperability evidence.

### E2E harness fixes
1. Local echo target requires `--no-block-local` in both e2e RushWay children.
2. Each e2e flow has a 30-second timeout.
3. Child diagnostic handling was hardened so stderr collection cannot become a second deadlock.

### Real MUX protocol finding and fix
The deterministic timeout exposed a real CONNECT deadlock:
- `mux_pool::SessionState::open_stream()` sends `MuxCommand::Syn` and the client-side `handle_tcp_proxy()` waits for a first `Data/Fin/Rst` frame before replying success to the local SOCKS5 client.
- `runtime::handle_mux_parts()` successfully dials the target and registers the stream, but previously sent no success frame when `Syn` completed.
- The local SOCKS5 client therefore waited for a CONNECT response, while the server-side target waited for application data; neither side advanced.

Fix in `runtime.rs` at `916a38b4`:
- After successful target connection and stream registration, the server sends an encrypted, zero-length `MuxCommand::Data` frame.
- The existing client logic already interprets the first `Data` frame as successful CONNECT, so this uses the current stream semantics without inventing a new frame type.

### Release gate
Do not call v0.0.3 100% and do not create a final tag/release yet.
Remaining proof:
1. Fresh CI after the MUX ACK fix, including e2e and proxy benchmarks.
2. Actual cloud GoWay v1.8.4 interoperability.
3. SOCKS5/HTTP lifecycle and error matrix.
4. WS/WSS/QUIC TCP+UDP interoperability.
5. 1/100/500/1000 stream stress and mixed workloads.
6. c1/c8/c32 benchmark evidence.
7. Windows x64, Debian 12 x64 and ARMv7/OpenWrt smoke.
8. Final v0.0.3 release/tag verification.

### Next action
Monitor Run #299. If e2e passes, inspect its throughput and continue the remaining benchmark steps. If it fails, use the new deterministic failure output rather than allowing another long-running hang. Only after that continue to GoWay v1.8.4 interoperability.

## Three-file relay contract
1. `AI_HANDOFF.md` — decisions, commits, blockers, next step.
2. `PROGRESS.md` — compact progress dashboard.
3. `SPEC.md` — source-derived GoWay compatibility contract.

Never call the project 100% complete merely because source paths exist.
