# RushWay AI Handoff — 2026-09-15 Continuation

> This is a canonical continuation record to be merged alongside `AI_HANDOFF.md`. It exists so a new AI conversation can resume from the latest verified state without repeating completed work.

## Verified state

- Repository: `CFM503/rushway`
- Main baseline: `b2df3980a1d35b54107e9322f0680be40054aa89`
- PR #6 (500-stream CI acceptance gate): merged.
- Run #369: **success**. This validates the merged 500-stream blocking baseline.
- Current blocking CI gate: 1 / 100 / 500 streams.
- 1000 streams remain a **non-blocking diagnostic target** and are not fixed.
- Do not declare v0.0.3 final.

## Completed work — do not redo

- RushWay CLI compatibility alignment, including legacy `rushway -help` normalization.
- Physical MUX reader was changed away from synchronously awaiting per-stream target dialing.
- MUX stream admission was made atomic to remove the check-then-insert race.
- Remote FIN lifecycle cleanup was added so logical streams are not left registered after half-close.
- CI stress runner FD limit was raised to `8192`; the remaining 1000-stream failure persisted, so FD exhaustion is not the remaining root cause.
- 500-stream stress is now the blocking acceptance gate.
- Run #369 is green after the gate merge.

## Unresolved 1000-stream failure

Observed repeatedly around flow ~410:

`SOCKS5 stage connect_reply_read timed out after 10s`

The flow has already completed TCP connect, SOCKS5 greeting/method negotiation, and CONNECT request write. It is waiting for the final SOCKS5 success, which depends on the MUX SYN → target dial → success ACK → client reader dispatch path.

## Current leading hypothesis — NOT PROVEN

`src/mux_pool.rs::client_reader_loop()` awaits `tx.send(frame)` on a bounded per-stream MPSC channel with capacity 32.

If one stream's receiver becomes slow and its channel fills, the single physical MUX reader can block. That can prevent it from dispatching frames for unrelated streams, including SYN success ACKs, matching the repeated ~400-flow timeout pattern.

**Do not replace this with an unbounded channel merely to make CI green.** That would risk unbounded memory growth and violate the RushWay-native backpressure design.

## Other open hypotheses

1. Shared physical MUX writer mutex contention between ACK/data/lifecycle frames.
2. Stream lifecycle or active-accounting race after FIN/RST/dial failure/session shutdown.
3. Polling-based pooled backpressure (`sleep(2ms)`) amplifying load instead of using event-driven notification.

## Required next engineering action

Continue on an isolated branch; preserve `main` as the known-good 500-stream baseline.

1. Inspect `src/mux_pool.rs::client_reader_loop()`.
2. Inspect `src/runtime.rs::handle_mux_parts()`.
3. Inspect MUX control/data semantics in `src/protocol.rs`.
4. Add low-noise phase evidence for:
   - SYN sent/received
   - target dial started/succeeded/failed
   - ACK queued/written
   - ACK received
   - SOCKS CONNECT success
5. Reproduce 1000 streams.
6. Determine exactly where the ACK path stops.
7. Make the smallest safe production fix supported by evidence.
8. Validate with GitHub Actions.
9. Update the handoff log with bug/root cause/Astra review/change/commit/validation/status/risk/next action.

## RushWay-native architecture rules

RushWay is not being redesigned as a GoWay clone. GoWay v1.8.4 interoperability is a late compatibility gate.

Required stream lifecycle:

`NEW → ADMITTED → SYN_SENT → DIALING → ESTABLISHED → HALF_CLOSED_LOCAL/HALF_CLOSED_REMOTE → CLOSING → CLOSED`

Key invariants:

- exactly one owner performs final stream cleanup;
- every admitted stream reaches CLOSED or explicit terminal failure;
- stream reservation and registration cannot race;
- FIN/RST during DIALING cancels or invalidates the dial task;
- terminal events do not delete state before the owner consumes them;
- physical session shutdown deterministically terminates child streams.

Future native stress ladder:

1. WS/WSS/QUIC 1000
2. 10 × 1000 sustained
3. 2000 with dynamic session scaling
4. physical session failure/recovery under load
5. deliberate backpressure limits
6. FIN/RST/cancel lifecycle matrix
7. GoWay interoperability only after native gates pass

## New-chat contract

A new AI must read:

- `AI_HANDOFF.md`
- `AI_HANDOFF_2026-09-15_CONTINUATION.md`
- `PROGRESS.md`
- `SPEC.md`

Then continue directly from the **1000-stream MUX reader/SYN-ACK investigation**. Do not redo the 500-stream gate work and do not restart the project plan.
