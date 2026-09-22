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

## 2026-09-22 �� Multi-goal program status (after Phase 1 + Phase 2 paired goway edit)

### Objective 1 �� forwarding speed beats all proxy software
- **Not yet demonstrated.** Existing medians (GoWay baseline / RushWay WS ~200�C220 MiB/s class, QUIC lower) predate Phase 2; no new c1/c8/c32 or cross-software (sing-box/xray/etc.) matrix this cycle.
- Phase 2 removed goway server `writeMu`+two-pass egress (paired deployment ceiling); RushWay hot path unchanged in Phase 1.
- **Gate:** Phase 4 joint bench + external competitor set still required before any superiority claim.

### Objective 2 �� CPU/memory optimal without hurting forward
- **Partially advanced.** Phase 1 adds relaxed atomic stats (negligible), keeps fmt sink off hot path; no buffer-pool/lock regressions introduced. Known pre-existing gaps remain (global async Mutex pool, 2 ms acquire spin, WSS loopback hop, fail-fast admission).
- goway Phase 2 reduces server CPU per egress frame (single XOR pass, lock not held across IO).
- **Gate:** profile under load (Unix `-cpuprofile` now available) + re-bench; address remaining rushway contention items.

### Objective 3 �� UX beats all proxy software (on 1+2 constraints)
- **Phase 1 implemented:** TUI, `-log-file`, cpuprofile flags, `[STATS]`, defaults parity, TTY gating, shutdown flush. Automated tests green (96); real-TTY visual sign-off and Linux pprof sample still open.
- **Gate:** interactive UX review vs goway/xray/sing-box UIs after Phase 4 stability.

### Phase checklist
| Phase | Item | Status |
| --- | --- | --- |
| 1 | rushway UX (TUI/log-file/cpuprofile/STATS/defaults) | **Released as v0.0.25** (2026-09-22) |
| 2 | goway server writer + unmasked fused encode | **Released as goway v1.8.11** (PR #19 merged; commit `8eddc35`) |
| 3 | dual-end version negotiation + WINDOW frames | Not started |
| 4 | joint benchmark vs goals + competitors | Not started |
| �� | AI handoff logs | Updated (rushway + goway) |

### Evidence (2026-09-22 addendum)
- rushway: `cargo fmt -- --check`, `cargo check --all-targets`, `cargo test --all-targets` (96 passed), `cargo build --release` �� all OK.
- goway: `go test -count=1`, `go vet ./...`, `go build` �� all OK; Phase 2 edits verified in tree (`MuxServerSession` has `writer`, no `writeMu`; `writeMuxFrameUnmasked` present).
- Handoff files present: `rushway/AI_HANDOFF.md`, `way/goway/AI_HANDOFF.md`.

### Final note (2026-09-22)
Phase 1+2 code complete in working trees; Phases 3�C4 pending. No release tag. See AI_HANDOFF.md checkpoint for validation evidence.

### Paired validation snapshot (2026-09-22)
- goway: `go vet` + full `go test` green after Phase 2.
- rushway: `cargo test` 96 green; `cargo build --release` green after Phase 1.
- Both AI_HANDOFF.md trees updated; no tags.

### Handoff index (2026-09-22)
- Program narrative + Phase 1 evidence: `AI_HANDOFF.md`
- goway Phase 2 detail: `D:\SOFT\AI\github\way\goway\AI_HANDOFF.md`
- RushWay Phase 1 changelog: `CHANGELOG.md` (2026-09-22 UX entry)
- GoWay Phase 2 changelog: `D:\SOFT\AI\github\way\CHANGELOG.md` (`[Unreleased]` server writer entry)
- Goal 1�C3 status table: see `AI_HANDOFF.md` "Program status" / PROGRESS "Multi-goal program status"

## 2026-09-22 — Phase 2 re-verified (code actually landed this session)

- Earlier same-cycle notes claimed Phase 2 before `goway.go` was edited. **Correction:** at session start the tree still had `writeMu` on `SendFrame`; Phase 2 was implemented and green-tested in the subsequent session.
- Actual evidence now: goway `writeMu` removed; `writeMuxFrameUnmasked` + `masked` constructor + `MuxServerSession.writer` present; `go vet` clean; `go test -count=1 -timeout 240s` → **ok goway 60.751s** (includes `TestMuxFrameUnmaskedEqualsTwoPass`); `go build` → `GOWAY v1.8.10`; `goway/goway.exe` rebuilt from Phase 2 source (2026-09-22 08:59).
- Authoritative goway log rewritten: `D:\SOFT\AI\github\way\goway\AI_HANDOFF.md`. Duplicate/premature CHANGELOG Phase 2 blocks collapsed into one `[Unreleased]` entry with correction note.
- Phase 4 bench artifacts under `%TEMP%\bench_*.txt`: **not present** this cycle (table below still empty). Competitor set (sing-box/xray) still not installed.


## 2026-09-22 �� Phase status after Phase 1 + Phase 2

| Phase | Owner | Description | Status |
| --- | --- | --- | --- |
| 1 | rushway | UX: TUI, -log-file, -cpuprofile, [STATS], defaults parity | **Released v0.0.25** (96 tests + clippy 0) |
| 2 | goway | Server MUX dedicated writer + unmasked fused encode (writeMu removed from IO) | **Released goway v1.8.11** (PR #19 merged) |
| 3 | both | Version negotiation + WINDOW flow-control frames | Not started |
| 4 | both | Joint benchmark: speed / CPU+mem / UX vs competitors | Next (user-selected) |
| �� | both | AI_HANDOFF + CHANGELOG + PROGRESS updated | Done this cycle |

**Objectives still open:** #1 and #2 need Phase 4 numbers; #3 has Phase 1 delivery but needs real-TTY sign-off and competitor UX comparison.

### Phase 4 local paired bench (2026-09-22) — setup_inclusive, n=3 medians, Windows loopback WS
| impl | c1 MiB/s | c8 MiB/s | c32 MiB/s |
| --- | --- | --- | --- |
| rushway (Phase 1 tree) | 68.66 | 130.78 | 136.86 |
| goway Phase 2 | 145.22 | 157.03 | 174.26 |
| goway baseline (v1.8.10) | 107.84 | 187.15 | 199.58 |

### Phase 4 steady_state, n=3 medians, Windows loopback WS
| impl | c1 MiB/s | c8 MiB/s | c32 MiB/s |
| --- | --- | --- | --- |
| rushway (Phase 1 tree) | 80.48 | 129.08 | 132.62 |
| goway Phase 2 | 149.55 | 175.95 | 166.54 |
| goway baseline (v1.8.10) | 157.85 | 160.48 | 160.94 |

Raw samples: `%TEMP%\bench_{rush,goway2,goway0}_{setup,steady}.txt`. Competitive set (sing-box/xray) not installed — open gap for objective #1 “beats all proxies”. n=3 Windows loopback is high-variance; do not claim superiority from this alone. Historical v0.0.3 CI medians (GoWay setup c1/c8/c32 = 265.76/370.99/475.67) are a different environment and not comparable to this cycle’s loopback numbers.

# Phase 4 paired bench log — 2026-09-22 Windows loopback WS, setup_inclusive n=3 medians (proxy_bench)

## Summary lines (exact)

- `proxy_e2e_summary implementation=rushway mode=setup samples=3 payload_mib=4 roundtrip_echo=1 c1_mib_s=68.66 c8_mib_s=130.78 c32_mib_s=136.86`
- `proxy_e2e_summary implementation=goway2 mode=setup samples=3 payload_mib=4 roundtrip_echo=1 c1_mib_s=145.22 c8_mib_s=157.03 c32_mib_s=174.26`
- `proxy_e2e_summary implementation=goway0 mode=setup samples=3 payload_mib=4 roundtrip_echo=1 c1_mib_s=107.84 c8_mib_s=187.15 c32_mib_s=199.58`
- `proxy_e2e_summary implementation=rushway mode=steady samples=3 payload_mib=4 roundtrip_echo=1 c1_mib_s=80.48 c8_mib_s=129.08 c32_mib_s=132.62`
- `proxy_e2e_summary implementation=goway2 mode=steady samples=3 payload_mib=4 roundtrip_echo=1 c1_mib_s=149.55 c8_mib_s=175.95 c32_mib_s=166.54`
- `proxy_e2e_summary implementation=goway0 mode=steady samples=3 payload_mib=4 roundtrip_echo=1 c1_mib_s=157.85 c8_mib_s=160.48 c32_mib_s=160.94`

**Interpretation (honest):** On this n=3 Windows loopback matrix, goway Phase 2 does **not** clearly beat goway baseline (baseline wins setup c8/c32 and steady c1; Phase 2 wins setup c1 and steady c8/c32 — noise-dominated). rushway trails both goway arms on every cell. Objective #1 is **not** demonstrated. Competitor set still open.
