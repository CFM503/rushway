# RushWay AI Relay — New Chat Checkpoint

> Read this file first when a new AI conversation starts. Continue from the last state; do not re-plan completed work.

## Current repository state
- Repository: `CFM503/rushway`
- Main branch: `main`
- Main merge baseline: `b2df3980a1d35b54107e9322f0680be40054aa89`
- Run #369: **SUCCESS**; push-triggered `RushWay CI`; main is green.
- Current blocking acceptance gate: **500 concurrent streams** across the existing staged stress path.
- 1000 concurrent streams: **NOT FIXED**; remains the active reliability target.

## What has already been completed
- RushWay CLI compatibility work was completed, including legacy `-help` normalization.
- MUX SYN processing was changed from synchronous target dialing in the physical MUX reader to independent per-stream async dialing.
- Client-side MUX stream-slot admission was corrected to use atomic capacity reservation.
- FIN/remote-close cleanup work was added to the TCP proxy/MUX lifecycle.
- Phase-aware SOCKS5 stress diagnostics were added to `src/bin/e2e_bench.rs`.
- CI stress file-descriptor limit was raised to `8192`; FD exhaustion was tested and rejected as the root cause.
- PR #6 changed the blocking stress gate from `1/100/500/1000` to `1/100/500` while retaining 1000 as a non-blocking diagnostic target.
- PR #6 was merged to main.
- Run #369 subsequently completed successfully.

## 1000-stream failure evidence
Repeated WS 1000 failures occur around flow ~407–413.
The latest diagnostic phase was:
`SOCKS5 stage connect_reply_read timed out after 10s`

Before that timeout, the flow had completed:
1. TCP connect
2. SOCKS5 greeting write
3. method response read
4. CONNECT request write

Therefore the stall is after SOCKS5 CONNECT is sent and before the final success reply reaches the client.

`ulimit -n 8192` did not solve the failure, so the original FD-limit hypothesis is rejected.

## Current leading hypothesis
`src/mux_pool.rs::client_reader_loop()` currently obtains the per-stream bounded MPSC sender and performs:
`tx.send(frame).await`

The channel capacity is 32. Because the physical MUX reader is a single dispatcher, a slow/full stream receiver can make the physical reader await indefinitely. That can prevent ACK/control frames for other streams from being dispatched and matches the repeated ~400 boundary plus `connect_reply_read` timeout.

This is a **hypothesis, not yet proven**.

## Competing hypotheses
- Shared physical MUX writer mutex contention.
- Stream active-count/lifecycle cleanup race.
- Stale stream state after FIN/RST or physical-session shutdown.
- Session-pool backpressure polling using `sleep(2ms)` amplifying scheduling pressure.

## Required next diagnostic evidence
For a failing flow near the boundary, establish the first missing phase:
1. client SYN sent
2. server SYN received/admitted
3. server dial started
4. server dial succeeded/failed
5. server ACK queued/written
6. client physical reader received ACK
7. client stream owner consumed ACK / SOCKS5 CONNECT succeeded

Do not claim 1000 fixed until executable CI evidence proves it.

## Architecture constraints
RushWay is **not a GoWay port**. RushWay should be internally coherent with its own runtime contracts. GoWay v1.8.4 interoperability is a final compatibility gate, not the architecture authority.

Target architecture:
- Connection Manager
- Session Manager
- Stream Manager
- Backpressure Layer
- Transport Adapter (WS/WSS/QUIC)

Stream lifecycle:
`NEW → ADMITTED → SYN_SENT → DIALING → ESTABLISHED → HALF_CLOSED_LOCAL / HALF_CLOSED_REMOTE → CLOSING → CLOSED`

Required invariants:
- exactly one owner performs final stream cleanup;
- every admitted stream reaches CLOSED or explicit terminal failure;
- stream reservation and registration cannot race;
- FIN/RST during DIALING cancels/invalidate dial work immediately;
- terminal events do not delete state before the owner consumes them;
- physical session shutdown deterministically terminates child streams.

Backpressure must be explicit and observable. Do not replace bounded data flow with an unbounded channel merely to make 1000 green.

## Current development branch
- Investigation branch: `ai/mux-reader-1000-diagnostics`
- Purpose: isolate/fix the physical-MUX reader dispatch bottleneck and validate 1000-stream reliability.
- Keep `main`'s 500-stream green gate intact until a reviewable fix exists.

## Validation ladder
1. `cargo fmt -- --check`
2. `cargo check --all-targets`
3. unit tests
4. release build
5. single WS/WSS/QUIC E2E
6. blocking staged 1/100/500 WS/WSS/QUIC
7. diagnostic/non-blocking 1000
8. after 1000 is stable: 2000 streams, 10×1000 sustained, physical-session kill/recovery, deliberate backpressure, FIN/RST/cancel stress
9. final GoWay bidirectional interoperability
10. release hardening and final v0.0.3 tag

## Astra engineering loop
For every meaningful bug:
1. decompose observable failure phases;
2. inspect implementation/reference behavior;
3. perform concurrency/lifecycle/protocol/security/performance review;
4. implement the smallest architecture-correct fix;
5. validate through GitHub Actions;
6. record evidence in `AI_HANDOFF.md` in the same engineering cycle;
7. continue to the next root-cause layer.

Never treat a green partial test as proof of full completion.

## New-chat instruction
When a new conversation begins, read this file and the repository's `AI_HANDOFF.md`, `PROGRESS.md`, and `SPEC.md`. Continue from the 1000-stream investigation point above. Do not ask the user to repeat the project history. First inspect the current `main` SHA and investigation branch, then inspect `src/mux_pool.rs` and `src/runtime.rs`, reproduce/diagnose the reader-dispatch hypothesis, and proceed with the smallest correct fix.

## User state
The user may open another conversation at any time. They expect autonomous continuation and do not want to be asked to redo already completed planning. When a fix is ready, create a reviewable PR and clearly tell the user to merge it. After merge, verify the resulting main CI before proceeding.
