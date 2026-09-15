# RushWay v0.0.3 — Continuous Handoff Progress

> Compatibility baseline: GoWay v1.8.4 at `538dbee86b9fbf248a68c8c6d8eee5d6f8bdb0dc`.

## Current checkpoint — 2026-09-15

**v0.0.3 engineering closure reached for the validated automated CI scope.** PR #22 is merged, the explicit MUX stream lifecycle/cancellation fix is on `main`, and final CI Run #429 is green. Formal tag/release creation remains the last repository operation.

### Final lifecycle fix

- PR #22: `fix: make MUX stream cancellation explicit for v0.0.3`.
- Source commit: `ad2eff5b759212d7ab0383e0e075fdce6440ad87`.
- Merge commit on `main`: `2503617e581e4465230c1a8be98ee0da5875453b`.
- `src/runtime.rs::StreamEntry` now carries an explicit `watch` cancellation sender.
- DIALING selects on cancellation and re-checks cancellation before establishing the stream success path.
- RST no longer removes the stream-table entry directly; logical stream work owns final removal.
- Focused tests: `dialing_cancel_signal_wakes_immediately`, `cancellation_precedes_slow_dial`.
- Issue #20 is closed as completed.

### Final integrated validation

- GitHub Actions Run #429: `34949750548` — **success**.
- Rust fmt/check/test/release: pass.
- Full native test suite: pass.
- WS/WSS/QUIC standalone E2E: pass.
- WS 1/100/500 and 1000 diagnostic: pass.
- WSS 1/100/500 and 1000 diagnostic: pass.
- QUIC 1/100/500 and 1000 diagnostic: pass.
- Cross-proxy benchmark and repeated benchmark medians: pass.
- Windows x64: pass.
- Debian 12 x64: pass.
- ARMv7 Linux: pass.
- GoWay v1.8.4 exact checkout/build/comparison: pass.
- GoWay setup-inclusive medians: c1 `265.76`, c8 `370.99`, c32 `475.67` MiB/s.
- GoWay steady-state medians: c1 `335.77`, c8 `328.98`, c32 `466.01` MiB/s.

### Release boundary

The repository CI proves the automated source/build/stress/GoWay benchmark-comparison scope above. The workflow does **not** contain dedicated evidence for manual OpenWrt/real-device smoke or a true bidirectional WS/WSS/QUIC TCP+UDP interoperability matrix, so those are not claimed as completed by CI.

### AI relay rule

Every new AI must begin by reading `AI_HANDOFF.md`, `PROGRESS.md`, and `SPEC.md`, then inspect the newest CI run before modifying protocol behavior. Record exact run IDs, commit SHAs, test output and blockers. A skipped GoWay job must never be reported as successful interoperability.

### Three-file relay contract

1. `AI_HANDOFF.md` — chronological engineering decisions and next action.
2. `PROGRESS.md` — compact dashboard.
3. `SPEC.md` — source-derived GoWay compatibility contract.

### Final state

- PR #22 merged.
- Issue #20 completed.
- Final validated source merge: `2503617e581e4465230c1a8be98ee0da5875453b`.
- Formal `v0.0.3` tag/GitHub Release: pending the final repository release operation.
