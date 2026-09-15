# RushWay Final AI Relay — 2026-09-15

## Release closure

- Target: RushWay `v0.0.3`.
- GoWay compatibility baseline: v1.8.4 at `538dbee86b9fbf248a68c8c6d8eee5d6f8bdb0dc`.
- Lifecycle fix source commit: `ad2eff5b759212d7ab0383e0e075fdce6440ad87`.
- PR #22 merged to main as `2503617e581e4465230c1a8be98ee0da5875453b`.
- Final integrated CI Run #429: `34949750548` — success.
- Final release metadata commit before tag: `d53533de0bd5b8d6fa182bd333b2d4ef701e2cab`.
- Tag: `v0.0.3` points to `d53533de0bd5b8d6fa182bd333b2d4ef701e2cab`.
- Formal GitHub Release: `RushWay v0.0.3`, published, non-draft, non-prerelease.
- Release workflow Run #1: `34950354357` — success.

## Final engineering change

`src/runtime.rs` now gives every logical MUX stream an explicit cancellation signal using Tokio `watch`. DIALING selects on cancellation and re-checks cancellation before the established success path. RST no longer removes the shared stream-table entry directly; the logical stream task owns the final table removal path. Focused lifecycle tests cover immediate cancellation wake-up and cancellation precedence over a pending dial.

## Final automated acceptance

- Rust fmt/check/test/release: pass.
- Full native test suite: pass.
- Standalone WS/WSS/QUIC E2E: pass.
- WS 1/100/500 + 1000 diagnostic: pass.
- WSS 1/100/500 + 1000 diagnostic: pass.
- QUIC 1/100/500 + 1000 diagnostic: pass.
- Cross-proxy and repeated benchmark medians: pass.
- Windows x64: pass.
- Debian 12 x64: pass.
- ARMv7 Linux: pass.
- GoWay v1.8.4 exact checkout/build/comparison: pass.

## Important evidence boundary

Run #429 validates the automated source/build/stress/benchmark scope listed above. It does not contain a dedicated OpenWrt/real-device smoke step or a true bidirectional WS/WSS/QUIC TCP+UDP interoperability matrix. Those must remain unclaimed until separate executable/device evidence exists.

## Issue/PR state

- Issue #20: closed as completed.
- PR #22: merged.
- Formal release: complete.
- No new source blocker remains for the released v0.0.3 revision.

## Next action for the next AI

Treat `v0.0.3` as the released baseline. Any future change must start a new engineering cycle, use a new branch/PR, and preserve the three-file relay discipline. Do not rewrite release evidence; append new findings as a new dated relay entry.
