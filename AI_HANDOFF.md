# RushWay AI Relay Handoff

> Chronological AI-to-AI engineering handoff. Read this together with `PROGRESS.md` and `SPEC.md` before changing code.

## 2026-09-13 — Current continuation checkpoint

### Repository / target

- Repository: `CFM503/rushway`
- Compatibility baseline: GoWay v1.8.4 at `538dbee86b9fbf248a68c8c6d8eee5d6f8bdb0dc`
- Intended release: `v0.0.1`
- Target artifacts: Windows x64, Debian 12 x64, KWRT/OpenWrt ARMv7
- Current code branch: `main`

### Newly completed in this continuation

Commit `133d6fad53053b889c96233c122cfdfc68191290` updates `src/main.rs`:

- Added argv normalization for GoWay single-hyphen multi-character options before Clap parsing.
- Covered `-up`, `-fakehost`, `-mux`, `-no-mux`, `-mux-sessions`, `-allow-open`, `-verify-ssl`, `-socket-buffer`, `-no-tcp-nodelay`, `-no-tcp-keepalive`, `-dns`, `-block-local`, `-no-block-local`, `-max-conn`, `-connection-timeout`.
- Preserved the ordinary short forms `-p`, `-k`, `-u`, and `-W`.
- Added parser tests for legacy single-dash normalization and preservation of short forms.
- `socket-buffer` and `no-tcp-keepalive` are now propagated into `RuntimeConfig` instead of being discarded.
- JSON loading now accepts `tcp_keepalive`/`tcpKeepAlive` and `socket_buffer`/`socketBuffer`.
- `-dns` now validates the supplied value as an IP address; full remote-DNS runtime behavior is still pending.

### Source-derived DNS contract confirmed

GoWay v1.8.4's remote resolver uses a custom Go `net.Resolver`, a 5-second resolver timeout, a 5-minute positive cache, remote DNS lookup first, and system-DNS fallback on failure. The resolver is used for server target resolution and upstream client host resolution while preserving hostname-based TLS/SNI semantics. This contract was re-checked from the baseline source and its remote-DNS implementation plan.

RushWay does not yet implement that complete resolver contract. Do not mark DNS compatibility complete merely because `-dns` now parses and validates.

### Current execution/test boundary

The current engineering container has no `cargo`, `rustc`, or `rustup` available, and the GitHub connector reports no workflow runs for the new code commit. Therefore current-head compilation and tests remain unverified.

Do not claim:

- `cargo fmt -- --check`
- `cargo check --all-targets`
- `cargo test --all-targets --all-features`
- `cargo build --release`
- GoWay <-> RushWay runtime interoperability
- WSS/QUIC UDP runtime success
- stress/benchmark results
- release artifacts

### Remaining implementation priority

1. Implement the GoWay-compatible remote DNS resolver: 5s timeout, remote first, system fallback, 5-minute positive cache, and use it in both server target dialing and upstream host dialing without breaking TLS/SNI hostname identity.
2. Audit socket-option propagation into non-MUX and WSS upstream TCP paths; server runtime already applies `socket2` policy.
3. Audit exact SOCKS5 REP/error/FRAG lifecycle and HTTP CONNECT 400/error-close behavior against the baseline.
4. Audit GoWay QUIC dead-IP/retry/pool behavior and TLS/SNI semantics.
5. Run the real Rust verification gate once a working Rust environment is available.
6. Only after all executable evidence passes, update the release state to 100% and create/tag `v0.0.1`.

### Three-file relay contract

Only these three files are canonical handoff state:

1. `AI_HANDOFF.md` — chronological decisions/commits/blockers/next step.
2. `PROGRESS.md` — compact project dashboard.
3. `SPEC.md` — source-derived GoWay compatibility contract.

## Historical implementation expansion checkpoints

The implementation was expanded substantially beyond the earlier WSS pooling checkpoint:

- Plain WS non-MUX TCP path added in `src/nonmux.rs` and wired from `src/main.rs`.
- WSS non-MUX TCP path added and wired.
- QUIC / QUIC+TLS module added with GoWay-derived ALPN, timeout/window values, TCP stream bootstrap and UDP framing.
- QUIC client physical-connection reuse with single-flight dialing barrier is implemented.
- QUIC UDP framing uses a 2-byte big-endian length prefix and applies the configured XOR transform to payloads.
- MUX XOR compatibility audit established full encoded-frame transformation for the current MUX paths.
- Runtime has `max_connections`, `block_local`, `tcp_nodelay`, `tcp_keepalive` and `socket_buffer` policy fields.

Never call the implementation 100% complete merely because source paths exist. The 100% gate requires executable evidence plus release validation.
