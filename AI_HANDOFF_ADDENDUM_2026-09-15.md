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
