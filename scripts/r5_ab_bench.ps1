# Interleaved A/B: candidate 3 baseline (encode pool) vs Round-2 candidate 5 (direct-encode bulk)
# (bulk direct-read into encode buffer (zero-copy payload)).
param(
    [int]$Samples = 8,
    [ValidateSet("setup", "steady")]
    [string[]]$Modes = @("setup", "steady"),
    [string]$OutCsv = ""
)

$ErrorActionPreference = "Stop"
$RepoRoot = Split-Path $PSScriptRoot -Parent
$BenchBin = Join-Path $RepoRoot "target\release\proxy_bench.exe"
$BinA = Join-Path $RepoRoot "bench\oldbin\rushway_r3_base.exe"  # HEAD bbbc0a9 encode-pool only
$BinB = Join-Path $RepoRoot "target\release\rushway.exe"         # direct-encode candidate
$Key = "rushway-proxy-bench-test-key"
$WorkDir = Join-Path $env:TEMP "w2bench"
New-Item -ItemType Directory -Force -Path $WorkDir | Out-Null
if (-not $OutCsv) { $OutCsv = Join-Path $RepoRoot "bench\r5_ab_rushway.csv" }
New-Item -ItemType Directory -Force -Path (Split-Path $OutCsv) | Out-Null

foreach ($p in @($BenchBin, $BinA, $BinB)) {
    if (-not (Test-Path $p)) { throw "missing binary: $p" }
}

function Get-FreePort {
    $l = [System.Net.Sockets.TcpListener]::new([System.Net.IPAddress]::Loopback, 0)
    $l.Start()
    $port = $l.LocalEndpoint.Port
    $l.Stop()
    return $port
}

function Wait-Port([int]$Port) {
    for ($i = 0; $i -lt 60; $i++) {
        try {
            $c = [System.Net.Sockets.TcpClient]::new()
            $task = $c.ConnectAsync("127.0.0.1", $Port)
            if ($task.Wait(500) -and $c.Connected) { $c.Close(); return $true }
            $c.Close()
        } catch {}
        Start-Sleep -Milliseconds 50
    }
    return $false
}

function Start-RushPair([string]$Bin, [int]$Sp, [int]$Cp, [string]$Tag) {
    $sOut = Join-Path $WorkDir "ab_${Tag}_${Sp}_srv_out.txt"
    $sErr = Join-Path $WorkDir "ab_${Tag}_${Sp}_srv_err.txt"
    $cOut = Join-Path $WorkDir "ab_${Tag}_${Cp}_cli_out.txt"
    $cErr = Join-Path $WorkDir "ab_${Tag}_${Cp}_cli_err.txt"
    $up = "ws://127.0.0.1:$Sp/"
    $s = Start-Process $Bin -ArgumentList @("-p", "$Sp", "-k", $Key, "--no-block-local", "--log", "WARN") -WindowStyle Hidden -PassThru -RedirectStandardOutput $sOut -RedirectStandardError $sErr
    $c = Start-Process $Bin -ArgumentList @("-p", "$Cp", "-k", $Key, "--no-block-local", "--log", "WARN", "--up", $up) -WindowStyle Hidden -PassThru -RedirectStandardOutput $cOut -RedirectStandardError $cErr
    return @(
        [pscustomobject]@{ Id = $s.Id; Proc = $s; Label = "srv" },
        [pscustomobject]@{ Id = $c.Id; Proc = $c; Label = "cli" }
    )
}

function Stop-Procs($procs) {
    foreach ($p in $procs) {
        try { if ($p -and $p.Proc) { Stop-Process -Id $p.Proc.Id -Force -ErrorAction SilentlyContinue } } catch {}
    }
}

if (-not (Test-Path $OutCsv)) {
    "impl,mode,sample,c1_mib_s,c8_mib_s,c32_mib_s,cpu_s,rss_peak_mb" | Set-Content -Path $OutCsv -Encoding ascii
}

$rawLines = @()

function Invoke-OneRun([string]$Impl, [string]$Bin, [string]$Mode, [int]$Sample) {
    $Sp = Get-FreePort
    $Cp = Get-FreePort
    $procs = @()
    try {
        $procs = Start-RushPair $Bin $Sp $Cp "${Impl}_$Sample"
        if (-not (Wait-Port $Sp)) { throw "server port $Sp did not open" }
        if (-not (Wait-Port $Cp)) { throw "client port $Cp did not open" }

        $outFile = Join-Path $WorkDir "ab_bench_out.txt"
        $errFile = Join-Path $WorkDir "ab_bench_err.txt"
        Remove-Item $outFile, $errFile -ErrorAction SilentlyContinue

        $cpu0 = 0.0
        foreach ($p in $procs) {
            try { $p.Proc.Refresh(); $cpu0 += [double]$p.Proc.CPU } catch {}
        }

        $steadyFlag = if ($Mode -eq "steady") { @("--steady-state") } else { @() }
        $benchArgs = @("--implementation", "rushway", "--external", "--client-port", "$Cp") + $steadyFlag
        $bench = Start-Process $BenchBin -ArgumentList $benchArgs -WindowStyle Hidden -PassThru `
            -RedirectStandardOutput $outFile -RedirectStandardError $errFile

        $peakRss = [long]0
        $deadline = [DateTime]::UtcNow.AddSeconds(180)
        while (-not $bench.WaitForExit(200)) {
            $rss = [long]0
            foreach ($p in $procs) {
                try { $gp = Get-Process -Id $p.Id -ErrorAction Stop; $rss += $gp.WorkingSet64 } catch {}
            }
            if ($rss -gt $peakRss) { $peakRss = $rss }
            if ([DateTime]::UtcNow -gt $deadline) {
                try { $bench.Kill() } catch {}
                throw "proxy_bench timed out after 180s"
            }
        }
        $cpu1 = 0.0
        foreach ($p in $procs) {
            try { $p.Proc.Refresh(); $cpu1 += [double]$p.Proc.CPU } catch {}
        }
        $cpuUsed = [Math]::Max(0.0, $cpu1 - $cpu0)

        $line = ""
        if (Test-Path $outFile) {
            $line = Get-Content $outFile | Where-Object { $_ -match "^proxy_e2e " } | Select-Object -Last 1
        }
        if (-not $line) {
            $err = if (Test-Path $errFile) { (Get-Content $errFile -Raw) } else { "" }
            throw "no result line; bench_stderr=$err"
        }

        $c1 = [double]([regex]::Match($line, "c1_mib_s=([0-9.]+)").Groups[1].Value)
        $c8 = [double]([regex]::Match($line, "c8_mib_s=([0-9.]+)").Groups[1].Value)
        $c32 = [double]([regex]::Match($line, "c32_mib_s=([0-9.]+)").Groups[1].Value)
        $rssMb = [Math]::Round($peakRss / 1MB, 1)
        $cpuR = [Math]::Round($cpuUsed, 2)

        "$Impl,$Mode,$Sample,$c1,$c8,$c32,$cpuR,$rssMb" | Add-Content -Path $OutCsv -Encoding ascii
        $msg = "$Impl mode=$Mode sample=$Sample c1=$c1 c8=$c8 c32=$c32 cpu_s=$cpuR rss_mb=$rssMb"
        Write-Host $msg
        $script:rawLines += $msg
    } finally {
        Stop-Procs $procs
        Start-Sleep -Milliseconds 300
    }
}

foreach ($mode in $Modes) {
    for ($sample = 1; $sample -le $Samples; $sample++) {
        if ($sample % 2 -eq 1) {
            $order = @(
                @{ Impl = "rushway_base"; Bin = $BinA },
                @{ Impl = "rushway_r5";   Bin = $BinB }
            )
        } else {
            $order = @(
                @{ Impl = "rushway_r5";   Bin = $BinB },
                @{ Impl = "rushway_base"; Bin = $BinA }
            )
        }
        foreach ($arm in $order) {
            try {
                Invoke-OneRun -Impl $arm.Impl -Bin $arm.Bin -Mode $mode -Sample $sample
            } catch {
                $msg = "$($arm.Impl) mode=$mode sample=$sample FAILED: $($_.Exception.Message)"
                Write-Warning $msg
                $rawLines += $msg
            }
        }
    }
}

Write-Host "`n=== interleaved A/B ==="
function Median($vals) {
    $s = @($vals | Sort-Object)
    $n = $s.Count
    if ($n -eq 0) { return 0 }
    if ($n % 2 -eq 1) { return [double]$s[[int](($n - 1) / 2)] }
    return ([double]$s[$n / 2 - 1] + [double]$s[$n / 2]) / 2
}
function Get-SignCrit([int]$n) {
    if ($n -le 0) { return 1 }
    $logF = @(0.0)
    for ($i = 1; $i -le $n; $i++) { $logF += $logF[$i - 1] + [Math]::Log($i) }
    $den = $n * [Math]::Log(2)
    for ($k = 1; $k -le $n; $k++) {
        $p = 0.0
        for ($i = $k; $i -le $n; $i++) {
            $p += [Math]::Exp($logF[$n] - $logF[$i] - $logF[$n - $i] - $den)
        }
        if (2 * $p -le 0.05) { return $k }
    }
    return $n + 1
}
$rows = @(Get-Content $OutCsv | Select-Object -Skip 1 | ForEach-Object { , @($_ -split ",") })
$metricDefs = @(
    @{ Name = "c1";  Col = 3; Hi = $true;  Dec = 2 },
    @{ Name = "c8";  Col = 4; Hi = $true;  Dec = 2 },
    @{ Name = "c32"; Col = 5; Hi = $true;  Dec = 2 },
    @{ Name = "cpu"; Col = 6; Hi = $false; Dec = 2 },
    @{ Name = "rss"; Col = 7; Hi = $false; Dec = 1 }
)
$verdict = @()
foreach ($mode in $Modes) {
    $m = @{}
    foreach ($impl in @("rushway_base", "rushway_r5")) {
        $r = @($rows | Where-Object { $_[0] -eq $impl -and $_[1] -eq $mode })
        if ($r.Count -eq 0) { continue }
        $m[$impl] = @{
            Rows = $r
            BySample = @{}
        }
        foreach ($row in $r) { $m[$impl].BySample[[int]$row[2]] = $row }
        $m[$impl] | Add-Member -NotePropertyName Medians -NotePropertyValue ([pscustomobject]@{
            c1  = [Math]::Round((Median @($r | ForEach-Object { [double]$_[3] })), 2)
            c8  = [Math]::Round((Median @($r | ForEach-Object { [double]$_[4] })), 2)
            c32 = [Math]::Round((Median @($r | ForEach-Object { [double]$_[5] })), 2)
            cpu = [Math]::Round((Median @($r | ForEach-Object { [double]$_[6] })), 2)
            rss = [Math]::Round((Median @($r | ForEach-Object { [double]$_[7] })), 1)
        }) -Force
    }
    if (-not ($m.ContainsKey("rushway_base") -and $m.ContainsKey("rushway_r5"))) { continue }
    $b = $m["rushway_base"]; $w = $m["rushway_r5"]
    $bMed = $b.Medians; $wMed = $w.Medians
    $line = "REF mode=$mode (independent medians, informational) base(n=$($b.Rows.Count)): c1=$($bMed.c1) c8=$($bMed.c8) c32=$($bMed.c32) cpu=$($bMed.cpu) rss=$($bMed.rss) | r5(n=$($w.Rows.Count)): c1=$($wMed.c1) c8=$($wMed.c8) c32=$($wMed.c32) cpu=$($wMed.cpu) rss=$($wMed.rss)"
    Write-Host $line
    $rawLines += $line

    $pairIds = @($b.BySample.Keys | Where-Object { $w.BySample.ContainsKey($_) } | Sort-Object)
    if ($pairIds.Count -eq 0) {
        Write-Warning "mode=$mode - no paired samples"
        continue
    }
    $modeRegressed = $false
    $modeForward = $false
    foreach ($md in $metricDefs) {
        $bad = 0; $good = 0; $ties = 0
        $deltas = @()
        foreach ($s in $pairIds) {
            $dv = [double]$w.BySample[$s][$md.Col] - [double]$b.BySample[$s][$md.Col]
            $deltas += $dv
            $isBad = if ($md.Hi) { $dv -lt 0 } else { $dv -gt 0 }
            if ($dv -eq 0) { $ties++ }
            elseif ($isBad) { $bad++ } else { $good++ }
        }
        $effN = $pairIds.Count - $ties
        $crit = Get-SignCrit $effN
        $medDelta = [Math]::Round((Median $deltas), $md.Dec)
        $st = if ($bad -ge $crit) { "REGRESSED"; $modeRegressed = $true }
              elseif ($good -ge $crit) { "FORWARD"; $modeForward = $true }
              else { "NOISE" }
        $favoredDelta = if ($md.Hi) { $medDelta } else { [Math]::Round(-$medDelta, $md.Dec) }
        $mline = "PAIRED mode=$mode metric=$($md.Name) n=$($pairIds.Count) eff_n=$effN crit=$crit bad=$bad good=$good ties=$ties med_delta=$medDelta median_favours_r5=$favoredDelta => $st"
        Write-Host $mline
        $rawLines += $mline
    }
    $v = if ($modeRegressed) { "REGRESSION vs baseline beyond noise" }
         elseif ($modeForward) { "FORWARD vs baseline on at least one metric, none regressed beyond noise" }
         else { "NOISE (no metric moved beyond noise)" }
    $vline = "VERDICT mode=$mode : $v"
    Write-Host $vline
    $rawLines += $vline
    $verdict += $v
}

$rawLines | Add-Content -Path (Join-Path (Split-Path $OutCsv) "r5_ab_raw.log") -Encoding utf8
Write-Host "`ncsv: $OutCsv"
if ($verdict -contains "REGRESSION vs baseline beyond noise") { exit 1 } else { exit 0 }

