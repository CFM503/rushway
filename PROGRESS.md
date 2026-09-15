# RushWay v0.0.3 — Continuous Handoff Progress

> Compatibility baseline: GoWay v1.8.4 at `538dbee86b9fbf248a68c8c6d8eee5d6f8bdb0dc`.

## Current checkpoint — 2026-09-15

**v0.0.3 is formally released.** The lifecycle fix is merged, final integrated CI is green, the release tag and GitHub Release are published, and the final AI relay record is stored in `AI_HANDOFF_FINAL_2026-09-15.md`.

### Final lifecycle fix

- PR #22: `fix: make MUX stream cancellation explicit for v0.0.3`.
- Source commit: `ad2eff5b759212d7ab0383e0e075fdce6440ad87`.
- Merge commit: `2503617e581e4465230c1a8be98ee0da5875453b`.
- `src/runtime.rs` adds explicit per-stream `watch` cancellation.
- DIALING selects on cancellation and re-checks cancellation before the established success path.
- RST no longer removes the shared stream-table entry directly; logical stream work owns final removal.
- Focused lifecycle tests: `dialing_cancel_signal_wakes_immediately`, `cancellation_precedes_slow_dial`.
- Issue #20: closed as completed.

### Final integrated validation

- GitHub Actions Run #429: `34949750548` — **success**.
- Rust fmt/check/test/release: pass.
- Full native test suite: pass.
- WS/WSS/QUIC standalone E2E: pass.
- WS 1/100/500 and 1000 diagnostic: pass.
- WSS 1/100/500 and 1000 diagnostic: pass.
- QUIC 1/100/500 and 1000 diagnostic: pass.
- Cross-proxy and repeated benchmark medians: pass.
- Windows x64: pass.
- Debian 12 x64: pass.
- ARMv7 Linux: pass.
- GoWay v1.8.4 exact checkout/build/repeated comparison: pass.
- GoWay setup-inclusive medians: c1 `265.76`, c8 `370.99`, c32 `475.67` MiB/s.
- GoWay steady-state medians: c1 `335.77`, c8 `328.98`, c32 `466.01` MiB/s.

### Formal release

- Tag: `v0.0.3`.
- Tag target: `d53533de0bd5b8d6fa182bd333b2d4ef701e2cab`.
- GitHub Release: `RushWay v0.0.3`.
- Draft: false.
- Prerelease: false.
- Release workflow Run #1: `34950354357` — success.

### Evidence boundary

The automated CI/release evidence above does not include a dedicated OpenWrt/real-device smoke step or a true bidirectional WS/WSS/QUIC TCP+UDP interoperability matrix. Those remain outside the released automation evidence and are not claimed as completed by Run #429.

### AI relay rule

Every new AI must read `AI_HANDOFF.md`, `PROGRESS.md`, `SPEC.md`, and the final dated relay record before changing release-line behavior. New work must start a new branch/PR and preserve existing release evidence.

### Final state

- PR #22 merged.
- Issue #20 completed.
- `v0.0.3` formally released.
- No release-blocking source issue remains in the validated automated scope.
