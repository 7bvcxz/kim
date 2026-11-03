# Measures idle cost of the GUI evaluation prototypes (evidence for docs/adr/0014-gui-technology.md).
#
# For each binary: start it (window off-screen, not activated, no taskbar entry), wait 8 s for
# start-up to settle, then sample process CPU time and memory over a 30 s idle window.
# Usage (from research/gui-eval):  powershell -NoProfile -File measure.ps1 [-Runs 3]
param([int]$Runs = 3, [int]$Settle = 8, [int]$Window = 30)

$ErrorActionPreference = 'Stop'
$bins = @('eval-winit', 'eval-egui-glow', 'eval-egui-wgpu')
$rows = @()
foreach ($name in $bins) {
    $exe = Join-Path $PSScriptRoot "bin\$name.exe"
    if (-not (Test-Path $exe)) { Write-Warning "missing $exe"; continue }
    $size = (Get-Item $exe).Length
    for ($r = 1; $r -le $Runs; $r++) {
        $out = Join-Path $env:TEMP "kim-gui-eval-$name-$r.txt"
        $p = Start-Process -FilePath $exe -ArgumentList ($Settle + $Window + 5) -NoNewWindow -PassThru -RedirectStandardOutput $out
        Start-Sleep -Seconds $Settle
        $p.Refresh(); $cpu0 = $p.TotalProcessorTime.TotalMilliseconds
        Start-Sleep -Seconds $Window
        $p.Refresh()
        $cpu1 = $p.TotalProcessorTime.TotalMilliseconds
        $ws = $p.WorkingSet64; $priv = $p.PrivateMemorySize64; $peak = $p.PeakWorkingSet64
        $handles = $p.HandleCount; $threads = $p.Threads.Count
        $p.WaitForExit(15000) | Out-Null
        if (-not $p.HasExited) { $p.Kill() }
        $first = (Get-Content $out -ErrorAction SilentlyContinue | Select-String 'first_frame_ms=(\d+)').Matches.Groups[1].Value
        $rows += [pscustomobject]@{
            binary = $name; run = $r; size_kib = [math]::Round($size / 1KB)
            first_frame_ms = $first
            idle_cpu_ms_per_30s = [math]::Round($cpu1 - $cpu0, 1)
            idle_cpu_pct_one_core = [math]::Round(100 * ($cpu1 - $cpu0) / ($Window * 1000), 3)
            working_set_mib = [math]::Round($ws / 1MB, 1)
            private_mib = [math]::Round($priv / 1MB, 1)
            peak_ws_mib = [math]::Round($peak / 1MB, 1)
            handles = $handles; threads = $threads
        }
    }
}
$rows | Format-Table -AutoSize | Out-String -Width 200
$rows | ConvertTo-Json | Set-Content -Encoding utf8 (Join-Path $PSScriptRoot 'results.json')
