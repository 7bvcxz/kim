<#
.SYNOPSIS
  Step 08 smoke test: a detached Subak Host keeps its ConPTY shell running while clients
  come and go (docs/CONPTY_PROOF.md, ADR 0016).

.DESCRIPTION
  Phases
    Auto     (default, headless) Start a Host on a private endpoint, start a long-running
             counter in its shell, kill the attached client, check by process id that the Host
             and the shell still run, reattach and show that the counter kept counting, run
             `claude --version` inside the pseudo console when Claude Code is on PATH, then stop
             the Host and check that nothing is left. Opens no window.
    Prepare  Start a Host plus a counter and remember the process ids in
             %TEMP%\subak-smoke-<endpoint>.json. With -Attach, attach this console to the shell
             afterwards (use this inside Windows Terminal, then close the window).
    Verify   After closing a window, sleeping, logging off or rebooting: report whether the
             remembered Host and shell are still the same processes, reattach headlessly and
             show the counter advancing. Stops the Host unless -KeepHost.

  Every phase uses its own endpoint (never the default per-user Host) and never installs,
  updates or elevates anything. Only Prepare -Attach shows terminal output, in the console it
  is run from.

.EXAMPLE
  powershell -NoProfile -ExecutionPolicy Bypass -File scripts\conpty-smoke.ps1
.EXAMPLE
  # In a Windows Terminal tab:
  powershell -NoProfile -ExecutionPolicy Bypass -File scripts\conpty-smoke.ps1 -Phase Prepare -Endpoint subak-smoke-wt -Attach
  # Close the Windows Terminal window with its X button, then from any other console:
  powershell -NoProfile -ExecutionPolicy Bypass -File scripts\conpty-smoke.ps1 -Phase Verify -Endpoint subak-smoke-wt
#>
[CmdletBinding()]
param(
    [ValidateSet('Auto', 'Prepare', 'Verify')]
    [string]$Phase = 'Auto',
    [string]$Endpoint = '',
    [string]$Subak = '',
    [switch]$Attach,
    [switch]$KeepHost
)

$ErrorActionPreference = 'Stop'
$script:failures = 0
$script:clients = New-Object System.Collections.ArrayList

function Find-Subak {
    if ($Subak) { return (Resolve-Path $Subak).Path }
    $candidates = @()
    if ($env:CARGO_TARGET_DIR) { $candidates += Join-Path $env:CARGO_TARGET_DIR 'debug\subak.exe' }
    $candidates += Join-Path $PSScriptRoot '..\target\debug\subak.exe'
    $candidates += Join-Path $PSScriptRoot '..\target\release\subak.exe'
    foreach ($c in $candidates) { if (Test-Path $c) { return (Resolve-Path $c).Path } }
    $cmd = Get-Command subak.exe -ErrorAction SilentlyContinue
    if ($cmd) { return $cmd.Source }
    throw 'subak.exe not found: build it (cargo build -p subak) or pass -Subak <path>'
}

function Step([string]$text) { Write-Host "`n== $text" -ForegroundColor Cyan }
function Pass([string]$text) { Write-Host "PASS  $text" -ForegroundColor Green }
function Fail([string]$text) { Write-Host "FAIL  $text" -ForegroundColor Red; $script:failures++ }
function Check([bool]$ok, [string]$text) { if ($ok) { Pass $text } else { Fail $text } }

function Invoke-SubakJson([string[]]$arguments) {
    $out = & $script:exe @arguments --json
    $code = $LASTEXITCODE
    $doc = $out | Out-String | ConvertFrom-Json
    return @{ Code = $code; Doc = $doc }
}

function Get-HostStatus {
    $r = Invoke-SubakJson @('host', 'status', '--endpoint', $script:ep)
    if ($r.Code -ne 0) { return $null }
    return $r.Doc.data
}

function Test-Alive([int]$processId) {
    return [bool](Get-Process -Id $processId -ErrorAction SilentlyContinue)
}

# A headless attach client: redirected stdin/stdout/stderr, no window.
function New-Client([string]$size = '100x30') {
    $psi = New-Object System.Diagnostics.ProcessStartInfo
    $psi.FileName = $script:exe
    $psi.Arguments = "host attach --endpoint $($script:ep) --size $size"
    $psi.UseShellExecute = $false
    $psi.CreateNoWindow = $true
    $psi.RedirectStandardInput = $true
    $psi.RedirectStandardOutput = $true
    $psi.RedirectStandardError = $true
    $p = [System.Diagnostics.Process]::Start($psi)
    [void]$script:clients.Add($p)
    $c = [pscustomobject]@{
        Process = $p
        Text    = New-Object System.Text.StringBuilder
        Buffers = @((New-Object byte[] 65536), (New-Object byte[] 4096))
        Tasks   = @($null, $null)
        Streams = @($p.StandardOutput.BaseStream, $p.StandardError.BaseStream)
    }
    return $c
}

# Drains everything the client has written so far (stdout and stderr) without blocking.
function Update-Client($c) {
    for ($i = 0; $i -lt 2; $i++) {
        while ($true) {
            $t = $c.Tasks[$i]
            if ($null -eq $t) {
                $c.Tasks[$i] = $c.Streams[$i].ReadAsync($c.Buffers[$i], 0, $c.Buffers[$i].Length)
                continue
            }
            if ($t -is [string] -or -not $t.IsCompleted) { break }  # 'eof', or nothing new yet
            if ($t.Status -eq 'RanToCompletion' -and $t.Result -gt 0) {
                [void]$c.Text.Append([System.Text.Encoding]::UTF8.GetString($c.Buffers[$i], 0, $t.Result))
                $c.Tasks[$i] = $null
            } else {
                $c.Tasks[$i] = 'eof'
            }
        }
    }
}

function Get-Visible($c) {
    Update-Client $c
    $raw = $c.Text.ToString()
    $esc = [char]27
    $bel = [char]7
    # Windows PowerShell 5.1 has no `e escape; strip CSI and OSC sequences.
    return [regex]::Replace($raw, "$esc\[[0-9;?]*[ -/]*[@-~]|$esc\][^$bel]*($bel|$esc\\)", '')
}

function Wait-Text($c, [string]$pattern, [int]$seconds = 20) {
    $until = (Get-Date).AddSeconds($seconds)
    while ((Get-Date) -lt $until) {
        $text = Get-Visible $c
        if ($text -match $pattern) { return $text }
        Start-Sleep -Milliseconds 100
    }
    $tail = $text.Substring([Math]::Max(0, $text.Length - 1200))
    throw "timed out waiting for /$pattern/ from client pid $($c.Process.Id); last output:`n$tail"
}

function Send-Keys($c, [string]$text) {
    # Raw bytes: the .NET Framework StandardInput writer may prepend a UTF-8 BOM to the first
    # write, which the shell would receive as typed characters.
    $bytes = [System.Text.Encoding]::UTF8.GetBytes($text)
    $c.Process.StandardInput.BaseStream.Write($bytes, 0, $bytes.Length)
    $c.Process.StandardInput.BaseStream.Flush()
}

function Get-LastTick([string]$text) {
    $m = [regex]::Matches($text, 'tick-(\d+)')
    if ($m.Count -eq 0) { return 0 }
    return [int]$m[$m.Count - 1].Groups[1].Value
}

$CounterCommand = 'for /l %i in (1,1,100000) do @(echo tick-%i& ping -n 2 127.0.0.1 >nul)'

function Start-HostWithCounter {
    Step "Start a detached Host on \\.\pipe\$($script:ep)"
    $r = Invoke-SubakJson @('host', 'start', '--endpoint', $script:ep)
    if ($r.Code -ne 0) { throw "host start failed: $($r.Doc.error.message)" }
    $d = $r.Doc.data
    $state = [ordered]@{
        endpoint      = $script:ep
        host_pid      = [int]$d.host.pid
        shell_pid     = [int]$d.pane.pid
        started_at    = $d.host.started_at
        job_breakaway = $d.host.job_breakaway
        in_job        = $d.host.in_job
    }
    Write-Host ("Host pid {0} (job breakaway: {1}, in a job: {2}); shell pid {3} ({4})" -f `
            $state.host_pid, $state.job_breakaway, $state.in_job, $state.shell_pid, $d.pane.command)
    Check (Test-Alive $state.host_pid) "Host process $($state.host_pid) runs"
    Check (Test-Alive $state.shell_pid) "shell process $($state.shell_pid) runs"

    Step 'Start a long-running counter in the shell'
    $c = New-Client
    Send-Keys $c ($CounterCommand + "`r")
    $text = Wait-Text $c 'tick-3\b'
    $state.tick_before = Get-LastTick $text
    Write-Host "counter at tick $($state.tick_before)"
    return @{ State = $state; Client = $c }
}

function Show-Reattach($state, [int]$tickBefore) {
    Step 'Reattach from a new client and read the counter'
    $c = New-Client
    $text = Wait-Text $c ("tick-" + ($tickBefore + 3) + '\b') 30
    $tick = Get-LastTick $text
    Check ($tick -gt $tickBefore + 2) "the counter kept running while no client was attached (tick $tickBefore -> $tick)"
    return $c
}

function Stop-SmokeHost($state) {
    Step 'Stop the Host explicitly'
    $r = Invoke-SubakJson @('host', 'stop', '--endpoint', $script:ep)
    Check ($r.Code -eq 7 -and $r.Doc.error.code -eq 'confirmation_required') 'stop without --yes is refused (exit 7)'
    $r = Invoke-SubakJson @('host', 'stop', '--endpoint', $script:ep, '--yes')
    Check ($r.Code -eq 0) "host stop --yes (shell exit code $($r.Doc.data.shell.exit_code), forced: $($r.Doc.data.shell.forced))"
    Start-Sleep -Milliseconds 500
    Check (-not (Test-Alive $state.host_pid)) "Host process $($state.host_pid) is gone"
    Check (-not (Test-Alive $state.shell_pid)) "shell process $($state.shell_pid) is gone"
    $r = Invoke-SubakJson @('host', 'status', '--endpoint', $script:ep)
    Check ($r.Code -eq 3) 'host status now reports host_not_running (exit 3)'
}

$script:exe = Find-Subak
if (-not $Endpoint) { $Endpoint = 'subak-smoke-' + [guid]::NewGuid().ToString('N').Substring(0, 8) }
$script:ep = $Endpoint
$stateFile = Join-Path $env:TEMP "subak-smoke-$Endpoint.json"
Write-Host ("subak: {0} ({1})" -f $script:exe, (& $script:exe --version))
Write-Host ("Windows: {0}  user session: {1}" -f [Environment]::OSVersion.VersionString, (Get-Process -Id $PID).SessionId)

try {
switch ($Phase) {
    'Auto' {
        $started = Start-HostWithCounter
        $state = $started.State
        $a = $started.Client

        Step 'Kill the attached client (as if its window or terminal died)'
        $clientPid = $a.Process.Id
        Stop-Process -Id $clientPid -Force
        $a.Process.WaitForExit(10000) | Out-Null
        Check (-not (Test-Alive $clientPid)) "client $clientPid is gone"
        Check (Test-Alive $state.host_pid) "Host $($state.host_pid) still runs"
        Check (Test-Alive $state.shell_pid) "shell $($state.shell_pid) still runs"
        $st = Get-HostStatus
        Check ($st -and [int]$st.host.pid -eq $state.host_pid -and [int]$st.pane.pid -eq $state.shell_pid) `
            "status reports the same Host and shell ($($st.pane.job_processes) process(es) in the pane job)"

        Start-Sleep -Seconds 5
        $b = Show-Reattach $state $state.tick_before

        Step 'Detach with Ctrl+B d'
        Send-Keys $b ([string][char]2 + 'd')
        Check ($b.Process.WaitForExit(10000) -and $b.Process.ExitCode -eq 0) 'client exited 0 after Ctrl+B d'
        Check ((Get-HostStatus).clients.attached -eq 0) 'no client attached; the shell keeps running'

        Step 'Claude Code inside the pseudo console (version only; no session is started)'
        $c = New-Client
        Send-Keys $c ([string][char]3)  # Ctrl+C ends the counter loop
        Start-Sleep -Milliseconds 500
        if (Get-Command claude -ErrorAction SilentlyContinue) {
            Send-Keys $c "claude --version & echo mark^er-claude-done`r"
            $text = Wait-Text $c 'marker-claude-done' 60
            $line = ($text -split "`r?`n" | Where-Object { $_ -match '\d+\.\d+\.\d+' -and $_ -notmatch 'Microsoft' } | Select-Object -Last 1)
            Check ([bool]$line) "claude --version ran inside the ConPTY: $($line.Trim())"
        } else {
            Write-Host 'SKIP  claude is not on PATH (not tested)'
        }
        Send-Keys $c ([string][char]2 + 'd')
        $c.Process.WaitForExit(10000) | Out-Null

        if ($KeepHost) {
            Write-Host "Host kept on endpoint $($script:ep); stop it with: subak host stop --endpoint $($script:ep) --yes"
        } else {
            Stop-SmokeHost $state
        }
    }
    'Prepare' {
        $started = Start-HostWithCounter
        $state = $started.State
        Send-Keys $started.Client ([string][char]2 + 'd')
        $started.Client.Process.WaitForExit(10000) | Out-Null
        $state.prepared_at = (Get-Date).ToString('o')
        $state | ConvertTo-Json | Set-Content -Encoding UTF8 $stateFile
        Write-Host "remembered in $stateFile"
        Write-Host "Next: close this window / sleep / log off, then run: conpty-smoke.ps1 -Phase Verify -Endpoint $($script:ep)"
        if ($Attach) {
            Step 'Attaching this console (Ctrl+B d detaches; or close the window now)'
            & $script:exe host attach --endpoint $script:ep
        }
    }
    'Verify' {
        if (-not (Test-Path $stateFile)) { throw "no state file $stateFile; run -Phase Prepare -Endpoint $Endpoint first" }
        $state = Get-Content $stateFile -Raw | ConvertFrom-Json
        Step "Verify the Host prepared at $($state.prepared_at)"
        $st = Get-HostStatus
        if (-not $st) {
            Write-Host ("The Host is gone (host_not_running). Host pid {0} alive: {1}; shell pid {2} alive: {3}." -f `
                    $state.host_pid, (Test-Alive $state.host_pid), $state.shell_pid, (Test-Alive $state.shell_pid))
            Write-Host 'Expected after logoff, shutdown or reboot; a failure after closing a window or sleep.'
            $script:failures++
        } else {
            Check ([int]$st.host.pid -eq $state.host_pid) "same Host process $($state.host_pid) (started $($st.host.started_at))"
            Check ([int]$st.pane.pid -eq $state.shell_pid) "same shell process $($state.shell_pid), running: $($st.pane.running)"
            $c = New-Client
            $first = Get-LastTick (Wait-Text $c 'tick-\d+' 30)
            $second = Get-LastTick (Wait-Text $c ('tick-' + ($first + 2) + '\b') 15)
            Check ($second -gt $first) "the counter is live (tick $first -> $second; $($state.tick_before) when prepared)"
            Send-Keys $c ([string][char]3 + [string][char]2 + 'd')
            $c.Process.WaitForExit(10000) | Out-Null
            if (-not $KeepHost) { Stop-SmokeHost $state; Remove-Item $stateFile -ErrorAction SilentlyContinue }
        }
    }
}
} catch {
    Fail "aborted: $($_.Exception.Message)"
} finally {
    # Leave nothing behind: headless clients are killed, and the smoke Host is stopped unless it
    # was kept on purpose (-KeepHost, or a successful Prepare).
    foreach ($p in $script:clients) { if (-not $p.HasExited) { $p.Kill() } }
    $keep = $KeepHost -or ($Phase -eq 'Prepare' -and $script:failures -eq 0)
    if (-not $keep) {
        $st = Get-HostStatus
        if ($st) {
            & $script:exe host stop --endpoint $script:ep --yes | Out-Null
            Write-Host "cleanup: stopped Host $($st.host.pid) on $($script:ep)"
        }
    }
}

if ($script:failures -gt 0) {
    Write-Host "`n$($script:failures) check(s) failed" -ForegroundColor Red
    exit 1
}
Write-Host "`nall checks passed" -ForegroundColor Green
exit 0
