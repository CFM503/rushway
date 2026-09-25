# Interleaved A/B: baseline v0.0.28 vs Round-3 candidate (TCP_NODELAY + lock-free CreditGate + thread-local encode pool)
param(
    [int]$Samples = 8,
    [ValidateSet("setup", "steady")]
    [string[]]$Modes = @("setup", "steady"),
    [string]$OutCsv = ""
)

$ErrorActionPreference = "Stop"
$RepoRoot = Split-Path $PSScriptRoot -Parent
$BenchBin = Join-Path $RepoRoot "target\release\proxy_bench.exe"
$BinA = Join-Path $RepoRoot "bench\oldbin\rushway_r3_base.exe"
$BinB = Join-Path $RepoRoot "target\release\rushway.exe"
$Key = "rushway-proxy-bench-test-key"
$WorkDir = Join-Path $env:TEMP "w2bench"
New-Item -ItemType Directory -Force -Path $WorkDir | Out-Null
if (-not $OutCsv) { $OutCsv = Join-Path $RepoRoot "bench\r6_ab_rushway.csv" }
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
                @{ Impl = "rushway_r6";   Bin = $BinB }
            )
        } else {
            $order = @(
                @{ Impl = "rushway_r6";   Bin = $BinB },
                @{ Impl = "rushway_base"; Bin = $BinA }
            )
        }
        foreach ($arm in $order) {
            Write-Host ">>> [$mode] sample=$sample arm=$($arm.Impl)"
            Invoke-OneRun $arm.Impl $arm.Bin $mode $sample
        }
    }
}

function Median([double[]]$xs) {
    if ($xs.Count -eq 0) { return 0.0 }
    $sorted = [double[]]($xs | Sort-Object)
    $mid = [int]($sorted.Count / 2)
    if ($sorted.Count % 2 -eq 1) { return $sorted[$mid] }
    return ($sorted[$mid - 1] + $sorted[$mid]) / 2.0
}

function Get-SignCrit([int]$n) {
    $table = @{
        1  = 1; 2  = 2; 3  = 3; 4  = 4; 5  = 5
        6  = 6; 7  = 7; 8  = 8; 9  = 8; 10 = 9
        11 = 9; 12 = 10; 13 = 10; 14 = 11; 15 = 12
        16 = 12; 17 = 13; 18 = 13; 19 = 14; 20 = 15
    }
    if ($table.ContainsKey($n)) { return $table[$n] }
    return [Math]::Ceiling($n * 0.75)
}

$metricDefs = @(
    @{ Name = "c1";  Prop = "c1_mib_s";     Hi = $true;  Dec = 2 },
    @{ Name = "c8";  Prop = "c8_mib_s";     Hi = $true;  Dec = 2 },
    @{ Name = "c32"; Prop = "c32_mib_s";    Hi = $true;  Dec = 2 },
    @{ Name = "cpu"; Prop = "cpu_s";        Hi = $false; Dec = 2 },
    @{ Name = "rss"; Prop = "rss_peak_mb";  Hi = $false; Dec = 1 }
)

Write-Host "`n=== Interleaved A/B paired summary (rushway_base vs rushway_r6, n=$Samples) ==="
$rawLines += "`n=== Interleaved A/B paired summary (rushway_base vs rushway_r6, n=$Samples) ==="

$all = Import-Csv $OutCsv
$verdict = @()
foreach ($mode in $Modes) {
    $sub = @($all | Where-Object { $_.mode -eq $mode })
    $baseRows = @($sub | Where-Object { $_.impl -eq "rushway_base" })
    $r6Rows = @($sub | Where-Object { $_.impl -eq "rushway_r6" })
    if ($baseRows.Count -eq 0 -or $r6Rows.Count -eq 0) { continue }

    $baseMap = @{}
    foreach ($r in $baseRows) { $baseMap[[int]$r.sample] = $r }
    $r6Map = @{}
    foreach ($r in $r6Rows) { $r6Map[[int]$r.sample] = $r }

    $bMedC1 = [Math]::Round((Median @($baseRows | ForEach-Object { [double]$_.c1_mib_s })), 2)
    $bMedC8 = [Math]::Round((Median @($baseRows | ForEach-Object { [double]$_.c8_mib_s })), 2)
    $bMedC32 = [Math]::Round((Median @($baseRows | ForEach-Object { [double]$_.c32_mib_s })), 2)
    $bMedCpu = [Math]::Round((Median @($baseRows | ForEach-Object { [double]$_.cpu_s })), 2)
    $bMedRss = [Math]::Round((Median @($baseRows | ForEach-Object { [double]$_.rss_peak_mb })), 1)

    $wMedC1 = [Math]::Round((Median @($r6Rows | ForEach-Object { [double]$_.c1_mib_s })), 2)
    $wMedC8 = [Math]::Round((Median @($r6Rows | ForEach-Object { [double]$_.c8_mib_s })), 2)
    $wMedC32 = [Math]::Round((Median @($r6Rows | ForEach-Object { [double]$_.c32_mib_s })), 2)
    $wMedCpu = [Math]::Round((Median @($r6Rows | ForEach-Object { [double]$_.cpu_s })), 2)
    $wMedRss = [Math]::Round((Median @($r6Rows | ForEach-Object { [double]$_.rss_peak_mb })), 1)

    $line = "REF mode=$mode (independent medians, informational) base(n=$($baseRows.Count)): c1=$bMedC1 c8=$bMedC8 c32=$bMedC32 cpu=$bMedCpu rss=$bMedRss | r6(n=$($r6Rows.Count)): c1=$wMedC1 c8=$wMedC8 c32=$wMedC32 cpu=$wMedCpu rss=$wMedRss"
    Write-Host $line
    $rawLines += $line

    $pairIds = @($baseMap.Keys | Where-Object { $r6Map.ContainsKey($_) } | Sort-Object)
    if ($pairIds.Count -eq 0) {
        Write-Warning "mode=$mode - no paired samples"
        continue
    }
    $modeRegressed = $false
    $modeForward = $false
    foreach ($md in $metricDefs) {
        $p = $md.Prop
        $bad = 0; $good = 0; $ties = 0
        $deltas = @()
        foreach ($s in $pairIds) {
            $bv = [double]($baseMap[$s].$p)
            $wv = [double]($r6Map[$s].$p)
            $dv = $wv - $bv
            $deltas += $dv
            $isBad = if ($md.Hi) { $dv -lt 0 } else { $dv -gt 0 }
            if ($dv -eq 0) { $ties++ }
            elseif ($isBad) { $bad++ } else { $good++ }
        }
        $effN = $pairIds.Count - $ties
        $crit = if ($effN -gt 0) { Get-SignCrit $effN } else { 999 }
        $medDelta = [Math]::Round((Median $deltas), $md.Dec)
        $st = if ($effN -gt 0 -and $bad -ge $crit) { "REGRESSED"; $modeRegressed = $true }
              elseif ($effN -gt 0 -and $good -ge $crit) { "FORWARD"; $modeForward = $true }
              else { "NOISE" }
        $favoredDelta = if ($md.Hi) { $medDelta } else { [Math]::Round(-$medDelta, $md.Dec) }
        $mline = "PAIRED mode=$mode metric=$($md.Name) n=$($pairIds.Count) eff_n=$effN crit=$crit bad=$bad good=$good ties=$ties med_delta=$medDelta median_favours_r6=$favoredDelta => $st"
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

$rawLines | Add-Content -Path (Join-Path (Split-Path $OutCsv) "r6_ab_raw.log") -Encoding utf8
Write-Host "`ncsv: $OutCsv"
if ($verdict -contains "REGRESSION vs baseline beyond noise") { exit 1 } else { exit 0 }
