# RushWay AI Relay Handoff

> Chronological AI-to-AI engineering handoff. Read this together with `PROGRESS.md` and `SPEC.md` before changing code.

## 2026-09-13 — Current continuation checkpoint

### Repository / target

- Repository: `CFM503/rushway`
- Compatibility baseline: GoWay v1.8.4 at `538dbee86b9fbf248a68c8c6d8eee5d6f8bdb0dc`
- Intended release: `v0.0.1`
- Target artifacts: Windows x64, Debian 12 x64, KWRT/OpenWrt ARMv7
- Current branch: `main`

### Newly completed in this continuation

- `133d6fad53053b889c96233c122cfdfc68191290` — normalized GoWay single-hyphen multi-character CLI options before Clap parsing; propagated socket buffer and keepalive options; added parser tests.
- `1cf7e24e00896fefb7f41491ed9b2309de04bfea` — synchronized AI interruption handoff.
- `c09a22be2780a68023526ec86f4a19edc9b957a8` — added a centralized remote-DNS resolver with 5-second timeout, UDP DNS, TCP fallback on truncated response, system DNS fallback, and 5-minute positive cache.
- `051463bc552d402db61355effce6d79b2370c530` — enabled CLI/JSON DNS configuration through the centralized resolver.
- `8c54f465e0fc11f005596138be21336fadf89510` — routed server-side runtime target TCP/UDP hostname resolution through the centralized resolver.
- `dd54bd4f02a89129e8c88c59fb19211b4e130991` — routed non-MUX upstream/target hostname resolution through the centralized resolver and applied the configured TCP socket policy on those connections.

### Current DNS implementation boundary

The resolver now exists as a shared process-level component and follows the source-derived GoWay shape:

- remote DNS server selected by `-dns` or JSON `dns` / `dnsServer`;
- remote DNS query first;
- 5-second resolve timeout;
- UDP query with TCP retry when the response is truncated;
- system DNS fallback after remote failure;
- 5-minute positive cache;
- IP literals bypass DNS.

It is currently integrated into the core server target path and plain non-MUX path. MUX upstream, WSS upstream and QUIC upstream hostname dialing still need the same resolver integration before DNS compatibility can be considered complete.

### CI reality check

The repository's GitHub Actions pipeline has previously completed successfully on the same day (for example run `34756277983`, commit `994a52c7797f408654057d881effebb9e4af07f2`). New runs after the continuation commits are failing almost immediately (about four seconds) and the GitHub connector cannot retrieve the job logs; this is not sufficient evidence of a Rust compiler/test failure. Treat the CI state as **inconclusive infrastructure/setup failure**, not as a code-pass and not as a proven compiler failure.

Do not claim current-head `cargo check`, `cargo test`, release build, runtime interop or artifact builds as passed until an actual job log or local compiler result proves them.

### Important remaining implementation work

1. Integrate the central DNS resolver into plain MUX upstream, WSS upstream and QUIC upstream dialing.
2. Ensure socket buffer/keepalive/NODELAY policy is applied consistently to all TCP upstream paths, not only runtime/non-MUX paths.
3. Complete GoWay CLI compatibility for remaining named flags (`-log`, `-log-file`, `-tui`, `-version`, profiling options) or explicitly prove they are non-functional/diagnostic-only in the baseline compatibility surface.
4. Audit SOCKS5 REP/error/FRAG/close parity and HTTP CONNECT malformed/header failure responses.
5. Audit QUIC retry/dead-IP/pool semantics and exact TLS/SNI behavior.
6. Obtain a real Rust 1.82 build/test environment; run fmt/check/test/release and fix every compiler/test issue.
7. Run the full GoWay interoperability, stress, benchmark and three-platform artifact matrix.
8. Only after all executable evidence passes, tag/smoke-test `v0.0.1` and call the project 100% complete.

### Three-file relay contract

Only these three files are canonical handoff state:

1. `AI_HANDOFF.md` — chronological decisions/blockers/next step.
2. `PROGRESS.md` — compact project dashboard.
3. `SPEC.md` — source-derived GoWay compatibility contract.

## Historical transport expansion

The major transport families already represented in code remain: plain WS, WSS, non-MUX, QUIC/QUIC+TLS; MUX pooling exists for plain WS/WSS and QUIC has physical connection reuse. Runtime socket policy and protocol framing work must still be proven by executable tests before release.

Never call the implementation 100% complete merely because source paths exist. The 100% gate requires executable evidence plus release validation.
