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

- `133d6fad53053b889c96233c122cfdfc68191290` — GoWay single-hyphen CLI normalization, socket option propagation and parser tests.
- `c09a22be2780a68023526ec86f4a19edc9b957a8` — centralized GoWay-style remote DNS resolver.
- `051463bc552d402db61355effce6d79b2370c530` — CLI/JSON DNS configuration.
- `8c54f465e0fc11f005596138be21336fadf89510` — server runtime target DNS integration.
- `dd54bd4f02a89129e8c88c59fb19211b4e130991` — non-MUX DNS and socket-policy integration.
- `da7c6bf6e4f39e19441976d7b465667cbf6f278c` — accepted GoWay logging/profiling CLI names.
- `3cd5d6af6b28885cc6e4922d8296a2567ff2ea18` — fixed invalid Clap `args.version` reference.
- `cb24a792a072bd3f16f598e27cd80bdbe5de9f61` — normalized GoWay boolean forms such as `-mux=true`, `-mux=false`, `-block-local=false`, and `-no-tcp-keepalive=false` into valid Clap semantics.

### Important CLI compatibility result

GoWay's own v1.8.4 integration test starts processes with forms including `-log ERROR`, `-mux=true`, and `-block-local=false`. RushWay now accepts these single-hyphen forms and translates boolean `=true/=false` values to the corresponding positive/negative Clap flags. This is an important interoperability prerequisite.

The GoWay CLI names `-log`, `-log-file`, `-tui`, `-version`, `-cpuprofile`, and `-cpuprofile-duration` are now accepted. `-log` changes RushWay's tracing level; file/TUI/profiling functionality is still explicitly reported as compatibility stubs rather than falsely claimed as fully implemented.

### Current DNS implementation boundary

Central resolver behavior:

- configured remote DNS server via `-dns` or JSON `dns` / `dnsServer`;
- 5-second resolution timeout;
- UDP lookup;
- TCP retry when UDP response is truncated;
- system DNS fallback after remote failure;
- 5-minute positive cache;
- IP literals bypass DNS.

Currently integrated into server-side target dialing and the plain non-MUX client/server path. MUX upstream, WSS upstream and QUIC upstream hostname dialing still require integration.

### CI reality check

A same-day earlier run `34756277983` on commit `994a52c7797f408654057d881effebb9e4af07f2` completed successfully. Continuation runs since then, including the latest `34763815521`, terminate in about four seconds with `failure`; `test` and `goway-comparison` are marked failed, downstream artifact jobs are skipped, and job logs/steps are unavailable through the connector. This pattern is treated as an **inconclusive Actions infrastructure/setup failure**, not as a proven Rust compile failure. Do not mark any compile/test/build gate passed from these red checks.

### Remaining implementation priority

1. Integrate central DNS into plain MUX upstream, WSS upstream and QUIC upstream dialing while preserving original hostname/SNI where required.
2. Apply socket buffer/keepalive/NODELAY consistently across MUX/WSS upstream TCP connections.
3. Finish SOCKS5 REP/error/FRAG/close parity and HTTP CONNECT malformed-request/status parity.
4. Finish QUIC retry/dead-IP/pool and exact TLS/SNI audit.
5. Obtain a real Rust environment and run fmt/check/test/release; fix every actual compiler/test error.
6. Execute GoWay -> RushWay and RushWay -> GoWay transport matrix, stress 1/100/500/1000 streams, large and slow/fast mixed workloads, benchmarks and target builds.
7. Tag/smoke-test `v0.0.1` only after executable evidence is complete.

### Three-file relay contract

Only these three files are canonical handoff state:

1. `AI_HANDOFF.md` — chronological decisions/blockers/next step.
2. `PROGRESS.md` — compact project dashboard.
3. `SPEC.md` — source-derived GoWay compatibility contract.

Never call the implementation 100% complete merely because source paths exist. The 100% gate requires executable evidence plus release validation.
