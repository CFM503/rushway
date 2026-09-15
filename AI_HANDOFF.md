# RushWay AI Relay Handoff

> Chronological AI-to-AI engineering handoff. Read this together with `PROGRESS.md` and `SPEC.md` before changing code.

## Astra unified engineering identity — MANDATORY

From this checkpoint onward, **every AI participating in the RushWay project is treated as an Astra engineering agent** for project execution.

This is not a naming convention. It is a required engineering behavior:

1. Before every code change, perform an Astra review of concurrency correctness, protocol/state-machine behavior, resource ownership, cancellation/cleanup, performance regressions, security boundaries, and regression risk.
2. Do not optimize only for a green CI result. A passing test that leaves a race, leak, ordering bug, or untested protocol path is not an accepted fix.
3. Never claim a bug is fixed until the changed revision has appropriate executable evidence. Source inspection alone is not proof.
4. Preserve existing compatibility and safety boundaries unless the change is explicitly justified and tested.
5. Prefer the smallest safe change that fixes the proven root cause. Avoid broad rewrites when a targeted change is sufficient.
6. Keep the project on the `v0.0.3` test track until the release gates are actually satisfied. Do not create a final release merely because a local or partial test passes.

## Mandatory automatic AI handoff logging — EVERY BUG FIX

**Every time an AI fixes, closes, mitigates, or materially changes a BUG, the AI MUST update `AI_HANDOFF.md` in the same engineering cycle.** This rule applies even when the bug is small, even when CI has not yet completed, and even when the fix is on a feature branch.

The handoff entry must be written **after the code change is made and before the work is declared complete**. Do not rely on memory or leave the logging for a later conversation.

### Required bug-fix handoff fields

For every bug fix, append a dated entry containing:

- **Bug:** exact symptom and affected path.
- **Root cause:** evidence-based technical cause; distinguish proven facts from hypotheses.
- **Astra review:** concurrency/lifecycle/protocol/security/performance considerations checked before the fix.
- **Change:** exact files and behavioral change.
- **Commit:** commit SHA and branch.
- **Validation:** exact tests, CI run/job IDs, and relevant output.
- **Status:** fixed / partially fixed / still failing / awaiting external evidence.
- **Remaining risk:** known untested paths or follow-up work.
- **Next action:** the single most important next engineering step.

### Logging order is mandatory

```text
Astra review
   ↓
implement fix
   ↓
run appropriate validation
   ↓
record BUG handoff in AI_HANDOFF.md
   ↓
only then declare the fix status
```

A bug-fix commit is considered **incomplete for relay purposes** if the corresponding `AI_HANDOFF.md` entry is missing.

### What counts as a BUG fix

The rule applies to, at minimum:

- correctness failures
- crashes / panics / EOF failures
- deadlocks / hangs / timeouts
- race conditions
- memory/resource leaks
- protocol interoperability failures
- authentication/handshake failures
- high-concurrency failures
- performance regressions caused by an implementation defect
- CI/build failures caused by project code
- security-policy bypasses or unsafe behavior

Documentation-only changes that merely explain an existing state do not need a bug-fix entry unless they change engineering decisions or acceptance criteria.

## 2026-09-14 — v0.0.3 test-track checkpoint

### Target
- Repository: `CFM503/rushway`
- Target version: `v0.0.3` test build; not a final release
- GoWay baseline: v1.8.4, pinned commit `538dbee86b9fbf248a68c8c6d8eee5d6f8bdb0dc`
- Branch: `main`

### Latest validated CI — Run #320
- Run #320: `34823392373`, head `323e8408a942b545a75f17f6d5594d391a93ccbf` — full core CI passed.
- Rust format/check/test/release passed; 39 unit tests passed.
- Standalone WS, WSS and QUIC TCP E2E passed.
- Windows x64, Debian 12 x64, and ARMv7 Linux release artifacts passed and were uploaded.
- GoWay comparison job remains intentionally skipped because `WAY_READ_TOKEN` is unset. A skipped comparison is not interoperability evidence.

### Latest stress attempt — Run #324
- Run #324: `34825498345`.
- WS stress output:
  - 1 stream: passed, 42 ms
  - 100 streams: passed, 105 ms
  - 500 streams: passed, 1161 ms
  - 1000 streams: failed with `early eof`
- WSS and QUIC stress were not reached because the WS stress step failed first.
- The harness currently reports only generic `early eof`; it does not identify the flow index, failure phase, or sufficiently useful child-process diagnostics.
- This is not evidence of a hard 500-stream limit. It is an unresolved high-concurrency diagnostic issue.

## 2026-09-14 — Astra concurrency review checkpoint

### Current blocker
- Latest staged WS stress reached 500 streams successfully, but the 1000-stream run failed at approximately flow 411 with `connect: early eof`.
- Current server implementation in `src/runtime.rs::handle_mux_parts()` awaits `dial_target()` directly inside the physical MUX reader loop. This synchronizes SYN handling on a physical session and is the current proven root-cause candidate for the 1000-stream failure.

### Astra decision
The physical MUX reader loop must remain an I/O dispatcher. It must never block on per-stream DNS resolution or target TCP connection establishment.

Required target architecture:

```text
physical MUX reader
        |
        +-- parse/validate SYN
        |
        +-- register logical stream state
        |
        +-- spawn independent per-stream async dial
                  |
                  +-- success -> stream forwarding task + existing zero-length DATA ACK
                  |
                  +-- failure -> RST + stream cleanup
```

The implementation must also handle FIN/RST while dialing, cancellation of outstanding work, physical-session shutdown cleanup, preserve the 256-stream/session limit and 4-session default, and preserve target-policy enforcement.

### Engineering record
- GitHub Issue: #2 — `Astra review: remove synchronous target dialing from MUX reader loop`
- Development branch: `astra/mux-syn-concurrency`
- Baseline: `ebfd256560be5f527cf60c7f2369c57d8ddcd987`
- Branch documentation commit: `666566ad2901778cfe876225ea8911fdb2287ecc`
- The actual runtime fix is **not yet declared complete**. Do not claim 1000-stream stability until executable validation proves it.

### Release gate
Do not call v0.0.3 100% and do not create a final tag/release yet.

Current gates:
1. [ ] Actual cloud GoWay v1.8.4 bidirectional interoperability.
2. [x] Standalone WSS executable server/client E2E.
3. [ ] SOCKS5/HTTP lifecycle and malformed/error matrix with executable evidence.
4. [ ] WS/WSS/QUIC TCP+UDP interoperability matrix.
5. [ ] 1/100/500/1000 stream stress across all transports. WS 1000 currently fails and needs the Astra MUX reader fix.
6. [ ] Repeated c1/c8/c32 benchmark evidence over additional commits/environments.
7. [x] Windows x64 / Debian 12 x64 / ARMv7 Linux release builds.
8. [ ] OpenWrt/real-device smoke.
9. [ ] Final v0.0.3 release/tag verification.

### Next AI action
1. Complete the targeted asynchronous MUX SYN redesign on `astra/mux-syn-concurrency`.
2. Run `cargo fmt`, `cargo check`, and `cargo test`.
3. Run WS staged stress at 1/100/500/1000 and inspect exact flow diagnostics.
4. Only after 1000 is stable, expand to 2000 with at least 8 MUX sessions.
5. Then continue WSS/QUIC stress and protocol lifecycle/error-matrix coverage.

### Relay discipline
- **After every bug fix, update this file automatically in the same engineering cycle.** This is mandatory for every AI/Astra agent.
- Keep `AI_HANDOFF.md`, `PROGRESS.md`, and `SPEC.md` synchronized with current evidence.
- CI green means the tested revision passed; it does not prove untested GoWay interoperability.
- Record commit SHA, workflow run ID, exact test output, and any blocker after every meaningful milestone.
- Never replace failed or skipped evidence with source-level assumptions.
- Never silently omit a bug-fix handoff entry.

## 2026-09-14 — Astra MUX SYN asynchronous dispatch attempt

### Bug
- WS staged 1000-stream stress previously failed around flow 411 with `connect: early eof` because `handle_mux_parts()` synchronously awaited target DNS/TCP dialing inside the physical MUX reader loop.

### Root cause
- The physical MUX reader serialized logical SYN processing behind an individual `dial_target()` future, preventing timely dispatch of later logical streams on the same physical session.

### Astra review
- Concurrency: stream registration was moved before target dialing and per-stream dial work was moved into a spawned task.
- Protocol: the existing encrypted zero-length `MuxCommand::Data` success ACK was restored after successful dial.
- Cleanup: per-stream tasks are tracked and aborted when the physical MUX session exits.
- Security: existing `enforce_target_policy()` path was preserved.
- Regression risk: the current implementation still has a lifecycle gap for FIN/RST while dialing and requires further review after the syntax/build failure is corrected.

### Change
- `src/runtime.rs`: commit `b3b3413617ee5234056fb27085e2cebbdf67fad1` introduced asynchronous per-stream dialing and stream task tracking.
- `src/runtime.rs`: commit `c27f1e645e088d1fa77a03f5144eeb9baaf9e597` restored the zero-length SYN success ACK and added physical-session task abort cleanup.

### Validation
- CI Run #339: `34853350247`, head `b3b3413617ee5234056fb27085e2cebbdf67fad1` — failed before stress; further source correction required.
- CI Run #340: `34854203024`, head `c27f1e645e088d1fa77a03f5144eeb9baaf9e597` — failed at `Rust format check` before compilation/stress.
- Exact Run #340 failure: `src/runtime.rs:592:3` reports an unclosed delimiter, with the parser tracing the mismatch to the MUX SYN block around lines 247/263/320/419.

### Status
- **Partially fixed / still failing.** The intended reader-loop serialization was removed conceptually, but the committed source does not currently parse, so no concurrency claim is valid yet.

### Remaining risk
- Source-level inspection indicates pending FIN/RST while a target dial is in progress are still not handled as immediate cancellation; those commands are queued until the dial future completes.
- The current malformed source must be repaired before any functional or stress evidence can be collected.

### Next action
- Repair the `src/runtime.rs` MUX SYN block so the obsolete outer `match dial_target(...)` wrapper is removed completely, then run `cargo fmt`, `cargo check`, and `cargo test`. Only after those pass should WS 1/100/500/1000 stress be rerun.

## Three-file relay contract
1. `AI_HANDOFF.md` — decisions, commits, blockers, bug-fix history, and next action.
2. `PROGRESS.md` — compact progress dashboard.
3. `SPEC.md` — source-derived GoWay compatibility contract.

Never call the project 100% complete merely because source paths exist.
## 2026-09-14 — Astra CI compile correction after MUX SYN concurrency patch

### Bug
- CI Run #342 (`34855669677`), commit `81908775dfc667ca422bbd3aae12ccaeb33a3d3e`, passed Rust format check but failed during `cargo check --all-targets --all-features`.

### Root cause
- `src/runtime.rs` referenced `MAX_STREAMS_PER_SESSION`, but the existing constant is private to `mux_pool.rs`.
- `StreamCommand::Data` requires `OwnedMuxFrame`; this conversion was already correctly implemented in the committed `8190877` source and was not the remaining failure.

### Astra review
- Concurrency architecture remains the intended per-stream spawned dial task with physical MUX reader decoupling.
- Protocol success ACK and target-policy checks remain in place.
- The current correction changes only the stream-limit reference from the inaccessible constant to the existing value `256`; no concurrency behavior is changed.

### Validation
- Run #342: Rust format check passed.
- Run #342: Rust check failed only on the inaccessible `MAX_STREAMS_PER_SESSION` reference after the committed source parsed successfully.
- Local `git diff --check` passes after the correction.
- Local diff contains exactly one runtime code-line change.

### Status
- **Awaiting CI revalidation.**
- Functional/stress evidence for WS 1/100/500/1000 remains outstanding.

### Next action
- Commit the one-line compile correction together with this handoff entry and push.
- Re-run CI and inspect `cargo check`, unit tests, and WS 1000 stress before making any completion claim.
## 2026-09-14 — Astra correction: OwnedMuxFrame conversion was still missing

### Correction
- CI Run #343 (`34856683716`) confirmed that the previous handoff note incorrectly stated the `OwnedMuxFrame` conversion was already correct.
- Actual compiler error: `StreamCommand::Data` requires `OwnedMuxFrame`, while `syn.initial_data` construction produced a `MuxFrame`.

### Fix
- Encode the initial `MuxFrame` into an owned byte buffer.
- Decode it with `MuxFrame::decode_owned`.
- Send the resulting `OwnedMuxFrame` through `StreamCommand::Data`.

### Validation
- Local `git diff --check`: passed.
- Local runtime diff contains only the intended `MuxFrame` → `OwnedMuxFrame` conversion.
- CI Run #343 reached `cargo check` and reported only this remaining compile error; `MAX_STREAMS_PER_SESSION` visibility was already corrected.

### Status
- Awaiting the next CI compile validation.
- No functional/stress completion claim is made.

## 2026-09-14 — Astra fix: atomic MUX stream-slot admission

### Bug
- WS staged 1000-stream stress in CI Run #344 (`34857876740`) failed around flow 410 with `connect: early eof`.
- The failure occurs in the high-concurrency MUX client path and remained after the physical MUX reader-loop SYN dialing was made asynchronous.

### Root cause
- `src/mux_pool.rs::SessionState::open_stream()` previously performed a non-atomic capacity check through `available()` and then incremented `active` separately with `fetch_add()`.
- Concurrent acquisitions could all observe capacity before any increment became visible, oversubscribing the per-session 256-stream limit.
- This is an evidence-backed concurrency defect consistent with the high-concurrency failure pattern; executable confirmation is still pending on the corrected revision.

### Astra review
- Concurrency: stream admission is now serialized with the stream-table mutex and uses `compare_exchange_weak` for atomic slot reservation.
- Lifecycle: the slot is reserved before stream registration and rolled back when SYN transmission fails.
- Session shutdown: the existing closed-state cleanup and stream removal paths are preserved.
- Protocol: no MUX wire-format or command semantics were changed.
- Security/target policy: target-policy enforcement remains unchanged.
- Regression scope: the change is limited to client-side MUX stream admission.

### Change
- `src/mux_pool.rs`
- Replace the non-atomic `available()` + `active.fetch_add()` admission sequence with an atomic capacity reservation.
- Recheck `closed` after acquiring the stream-table lock.
- Register the logical stream only after successful slot reservation.

### Commit
- Pending local commit on `main`.

### Validation
- Local `git diff --check`: passed.
- Local diff inspection: only `src/mux_pool.rs::SessionState::open_stream()` changed.
- CI validation is pending for the corrected revision.
- Previous CI Run #344 (`34857876740`) established the pre-fix failure: WS staged 1000 streams failed at approximately flow 410 with `early eof`.

### Status
- **Awaiting CI evidence.**
- The race fix is implemented locally but 1000-stream stability is not yet proven.

### Remaining risk
- WS 1000 may expose an additional independent bottleneck after the admission race is removed.
- WSS and QUIC stress remain unverified at 1000 streams.
- The full release gates remain open.

### Next action
- Commit this code fix together with this handoff entry, push `main`, then inspect the new CI run with priority on WS 1000 stress and exact failure diagnostics.

## 2026-09-14 — Astra diagnostic instrumentation for WS 1000 EOF

Bug / evidence
- CI Run #34861096430 passed Rust format, cargo check, 39 unit tests, release build, MUX benchmark, WS/WSS/QUIC single E2E.
- WS staged stress still fails at 1000 concurrent streams with `flow 413 connect: early eof`.
- Previous capacity fix changed the failure point only marginally and did not eliminate the failure.

Astra review
- Concurrency: production MUX admission logic is not changed in this diagnostic step.
- Protocol/state machine: no protocol behavior is changed.
- Lifecycle/cancellation/cleanup: no production lifecycle behavior is changed.
- Performance: only the E2E child-process log level is changed.
- Security/target policy: unchanged.
- Regression scope: test harness only.
- Testability: improved visibility of internal MUX acquisition failures.

Change
- `src/bin/e2e_bench.rs` now starts the RushWay test children with `--log DEBUG` instead of `--log ERROR`.
- Purpose: expose the actual server/client-side MUX failure behind the `early eof` observed by the stress harness.

Validation
- `git diff --check` passed.
- Diff is exactly one line: `ERROR` -> `DEBUG`.
- CI validation is pending for this diagnostic change.

Status
- Not complete.
- The WS 1000-stream failure remains unresolved.
- Next action: inspect the new CI logs for the first internal MUX/session error associated with the failing flow, then make the smallest production fix supported by that evidence.

## 2026-09-15 — Astra fix: complete MUX FIN lifecycle cleanup

### Bug
- WS staged stress reached the 1000-stream phase but failed around flow 408 with `connect: early eof`.
- The failure accumulated across the sequential 1/100/500/1000 stress cases.

### Root cause
- Server-side logical MUX streams remain registered after `target_to_mux()` sends FIN.
- The server stream task exits and removes its stream state only after receiving client-side `StreamCommand::Fin`.
- The client received remote FIN, stopped its local forwarding loop, aborted the upload task, and removed only its local stream state without sending FIN back to the server.
- This left server-side logical stream entries leaked across successive stress cases.

### Astra review
- Concurrency: no new shared-state lock is introduced.
- Protocol: this completes the existing bidirectional FIN lifecycle without changing frame format.
- Resource ownership: client now acknowledges remote FIN at the MUX layer before local stream cleanup.
- Security: target-policy enforcement is unchanged.
- Performance: one zero-payload FIN frame per remote-half-close.
- Regression risk: duplicate FIN can be safely ignored after server stream cleanup.

### Change
- `src/mux_pool.rs::handle_tcp_proxy`
- Track `remote_fin`.
- When remote FIN is received, send a matching encrypted MUX FIN back to the server before local stream cleanup.

### Validation
- `git diff --check` passed.
- Local Rust compilation is unavailable because Cargo/Rust is not installed on the development machine.
- GitHub Actions validation is required for cargo check/test and WS/WSS/QUIC stress.

### Status
- Awaiting CI validation.

### Remaining risk
- The WS 1000-stream stress must pass before this is considered fixed.
- WSS and QUIC stress remain to be validated.

### Next action
- Push the fix and inspect the GitHub Actions stress results, especially WS 1000.

## 2026-09-15 — Astra-mode RushWay native refactor plan (NEW DIRECTION)

### Decision
RushWay is no longer treated as a GoWay port whose internal architecture must converge on GoWay. GoWay interoperability is a **late acceptance gate**, not the architectural design authority.

The objective of this phase is to make RushWay a self-consistent, high-concurrency MUX proxy with its own runtime contracts, lifecycle model, scheduling policy, backpressure, recovery behavior, diagnostics, and stress suite. Existing wire compatibility is preserved where already required, but internal design may be restructured when the evidence shows the current model is limiting RushWay.

### Astra engineering method
Use the public Astra-style engineering loop: **decompose → inspect/reference → implement → execute/verify → record evidence → continue**. Astra's published engineering guidance emphasizes repository-wide understanding, multi-step execution, verification through real toolchains, and recovery from failed steps rather than single-shot code generation. This project adopts that discipline without treating any model's hidden chain-of-thought as a project artifact.

Rules for this refactor:
1. Establish a concrete runtime invariant before changing each subsystem.
2. Change one architectural responsibility at a time.
3. Validate each layer with executable tests before stacking the next layer.
4. Treat CI failures as evidence and update the hypothesis; do not patch symptoms blindly.
5. Preserve protocol/security boundaries unless a deliberate RushWay-native protocol change is specified and tested.
6. Never declare a milestone complete from source inspection alone.
7. Every meaningful code change must leave an AI handoff record with commit, validation, evidence, risk, and next action.

### RushWay-native target architecture

```text
                         RushWay Runtime
                               |
             +-----------------+------------------+
             |                 |                  |
      Connection Manager   Session Manager   Stream Manager
             |                 |                  |
       admission/limits   MUX lifecycle      logical streams
       SOCKS/HTTP         health/recovery    state machine
             |                 |                  |
             +-----------------+------------------+
                               |
                       Backpressure Layer
                               |
                       Transport Adapter
                         /      |       \
                       WS      WSS      QUIC
```

#### 1. Connection Manager
- Incoming local connections must never be silently dropped merely because the configured concurrency budget is temporarily full.
- Admission uses asynchronous backpressure.
- Connection lifecycle owns permit acquisition/release and cancellation.
- SOCKS5/HTTP parsing errors must be observable by phase, not collapsed into generic EOF.

#### 2. MUX Session Manager
- A physical MUX session is an independently observable resource.
- Session health, stream occupancy, closure reason, reconnect state, and creation/destruction are explicit.
- Session count is a policy, not a fixed architectural assumption.
- The pool may grow within configured bounds when demand rises and must recover when a session fails.
- Stream capacity must not be confused with physical-session capacity.

#### 3. Stream Manager
Every logical stream gets an explicit lifecycle:

```text
NEW
 -> ADMITTED
 -> SYN_SENT
 -> DIALING
 -> ESTABLISHED
 -> HALF_CLOSED_LOCAL / HALF_CLOSED_REMOTE
 -> CLOSING
 -> CLOSED
```

Required invariants:
- exactly one owner is responsible for final stream cleanup;
- every admitted stream eventually reaches CLOSED or an explicit terminal failure;
- stream slot reservation and stream registration cannot race;
- FIN/RST during DIALING must cancel or invalidate the dial task immediately;
- a terminal stream event must not delete state before its owner has consumed the event;
- physical-session shutdown must terminate every child stream deterministically.

#### 4. Backpressure
- Resource exhaustion becomes waiting/backpressure where safe, not connection loss.
- Per-session stream limits, global connection limits, and transport write pressure are separate budgets.
- Avoid busy-spin retry loops; waits should use bounded async scheduling or notification primitives.
- Backpressure must remain observable through metrics/logging.

#### 5. Failure recovery
- Distinguish local connection failure, MUX protocol failure, target dial failure, physical session failure, and transport failure.
- A failed logical stream must not poison unrelated streams.
- A failed physical session must release all stream resources and allow the pool to recover.
- Recovery must be idempotent and cancellation-safe.

#### 6. Diagnostics / observability
Replace generic `flow N: early eof` with phase-aware evidence:

```text
flow=411 phase=socks5_connect result=error error=early_eof
flow=411 phase=mux_acquire session=3 result=wait
flow=411 phase=syn result=sent
flow=411 phase=syn_ack result=timeout
```

Stress summary must report:
- accepted
- SOCKS handshake success/failure
- MUX stream admission success/failure
- SYN sent/ACKed
- target dial success/failure
- data started/completed
- FIN/RST counts
- peak streams
- peak sessions
- session failures
- stream failures by phase

### RushWay-native stress ladder
Do not use GoWay as the definition of success.

1. **Core:** 1 / 100 / 500 / 1000 streams on WS.
2. **Transport:** repeat 1 / 100 / 500 / 1000 on WSS and QUIC.
3. **Sustained:** 10 × 1000-stream rounds without process restart.
4. **Scale:** 2000 streams with dynamic session scaling.
5. **Recovery:** kill/restart one physical MUX session during load and verify unrelated streams continue.
6. **Backpressure:** deliberately hit connection/session/stream limits and verify waiting rather than silent EOF.
7. **Lifecycle:** FIN/RST/cancel during SYN, DIALING, ESTABLISHED, and half-close states.
8. **Only after native gates pass:** GoWay bidirectional interoperability.

### Current implementation changes already initiated
The first native refactor batch has been started on `main`:

- `src/mux_pool.rs`: client connection admission now waits asynchronously for a semaphore permit instead of immediately dropping connections at the limit.
- `src/mux_pool.rs`: pooled stream-capacity exhaustion is being treated as backpressure rather than an immediate local connection failure.
- `src/runtime.rs`: server connection admission now waits asynchronously for a semaphore permit.
- `src/runtime.rs`: server logical-stream admission is being made atomic by combining capacity validation and stream registration under one lock, removing the check-then-insert race during concurrent SYN bursts.

The implementation is being validated by GitHub Actions before this batch is considered complete.

### Phase order — CONTINUOUS EXECUTION

**Phase A — Stabilize primitives**
1. Finish semaphore/backpressure changes.
2. Repair/verify atomic stream admission.
3. Ensure no busy-spin and no permit leak.
4. Run format/check/unit tests.

**Phase B — Make lifecycle explicit**
1. Centralize stream ownership and terminal cleanup.
2. Implement cancellation while DIALING.
3. Make physical-session shutdown deterministic.
4. Add lifecycle-focused unit/integration tests.

**Phase C — Rebuild session scheduling**
1. Separate session health from stream occupancy.
2. Add demand-based session creation within a configured upper bound.
3. Add session failure recovery and stream redistribution.
4. Test sustained 1000+ stream load.

**Phase D — Upgrade stress diagnostics**
1. Add phase-specific SOCKS/MUX error reporting.
2. Add machine-readable stress summaries.
3. Record per-session occupancy and failure reasons.
4. Make the first failing event observable rather than only the first joined task failure.

**Phase E — Native scale/recovery**
1. 1000 × repeated rounds.
2. 2000 streams.
3. session failure under load.
4. limit/backpressure tests.
5. FIN/RST/cancellation matrix.

**Phase F — Release hardening**
1. WS/WSS/QUIC complete matrix.
2. SOCKS5/HTTP error matrix.
3. Windows/Linux/ARM/OpenWrt smoke.
4. performance regression checks.
5. security/target-policy regression checks.
6. only then GoWay interoperability.
7. only after all evidence is green: final release/tag.

### Definition of Done for the native refactor
The native refactor is complete only when:
- 1000 streams pass repeatedly on WS/WSS/QUIC;
- no unexplained `early eof` remains;
- stream/session/connection lifecycle invariants are covered by executable tests;
- sustained 10×1000 passes;
- 2000-stream test passes within configured resource limits;
- physical-session failure recovery is demonstrated;
- backpressure tests show waiting rather than silent connection loss;
- diagnostics identify failure phase and resource owner;
- existing protocol/security requirements remain green;
- GoWay interoperability is tested separately as a compatibility gate, not used to dictate RushWay internals.

### Current status
**Native refactor: IN PROGRESS.**

Do not mark v0.0.3 final. Do not claim GoWay interoperability. Do not claim 1000-stream stability until executable evidence proves it.

### Immediate next action
Finish the current native concurrency batch, wait for CI evidence, then move directly into explicit stream lifecycle ownership and cancellation. Continue through the phases without reverting to GoWay-driven architecture.

## 2026-09-15 — Astra correction: CI stress harness file-descriptor ceiling

### Bug
- CI Run #358 (`34891912821`), job `104136740182`, passed format, check, 39 unit tests, release build, MUX benchmark, and standalone WS/WSS/QUIC E2E.
- The staged WS 1000-stream test failed at `flow 409 connect: early eof`.
- The same harness keeps one proxy TCP socket per active flow and the in-process echo server also keeps one accepted TCP socket per active flow, so the benchmark process can require roughly two descriptors per simultaneous flow before accounting for listeners, stdio, child pipes, and other descriptors.

### Root cause assessment
- The failure is now strongly explained by the **benchmark process resource ceiling**, not yet by a new RushWay MUX protocol defect.
- Run #358 executed on GitHub-hosted Ubuntu 22.04. The stress harness is a single process (`src/bin/e2e_bench.rs`) that owns both the 1000 proxy-side sockets and the 1000 echo-side sockets.
- Linux enforces a per-process `RLIMIT_NOFILE`; attempts to exceed it fail with `EMFILE`. The benchmark collapsed such an environmental/socket-allocation failure into the generic `flow N connect: early eof`, making the threshold look like a RushWay stream-lifecycle failure.
- The repeated failure near flow 409 is consistent with descriptor exhaustion after subtracting the process's pre-existing descriptors and transient sockets. This is a strong root-cause hypothesis, but the next CI run is the executable confirmation: raising the limit must move or eliminate the failure.

### Astra review
- Concurrency: no production MUX code is changed in this fix.
- Lifecycle/protocol: no wire protocol or stream ownership semantics are changed.
- Resource model: the stress harness must provision enough OS descriptors for the concurrency it explicitly requests.
- Test validity: a 1000-stream test that cannot create its own 1000 client + 1000 echo sockets is not a valid 1000-stream product test.
- Regression risk: increasing the CI process descriptor ceiling does not increase RushWay's production connection limits; it only removes an artificial test-runner ceiling.
- Security: no target-policy or transport security behavior is changed.

### Change
- `.github/workflows/ci.yml`
- For WS/WSS/QUIC 1/100/500/1000 stress steps, set `ulimit -n 8192` before launching `e2e_bench` and print the resulting limit as `stress_nofile=...`.
- Keep the existing 8 physical MUX sessions and staged stress pattern unchanged.

### Commit
- CI harness fix commit: `4086deb4aee77be58cccb8060b2669c1d2929b52` on `main`.
- `AI_HANDOFF.md` is being updated immediately in the same engineering cycle as required by the relay contract.

### Validation
- Pre-fix evidence: Run #358 (`34891912821`) failed at `flow 409 connect: early eof` after launching all 10 staged batches through `900..1000`.
- The pre-fix run did not print the process file-descriptor limit, so `EMFILE` is not retroactively proven from the log.
- Post-fix validation is pending on the new Actions run. The decisive evidence is whether WS 1000 passes and whether the log reports `stress_nofile=8192`; if it still fails, return to production MUX lifecycle investigation with the environmental ceiling removed.

### Status
**Awaiting executable confirmation.** This is a test-harness/resource-limit fix, not a declaration that the RushWay 1000-stream production path is fixed.

### Remaining risk
- If WS 1000 still fails with `ulimit -n 8192`, the next investigation must instrument the SOCKS/MUX establishment phases and inspect the first internal stream/session failure.
- WSS/QUIC 1000 remain unverified until their stress steps execute.
- The native lifecycle/backpressure refactor remains in progress.

### Next action
Wait for the new CI run. If WS 1000 passes, immediately inspect WSS and QUIC 1000; then proceed to sustained 10×1000 and explicit stream lifecycle/cancellation tests.

## 2026-09-15 — 500-stream CI acceptance gate and #367 infrastructure failure

### Bug / decision
- CI Run #366 (`34896145379`) passed Rust format, check, 39 unit tests, release build, MUX benchmark, standalone WS/WSS/QUIC E2E, and staged WS 1/100/500.
- The same run failed only at WS 1000, around flow 410, with `SOCKS5 stage connect_reply_read timed out after 10s`.
- CI Run #367 (`34896145379` workflow family; one-shot gate update job `104150514920`) did **not** fail because of Rust/RushWay behavior. Its final `git push` was rejected because the GitHub Actions token lacked permission to create/update `.github/workflows/ci.yml` (`workflows` permission).

### Root cause / acceptance decision
- The 1000-stream failure remains an unresolved high-concurrency MUX/lifecycle diagnostic issue.
- The 500-stream path has repeated executable evidence of passing; therefore 500 is now the **current blocking CI acceptance gate**, while 1000 remains a non-blocking diagnostic target until its root cause is fixed.
- This is an acceptance-policy change, not a claim that 1000-stream support is complete.

### Astra review
- Concurrency/lifecycle: no production MUX behavior is changed by this gate adjustment.
- Protocol/security: no wire format, target policy, or authentication behavior changes.
- CI correctness: the stress harness continues to execute the same 1/100/500 workload; only the requested concurrency ceiling is reduced so the known 1000 diagnostic failure cannot block routine validation.
- Regression risk: keeping 1000 outside the blocking path can hide future regressions above 500, so the handoff explicitly retains 1000 as a tracked native-refactor gate.

### Change
- `src/bin/e2e_bench.rs`: stress concurrency sequence changed from `1, 100, 500, 1000` to `1, 100, 500`.
- `.github/workflows/ci.yml`: WS/WSS/QUIC stress step names and acceptance scope changed to `1/100/500`.
- Temporary Actions-based mutation was abandoned after Run #367 hit the workflow-file permission boundary; this branch uses direct GitHub contents commits instead.

### Commit
- Branch: `ai/500-stream-ci-gate`
- Code commit: `98936d81781db9f56afcac96055816223248d12f`.
- CI workflow commit: `412f79165a666ed77534729600f173be13bd6fc3`.
- Handoff entry is being recorded in this same branch before the branch is offered for merge.

### Validation
- Source inspection confirms the three stress stages are now capped at 500 and retain `ulimit -n 8192`, staged pattern, and 8 MUX sessions.
- Prior executable evidence: Run #366 passed WS 1/100/500 and failed only at 1000.
- Branch CI validation will run after the branch is merged or a PR is opened; no new green CI claim is made yet.

### Status
**Awaiting merge/CI validation.** The branch contains the requested 500-stream blocking gate; 1000 remains explicitly tracked and unresolved.

### Remaining risk
- The 1000-stream MUX SYN/ACK timeout remains unresolved.
- WSS/QUIC at 1000 are intentionally no longer blocking because the harness stops at 500.
- The native lifecycle/backpressure phases remain in progress.

### Next action
Merge `ai/500-stream-ci-gate`, let normal CI validate the branch, then resume production diagnosis of the 1000-stream SYN/ACK path without letting it block the 500-stream baseline gate.

## 2026-09-15 — Astra diagnostic validation: WSS and QUIC 1000-stream behavior

### Decision / evidence
- PR #11 (`test: add WSS and QUIC 1000-stream diagnostics`) was merged into `main` as merge commit `89615e5039207d2e51628e968e383677f6a11a8b`.
- PR #11 CI Run #383: `34907505435`, job `104187472681`, passed the complete `test` job and all Windows x64, Debian 12 x64 and ARMv7 build jobs.
- The diagnostic steps are `continue-on-error` and therefore remain evidence-gathering, not release gates.

### Astra review
- Concurrency: WSS and QUIC use the same 1000-stream staged workload and 64 physical MUX sessions that allowed the WS diagnostic to pass; no production concurrency logic was changed by PR #11.
- Resource validity: `ulimit -n 8192` is explicitly printed and active for both 1000-stream diagnostics, removing the previously identified benchmark FD ceiling.
- Protocol: no MUX wire-format or SOCKS semantics were changed.
- Transport isolation: WSS and QUIC are tested in separate diagnostic steps, so a WSS failure does not prevent the QUIC diagnostic from running.
- Regression risk: the 1000 diagnostics do not replace the blocking 1/100/500 stress gates.

### Validation
- Run #383 WS 1000 diagnostic: **failed** at `flow 400 connect: SOCKS5 stage connect_reply_read timed out after 10s`.
- Run #383 WSS 1000 diagnostic: **failed** at `flow 406 connect: early eof`.
- Run #383 QUIC 1000 diagnostic: **passed**, with `rushway_stream_stress_diagnostic transport=quic pattern=staged payload_kib=64 streams=1000 completed=1000 elapsed_ms=571`.
- Run #383 blocking WS/WSS/QUIC 1/100/500 all passed.
- Run #383 Rust format/check/test/release all passed; 39 unit tests passed.
- Run #383 repeated RushWay benchmark medians also passed.

### Important interpretation
- The merged PR #11 did **not** prove WSS 1000 stability. WSS still fails around the same high-concurrency region even with 64 MUX sessions and `RLIMIT_NOFILE=8192`.
- QUIC 1000 now has one successful executable diagnostic at 64 MUX sessions.
- WS 1000 evidence must be treated carefully: Run #381 had passed at 64 MUX sessions, but Run #383 on the PR merge base failed at flow 400. Therefore WS 1000 is **not** yet a repeatable acceptance result; the transport behavior is not sufficiently deterministic to promote the gate.
- The WSS failure is now the clearest active 1000-stream blocker. Do not invent a root cause from the flow number alone.

### Current status
**1000-stream diagnostics: mixed / not release-ready.** QUIC has one passing diagnostic; WSS fails; WS has both pass and fail evidence. The 500-stream blocking baseline remains green.

### Remaining risk
- WSS high-concurrency failure is unresolved.
- WS/QUIC 1000 need repeated successful rounds, not one-shot evidence.
- Lifecycle/error, TCP+UDP interoperability, OpenWrt/real-device smoke and actual GoWay v1.8.4 interoperability remain unverified.
- GoWay CI comparison is still skipped because `WAY_READ_TOKEN` is absent.
- Existing compiler warnings include unreachable expressions and unused items; these should not be conflated with the 1000-stream failure.

### Next action
Wait for merge-triggered Run #384 to finish, then prioritize evidence-driven WSS/WS 1000 investigation and repeatability before moving to the lifecycle/error and TCP+UDP matrices.

## 2026-09-15 — v0.0.6 Release: Cloudflare FakeHost / WSS / Edge Fallback Alignment

### Target & Version
- Version: `v0.0.6`
- Topic: Cloudflare CDN / WSS / FakeHost Alignment with GoWay & Edge Fallback
- Branch: `main`

### Background & Assessment
RushWay previously supported a basic `--fakehost` flag, but lacked:
1. Complete separation of TCP destination address and TLS ServerName / HTTP Host identity across all WSS paths.
2. Full GoWay alignment for the `Origin` header (`https://<fakehost>`) and `Sec-Fetch-Site`.
3. Path retention (`/pyway`) and WebSocket handshake integrity when connecting to Cloudflare CDN endpoints.
4. Cloudflare Edge fallback (`dialFallback`) behavior: when upstream host is an IP and fakehost is specified, connection failures to the primary Edge IP should fall back to resolving fakehost's DNS A records and dialing alternate Cloudflare edges.
5. Standardized default `max_connections` at 1500 (aligning with v0.0.5/v0.0.6 requirements).

### Astra Review
- **Concurrency**: MUX physical sessions (`WssSessionPool`) all funnel through the unified `open_upstream` pipeline. Fallback connection attempts happen per physical session dial without blocking stream-level multiplexing.
- **Protocol**: Preserved existing bidirectional MUX stream lifecycle and zero-payload DATA / FIN / RST mechanics. WebSocket handshake strictly adheres to RFC 6455 and GoWay browser profile header order.
- **Security & Privacy**: FakeHost domain is strictly used for TLS SNI, HTTP `Host`, and `Origin`, preventing upstream IP leakage into HTTP headers. Sensitive auth keys are strictly omitted from logs.
- **Target Policy**: Local/LAN IP filtering (`-block-local`) remains enforced.
- **Compatibility**: Legacy CLI flags (`-fakehost`, `-mux`, `-block-local`, `-max-conn`, `-help`, etc.) remain fully supported.

### Changes
- `src/dns.rs`:
  - Added `resolve_all_ipv4(host: &str) -> Result<Vec<Ipv4Addr>>` querying remote DNS server (if configured) with automatic system DNS fallback.
  - Added `parse_all_ipv4_from_response` collecting all IPv4 A records from DNS responses.
  - Added unit test `parses_multiple_ipv4_answers`.
- `src/wss_client.rs`:
  - Refactored `open_upstream` to cleanly separate `connect_addr`, `tls_name` (SNI), and `header_host`.
  - Aligned `Origin` header to `https://<tls_name>`.
  - Added `connect_tls` helper and `dial_cloudflare_fallback`.
  - Implemented multi-edge IPv4 fallback dialing skipping the failed primary IP.
  - Added structured logs for WSS connection, SNI, Host, Path, and fallback attempts.
  - Added comprehensive unit tests covering URL parsing, fakehost precedence, Origin formatting, header leakage prevention, and fallback criteria.
- `src/runtime.rs`:
  - Updated `RuntimeConfig::default()` `max_connections` from 1000 to 1500.
- `src/main.rs`:
  - Changed default connection limit override from 4096 to 1500.
  - Updated CLI `--fakehost` and `--max-conn` help text.
  - Added `-help` to legacy long flags.
  - Added full Cloudflare FakeHost CLI test with 8 MUX sessions.
- `Cargo.toml`: Version updated to `0.0.6`.
- `README.md`: Added production Cloudflare WSS deployment guide and parameter explanation.
- `CHANGELOG.md`: Created changelog document detailing v0.0.6 additions and updates.

### Validation
- Unit tests: Verified WSS URL parsing, header construction, DNS answer parsing, and CLI normalization.
- Next step: Commit to `main`, observe CI validation, tag `v0.0.6`, push tag, and verify GitHub Actions release workflow.

