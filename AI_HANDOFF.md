# RushWay AI Relay Handoff

> Chronological AI-to-AI engineering handoff. Read this together with `PROGRESS.md` and `SPEC.md` before changing code.

## 2026-09-14 — Astra engineering protocol established

### Mandatory AI identity
- **All future AIs working on RushWay are to operate under the identity and engineering standard `Astra`.**
- Do not treat Astra as a cosmetic persona. It is the project's engineering review discipline.
- Every meaningful code change must first be reviewed from Astra's perspective before modification:
  1. concurrency correctness and race/ordering behavior;
  2. protocol compatibility and stream/session lifecycle;
  3. resource ownership, cancellation, cleanup, and failure isolation;
  4. performance under load and avoidance of hidden serialization;
  5. security/target-policy boundaries and regression risk;
  6. testability and an explicit validation gate.
- No AI may declare a change safe, stable, interoperable, or production-ready merely from source inspection. Use executable evidence whenever available.
- Do not weaken tests, limits, protocol semantics, or security policy merely to make CI green.
- Preserve exact evidence: commit SHA, workflow run ID, relevant job/step, exact failure symptom, and validation result.
- When uncertain, prefer a narrow, reversible change over a broad rewrite.

### Current RushWay workstream
- Repository: `CFM503/rushway`
- Target version: `v0.0.3` test track; **not a final release**
- GoWay baseline: v1.8.4, pinned commit `538dbee86b9fbf248a68c8c6d8eee5d6f8bdb0dc`
- Main branch remains the reference branch for relay context.
- Astra working branch: `astra/mux-syn-concurrency`
- Astra design review document on working branch: `docs/ASTRA_MUX_CONCURRENCY_REVIEW.md`
- GitHub issue: #2 — `Astra review: remove synchronous target dialing from MUX reader loop`

### Current validated base and high-concurrency blocker
- Latest full core validation before the current blocker was Run #320: `34823392373`, head `323e8408a942b545a75f17f6d5594d391a93ccbf`.
- Current high-concurrency stress harness uses staged mode after earlier burst diagnostics.
- Latest important stress evidence: Run #332: `34846337087`.
- Run #332 results:
  - Rust format check: passed.
  - Rust check: passed.
  - `cargo test`: passed; 39 tests plus MUX benchmark tests passed.
  - release build: passed.
  - MUX hot-path benchmark: passed.
  - normal WS E2E: passed.
  - standalone WSS E2E: passed.
  - standalone QUIC E2E: passed.
  - staged WS stress: 1 passed, 100 passed, 500 passed; 1000 failed.
  - failure symptom: `flow 411 connect: early eof`.
- Therefore **500-stream stability is currently demonstrated, but 1000-stream stability is not**.

### Astra root-cause assessment
The current service-side MUX reader in `src/runtime.rs::handle_mux_parts()` handles `MuxCommand::Syn` by awaiting `dial_target()` directly inside the physical-session reader loop.

This creates hidden per-session serialization:
- physical MUX reader receives SYN;
- DNS resolution / TCP connect executes inline;
- later SYN frames on the same physical session cannot be dispatched until the earlier dial completes.

This matches the observed staged-1000 failure pattern: 500 succeeds, while 1000 reaches flow 411 and then gets `early eof`.

### Astra required design invariant for the next code change
The physical MUX reader loop must become an I/O dispatcher only. It must never wait for an individual target's DNS/TCP connection establishment.

Required behavior:
1. Parse and validate SYN synchronously.
2. Register logical stream state before starting the target dial.
3. Spawn an independent per-stream async dial task.
4. On dial success, start the existing forwarding task and preserve the existing encrypted zero-length `MuxCommand::Data` success ACK semantics.
5. On dial failure, send `Rst` and remove the stream state.
6. `DATA`, `FIN`, and `RST` arriving while a dial is pending must be handled without blocking other streams.
7. Cancellation and cleanup must prevent leaked dial tasks or sockets.
8. Physical-session shutdown must invalidate outstanding logical streams cleanly.
9. Preserve `MAX_STREAMS_PER_SESSION = 256` and the existing default of 4 physical MUX sessions.
10. Do not weaken `block_local` / target-policy enforcement.

### Important existing protocol fixes
1. `916a38b4279da84a6192aef0be4661981e1d351a` — MUX SYN completion sends encrypted zero-length `MuxCommand::Data`, fixing the deterministic CONNECT deadlock found in Run #292.
2. `302be63036fe4435d99efbc5bdf890c22ace3f43` — benchmark child startup gets fixed auth and `--no-block-local`; this fixed the Run #300 benchmark self-check failure without changing proxy protocol behavior.
3. ARMv7 SHA-256 path uses `ring`; `sha1` remains direct for WebSocket handshake compatibility.
4. `ede3fdfd57a09e393c6cb5fcd2ff41cd502582c9` — default listener connection capacity raised to 4096 when no explicit `--config` or `--max-conn` is provided. Explicit configuration remains authoritative.
5. `ebfd256560be5f527cf60c7f2369c57d8ddcd987` — staged high-concurrency stress support was added.

### Validation gate after the SYN concurrency fix
Do not call v0.0.3 complete or release it until the following gates are satisfied:
1. `cargo fmt`.
2. `cargo check`.
3. `cargo test`.
4. WS staged stress: 1 / 100 / 500 / 1000 must pass.
5. Only after 1000 is stable: 2000-stream stress using at least 8 MUX sessions (`8 × 256 = 2048` logical-stream theoretical capacity).
6. Then WSS staged stress at 1000/2000.
7. Then QUIC staged stress at 1000/2000.
8. Then executable SOCKS5/HTTP lifecycle and malformed/error matrix coverage.
9. Then actual cloud GoWay v1.8.4 bidirectional interoperability using the pinned commit.
10. Then repeated benchmark evidence and OpenWrt/real-device smoke.
11. Final release/tag verification remains last.

### Release gate
Do not call v0.0.3 100% and do not create a final tag/release yet.

Current known gates:
1. [ ] Actual cloud GoWay v1.8.4 bidirectional interoperability.
2. [x] Standalone WSS executable server/client E2E.
3. [ ] SOCKS5/HTTP lifecycle and malformed/error matrix with executable evidence.
4. [ ] WS/WSS/QUIC TCP+UDP interoperability matrix.
5. [ ] 1/100/500/1000 stream stress across all transports. WS 1000 currently fails with `flow 411 connect: early eof`.
6. [ ] Repeated c1/c8/c32 benchmark evidence over additional commits/environments.
7. [x] Windows x64 / Debian 12 x64 / ARMv7 Linux release builds.
8. [ ] OpenWrt/real-device smoke.
9. [ ] Final v0.0.3 release/tag verification.

### Relay discipline for every future Astra AI
- Start by reading `AI_HANDOFF.md`, `PROGRESS.md`, and `SPEC.md`.
- Before modifying code, explicitly identify the relevant Astra invariants and likely regression surfaces.
- Prefer narrow, reversible commits with one engineering purpose.
- After every meaningful modification, run the smallest useful executable validation first, then the full relevant gate.
- Never infer interoperability from compilation, unit tests, or source symmetry alone.
- Never hide a failure by reducing concurrency, removing test cases, changing the expected result, or disabling a security check.
- If the current evidence conflicts with an assumption, trust the executable evidence and update the handoff.
- End each work session by recording: current branch/commit, validation performed, exact blockers, and the single best next action.

## Three-file relay contract
1. `AI_HANDOFF.md` — decisions, commits, blockers, next action, and Astra engineering protocol.
2. `PROGRESS.md` — compact progress dashboard.
3. `SPEC.md` — source-derived GoWay compatibility contract.

Never call the project 100% complete merely because source paths exist.
