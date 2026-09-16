$ErrorActionPreference = 'Stop'

# PGO build pipeline for RushWay (Windows x64, MSVC).
#
#   Phase 1: build release binaries with profile instrumentation.
#   Phase 2: training workload.
#   Phase 3: merge profiles with llvm-profdata (needs `rustup component
#            add llvm-tools`).
#   Phase 4: rebuild release with `-Cprofile-use`.
#
# TRAINING DATA WARNING (measured 2026-09-16, localhost 4 MiB echo):
#   * Unit-test training REGRESSED steady-state relay throughput ~15-20%
#     (c8/c32 medians): the profile marks async relay loops as cold.
#   * Relay-traffic training (12 bulk rounds through real server/client,
#     processes exited cleanly via Ctrl+Break graceful shutdown) landed
#     WITHIN NOISE of the normal build (c8/c32 medians roughly -5%,
#     overlapping run-to-run variance). No consistent gain on loopback.
#   Do NOT ship a PGO binary without a VPS-line before/after win.
#   Killed processes never flush .profraw: training drivers must let
#   RushWay exit cleanly (Ctrl+C / Ctrl+Break graceful shutdown).
#
# Default training below uses `cargo test` (leaf coverage only). For relay
# training, run real traffic between Phase 1 and Phase 3 instead (see
# AI_HANDOFF.md 2026-09-16 Perf entries for the proven driver pattern).

$Root = Join-Path $PSScriptRoot '..'
$PgoData = Join-Path $Root 'target\pgo-data'
$Merged = Join-Path $Root 'target\pgo-merged.profdata'
$Profdata = Join-Path $env:USERPROFILE `
  '.rustup\toolchains\stable-x86_64-pc-windows-msvc\lib\rustlib\x86_64-pc-windows-msvc\bin\llvm-profdata.exe'

if (-not (Test-Path $Profdata)) {
  throw "llvm-profdata not found. Run: rustup component add llvm-tools`nLooked at: $Profdata"
}

# Phase 1: instrumented build.
Remove-Item $PgoData -Recurse -Force -ErrorAction SilentlyContinue
New-Item -ItemType Directory -Path $PgoData | Out-Null
$env:RUSTFLAGS = "-Cprofile-generate=$PgoData"
cargo build --release --bins

# Phase 2: training (clean-exit workloads only).
cargo test --all-targets
$profraws = Get-ChildItem -LiteralPath $PgoData -ErrorAction SilentlyContinue
if (-not $profraws) {
  throw "No .profraw files generated under $PgoData"
}
Write-Host ("Training profiles: {0} files" -f $profraws.Count)

# Phase 3: merge.
& $Profdata merge -o $Merged $PgoData
if (-not (Test-Path $Merged)) {
  throw "Profile merge failed: $Merged not created"
}
Write-Host "Merged profile: $Merged"

# Phase 4: optimized build.
$env:RUSTFLAGS = "-Cprofile-use=$Merged"
cargo build --release --bins

$env:RUSTFLAGS = ""
Write-Host 'PGO build complete. Compare with e2e_bench before shipping:'
Write-Host '  target\release\e2e_bench.exe'
