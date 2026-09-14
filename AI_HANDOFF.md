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
