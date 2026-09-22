param(
    [int]$Samples = 5,
    [ValidateSet("setup", "steady")]
    [string[]]$Modes = @("setup", "steady"),
    [ValidateSet("rushway", "goway", "sing-box", "xray")]
    [string[]]$Impls = @("rushway", "goway", "sing-box", "xray"),
    [string]$OutCsv = ""
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
if (-not $OutCsv) { $OutCsv = Join-Path $RepoRoot "bench\w2_loopback.csv" }
New-Item -ItemType Directory -Force -Path (Split-Path $OutCsv) | Out-Null

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

function Wait-Port([int]$Port) {
    for ($i = 0; $i -lt 120; $i++) {
        try {
            $c = [System.Net.Sockets.TcpClient]::new()
            $c.Connect("127.0.0.1", $Port)
            $c.Close()
            return $true
        } catch { Start-Sleep -Milliseconds 25 }
    }
    return $false
}

function Write-XrayConfigs([int]$Sp, [int]$Cp) {
    $server = @"
{
  "log": {"loglevel": "warning"},
  "inbounds": [{
    "tag": "vless-ws-in", "listen": "127.0.0.1", "port": $Sp,
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
    "settings": {"vnext": [{"address": "127.0.0.1", "port": $Sp, "users": [{"id": "$Uuid", "encryption": "none"}]}]},
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

function Write-SingConfigs([int]$Sp, [int]$Cp) {
    $server = @"
{
  "log": {"level": "warn"},
  "inbounds": [{
    "type": "vless", "tag": "vless-ws-in",
    "listen": "127.0.0.1", "listen_port": $Sp,
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
    "server": "127.0.0.1", "server_port": $Sp,
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

function Start-ProxyPair([string]$Impl, [int]$Sp, [int]$Cp) {
    $listenS = if ($Impl -eq "goway") { ":$Sp" } else { "$Sp" }
    $listenC = if ($Impl -eq "goway") { ":$Cp" } else { "$Cp" }
    $up = "ws://127.0.0.1:$Sp/"
    $sOut = Join-Path $WorkDir "${Impl}_${Sp}_srv_out.txt"
    $sErr = Join-Path $WorkDir "${Impl}_${Sp}_srv_err.txt"
    $cOut = Join-Path $WorkDir "${Impl}_${Cp}_cli_out.txt"
    $cErr = Join-Path $WorkDir "${Impl}_${Cp}_cli_err.txt"
    switch ($Impl) {
        "rushway" {
            $s = Start-Process $RushBin -ArgumentList @("-p", $listenS, "-k", $Key, "--no-block-local", "--log", "WARN") -WindowStyle Hidden -PassThru -RedirectStandardOutput $sOut -RedirectStandardError $sErr
            $c = Start-Process $RushBin -ArgumentList @("-p", $listenC, "-k", $Key, "--no-block-local", "--log", "WARN", "--up", $up) -WindowStyle Hidden -PassThru -RedirectStandardOutput $cOut -RedirectStandardError $cErr
        }
        "goway" {
            $s = Start-Process $GowayBin -ArgumentList @("-p", $listenS, "-k", $Key, "--no-block-local", "--log", "WARN") -WindowStyle Hidden -PassThru -RedirectStandardOutput $sOut -RedirectStandardError $sErr
            $c = Start-Process $GowayBin -ArgumentList @("-p", $listenC, "-k", $Key, "--no-block-local", "--log", "WARN", "--up", $up) -WindowStyle Hidden -PassThru -RedirectStandardOutput $cOut -RedirectStandardError $cErr
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
    return @($s, $c)
}

function Stop-Procs($procs) {
    foreach ($p in $procs) {
        if ($p) { try { Stop-Process -Id $p.Id -Force -ErrorAction SilentlyContinue } catch {} }
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
                $spOk = Wait-Port $Sp
                $cpOk = Wait-Port $Cp
                if (-not $spOk -or -not $cpOk) { throw "ports did not open (server=$spOk client=$cpOk)" }

                $outFile = Join-Path $WorkDir "bench_out.txt"
                $errFile = Join-Path $WorkDir "bench_err.txt"
                Remove-Item $outFile, $errFile -ErrorAction SilentlyContinue

                try {
                    $cpu0 = 0.0
                    foreach ($p in $procs) { $p.Refresh(); $cpu0 += [double]$p.CPU }
                } catch { $cpu0 = 0.0 }

                $benchArgs = @("--implementation", $impl, "--external", "--client-port", "$Cp") + $steadyFlag
                $bench = Start-Process $BenchBin -ArgumentList $benchArgs -WindowStyle Hidden -PassThru `
                    -RedirectStandardOutput $outFile -RedirectStandardError $errFile

                $peakRss = [long]0
                $deadline = [DateTime]::UtcNow.AddSeconds(180)
                while (-not $bench.WaitForExit(200)) {
                    $rss = [long]0
                    foreach ($p in $procs) {
                        try {
                            $gp = Get-Process -Id $p.Id -ErrorAction Stop
                            $rss += $gp.WorkingSet64
                        } catch {}
                    }
                    if ($rss -gt $peakRss) { $peakRss = $rss }
                    if ([DateTime]::UtcNow -gt $deadline) {
                        try { $bench.Kill() } catch {}
                        throw "proxy_bench timed out after 180s"
                    }
                }

                $cpu1 = 0.0
                foreach ($p in $procs) {
                    try { $p.Refresh(); $cpu1 += [double]$p.CPU } catch {}
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
                        $state = try { $p.Refresh(); if ($p.HasExited) { "EXITED($($p.ExitCode))" } else { "alive" } } catch { "gone" }
                        $diag += "pid=$($p.Id):$state"
                    }
                    $errFiles = @()
                    foreach ($pf in @(
                        (Join-Path $WorkDir "${impl}_${Sp}_srv_err.txt"),
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

$rawLines | Add-Content -Path (Join-Path (Split-Path $OutCsv) "w2_raw.log") -Encoding utf8
Write-Host "`ncsv: $OutCsv"
