# Measures the agent's CPU time while idle (after it has converted one file).
# Usage: pwsh scripts/idle-check.ps1 [-Exe path] [-Seconds 60] [-MaxIdleMs 0]
param(
    [string]$Exe = "target\release\optimize-image-folder.exe",
    [int]$Seconds = 60,
    [double]$MaxIdleMs = 0
)
$ErrorActionPreference = "Stop"

$work = Join-Path ([IO.Path]::GetTempPath()) ("oif-idle-" + [guid]::NewGuid())
$home_ = Join-Path $work "home"
$photos = Join-Path $work "photos"
New-Item -ItemType Directory -Force -Path $home_, $photos | Out-Null
$cfg = Join-Path $home_ "config.toml"
$folder = $photos.Replace('\', '\\')
Set-Content -Path $cfg -Encoding utf8 -Value @"
folders = ["$folder"]
original = "delete"
settle_seconds = 1
effort = 3
"@

$log = Join-Path $work "agent.log"
$env:OIF_HOME = $home_
$p = Start-Process -FilePath (Resolve-Path $Exe) -ArgumentList @("--config", "`"$cfg`"", "run", "--log-stderr") `
    -PassThru -NoNewWindow -RedirectStandardError $log
try {
    Start-Sleep -Seconds 3
    Copy-Item "tests\fixtures\photo_orient6.jpg" (Join-Path $photos "a.jpg")
    $jxl = Join-Path $photos "a.jxl"
    $deadline = (Get-Date).AddSeconds(90)
    while (-not ((Test-Path $jxl) -and -not (Test-Path (Join-Path $photos "a.jpg"))) -and (Get-Date) -lt $deadline) {
        Start-Sleep -Milliseconds 500
    }
    if (-not (Test-Path $jxl)) {
        Get-Content $log
        throw "FAIL: the agent did not convert the test file"
    }
    Start-Sleep -Seconds 3

    $p.Refresh(); $before = $p.TotalProcessorTime
    Start-Sleep -Seconds $Seconds
    $p.Refresh(); $after = $p.TotalProcessorTime
    $deltaMs = ($after - $before).TotalMilliseconds

    "idle window       : ${Seconds}s"
    "CPU time consumed : $deltaMs ms"
    "threads           : $($p.Threads.Count)"
    "working set       : $([math]::Round($p.WorkingSet64 / 1MB, 1)) MB"
    "private memory    : $([math]::Round($p.PrivateMemorySize64 / 1MB, 1)) MB"
    Get-Content $log | Select-Object -First 20

    if ($deltaMs -gt $MaxIdleMs) { throw "FAIL: agent used CPU while idle" }
    "OK: no CPU used while idle"
}
finally {
    if (-not $p.HasExited) { Stop-Process -Id $p.Id -Force }
    Remove-Item -Recurse -Force $work -ErrorAction SilentlyContinue
}
