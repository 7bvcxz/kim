# Idle cost of the Noeul instance (ADR 0023). Release build, isolated profile, private endpoint,
# no tray icon, paste.float bound to Ctrl+Alt+Shift+F22 (so the hotkey thread runs as in a real
# start). Nothing is shown on the desktop and nothing is captured.
# Usage: powershell -NoProfile -File scripts\noeul-measure-idle.ps1 -Exe target\release\noeul.exe
param([string]$Exe = 'target\release\noeul.exe', [int]$IdleSeconds = 60)
$ErrorActionPreference = 'Stop'
$exe = (Resolve-Path $Exe).Path
$root = Join-Path $env:TEMP ("noeul-measure-" + $PID)
$cfg = Join-Path $root 'Roaming\mouv\noeul'
New-Item -ItemType Directory -Force $cfg | Out-Null
New-Item -ItemType Directory -Force (Join-Path $root 'Local') | Out-Null
'{"schema":"kim.hotkeys","schema_version":1,"tool":"noeul","overrides":[{"action":"paste.float","bindings":["Ctrl+Alt+Shift+F22"]}]}' |
  Set-Content -Encoding ascii (Join-Path $cfg 'hotkeys.json')
$env:APPDATA = Join-Path $root 'Roaming'
$env:LOCALAPPDATA = Join-Path $root 'Local'
$ep = "noeul-measure-$PID"
"binary_bytes=" + (Get-Item $exe).Length
$t0 = [Diagnostics.Stopwatch]::StartNew()
$start = & $exe start --no-tray --endpoint $ep --json | ConvertFrom-Json
$startMs = $t0.ElapsedMilliseconds
"start_to_answer_ms=$startMs (CLI start until the new instance answered IPC)"
$id = [int]$start.data.pid
$p = Get-Process -Id $id
Start-Sleep -Seconds 5
$p.Refresh()
$cpu0 = $p.TotalProcessorTime.TotalMilliseconds
$cs0 = ($p.Threads | Measure-Object -Property Id).Count
Start-Sleep -Seconds $IdleSeconds
$p.Refresh()
$cpu1 = $p.TotalProcessorTime.TotalMilliseconds
"idle_seconds=$IdleSeconds"
"cpu_ms_during_idle=" + ($cpu1 - $cpu0)
"working_set_mib=" + [math]::Round($p.WorkingSet64 / 1MB, 2)
"private_mib=" + [math]::Round($p.PrivateMemorySize64 / 1MB, 2)
"threads=" + $p.Threads.Count
"handles=" + $p.HandleCount
$st = & $exe status --endpoint $ep --json | ConvertFrom-Json
"registration=" + $st.data.hotkeys.registrations[0].status + " " + $st.data.hotkeys.registrations[0].keys
"image_bytes_held=" + $st.data.memory.bytes
$q = & $exe quit --yes --endpoint $ep --json | ConvertFrom-Json
"quit_exited=" + $q.data.exited
Remove-Item -Recurse -Force $root
