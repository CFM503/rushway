$ErrorActionPreference = 'Stop'

if (-not (Get-Command cargo -ErrorAction SilentlyContinue)) {
  throw 'cargo not found. Install Rust via rustup first.'
}

cargo test --all-targets
cargo build --release

$source = Join-Path $PSScriptRoot '..\target\release\rushway.exe'
if (-not (Test-Path $source)) {
  throw "Release binary not found: $source"
}

$outDir = Join-Path $PSScriptRoot '..\dist\windows-x64'
New-Item -ItemType Directory -Force -Path $outDir | Out-Null
Copy-Item $source (Join-Path $outDir 'rushway.exe') -Force
Write-Host "Built: $outDir\rushway.exe"
