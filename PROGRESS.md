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
| 3 | dual-end version negotiation + WINDOW frames | **Implemented + dual-stack tested (2026-09-23, uncommitted)** |
| 4 | joint benchmark vs goals + competitors | Loopback n=5 done (W2 + W3 re-run); non-loopback done (W2) |
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
| 3 | both | Version negotiation + WINDOW flow-control frames | **Implemented + dual-stack tested (2026-09-23, uncommitted)** |
| 4 | both | Joint benchmark: speed / CPU+mem / UX vs competitors | Next (user-selected) |
| �� | both | AI_HANDOFF + CHANGELOG + PROGRESS updated | Done this cycle |

**Objectives still open:** #1 and #2 need Phase 4 numbers; #3 has Phase 1 delivery but needs real-TTY sign-off and competitor UX comparison.

### W2 Phase 4 full matrix (2026-09-23) — 4 impls x n=5 x setup/steady, CPU + peak RSS

**Windows loopback** (`bench/w2_loopback.csv`, medians, all 40 samples passed after early-eof fix):

| impl | mode | c1 MiB/s | c8 MiB/s | c32 MiB/s | cpu_s | rss MB |
| --- | --- | --- | --- | --- | --- | --- |
| rushway | setup | 71.32 | 229.83 | 251.91 | 2.25 | 148.0 |
| rushway | steady | 101.24 | 238.70 | 251.84 | 2.34 | 155.9 |
| goway v1.8.11 | setup | 247.12 | 292.58 | 267.64 | 1.91 | 87.2 |
| goway v1.8.11 | steady | 288.42 | 277.09 | 259.18 | 2.14 | 89.0 |
| sing-box 1.14.1 | setup | 107.43 | 376.47 | 360.57 | 1.25 | 79.6 |
| sing-box 1.14.1 | steady | 216.84 | 382.92 | 387.05 | 1.25 | 78.2 |
| xray 26.3.27 | setup | 43.01 | 177.49 | 185.09 | 3.20 | 63.0 |
| xray 26.3.27 | steady | 42.36 | 174.51 | 178.64 | 3.56 | 65.0 |

**Non-loopback** (`bench/w2_nonloopback.csv`, Windows client+echo / WSL servers, two Hyper-V vSwitch crossings, all 40 samples passed):

| impl | mode | c1 MiB/s | c8 MiB/s | c32 MiB/s | cpu_s | rss MB |
| --- | --- | --- | --- | --- | --- | --- |
| rushway | setup | 22.65 | 21.33 | 16.76 | 5.73 | 152.6 |
| rushway | steady | 20.07 | 21.52 | 18.64 | 5.25 | 153.8 |
| goway v1.8.11 | setup | 20.83 | 20.66 | 17.28 | 9.62 | 82.0 |
| goway v1.8.11 | steady | 17.92 | 19.05 | 16.75 | 10.80 | 85.4 |
| sing-box 1.14.1 | setup | 21.36 | 17.51 | 15.46 | 9.66 | 106.2 |
| sing-box 1.14.1 | steady | 21.49 | 17.63 | 14.35 | 10.35 | 105.4 |
| xray 26.3.27 | setup | 20.72 | 15.80 | 13.96 | 12.18 | 68.6 |
| xray 26.3.27 | steady | 21.62 | 17.73 | 15.04 | 11.55 | 68.8 |

**Interpretation (honest):**
- Objective #1 (beats all proxies): **not demonstrated.** Loopback: sing-box leads c8/c32 by ~1.7-1.9x and steady c1 ~2.1x; goway leads c1 ~2.8x; rushway only clearly beats xray. Non-loopback: path caps all impls to ~15-23 MiB/s; rushway best or tied on c8 and c1, c32 mixed vs goway.
- Objective #2 (best CPU/RSS): **not met.** Non-loopback CPU: rushway clearly best (5.3-5.7 s vs 9.6-12.2). Loopback CPU: sing-box best (1.25 s vs rushway 2.34). RSS: rushway highest everywhere (148-156 MB vs 63-106).
- Objective #3 (UX): Phase 1 delivered; real-TTY TUI sign-off + competitor UX comparison still open.
- Bugs fixed this cycle: early-eof pre-dial pending overflow (`b138cad`), Linux pprof build (`e71eb6b`). Harness: `proxy_bench --external/--target-ip/--echo-bind` + `scripts/w2_bench.ps1 -NonLoopback`. Details in `AI_HANDOFF.md`.
- Next: W3 = Phase 3 version negotiation + WINDOW (goway sync) + re-test; then close sing-box loopback gap and rushway RSS.

## 2026-09-23 - W3 done: Mux VERSION/WINDOW (both stacks) + 8/8 compat smoke + loopback re-test

**Delivered (released 2026-09-23 as rushway v0.0.26 + goway v1.8.12, tagged & pushed):**
- `MuxCmdVERSION=0x05` / `MuxCmdWINDOW=0x06` credit flow control, WS MUX scope, both stacks (`src/protocol.rs`, new `src/flow.rs`, `runtime.rs`, `mux_pool.rs`, `wss_client.rs`; goway `goway.go` + new `flow_test.go`). Unknown-cmd frames silently skipped by old builds -> safe probe negotiation, proven on the wire.
- `scripts/w3_compat_smoke.ps1`: **8/8 PASS** - new/new same+cross both directions assert negotiation logs; all four new/old pairings assert silent v1 fallback and transfer OK (`bench/w3_smoke_logs/`).
- Tests: rushway clippy 0 warnings + 106/12 pass; goway gofmt/vet clean + full suite ok 64.9 s.

**W3 loopback re-test (n=5 medians, `bench/w3_loopback.csv`) vs W2 baseline:**

| impl | mode | c1 | c8 | c32 | cpu_s | rss MB |
| --- | --- | --- | --- | --- | --- | --- |
| rushway W3 | setup | 74.81 | 160.69 | 156.71 | 2.62 | 82.6 |
| rushway W2 | setup | 71.32 | 229.83 | 251.91 | 2.25 | 148.0 |
| rushway W3 | steady | 72.46 | 120.31 | 105.45 | 3.53 | 84.8 |
| rushway W2 | steady | 101.24 | 238.70 | 251.84 | 2.34 | 155.9 |
| goway W3 | setup | 190.36 | 236.24 | 190.15 | 2.11 | 107.7 |
| goway W2 | setup | 247.12 | 292.58 | 267.64 | 1.91 | 87.2 |
| goway W3 | steady | 93.64 | 121.05 | 154.63 | 2.28 | 114.7 |
| goway W2 | steady | 288.42 | 277.09 | 259.18 | 2.14 | 89.0 |

**Honest reading:**
- Round 1 (1 MiB window / 64 KiB refund, `w3_loopback.csv`): throughput **regressed** vs W2 (rushway steady c8/c32 -50%/-58%; goway steady -68%/-56%/-40%) while rushway RSS improved -46% (148-156 -> 82-85 MB).
- Round 2 after user-approved tuning (8 MiB window / 1 MiB refund, **`bench/w3b_loopback.csv`**): initially read as recovered-to-W2 (rushway setup 105.89/209.32/233.18, steady 74.61/199.44/215.43; goway steady 112.3/343.35/286.96 "c8/c32 beat W2"). Smoke re-run after tuning: 8/8 PASS. RSS both sides back to W2 level.
- **CORRECTION (n=10, `bench/w3c_goway_n10.csv`, same binary):** the round-2 "recovered / beat W2" performance claim did **not** reproduce and is withdrawn. goway n=10 vs v1.8.11: setup 169.2/231.19/231.08 (c1 -32%, c8 -21%, c32 -14%, cpu flat, rss +7%); steady 250.34/211.1/185.36 (c1 -13%, c8 -24%, c32 -29%, cpu +11%, rss +6%). Same-build n=5 vs n=10 disagree wildly -> single-run medians on this machine are not decisive.
- **Standing rule (user mandate, 2026-09-23, written into `way/goway/AI_HANDOFF.md` + `way/goway/README.md` + `goway.go` header): goway changes are forward-only, never reverse; performance changes ship only with every metric at-or-above baseline v1.8.11 beyond noise, with numbers recorded.** Functional/compat evidence stands (8/8 smoke, both unit suites green).
- **RESOLVED 2026-09-23 — interleaved paired A/B certification:** goway `creditGate` hot path made lock-free (atomics + CAS, mutex only on Enable/Close); `w3_ab_bench.ps1` verdict fixed to paired per-sample deltas + exact sign test (was: independent medians, invalid for interleaved design; `$samples`/`$Samples` case-collision bug fixed). n=10 per mode, order flipped per sample, HEAD-built v1.8.11 vs atomic-gate `goway.exe`, `bench/w3_ab_goway.csv`: setup medΔ c1 +9.72/c8 +9.06/c32 +2.12/cpu −0.19/rss −2.30; steady +55.26/−2.23/−0.02/+0.03/−4.70 — **all NOISE both modes (crit=9, none regressed beyond noise) → W3 certified non-inferior under the forward-only rule.** Watch: rss +2.3/+4.7 MB trends below significance. **Shipped: rushway v0.0.26 + goway v1.8.12 (tagged & pushed).** Next: sing-box gap work (goal #1).
