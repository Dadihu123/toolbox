# Idle resource baseline.
#
#   powershell -NoProfile -ExecutionPolicy Bypass -File scripts/measure-idle.ps1 [-Seconds 10]
#
# Reports RSS / private bytes / CPU for the host process with no window open, and
# the WebView2 processes that are actually children of it. Run before a release
# and compare against benchmarks/idle.json.

param(
    [int]$Seconds = 10,
    [string]$ProcessName = 'toolbox'
)

$ErrorActionPreference = 'Stop'

function Get-ChildrenOf([int]$ParentId) {
    # Walk the process tree once and return every descendant id.
    $all = Get-CimInstance Win32_Process -ErrorAction SilentlyContinue |
        Select-Object ProcessId, ParentProcessId
    $result = New-Object System.Collections.Generic.List[int]
    $frontier = @($ParentId)
    while ($frontier.Count -gt 0) {
        $next = @()
        foreach ($p in $frontier) {
            foreach ($c in $all | Where-Object { $_.ParentProcessId -eq $p }) {
                $result.Add([int]$c.ProcessId)
                $next += [int]$c.ProcessId
            }
        }
        $frontier = $next
    }
    return $result
}

$proc = Get-Process $ProcessName -ErrorAction SilentlyContinue | Select-Object -First 1
if (-not $proc) {
    Write-Host "process '$ProcessName' is not running — start the portable build first" -ForegroundColor Yellow
    exit 1
}

$proc.Refresh()
$cpu0 = $proc.CPU
$t0 = Get-Date
Start-Sleep -Seconds $Seconds
$proc.Refresh()
$cpu1 = $proc.CPU
$t1 = Get-Date

$elapsed = ($t1 - $t0).TotalSeconds
$cpuPercent = if ($elapsed -gt 0) { (($cpu1 - $cpu0) / $elapsed) * 100 } else { 0 }

# Only count WebView2 processes that belong to this app; other applications on the
# machine have their own, and including them makes the number meaningless.
$childIds = Get-ChildrenOf -ParentId $proc.Id
$wvRss = 0.0
$wvCount = 0
if ($childIds.Count -gt 0) {
    $wv = Get-Process -ErrorAction SilentlyContinue | Where-Object { $childIds -contains $_.Id }
    if ($wv) {
        $wvRss = [math]::Round((($wv | Measure-Object WorkingSet64 -Sum).Sum) / 1MB, 1)
        $wvCount = ($wv | Measure-Object).Count
    }
}

$report = [ordered]@{
    timestamp      = (Get-Date).ToString('o')
    windowsOpen    = $wvCount -gt 0
    hostRssMb      = [math]::Round($proc.WorkingSet64 / 1MB, 1)
    hostPrivateMb  = [math]::Round($proc.PrivateMemorySize64 / 1MB, 1)
    cpuPercent     = [math]::Round($cpuPercent, 2)
    threads        = $proc.Threads.Count
    handles        = $proc.HandleCount
    webviewProcs   = $wvCount
    webviewRssMb   = $wvRss
    totalRssMb     = [math]::Round($proc.WorkingSet64 / 1MB + $wvRss, 1)
}

$report.GetEnumerator() | ForEach-Object { "{0,-14} {1}" -f $_.Key, $_.Value }

$benchDir = Join-Path (Split-Path -Parent $PSScriptRoot) 'benchmarks'
New-Item -ItemType Directory -Path $benchDir -Force | Out-Null
$out = Join-Path $benchDir 'idle.json'
$report | ConvertTo-Json -Depth 4 | Set-Content $out -Encoding UTF8
Write-Host ""
Write-Host "written to $out" -ForegroundColor Green
