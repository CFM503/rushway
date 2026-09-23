param(
    [int]$Samples = 5,
    [ValidateSet("setup", "steady")]
    [string[]]$Modes = @("setup", "steady"),
    [ValidateSet("rushway", "goway", "sing-box", "xray")]
    [string[]]$Impls = @("rushway", "goway", "sing-box", "xray"),
    [string]$OutCsv = "",
    [switch]$NonLoopback
)

$ErrorActionPreference = "Stop"
$RepoRoot = Split-Path $PSScriptRoot -Parent
$BenchBin = Join-Path $RepoRoot "target\release\proxy_bench.exe"
$RushBin = Join-Path $RepoRoot "target\release\rushway.exe"
$GowayBin = "D:\SOFT\AI\github\way\goway\goway.exe"
$XrayBin = Join-Path $RepoRoot "tools\xray\xray.exe"
$SingDir = Join-Path $RepoRoot "tools\sing-box"
$SingBin = (Get-ChildItem $SingDir -Filter sing-box.exe -Recurse | Select-Object -First 1).FullName
$Key = "rushway-proxy-bench-test-key"
$Uuid = "aaaaaaaa-bbbb-cccc-dddd-eeeeeeeeeeee"
$WorkDir = Join-Path $env:TEMP "w2bench"
New-Item -ItemType Directory -Force -Path $WorkDir | Out-Null
if (-not $OutCsv) {
    $OutCsv = if ($NonLoopback) { Join-Path $RepoRoot "bench\w2_nonloopback.csv" } else { Join-Path $RepoRoot "bench\w2_loopback.csv" }
}
New-Item -ItemType Directory -Force -Path (Split-Path $OutCsv) | Out-Null

$WslDistro = "Debian"
$WslBinDir = "/root/w2bin"
$WslIp = ""
$WinIp = ""
$ClkTck = 100
if ($NonLoopback) {
    $null = wsl -d $WslDistro -- true
    $WslIp = (((wsl -d $WslDistro -- bash -lc 'hostname -I') -join " ").Trim() -split "\s+" | Select-Object -First 1)
    $routeToks = (((wsl -d $WslDistro -- bash -lc 'ip route show default') -join " ") -split "\s+")
    $viaIdx = [array]::IndexOf($routeToks, "via")
    if ($viaIdx -lt 0 -or ($viaIdx + 1) -ge $routeToks.Count) { throw "cannot resolve Windows host IP from WSL default route" }
    $WinIp = $routeToks[$viaIdx + 1]
    $tk = (wsl -d $WslDistro -- getconf CLK_TCK | Select-Object -First 1)
    if ($tk) { $ClkTck = [int]$tk.ToString().Trim() }
    if (-not $WslIp -or -not $WinIp) { throw "wsl topology discovery failed (wsl=$WslIp win=$WinIp)" }
    Write-Host "nonloopback topology: win=$WinIp wsl=$WslIp clk_tck=$ClkTck"
}

foreach ($p in @($BenchBin, $RushBin, $GowayBin, $XrayBin, $SingBin)) {
    if (-not (Test-Path $p)) { throw "missing binary: $p" }
}

function Get-FreePort {
    $l = [System.Net.Sockets.TcpListener]::new([System.Net.IPAddress]::Loopback, 0)
    $l.Start()
    $port = $l.LocalEndpoint.Port
    $l.Stop()
    return $port
}

function Wait-Port([int]$Port, [string]$TargetHost = "127.0.0.1") {
    for ($i = 0; $i -lt 60; $i++) {
        try {
            $c = [System.Net.Sockets.TcpClient]::new()
            $task = $c.ConnectAsync($TargetHost, $Port)
            if ($task.Wait(500) -and $c.Connected) { $c.Close(); return $true }
            $c.Close()
        } catch {}
        Start-Sleep -Milliseconds 50
    }
    return $false
}

function Write-XrayConfigs([int]$Sp, [int]$Cp, [string]$SListen = "127.0.0.1", [string]$CUpAddr = "127.0.0.1") {
    $server = @"
{
  "log": {"loglevel": "warning"},
  "inbounds": [{
    "tag": "vless-ws-in", "listen": "$SListen", "port": $Sp,
    "protocol": "vless",
    "settings": {"clients": [{"id": "$Uuid"}], "decryption": "none"},
    "streamSettings": {"network": "ws", "wsSettings": {"path": "/bench"}}
  }],
  "outbounds": [{"tag": "direct", "protocol": "freedom"}]
}
"@
    $client = @"
{
  "log": {"loglevel": "warning"},
  "inbounds": [{
    "tag": "socks-in", "listen": "127.0.0.1", "port": $Cp,
    "protocol": "socks",
    "settings": {"auth": "noauth", "udp": false}
  }],
  "outbounds": [{
    "tag": "proxy", "protocol": "vless",
    "settings": {"vnext": [{"address": "$CUpAddr", "port": $Sp, "users": [{"id": "$Uuid", "encryption": "none"}]}]},
    "streamSettings": {"network": "ws", "wsSettings": {"path": "/bench"}}
  }]
}
"@
    $sPath = Join-Path $WorkDir "xserver.json"
    $cPath = Join-Path $WorkDir "xclient.json"
    Set-Content -Path $sPath -Value $server -Encoding ascii
    Set-Content -Path $cPath -Value $client -Encoding ascii
    return @($sPath, $cPath)
}

function Write-SingConfigs([int]$Sp, [int]$Cp, [string]$SListen = "127.0.0.1", [string]$CUpAddr = "127.0.0.1") {
    $server = @"
{
  "log": {"level": "warn"},
  "inbounds": [{
    "type": "vless", "tag": "vless-ws-in",
    "listen": "$SListen", "listen_port": $Sp,
    "users": [{"uuid": "$Uuid"}],
    "transport": {"type": "ws", "path": "/bench"}
  }],
  "outbounds": [{"type": "direct", "tag": "direct"}]
}
"@
    $client = @"
{
  "log": {"level": "warn"},
  "inbounds": [{
    "type": "socks", "tag": "socks-in",
    "listen": "127.0.0.1", "listen_port": $Cp
  }],
  "outbounds": [{
    "type": "vless", "tag": "proxy",
    "server": "$CUpAddr", "server_port": $Sp,
    "uuid": "$Uuid",
    "transport": {"type": "ws", "path": "/bench"}
  }]
}
"@
    $sPath = Join-Path $WorkDir "sserver.json"
    $cPath = Join-Path $WorkDir "sclient.json"
    Set-Content -Path $sPath -Value $server -Encoding ascii
    Set-Content -Path $cPath -Value $client -Encoding ascii
    return @($sPath, $cPath)
}

function Convert-ToWslPath([string]$WinPath) {
    if ($WinPath -match "^([A-Za-z]):") {
        $drive = $Matches[1].ToLower()
        $rest = $WinPath.Substring(2) -replace "\\", "/"
        return "/mnt/$drive$rest"
    }
    return ($WinPath -replace "\\", "/")
}

function Start-WslProcess([string[]]$Argv, [string]$LogPath) {
    $runShWin = Join-Path $WorkDir "run.sh"
    if (-not (Test-Path $runShWin)) {
        $body = ('#!/bin/bash', 'pidfile="$1"', 'shift', 'echo $$ > "$pidfile"', 'exec "$@"') -join "`n"
        [System.IO.File]::WriteAllText($runShWin, $body)
    }
    $wslRun = Convert-ToWslPath $runShWin
    $pidFileWin = Join-Path $WorkDir ("wslsrv_" + [Guid]::NewGuid().ToString("N").Substring(0, 8) + ".pid")
    $wslPidFile = Convert-ToWslPath $pidFileWin
    $wslArgs = @('-d', $WslDistro, '--', 'bash', $wslRun, $wslPidFile) + $Argv
    $proc = Start-Process wsl.exe -ArgumentList $wslArgs -WindowStyle Hidden -PassThru -RedirectStandardOutput $LogPath -RedirectStandardError ($LogPath + ".err")
    for ($i = 0; $i -lt 150; $i++) {
        if (Test-Path $pidFileWin) {
            $lp = 0
            try { $lp = [int]((Get-Content $pidFileWin -Raw).Trim()) } catch {}
            if ($lp -gt 0) { return [pscustomobject]@{ Win = $proc; LinuxPid = $lp } }
        }
        if ($proc.HasExited) {
            $e = if (Test-Path ($LogPath + ".err")) { (Get-Content ($LogPath + ".err") -Raw) } else { "" }
            throw "wsl wrapper exited early code=$($proc.ExitCode): $e"
        }
        Start-Sleep -Milliseconds 100
    }
    throw "wsl run.sh did not produce pidfile for [$($Argv -join ' ')]"
}

function Get-WslCpuSeconds([int]$ProcId) {
    $out = wsl -d $WslDistro -- bash -c "cat /proc/$ProcId/stat 2>/dev/null"
    if ($LASTEXITCODE -ne 0 -or -not $out) { return 0.0 }
    $line = $out -join ""
    $close = $line.LastIndexOf(")")
    if ($close -lt 0) { return 0.0 }
    $fields = @(($line.Substring($close + 1) -split "\s+") | Where-Object { $_ -ne "" })
    if ($fields.Count -lt 13) { return 0.0 }
    return ([double]$fields[11] + [double]$fields[12]) / [double]$ClkTck
}

function Get-WslRssBytes([int]$ProcId) {
    $out = wsl -d $WslDistro -- bash -c "grep VmRSS /proc/$ProcId/status 2>/dev/null"
    if ($LASTEXITCODE -ne 0 -or -not $out) { return [long]0 }
    $m = [regex]::Match(($out -join " "), "VmRSS:\s+(\d+)\s+kB")
    if ($m.Success) { return [long]$m.Groups[1].Value * 1KB }
    return [long]0
}

function Start-ProxyPair([string]$Impl, [int]$Sp, [int]$Cp) {
    $upHost = if ($NonLoopback) { $WslIp } else { "127.0.0.1" }
    $up = "ws://${upHost}:$Sp/"
    $sOut = Join-Path $WorkDir "${Impl}_${Sp}_srv_out.txt"
    $sErr = Join-Path $WorkDir "${Impl}_${Sp}_srv_err.txt"
    $cOut = Join-Path $WorkDir "${Impl}_${Cp}_cli_out.txt"
    $cErr = Join-Path $WorkDir "${Impl}_${Cp}_cli_err.txt"
    $recs = @()
    $cfgs = $null

    if ($NonLoopback) {
        $sRec = [pscustomobject]@{ Kind = "wsl"; Id = 0; Proc = $null; Label = "srv" }
        switch ($Impl) {
            "rushway" {
                $w = Start-WslProcess -Argv @("$WslBinDir/rushway", "-p", "0.0.0.0:$Sp", "-k", $Key, "--no-block-local", "--log", "WARN") -LogPath $sOut
                $sRec.Id = $w.LinuxPid; $sRec.Proc = $w.Win
            }
            "goway" {
                $w = Start-WslProcess -Argv @("$WslBinDir/goway", "-p", "0.0.0.0:$Sp", "-k", $Key, "--no-block-local", "--log", "WARN") -LogPath $sOut
                $sRec.Id = $w.LinuxPid; $sRec.Proc = $w.Win
            }
            "xray" {
                $cfgs = Write-XrayConfigs $Sp $Cp "0.0.0.0" $WslIp
                $w = Start-WslProcess -Argv @("$WslBinDir/xray", "run", "-c", (Convert-ToWslPath $cfgs[0])) -LogPath $sOut
                $sRec.Id = $w.LinuxPid; $sRec.Proc = $w.Win
            }
            "sing-box" {
                $cfgs = Write-SingConfigs $Sp $Cp "0.0.0.0" $WslIp
                $w = Start-WslProcess -Argv @("$WslBinDir/sing-box", "run", "-c", (Convert-ToWslPath $cfgs[0])) -LogPath $sOut
                $sRec.Id = $w.LinuxPid; $sRec.Proc = $w.Win
            }
        }
        $recs += $sRec
        switch ($Impl) {
            "rushway" {
                $c = Start-Process $RushBin -ArgumentList @("-p", "$Cp", "-k", $Key, "--no-block-local", "--log", "WARN", "--up", $up) -WindowStyle Hidden -PassThru -RedirectStandardOutput $cOut -RedirectStandardError $cErr
            }
        "goway" {
            $gUp = Start-Process $GowayBin -ArgumentList @("-p", ":$Cp", "-k", $Key, "--no-block-local", "--log", "WARN", "--up", $up) -WindowStyle Hidden -PassThru -RedirectStandardOutput $cOut -RedirectStandardError $cErr
            $recs += [pscustomobject]@{ Kind = "win"; Id = $gUp.Id; Proc = $gUp; Label = "cli" }
            return $recs
        }
            "xray" {
                $c = Start-Process $XrayBin -ArgumentList @("run", "-c", $cfgs[1]) -WindowStyle Hidden -PassThru -RedirectStandardOutput $cOut -RedirectStandardError $cErr
            }
            "sing-box" {
                $c = Start-Process $SingBin -ArgumentList @("run", "-c", $cfgs[1]) -WindowStyle Hidden -PassThru -RedirectStandardOutput $cOut -RedirectStandardError $cErr
            }
        }
        $recs += [pscustomobject]@{ Kind = "win"; Id = $c.Id; Proc = $c; Label = "cli" }
        return $recs
    }
    switch ($Impl) {
        "rushway" {
            $s = Start-Process $RushBin -ArgumentList @("-p", "$Sp", "-k", $Key, "--no-block-local", "--log", "WARN") -WindowStyle Hidden -PassThru -RedirectStandardOutput $sOut -RedirectStandardError $sErr
            $c = Start-Process $RushBin -ArgumentList @("-p", "$Cp", "-k", $Key, "--no-block-local", "--log", "WARN", "--up", $up) -WindowStyle Hidden -PassThru -RedirectStandardOutput $cOut -RedirectStandardError $cErr
        }
        "goway" {
            $s = Start-Process $GowayBin -ArgumentList @("-p", ":$Sp", "-k", $Key, "--no-block-local", "--log", "WARN") -WindowStyle Hidden -PassThru -RedirectStandardOutput $sOut -RedirectStandardError $sErr
            $c = Start-Process $GowayBin -ArgumentList @("-p", ":$Cp", "-k", $Key, "--no-block-local", "--log", "WARN", "--up", $up) -WindowStyle Hidden -PassThru -RedirectStandardOutput $cOut -RedirectStandardError $cErr
        }
        "xray" {
            $cfgs = Write-XrayConfigs $Sp $Cp
            $s = Start-Process $XrayBin -ArgumentList @("run", "-c", $cfgs[0]) -WindowStyle Hidden -PassThru -RedirectStandardOutput $sOut -RedirectStandardError $sErr
            $c = Start-Process $XrayBin -ArgumentList @("run", "-c", $cfgs[1]) -WindowStyle Hidden -PassThru -RedirectStandardOutput $cOut -RedirectStandardError $cErr
        }
        "sing-box" {
            $cfgs = Write-SingConfigs $Sp $Cp
            $s = Start-Process $SingBin -ArgumentList @("run", "-c", $cfgs[0]) -WindowStyle Hidden -PassThru -RedirectStandardOutput $sOut -RedirectStandardError $sErr
            $c = Start-Process $SingBin -ArgumentList @("run", "-c", $cfgs[1]) -WindowStyle Hidden -PassThru -RedirectStandardOutput $cOut -RedirectStandardError $cErr
        }
    }
    $recs += [pscustomobject]@{ Kind = "win"; Id = $s.Id; Proc = $s; Label = "srv" }
    $recs += [pscustomobject]@{ Kind = "win"; Id = $c.Id; Proc = $c; Label = "cli" }
    return $recs
}

function Stop-Procs($procs) {
    foreach ($p in $procs) {
        if (-not $p) { continue }
        if ($p.Kind -eq "win") {
            try { if ($p.Proc) { Stop-Process -Id $p.Proc.Id -Force -ErrorAction SilentlyContinue } } catch {}
        } else {
            try { if ($p.Proc) { Stop-Process -Id $p.Proc.Id -Force -ErrorAction SilentlyContinue } } catch {}
            wsl -d $WslDistro -- bash -c "kill -9 $($p.Id) 2>/dev/null" | Out-Null
        }
    }
    if ($NonLoopback) {
        wsl -d $WslDistro -- bash -c "pkill -9 -f 'w2bin/' 2>/dev/null" | Out-Null
    }
}

if (-not (Test-Path $OutCsv)) {
    "impl,mode,sample,c1_mib_s,c8_mib_s,c32_mib_s,cpu_s,rss_peak_mb" | Set-Content -Path $OutCsv -Encoding ascii
}

$rawLines = @()

foreach ($mode in $Modes) {
    $steadyFlag = if ($mode -eq "steady") { @("--steady-state") } else { @() }
    foreach ($impl in $Impls) {
        for ($sample = 1; $sample -le $Samples; $sample++) {
            $Sp = Get-FreePort
            $Cp = Get-FreePort
            $procs = @()
            try {
                $procs = Start-ProxyPair $impl $Sp $Cp
                $srvHost = if ($NonLoopback) { $WslIp } else { "127.0.0.1" }
                $spOk = Wait-Port $Sp $srvHost
                $cpOk = Wait-Port $Cp "127.0.0.1"
                if (-not $spOk -or -not $cpOk) { throw "ports did not open (server=$spOk client=$cpOk)" }

                $outFile = Join-Path $WorkDir "bench_out.txt"
                $errFile = Join-Path $WorkDir "bench_err.txt"
                Remove-Item $outFile, $errFile -ErrorAction SilentlyContinue

                $cpu0 = 0.0
                foreach ($p in $procs) {
                    if ($p.Kind -eq "win") {
                        try { $p.Proc.Refresh(); $cpu0 += [double]$p.Proc.CPU } catch {}
                    } else {
                        $cpu0 += Get-WslCpuSeconds $p.Id
                    }
                }

                $benchArgs = @("--implementation", $impl, "--external", "--client-port", "$Cp") + $steadyFlag
                if ($NonLoopback) { $benchArgs += @("--target-ip", $WinIp, "--echo-bind", "0.0.0.0") }
                $bench = Start-Process $BenchBin -ArgumentList $benchArgs -WindowStyle Hidden -PassThru `
                    -RedirectStandardOutput $outFile -RedirectStandardError $errFile

                $peakRss = [long]0
                $deadline = [DateTime]::UtcNow.AddSeconds(180)
                while (-not $bench.WaitForExit(200)) {
                    $rss = [long]0
                    foreach ($p in $procs) {
                        if ($p.Kind -eq "win") {
                            try {
                                $gp = Get-Process -Id $p.Id -ErrorAction Stop
                                $rss += $gp.WorkingSet64
                            } catch {}
                        } else {
                            $rss += Get-WslRssBytes $p.Id
                        }
                    }
                    if ($rss -gt $peakRss) { $peakRss = $rss }
                    if ([DateTime]::UtcNow -gt $deadline) {
                        try { $bench.Kill() } catch {}
                        throw "proxy_bench timed out after 180s"
                    }
                }

                $cpu1 = 0.0
                foreach ($p in $procs) {
                    if ($p.Kind -eq "win") {
                        try { $p.Proc.Refresh(); $cpu1 += [double]$p.Proc.CPU } catch {}
                    } else {
                        $cpu1 += Get-WslCpuSeconds $p.Id
                    }
                }
                $cpuUsed = [Math]::Max(0.0, $cpu1 - $cpu0)

                $line = ""
                if (Test-Path $outFile) {
                    $line = Get-Content $outFile | Where-Object { $_ -match "^proxy_e2e " } | Select-Object -Last 1
                }
                if (-not $line) {
                    $err = if (Test-Path $errFile) { (Get-Content $errFile -Raw) } else { "" }
                    $diag = @()
                    foreach ($p in $procs) {
                        if ($p.Kind -eq "win") {
                            $state = try { $p.Proc.Refresh(); if ($p.Proc.HasExited) { "EXITED($($p.Proc.ExitCode))" } else { "alive" } } catch { "gone" }
                        } else {
                            wsl -d $WslDistro -- bash -c "kill -0 $($p.Id) 2>/dev/null" | Out-Null
                            $state = if ($LASTEXITCODE -eq 0) { "alive" } else { "gone" }
                        }
                        $diag += "$($p.Label):$($p.Id):$state"
                    }
                    $errFiles = @()
                    $srvLogFile = if ($NonLoopback) { Join-Path $WorkDir "${impl}_${Sp}_srv_out.txt" } else { Join-Path $WorkDir "${impl}_${Sp}_srv_err.txt" }
                    foreach ($pf in @(
                        $srvLogFile,
                        (Join-Path $WorkDir "${impl}_${Cp}_cli_err.txt")
                    )) {
                        if ((Test-Path $pf) -and (Get-Item $pf).Length -gt 0) {
                            $errFiles += "[$pf]: " + ((Get-Content $pf -Raw).Trim())
                        }
                    }
                    throw "no result line; bench_stderr=$err procs=$($diag -join ' ') proxy_err=$($errFiles -join ' | ')"
                }

                $c1 = [double]([regex]::Match($line, "c1_mib_s=([0-9.]+)").Groups[1].Value)
                $c8 = [double]([regex]::Match($line, "c8_mib_s=([0-9.]+)").Groups[1].Value)
                $c32 = [double]([regex]::Match($line, "c32_mib_s=([0-9.]+)").Groups[1].Value)
                $rssMb = [Math]::Round($peakRss / 1MB, 1)
                $cpuR = [Math]::Round($cpuUsed, 2)

                "$impl,$mode,$sample,$c1,$c8,$c32,$cpuR,$rssMb" | Add-Content -Path $OutCsv -Encoding ascii
                $rawLines += "$impl mode=$mode sample=$sample c1=$c1 c8=$c8 c32=$c32 cpu_s=$cpuR rss_mb=$rssMb"
                Write-Host $rawLines[-1]
            } catch {
                $rawLines += "$impl mode=$mode sample=$sample FAILED: $($_.Exception.Message)"
                Write-Warning $rawLines[-1]
            } finally {
                Stop-Procs $procs
                Start-Sleep -Milliseconds 300
            }
        }
    }
}

Write-Host "`n=== medians ==="
function Median($vals) {
    $s = @($vals | Sort-Object)
    $n = $s.Count
    if ($n -eq 0) { return 0 }
    if ($n % 2 -eq 1) { return [double]$s[[int](($n - 1) / 2)] }
    return ([double]$s[$n / 2 - 1] + [double]$s[$n / 2]) / 2
}
foreach ($mode in $Modes) {
    foreach ($impl in $Impls) {
        $rows = @()
        if (Test-Path $OutCsv) {
            $rows = @(Get-Content $OutCsv | Select-Object -Skip 1 |
                Where-Object { $_ -match "^$impl,$mode," } |
                ForEach-Object { , @($_ -split ",") })
        }
        if ($rows.Count -eq 0) { continue }
        $m1 = Median @($rows | ForEach-Object { [double]$_[3] })
        $m8 = Median @($rows | ForEach-Object { [double]$_[4] })
        $m32 = Median @($rows | ForEach-Object { [double]$_[5] })
        $mc = Median @($rows | ForEach-Object { [double]$_[6] })
        $mr = Median @($rows | ForEach-Object { [double]$_[7] })
        $summary = "proxy_e2e_summary implementation=$impl mode=$mode samples=$($rows.Count) c1_mib_s=$([Math]::Round($m1,2)) c8_mib_s=$([Math]::Round($m8,2)) c32_mib_s=$([Math]::Round($m32,2)) cpu_s=$mc rss_peak_mb=$mr"
        Write-Host $summary
        $rawLines += $summary
    }
}

$rawLogName = if ($NonLoopback) { "w2_nonloop_raw.log" } else { "w2_raw.log" }
$rawLines | Add-Content -Path (Join-Path (Split-Path $OutCsv) $rawLogName) -Encoding utf8
Write-Host "`ncsv: $OutCsv"
