# RushWay AI Handoff Addendum — 2026-09-15

## PR #19 — Go-style native MUX Session Manager lifecycle

- PR: #19 — `refactor: align plain-WS MUX session manager with Go-style lifecycle`
- Merged commit: `1c891293905581eae12b8a2b783b47968dfd3b54`
- Feature commit: `148a1df2e03ef6dca28b0c6489d5a066432335cb`
- Validation run: GitHub Actions #421, `34941759404`
- Merge ref tested by Actions: `82383844504159c3ee1c46e4e1b877725674968f`

### Bug / engineering problem
Plain-WS pooled client startup coupled local listener readiness to synchronous upstream physical MUX session prewarm. High configured session counts therefore made service readiness dependent on upstream session establishment.

### Root cause
`src/mux_pool.rs::MuxSessionPool::prewarm()` created the full configured pool before the local proxy listener was bound. The WSS path had already exhibited the same lifecycle class of failure. This was a startup lifecycle/scheduling defect, not a MUX wire-format defect.

### Astra review
- Physical session creation remains serialized.
- Closed sessions are pruned.
- Session occupancy is snapshotted before selection.
- Listener readiness is independent of upstream warmup.
- Existing MUX wire protocol, 4-session default, configurable 1..64 sessions, 256 streams/session, and target-policy enforcement remain unchanged.
- Stream lifecycle/cancellation, deliberate backpressure, physical-session failure recovery, TCP+UDP matrix, device smoke, and GoWay interoperability remain separate gates.

### Validation
- Rust fmt/check/release: pass.
- Unit tests: 39/39 pass.
- WS/WSS/QUIC standalone E2E: pass.
- WS 1/100/500/1000 staged: 1000/1000 pass.
- WSS 1/100/500/1000 staged: 1000/1000 pass.
- QUIC 1/100/500/1000 staged: 1000/1000 pass.
- Repeated setup-inclusive and steady-state benchmarks: pass.
- MUX decode benchmark: `owned_vs_copy_speedup=578.90x`.
- Windows x64 / Debian 12 x64 / ARMv7: pass.
- GoWay comparison: skipped because `WAY_READ_TOKEN` is absent; this is not interoperability evidence.

### Status
**Fixed and accepted for this native session-manager phase.** PR #19 is merged into `main`.

### Remaining release risks
1. Sustained 10x1000 without restart is not yet evidenced.
2. 2000-stream dynamic scaling is not yet evidenced.
3. Physical-session failure/recovery under load is not yet evidenced.
4. FIN/RST/cancel during DIALING and explicit single-owner stream cleanup need executable coverage.
5. Deliberate global/session/stream backpressure tests need executable coverage.
6. WS/WSS/QUIC TCP+UDP interoperability matrix remains open.
7. OpenWrt/real-device smoke remains open.
8. Actual cloud GoWay v1.8.4 bidirectional interoperability remains open.
9. Final v0.0.3 tag/release verification remains blocked.

## Next action
Proceed with the next native phase: make logical Stream lifecycle ownership and cancellation explicit, especially FIN/RST/cancel during DIALING and deterministic cleanup on physical-session shutdown. Do not use GoWay as the architectural design authority; keep GoWay v1.8.4 as a separate late compatibility gate.

## 2026-09-15 — v0.0.3 final lifecycle closure

### Bug
`src/runtime.rs::handle_mux_parts()` represented logical stream state only by a command sender, and the physical MUX reader removed RST streams directly before notifying the logical stream task. During DIALING, cancellation therefore had no dedicated lifecycle signal and was coupled to bounded command-queue delivery.

### Root cause
The lifecycle had multiple cleanup paths and no explicit cancellation ownership boundary. An RST could delete the shared stream-table entry before the task that owned the actual dial/forwarding work had observed the cancellation. The existing `rx.recv()` branch was not an explicit cancellation primitive for a potentially long-running dial.

### Astra review
- Concurrency: physical MUX reader remains an I/O dispatcher; target dials stay in independent tasks.
- Lifecycle: each logical stream now carries an owned cancellation signal; RST no longer removes the shared entry directly.
- Cancellation: DIALING selects on the cancellation signal and re-checks cancellation after dial completion before emitting the established success ACK.
- Protocol: MUX wire format, encrypted zero-length DATA success ACK, target policy, 256 streams/session and existing WS/WSS/QUIC paths are preserved.
- Recovery: DIALING cancellation removes the logical stream entry from the stream task; physical-session shutdown still aborts child tasks and clears the table.
- Regression: focused lifecycle tests were added without changing transport framing.

### Change
- `src/runtime.rs` adds `tokio::sync::watch` cancellation to `StreamEntry`.
- FIN/RST clone the stream sender plus cancellation sender; RST no longer performs table removal itself.
- DIALING listens to `cancelled.changed()` and performs final stream-table cleanup in the logical stream task.
- Added executable tests:
  - `dialing_cancel_signal_wakes_immediately`
  - `cancellation_precedes_slow_dial`
- Source fix commit: `ad2eff5b759212d7ab0383e0e075fdce6440ad87` on `fix/stream-lifecycle-v003`.
- PR #22: `fix: make MUX stream cancellation explicit for v0.0.3`.
- PR #22 merged to `main` as `2503617e581e4465230c1a8be98ee0da5875453b`.

### Final validation
- Final integrated CI Run #429: `34949750548` — **success**.
- Rust format/check/test/release: pass.
- Full native test suite: pass.
- WS/WSS/QUIC standalone E2E: pass.
- WS 1/100/500 and 1000 diagnostic: pass.
- WSS 1/100/500 and 1000 diagnostic: pass.
- QUIC 1/100/500 and 1000 diagnostic: pass.
- Cross-proxy and repeated benchmark medians: pass.
- Linux x64 release artifact: pass.
- Windows x64: pass.
- Debian 12 x64: pass.
- ARMv7 Linux: pass.
- GoWay v1.8.4 exact checkout at `538dbee86b9fbf248a68c8c6d8eee5d6f8bdb0dc`: checkout pass, build pass, repeated comparison pass, steady-state comparison pass.
- GoWay setup-inclusive medians: c1 `265.76`, c8 `370.99`, c32 `475.67` MiB/s.
- GoWay steady-state medians: c1 `335.77`, c8 `328.98`, c32 `466.01` MiB/s.

### Status
**v0.0.3 code/test-track closure accepted for the validated CI scope.** Issue #20 is closed as completed. PR #22 is merged. Native 1000-stream regressions are green on the final lifecycle revision.

### Release note
The final CI workflow proves the repository's automated Rust/native, cross-platform build, and GoWay benchmark-comparison gates. It does not provide evidence for manual OpenWrt/real-device smoke or a true bidirectional WS/WSS/QUIC TCP+UDP interoperability matrix; those are not represented by separate CI steps in Run #429 and must not be described as tested when they were not.

### Next action
Create and verify the formal `v0.0.3` tag/GitHub Release against main commit `2503617e581e4465230c1a8be98ee0da5875453b`, then leave the relay state at release-complete with no open PRs.
