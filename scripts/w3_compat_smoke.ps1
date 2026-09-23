# W3 cross-implementation + old/new compatibility smoke for Mux VERSION/WINDOW.
# Matrix: both-new (same impl + cross, both directions), then every new/old
# client/server mix. New/new runs assert "peer VERSION received" in both logs;
# new/old runs assert it is ABSENT (silent v1 fallback on the wire).
param([string]$Only = "")
$ErrorActionPreference = "Stop"
$onlyList = @()
if ($Only) { $onlyList = @($Only.Split(",") | Where-Object { $_ }) }

$root = Split-Path -Parent $PSScriptRoot
$rwNew = Join-Path $root "target\release\rushway.exe"
$gwNew = "D:\SOFT\AI\github\way\goway\goway.exe"
$rwOld = Join-Path $root "bench\oldbin\rushway_old.exe"
$gwOld = Join-Path $root "bench\oldbin\goway_old.exe"
$bench = Join-Path $root "target\release\proxy_bench.exe"
$key = "rushway-proxy-bench-test-key"
$logDir = Join-Path $root "bench\w3_smoke_logs"
New-Item -ItemType Directory -Force -Path $logDir | Out-Null

function Wait-Port {
    param([int]$Port)
    for ($i = 0; $i -lt 120; $i++) {
        try {
            $c = New-Object System.Net.Sockets.TcpClient
            $t = $c.ConnectAsync("127.0.0.1", $Port)
            if ($t.Wait(500)) { $c.Close(); return $true }
            $c.Close()
        } catch { }
        Start-Sleep -Milliseconds 25
    }
    return $false
}

function Start-Proxy {
    param([string]$Bin, [int]$Port, [bool]$IsGoway, [string]$Up, [string]$LogLevel, [string]$Tag)
    $p = if ($IsGoway) { ":$Port" } else { "$Port" }
    $a = @("-p", $p, "-k", $key, "--no-block-local", "--log", $LogLevel)
    if ($Up) { $a += @("--up", $Up) }
    $out = Join-Path $logDir "$Tag.out.log"
    $err = Join-Path $logDir "$Tag.err.log"
    Remove-Item $out, $err -ErrorAction SilentlyContinue
    return Start-Process -FilePath $Bin -ArgumentList $a -PassThru -WindowStyle Hidden `
        -RedirectStandardOutput $out -RedirectStandardError $err
}

# Kill strays from earlier attempts (dev machine, bench binaries only).
Get-Process rushway, goway, rushway_old, goway_old, proxy_bench -ErrorAction SilentlyContinue |
    Where-Object { $_.Path -and ($_.Path -like "*rushway*" -or $_.Path -like "*goway*") } |
    Stop-Process -Force -ErrorAction SilentlyContinue
Start-Sleep -Milliseconds 300

$combos = @(
    @{ n = "1_rwnew_srv_rwnew_cli";     srv = "rwnew"; cli = "rwnew"; expect = "present" },
    @{ n = "2_gwnew_srv_gwnew_cli";     srv = "gwnew"; cli = "gwnew"; expect = "present" },
    @{ n = "3_gwnew_srv_rwnew_cli";     srv = "gwnew"; cli = "rwnew"; expect = "present" },
    @{ n = "4_rwnew_srv_gwnew_cli";     srv = "rwnew"; cli = "gwnew"; expect = "present" },
    @{ n = "5_gwnew_srv_rwold_cli";     srv = "gwnew"; cli = "rwold"; expect = "absent" },
    @{ n = "6_rwold_srv_gwnew_cli";     srv = "rwold"; cli = "gwnew"; expect = "absent" },
    @{ n = "7_rwnew_srv_gwold_cli";     srv = "rwnew"; cli = "gwold"; expect = "absent" },
    @{ n = "8_gwold_srv_rwnew_cli";     srv = "gwold"; cli = "rwnew"; expect = "absent" }
)

function Resolve-Bin([string]$tag) {
    switch ($tag) {
        "rwnew" { return $rwNew }
        "gwnew" { return $gwNew }
        "rwold" { return $rwOld }
        "gwold" { return $gwOld }
    }
}

$results = @()
$base = 19200
$idx = 0
$failed = $false

foreach ($combo in $combos) {
    if ($onlyList.Count -gt 0 -and $onlyList -notcontains $combo.n) { continue }
    $idx++
    $srvPort = $base + $idx * 10
    $cliPort = $srvPort + 1
    $srvTag = "$($combo.n)_srv"
    $cliTag = "$($combo.n)_cli"
    $procs = @()
    Write-Output "=== $($combo.n) srv=$($combo.srv):$srvPort cli=$($combo.cli):$cliPort ==="
    try {
        $srvBin = Resolve-Bin $combo.srv
        $cliBin = Resolve-Bin $combo.cli
        $procs += Start-Proxy -Bin $srvBin -Port $srvPort -IsGoway ($combo.srv -like "gw*") -Up "" -LogLevel DEBUG -Tag $srvTag
        if (-not (Wait-Port $srvPort)) { throw "server port $srvPort did not open" }
        $procs += Start-Proxy -Bin $cliBin -Port $cliPort -IsGoway ($combo.cli -like "gw*") `
            -Up "ws://127.0.0.1:$srvPort/" -LogLevel DEBUG -Tag $cliTag
        if (-not (Wait-Port $cliPort)) { throw "client port $cliPort did not open" }

        $benchLog = Join-Path $logDir "$($combo.n).bench.log"
        $line = & $bench --implementation $combo.n --external --client-port $cliPort 2>&1 | Tee-Object -FilePath $benchLog
        if ($LASTEXITCODE -ne 0) { throw "proxy_bench exited $LASTEXITCODE : $line" }
        $lineStr = ($line | Out-String)
        if ($lineStr -notmatch "c1_mib_s" -or $lineStr -notmatch "c32_mib_s") {
            throw "proxy_bench output missing throughput: $lineStr"
        }

        $logs = Get-ChildItem (Join-Path $logDir "$($combo.n)_*.log") -ErrorAction SilentlyContinue
        $hits = 0
        foreach ($f in $logs) {
            $hits += (Select-String -Path $f.FullName -Pattern "peer VERSION received" -SimpleMatch -ErrorAction SilentlyContinue | Measure-Object).Count
        }
        if ($combo.expect -eq "present" -and $hits -lt 2) {
            throw "expected VERSION negotiation on both sides, found $hits log hits"
        }
        if ($combo.expect -eq "absent" -and $hits -ne 0) {
            throw "expected v1 fallback (no VERSION), found $hits log hits"
        }

        $short = ($lineStr -replace ".*proxy_e2e", "proxy_e2e" -replace "\s+", " ").Trim()
        Write-Output "PASS $($combo.n) version_$($combo.expect) $short"
        $results += [pscustomobject]@{ Combo = $combo.n; Result = "PASS"; Note = "version_$($combo.expect)" }
    } catch {
        Write-Output "FAIL $($combo.n): $($_.Exception.Message)"
        $results += [pscustomobject]@{ Combo = $combo.n; Result = "FAIL"; Note = $_.Exception.Message }
        $failed = $true
    } finally {
        foreach ($p in $procs) {
            if ($p -and -not $p.HasExited) { Stop-Process -Id $p.Id -Force -ErrorAction SilentlyContinue }
        }
        Get-Process rushway, goway, rushway_old, goway_old, proxy_bench -ErrorAction SilentlyContinue |
            Where-Object { $_.Path -and ($_.Path -like "*rushway*" -or $_.Path -like "*goway*") } |
            Stop-Process -Force -ErrorAction SilentlyContinue
        Start-Sleep -Milliseconds 300
    }
}

Write-Output "=== SUMMARY ==="
$results | Format-Table -AutoSize | Out-String | Write-Output
$passCount = ($results | Where-Object Result -eq "PASS").Count
Write-Output "PASS $passCount/$($results.Count)"
if ($failed) { exit 1 } else { exit 0 }
