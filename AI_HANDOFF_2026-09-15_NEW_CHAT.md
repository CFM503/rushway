# RushWay AI Handoff — New Chat Resume Point

## Project
- Repository: `CFM503/rushway`
- Default branch: `main`
- Architecture direction: RushWay-native; **not a GoWay port**.
- GoWay is only a late interoperability/acceptance gate, not the architecture authority.
- User expects autonomous Astra-style engineering: inspect → reason from evidence → implement → CI verify → record evidence → continue.
- User may open a new conversation; do not ask them to repeat the project history. Read this file first, then `AI_HANDOFF.md`.

## Current stable baseline
- Main merge commit: `b2df3980a1d35b54107e9322f0680be40054aa89`
- PR #6: 500-stream CI acceptance gate.
- Current blocking stress gate: **500 concurrent streams**.
- 1000 streams remain a non-blocking reliability target.
- GitHub Actions **Run #369 completed successfully** for the main merge commit.
- Run #369 was triggered by `push`, not a PR event. Therefore queries that only return PR-triggered workflow runs can incorrectly look empty; use the repository Actions runs endpoint when checking current CI.
- GitHub documents that `push` events can trigger workflows and that the workflow file must be present for the relevant ref/event. See GitHub Actions event/workflow documentation if needed.

## Current investigation branch
- Branch: `ai/mux-reader-1000-diagnostics`
- Goal: diagnose and correctly fix the 1000-stream concurrency/lifecycle failure without weakening backpressure or making the data path unbounded.
- `main` must remain green and untouched until a reviewable fix is ready.

## 1000-stream failure evidence
Previous 1000-stream runs repeatedly failed around flow ~400–410.

Typical exact failure:
`SOCKS5 stage connect_reply_read timed out after 10s`

Completed before failure:
1. TCP connect
2. SOCKS5 greeting write
3. method response read
4. CONNECT request write

Therefore the client was waiting for the final SOCKS5 success, which depends on the MUX SYN → server admission/dial → ACK → client dispatch path.

`ulimit -n 8192` was tested and did not eliminate the failure, so the simple file-descriptor-limit hypothesis was rejected.

## Leading hypothesis
`src/mux_pool.rs` has a `client_reader_loop()` that reads frames from one physical MUX connection, finds the per-stream bounded MPSC sender, and currently does an awaited `tx.send(frame).await` (channel capacity 32).

Potential failure mode:
- one stream becomes slow and its channel fills;
- physical MUX reader awaits that send;
- physical reader stops dispatching frames for other streams;
- unrelated streams remain stuck waiting for ACK / SOCKS5 CONNECT success;
- this can produce the deterministic ~400–410 concurrency boundary.

This is a **hypothesis, not yet proven**.

## Other hypotheses
- physical MUX writer mutex contention;
- stream active-count / cleanup race;
- stale state after FIN/RST/session shutdown;
- session-pool 2ms polling backpressure amplifying scheduling pressure.

## Required evidence before claiming root cause
For a failing flow near the boundary, identify the first missing phase:
1. client SYN sent;
2. server SYN received/admitted;
3. server dial started;
4. server dial success/failure;
5. server ACK queued/written;
6. client physical reader received ACK;
7. client stream owner consumed ACK / SOCKS5 CONNECT succeeded.

## Architectural constraint
Do **not** solve this by simply changing the bounded channel to an unbounded channel or blindly increasing queue sizes. The correct solution must preserve:
- lossless data delivery;
- explicit backpressure;
- control-plane progress (SYN/ACK/FIN/RST/error) that cannot be starved by bulk data;
- deterministic stream lifecycle ownership.

Likely direction if the hypothesis is confirmed: a RushWay-native stream inbox/dispatch design separating control-plane progress from data backpressure, with explicit ownership and observable pressure.

## Stream lifecycle target
`NEW → ADMITTED → SYN_SENT → DIALING → ESTABLISHED → HALF_CLOSED_LOCAL / HALF_CLOSED_REMOTE → CLOSING → CLOSED`

Invariants:
- one final cleanup owner;
- every admitted stream reaches CLOSED or explicit terminal failure;
- reservation/registration cannot race;
- FIN/RST during DIALING cancels dial;
- terminal event does not delete state before owner consumes it;
- physical session shutdown terminates child streams deterministically.

## Validation ladder
After each meaningful fix:
1. `cargo fmt -- --check`
2. `cargo check --all-targets`
3. unit tests
4. release build
5. single WS/WSS/QUIC E2E
6. blocking 1/100/500 WS/WSS/QUIC
7. diagnostic/non-blocking 1000

Only after 1000 is stable:
- 2000 streams;
- 10×1000 sustained;
- physical MUX session kill/recovery;
- deliberate backpressure limits;
- FIN/RST/cancel in SYN/DIALING/ESTABLISHED/half-close;
- final GoWay bidirectional interoperability;
- release hardening.

## Current status
**IN PROGRESS — 1000-stream MUX reader/lifecycle investigation.**

## Immediate next action
1. Inspect current `main` SHA and investigation branch state.
2. Inspect `src/mux_pool.rs` `client_reader_loop()` and relevant `src/runtime.rs` MUX handling.
3. Add minimal phase-aware instrumentation or a targeted dispatch design test.
4. Run CI on the investigation branch.
5. Use the evidence to choose the smallest architecture-correct fix.
6. Update `AI_HANDOFF.md` in the same engineering cycle.
7. When a reviewable change is ready, create a PR and tell the user exactly which PR to merge.

## User interaction rule
The user does not need to do anything while investigation is in progress. Do not ask them to modify code manually. When user opens a new chat, resume from this file and continue the work.
