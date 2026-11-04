<#
.SYNOPSIS
  Step 14 / Milestone A smoke test: Subak Terminals keep running when the terminal window that
  runs their client closes, and a new client reattaches to the same live processes
  (docs/MILESTONE_A_VALIDATION.md, issue #14).

.DESCRIPTION
  Phases

    Auto     (default; headless) Creates Terminals on a private endpoint with `subak new` (the
             Host starts on demand): cmd.exe, Windows PowerShell, a cmd.exe running a small
             development server on 127.0.0.1 (an ephemeral port), and a PowerShell stand-in for
             an interactive program that remembers what it was told. Kills, detaches and
             reattaches headless clients, checks by process id and start time that every process
             survived, that the dev server kept counting and that the stand-in and PowerShell
             kept their state, runs `claude --version` in a pane when Claude Code is on PATH,
             then stops the Host and checks that nothing is left. Opens no window and never
             starts an interactive Claude Code session.

    The manual check of the acceptance criterion (a real Windows Terminal window closed while a
    live Claude Code session runs in a Subak Terminal) is split into four phases. The script
    never starts Claude Code interactively and never closes a window: you do both.

    Prepare  Run inside a Windows Terminal tab. Runs `subak new <Terminal>` (default `claude`, a
             cmd.exe Terminal) on a private endpoint (default `subak-milestone-a`), records the
             Host, the pane's shell, the tab's own processes and the Windows Terminal version,
             and prints what to do next. -Attach attaches this tab right away.
    Record   Run after Claude Code answered in the pane, from another tab or another console,
             while the Windows Terminal window is still open. Records the Claude Code
             process ids and the attached client.
    Verify   Run after closing the whole Windows Terminal window, from a console that is not
             that window (Win+R, `cmd`). Checks that the tab and its client are gone while the
             Host, the shell and Claude Code are the same live processes, and that the
             conversation is still on the pane's screen (by a marker code, never by content).
    Finish   Run after reattaching from a NEW Windows Terminal window and asking Claude Code
             again. Checks that the same Claude Code process answered, prints the evidence.
             -Cleanup then stops the private Host (this ends that Claude Code session).

  Evidence: every phase appends PASS/FAIL lines, process ids, start times, exit codes and
  versions to %TEMP%\subak-milestone-a\<endpoint>-evidence.md, ready to paste into the issue.
  It never contains terminal content or conversation text; paths are shown relative to
  %USERPROFILE%. Nothing here installs, updates, elevates or touches the default per-user Host.

.EXAMPLE
  powershell -NoProfile -ExecutionPolicy Bypass -File scripts\milestone-a-smoke.ps1
.EXAMPLE
  # The manual acceptance run (docs/MILESTONE_A_VALIDATION.md section 6):
  powershell -NoProfile -ExecutionPolicy Bypass -File scripts\milestone-a-smoke.ps1 -Phase Prepare -Attach
  powershell -NoProfile -ExecutionPolicy Bypass -File scripts\milestone-a-smoke.ps1 -Phase Record
  powershell -NoProfile -ExecutionPolicy Bypass -File scripts\milestone-a-smoke.ps1 -Phase Verify
  powershell -NoProfile -ExecutionPolicy Bypass -File scripts\milestone-a-smoke.ps1 -Phase Finish -Cleanup
#>
[CmdletBinding()]
param(
    [ValidateSet('Auto', 'Prepare', 'Record', 'Verify', 'Finish')]
    [string]$Phase = 'Auto',
    [string]$Endpoint = '',
    [string]$Terminal = 'claude',
    [string]$Subak = '',
    [string]$EvidenceDir = (Join-Path $env:TEMP 'subak-milestone-a'),
    [switch]$Attach,
    [switch]$Cleanup
)

$ErrorActionPreference = 'Stop'
$script:failures = 0
$script:clients = New-Object System.Collections.ArrayList

# ---------------------------------------------------------------------------------------------
# Output and evidence

function Hide-User([string]$text) {
    if ($env:USERPROFILE) { $text = $text.Replace($env:USERPROFILE, '%USERPROFILE%') }
    return $text
}

function Write-Evidence([string]$line) {
    Add-Content -Path $script:evidence -Value (Hide-User $line) -Encoding UTF8
}

function Step([string]$text) {
    Write-Host "`n== $text" -ForegroundColor Cyan
    Write-Evidence "`n**$text**`n"
}
function Note([string]$text) { Write-Host $text; Write-Evidence "- $text" }
function Pass([string]$text) { Write-Host "PASS  $text" -ForegroundColor Green; Write-Evidence "- PASS $text" }
function Fail([string]$text) {
    Write-Host "FAIL  $text" -ForegroundColor Red
    Write-Evidence "- **FAIL** $text"
    $script:failures++
}
function Check([bool]$ok, [string]$text) { if ($ok) { Pass $text } else { Fail $text } }
function Stamp { return (Get-Date).ToUniversalTime().ToString('yyyy-MM-ddTHH:mm:ss.fffZ') }

# ---------------------------------------------------------------------------------------------
# subak

function Find-Subak {
    if ($Subak) { return (Resolve-Path $Subak).Path }
    $candidates = @()
    if ($env:CARGO_TARGET_DIR) {
        $candidates += Join-Path $env:CARGO_TARGET_DIR 'release\subak.exe'
        $candidates += Join-Path $env:CARGO_TARGET_DIR 'debug\subak.exe'
    }
    $candidates += Join-Path $PSScriptRoot '..\target\release\subak.exe'
    $candidates += Join-Path $PSScriptRoot '..\target\debug\subak.exe'
    foreach ($c in $candidates) { if (Test-Path $c) { return (Resolve-Path $c).Path } }
    $cmd = Get-Command subak.exe -ErrorAction SilentlyContinue
    if ($cmd) { return $cmd.Source }
    throw 'subak.exe not found: build it (cargo build -p subak --release) or pass -Subak <path>'
}

# `subak --json --endpoint <ours> <arguments>`: options always come before any `--`.
function Invoke-SubakJson([string[]]$arguments) {
    $out = & $script:exe --json --endpoint $script:ep @arguments
    $code = $LASTEXITCODE
    $doc = $out | Out-String | ConvertFrom-Json
    return @{ Code = $code; Doc = $doc }
}

function Get-HostStatus {
    $r = Invoke-SubakJson @('host', 'status')
    if ($r.Code -ne 0) { return $null }
    return $r.Doc.data
}

function Get-TerminalInfo([string]$name) {
    $r = Invoke-SubakJson @('info', $name)
    if ($r.Code -ne 0) { return $null }
    return $r.Doc.data.terminal
}

# How often `$text` occurs in the pane's lines (history and screen). Only the count is kept.
function Get-MarkerCount([string]$name, [string]$text) {
    $r = Invoke-SubakJson @('capture', $name, '--history')
    if ($r.Code -ne 0) { return -1 }
    $n = 0
    foreach ($line in $r.Doc.data.lines) { $n += ([regex]::Matches([string]$line, [regex]::Escape($text))).Count }
    return $n
}

# ---------------------------------------------------------------------------------------------
# Processes, identified by pid AND start time (a pid alone can be reused)

function Get-ProcessTable {
    return @(Get-CimInstance Win32_Process |
            Select-Object ProcessId, ParentProcessId, Name, CreationDate, CommandLine)
}

function Get-StartTime($p) { return $p.CreationDate.ToUniversalTime().ToString('o') }

function Get-ProcessRecord([int]$processId) {
    $p = Get-CimInstance Win32_Process -Filter "ProcessId = $processId" -ErrorAction SilentlyContinue
    if (-not $p) { return $null }
    return [ordered]@{ pid = [int]$p.ProcessId; name = [string]$p.Name; started = (Get-StartTime $p) }
}

function Test-SameProcess($record) {
    $now = Get-ProcessRecord ([int]$record.pid)
    return [bool]($now -and $now.started -eq $record.started)
}

function Format-Record($r) { return "$($r.name) pid $($r.pid) (started $($r.started))" }

# Descendants of `root` (children created after their parent, so stale parent pids are skipped).
function Get-Descendants([int]$root) {
    $all = Get-ProcessTable
    $top = $all | Where-Object { $_.ProcessId -eq $root } | Select-Object -First 1
    $out = New-Object System.Collections.ArrayList
    if (-not $top) { return $out }
    $queue = New-Object System.Collections.Queue
    $queue.Enqueue($top)
    while ($queue.Count -gt 0) {
        $p = $queue.Dequeue()
        foreach ($c in $all) {
            if ($c.ParentProcessId -eq $p.ProcessId -and $c.ProcessId -ne $p.ProcessId -and
                $c.CreationDate -ge $p.CreationDate) {
                [void]$out.Add($c)
                $queue.Enqueue($c)
            }
        }
    }
    return $out
}

# This process's ancestors, nearest first (parents created before their children).
function Get-Ancestors([int]$start) {
    $all = Get-ProcessTable
    $chain = New-Object System.Collections.ArrayList
    $p = $all | Where-Object { $_.ProcessId -eq $start } | Select-Object -First 1
    while ($p) {
        $parent = $all | Where-Object { $_.ProcessId -eq $p.ParentProcessId } | Select-Object -First 1
        if (-not $parent -or $parent.CreationDate -gt $p.CreationDate) { break }
        [void]$chain.Add([ordered]@{ pid = [int]$parent.ProcessId; name = [string]$parent.Name; started = (Get-StartTime $parent) })
        $p = $parent
    }
    return $chain
}

# Claude Code below the pane's shell: the native claude.exe, or node running an npm install.
function Get-ClaudeProcesses([int]$shellPid) {
    $found = New-Object System.Collections.ArrayList
    foreach ($p in (Get-Descendants $shellPid)) {
        $isClaude = ($p.Name -like 'claude*') -or
            ($p.Name -eq 'node.exe' -and [string]$p.CommandLine -match 'claude')
        if ($isClaude) {
            [void]$found.Add([ordered]@{ pid = [int]$p.ProcessId; name = [string]$p.Name; started = (Get-StartTime $p) })
        }
    }
    return $found
}

# ---------------------------------------------------------------------------------------------
# Headless clients (the same technique as scripts/conpty-smoke.ps1): `subak attach` with
# redirected stdin, stdout and stderr and no window.

function New-Client([string]$target, [string]$extra = '') {
    $psi = New-Object System.Diagnostics.ProcessStartInfo
    $psi.FileName = $script:exe
    $psi.Arguments = "--endpoint $($script:ep) attach $target --size 100x30 $extra"
    $psi.UseShellExecute = $false
    $psi.CreateNoWindow = $true
    $psi.RedirectStandardInput = $true
    $psi.RedirectStandardOutput = $true
    $psi.RedirectStandardError = $true
    $p = [System.Diagnostics.Process]::Start($psi)
    [void]$script:clients.Add($p)
    return [pscustomobject]@{
        Process = $p
        Text    = New-Object System.Text.StringBuilder
        Mark    = 0
        Buffers = @((New-Object byte[] 65536), (New-Object byte[] 4096))
        Tasks   = @($null, $null)
        Streams = @($p.StandardOutput.BaseStream, $p.StandardError.BaseStream)
    }
}

function Update-Client($c) {
    for ($i = 0; $i -lt 2; $i++) {
        while ($true) {
            $t = $c.Tasks[$i]
            if ($null -eq $t) {
                $c.Tasks[$i] = $c.Streams[$i].ReadAsync($c.Buffers[$i], 0, $c.Buffers[$i].Length)
                continue
            }
            if ($t -is [string] -or -not $t.IsCompleted) { break }
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
    return [regex]::Replace($raw, "$esc\[[0-9;?]*[ -/]*[@-~]|$esc\][^$bel]*($bel|$esc\\)|$esc[()][0-9A-B]", '')
}

# Waits for `pattern` in what the client printed since the last match.
function Wait-Text($c, [string]$pattern, [int]$seconds = 30) {
    $until = (Get-Date).AddSeconds($seconds)
    $text = ''
    while ((Get-Date) -lt $until) {
        $all = Get-Visible $c
        $text = $all.Substring([Math]::Min($c.Mark, $all.Length))
        if ($text -match $pattern) { $c.Mark = $all.Length; return $text }
        Start-Sleep -Milliseconds 100
    }
    $tail = $text.Substring([Math]::Max(0, $text.Length - 1200))
    throw "timed out waiting for /$pattern/ from client pid $($c.Process.Id); last output:`n$tail"
}

function Send-Keys($c, [string]$text) {
    $bytes = [System.Text.Encoding]::UTF8.GetBytes($text)
    $c.Process.StandardInput.BaseStream.Write($bytes, 0, $bytes.Length)
    $c.Process.StandardInput.BaseStream.Flush()
}

function Wait-Exit($c, [int]$seconds = 15) {
    if (-not $c.Process.WaitForExit($seconds * 1000)) { return $null }
    return $c.Process.ExitCode
}

function Wait-Attached([int]$count) {
    $until = (Get-Date).AddSeconds(20)
    while ((Get-Date) -lt $until) {
        $st = Get-HostStatus
        if ($st -and [int]$st.clients.attached -eq $count) { return $true }
        Start-Sleep -Milliseconds 100
    }
    return $false
}

# ---------------------------------------------------------------------------------------------
# The stand-ins of the Auto phase

# An interactive program that remembers what it was told (Read-Host loop).
$StandinScript = @'
$n = 0; $first = $null
Write-Host "standin-ready-pid-$PID"
while ($true) {
    $l = Read-Host 'standin'
    if ($l -eq 'quit') { break }
    if (-not $l) { continue }
    $n++
    if (-not $first) { $first = $l }
    Write-Host ('standin-answer-{0}-pid-{1}-first-{2}-said-{3}' -f $n, $PID, $first, $l)
}
'@

# A development server on 127.0.0.1 (an ephemeral port) that counts the requests it answers.
$DevServerScript = @'
param([string]$PortFile)
$l = New-Object System.Net.Sockets.TcpListener ([System.Net.IPAddress]::Loopback, 0)
$l.Start()
$port = $l.LocalEndpoint.Port
Set-Content -Path ($PortFile + '.tmp') -Value $port -Encoding ascii
Move-Item -Force ($PortFile + '.tmp') $PortFile
Write-Host "devserver-listening-$port-pid-$PID"
$hits = 0
while ($true) {
    $c = $l.AcceptTcpClient()
    $s = $c.GetStream()
    $buf = New-Object byte[] 4096
    [void]$s.Read($buf, 0, $buf.Length)
    $hits++
    $body = "hits=$hits pid=$PID`n"
    $resp = "HTTP/1.0 200 OK`r`nContent-Type: text/plain`r`nContent-Length: $($body.Length)`r`nConnection: close`r`n`r`n$body"
    $b = [System.Text.Encoding]::ASCII.GetBytes($resp)
    $s.Write($b, 0, $b.Length)
    $c.Close()
    Write-Host "devserver-hit-$hits"
}
'@

function Invoke-DevServer([int]$port) {
    $c = New-Object System.Net.Sockets.TcpClient
    $c.Connect('127.0.0.1', $port)
    $s = $c.GetStream()
    $req = [System.Text.Encoding]::ASCII.GetBytes("GET / HTTP/1.0`r`nHost: 127.0.0.1`r`n`r`n")
    $s.Write($req, 0, $req.Length)
    $text = (New-Object System.IO.StreamReader($s)).ReadToEnd()
    $c.Close()
    if ($text -match 'hits=(\d+) pid=(\d+)') { return @{ Hits = [int]$Matches[1]; ProcessId = [int]$Matches[2] } }
    throw "not a dev server answer: $text"
}

function Test-PortClosed([int]$port) {
    try { Invoke-DevServer $port | Out-Null; return $false } catch { return $true }
}

function New-Terminal([string]$name, [string[]]$program) {
    $r = Invoke-SubakJson (@('new', $name, '--size', '100x30') + $program)
    if ($r.Code -ne 0) { throw "subak new $name failed: $($r.Doc.error.code): $($r.Doc.error.message)" }
    return $r.Doc.data
}

function Get-PaneRecord($created) {
    return Get-ProcessRecord ([int]$created.terminal.windows[0].panes[0].pid)
}

# Every process that must survive a client-side event, by pid and start time.
function Test-Survivors($survivors, [string]$event) {
    $dead = @($survivors | Where-Object { -not (Test-SameProcess $_) })
    Check ($dead.Count -eq 0) ("after $event, all $($survivors.Count) processes are the same live processes" +
        $(if ($dead.Count) { '; gone: ' + (($dead | ForEach-Object { Format-Record $_ }) -join ', ') } else { '' }))
}

# ---------------------------------------------------------------------------------------------
# Phases

function Invoke-Auto {
    Step "Create Terminals with subak new on \\.\pipe\$($script:ep) (the Host starts on demand)"
    $ps = @('--env', 'POWERSHELL_UPDATECHECK=Off', '--env', 'POWERSHELL_TELEMETRY_OPTOUT=1')
    $standin = [Convert]::ToBase64String([System.Text.Encoding]::Unicode.GetBytes($StandinScript))
    $cmd = New-Terminal 'cmd' @('--', 'cmd.exe', '/d')
    $hostPid = [int]$cmd.host.pid
    Check ([bool]$cmd.host.started) "subak new started Host pid $hostPid"
    $psT = New-Terminal 'ps' ($ps + @('--', 'powershell', '-NoLogo', '-NoProfile'))
    $dev = New-Terminal 'dev' @('--', 'cmd.exe', '/d')
    $agent = New-Terminal 'agent' ($ps + @('--', 'powershell', '-NoLogo', '-NoProfile', '-EncodedCommand', $standin))
    $st = Get-HostStatus
    $hostRec = Get-ProcessRecord $hostPid
    Note ("Host: $(Format-Record $hostRec); job breakaway: $($st.host.job_breakaway), in a job: $($st.host.in_job)")
    $survivors = New-Object System.Collections.ArrayList
    [void]$survivors.Add($hostRec)
    foreach ($t in @($cmd, $psT, $dev, $agent)) {
        $rec = Get-PaneRecord $t
        [void]$survivors.Add($rec)
        Note "Terminal $($t.name): $(Format-Record $rec)"
    }
    $agentPid = [int]$agent.terminal.windows[0].panes[0].pid

    Step 'Start a development server in the dev pane (typed, like npm run dev)'
    $serverFile = Join-Path $EvidenceDir "$($script:ep)-devserver.ps1"
    $portFile = Join-Path $EvidenceDir "$($script:ep)-port.txt"
    Set-Content -Path $serverFile -Value $DevServerScript -Encoding UTF8
    Remove-Item $portFile -ErrorAction SilentlyContinue
    $c = New-Client 'dev'
    Wait-Text $c '>' | Out-Null
    Send-Keys $c "powershell -NoLogo -NoProfile -ExecutionPolicy Bypass -File `"$serverFile`" `"$portFile`"`r"
    Wait-Text $c 'devserver-listening-\d+' 60 | Out-Null
    $until = (Get-Date).AddSeconds(20)
    while (-not (Test-Path $portFile) -and (Get-Date) -lt $until) { Start-Sleep -Milliseconds 100 }
    $port = [int](Get-Content $portFile -Raw).Trim()
    $h = Invoke-DevServer $port
    $devRec = Get-ProcessRecord $h.ProcessId
    [void]$survivors.Add($devRec)
    $devShell = [int]$dev.terminal.windows[0].panes[0].pid
    Check ($h.Hits -eq 1) "the dev server answers on 127.0.0.1:$port ($(Format-Record $devRec))"
    $parent = (Get-CimInstance Win32_Process -Filter "ProcessId = $($h.ProcessId)").ParentProcessId
    Check ([int]$parent -eq $devShell) "it is a child of the dev pane's shell (pid $devShell)"

    Step 'Client killed (TerminateProcess) while attached to the dev pane'
    $c.Process.Kill()
    [void]$c.Process.WaitForExit(10000)
    Test-Survivors $survivors 'a killed client'
    $h = Invoke-DevServer $port
    Check ($h.Hits -eq 2 -and $h.ProcessId -eq $devRec.pid) "the dev server kept serving with no client (hits $($h.Hits))"

    Step 'The interactive stand-in remembers; its client is killed'
    $a = New-Client 'agent'
    Wait-Text $a 'standin-ready-pid-\d+' 120 | Out-Null
    Send-Keys $a "remember-kiwi`r"
    Wait-Text $a "standin-answer-1-pid-$agentPid-first-remember-kiwi-said-remember-kiwi" | Out-Null
    $a.Process.Kill()
    [void]$a.Process.WaitForExit(10000)
    Test-Survivors $survivors 'the stand-in client was killed'

    Step 'PowerShell keeps a variable; its client detaches by the end of its input'
    $p = New-Client 'ps'
    $text = ''
    for ($i = 0; $i -lt 5 -and $text -notmatch 'ps-ready'; $i++) {
        try {
            Wait-Text $p 'PS .*>' 120 | Out-Null
            Send-Keys $p "`$kim = 'ps-state-' + (6*7); 'ps-rea' + 'dy'`r"
            $text = Wait-Text $p 'ps-ready' 10
        } catch { Write-Host "PowerShell did not answer yet (attempt $i)" }
    }
    $p.Process.StandardInput.Close()
    $code = Wait-Exit $p
    Check ($code -eq 0) "end of input detached the client (exit $code)"
    Test-Survivors $survivors 'a client detached by the end of its input'

    Step 'Claude Code inside a pane (version only; no session is started)'
    $k = New-Client 'cmd'
    Wait-Text $k '>' | Out-Null
    if (Get-Command claude -ErrorAction SilentlyContinue) {
        Send-Keys $k "claude --version & echo mark^er-claude-done`r"
        $text = Wait-Text $k 'marker-claude-done' 60
        $line = ($text -split "`r?`n" | Where-Object { $_ -match '\d+\.\d+\.\d+' -and $_ -notmatch 'Microsoft' } | Select-Object -Last 1)
        Check ([bool]$line) "claude --version ran in Terminal cmd: $(([string]$line).Trim())"
    } else {
        Note 'SKIP claude is not on PATH (not tested)'
    }
    Send-Keys $k "set KIMSTATE=cmd-state-7`r"
    Send-Keys $k ([string][char]2 + 'd')
    $code = Wait-Exit $k
    Check ($code -eq 0) "Ctrl+B d detached the client (exit $code)"
    Test-Survivors $survivors 'Ctrl+B d'

    Step 'subak detach from outside while a client is attached'
    $d = New-Client 'agent'
    Wait-Text $d 'standin-answer-1' | Out-Null
    $r = Invoke-SubakJson @('detach', 'agent')
    Check ($r.Code -eq 0 -and @($r.Doc.data.detached).Count -eq 1) 'subak detach agent detached one client'
    $code = Wait-Exit $d
    Check ($code -eq 0) "the detached client exited (exit $code)"
    Test-Survivors $survivors 'subak detach'
    Check (Wait-Attached 0) 'no client is attached; every Terminal keeps running'
    $h = Invoke-DevServer $port
    Check ($h.Hits -eq 3) "the dev server still counts with nobody attached (hits $($h.Hits))"

    Step 'Reattach: the same live processes with their state'
    $a = New-Client 'agent'
    Wait-Text $a 'standin-answer-1' | Out-Null
    Send-Keys $a "turn-two`r"
    $ok = $true
    try { Wait-Text $a "standin-answer-2-pid-$agentPid-first-remember-kiwi-said-turn-two" | Out-Null } catch { $ok = $false }
    Check $ok "the stand-in answered turn 2 as pid $agentPid, remembering turn 1"
    Send-Keys $a ([string][char]2 + 'd')
    [void](Wait-Exit $a)
    $p = New-Client 'ps'
    $ok = $false
    for ($i = 0; $i -lt 5 -and -not $ok; $i++) {
        Send-Keys $p "'ps-' + 'got-' + `$kim`r"
        try { Wait-Text $p 'ps-got-ps-state-42' 10 | Out-Null; $ok = $true } catch { }
    }
    Check $ok 'PowerShell kept $kim'
    Send-Keys $p ([string][char]2 + 'd')
    [void](Wait-Exit $p)
    $k = New-Client 'cmd'
    Wait-Text $k '>' | Out-Null
    Send-Keys $k "echo %KIMSTATE%-mark^er`r"
    $ok = $true
    try { Wait-Text $k 'cmd-state-7-marker' | Out-Null } catch { $ok = $false }
    Check $ok 'cmd.exe kept KIMSTATE'
    Send-Keys $k ([string][char]2 + 'd')
    [void](Wait-Exit $k)
    Test-Survivors $survivors 'reattaching'

    Step 'Stop the Host explicitly (with consent)'
    $r = Invoke-SubakJson @('host', 'stop')
    Check ($r.Code -eq 7 -and $r.Doc.error.code -eq 'terminals_running') 'host stop without --kill-terminals is refused (exit 7)'
    $r = Invoke-SubakJson @('host', 'stop', '--kill-terminals', '--yes')
    Check ($r.Code -eq 0) "host stop --kill-terminals --yes (exit $($r.Code))"
    Start-Sleep -Milliseconds 500
    $left = @($survivors | Where-Object { Test-SameProcess $_ })
    Check ($left.Count -eq 0) "every process ended with the Host ($($survivors.Count) checked)"
    Check (Test-PortClosed $port) "nothing listens on 127.0.0.1:$port any more"
    Remove-Item $serverFile, $portFile -ErrorAction SilentlyContinue
}

function Save-State($state) { $state | ConvertTo-Json -Depth 6 | Set-Content -Encoding UTF8 $script:stateFile }

function Read-State {
    if (-not (Test-Path $script:stateFile)) { throw "no state file $($script:stateFile); run -Phase Prepare -Endpoint $($script:ep) first" }
    return Get-Content $script:stateFile -Raw | ConvertFrom-Json
}

# Host, shell and (when recorded) Claude Code: the same live processes.
function Test-Recorded($state, [string]$when) {
    Check (Test-SameProcess $state.host) "$when the Host is the same live process: $(Format-Record $state.host)"
    Check (Test-SameProcess $state.shell) "$when the pane's shell is the same live process: $(Format-Record $state.shell)"
    $st = Get-HostStatus
    Check ($st -and [int]$st.host.pid -eq [int]$state.host.pid) "$when subak host status reports Host pid $($state.host.pid)"
    $t = Get-TerminalInfo $Terminal
    Check ($t -and $t.state -eq 'running' -and [int]$t.windows[0].panes[0].pid -eq [int]$state.shell.pid -and $t.id -eq $state.terminal_id) `
        "$when Terminal $Terminal ($($state.terminal_id)) is running with the same shell pid"
    if ($state.claude) {
        foreach ($c in @($state.claude)) {
            Check (Test-SameProcess $c) "$when Claude Code is the same live process: $(Format-Record $c)"
        }
    }
    return $st
}

function Invoke-Prepare {
    Step 'Environment'
    Note "Windows: $([Environment]::OSVersion.VersionString); session $((Get-Process -Id $PID).SessionId); PowerShell $($PSVersionTable.PSVersion)"
    Note "subak: $(& $script:exe --version)"
    $claude = Get-Command claude -ErrorAction SilentlyContinue
    if ($claude) { Note "Claude Code: $((& claude --version) | Select-Object -First 1)" } else { Note 'Claude Code: not on PATH' }
    $wt = $null
    try {
        $wtp = Get-Process WindowsTerminal -ErrorAction SilentlyContinue | Select-Object -First 1
        if ($wtp) { $wt = (Get-Item $wtp.Path).VersionInfo.ProductVersion }
    } catch { }
    Note "Windows Terminal: $(if ($wt) { $wt } else { 'version unknown' }); this tab: $(if ($env:WT_SESSION) { 'WT_SESSION set' } else { 'WT_SESSION not set' })"
    if (-not $env:WT_SESSION) {
        Write-Host 'WARNING: this does not look like a Windows Terminal tab (WT_SESSION is not set).' -ForegroundColor Yellow
        Write-Evidence '- WARNING: Prepare did not run inside Windows Terminal (WT_SESSION not set)'
    }
    $chain = Get-Ancestors $PID
    Note ("this tab's process chain: " + (($chain | ForEach-Object { "$($_.name) $($_.pid)" }) -join ' <- '))

    Step "subak new $Terminal on \\.\pipe\$($script:ep)"
    if (Get-HostStatus) {
        throw "a Host already serves $($script:ep); stop it first (subak --endpoint $($script:ep) host stop --kill-terminals --yes) or pass another -Endpoint"
    }
    $t0 = Stamp
    $r = Invoke-SubakJson @('new', $Terminal)
    if ($r.Code -ne 0) { throw "subak new $Terminal failed: $($r.Doc.error.code): $($r.Doc.error.message)" }
    $d = $r.Doc.data
    $st = Get-HostStatus
    $state = [ordered]@{
        endpoint      = $script:ep
        terminal      = $Terminal
        terminal_id   = $d.terminal_id
        marker        = 'kim-' + (Get-Random -Minimum 1000 -Maximum 9999)
        prepared_at   = $t0
        wt_session    = $env:WT_SESSION
        tab           = @($chain)
        host          = (Get-ProcessRecord ([int]$d.host.pid))
        shell         = (Get-ProcessRecord ([int]$d.terminal.windows[0].panes[0].pid))
        job_breakaway = $st.host.job_breakaway
        in_job        = $st.host.in_job
        claude        = $null
        clients       = $null
    }
    Check ([bool]$d.host.started) "subak new started $(Format-Record $state.host) (job breakaway: $($state.job_breakaway), in a job: $($state.in_job))"
    Note "Terminal $Terminal ($($state.terminal_id)): shell $(Format-Record $state.shell)"
    Note "marker code for this run: $($state.marker)"
    Save-State $state

    $sc = "powershell -NoProfile -ExecutionPolicy Bypass -File `"$PSCommandPath`""
    Write-Host @"

Next (docs/MILESTONE_A_VALIDATION.md section 6):
  1. In THIS tab: subak --endpoint $($script:ep) attach $Terminal   (done for you with -Attach)
  2. In the pane, start Claude Code yourself:  claude
  3. Ask it:  Remember the code $($state.marker). Reply with only: OK $($state.marker)
  4. When it answered, from ANOTHER tab of this window run:
       $sc -Phase Record -Endpoint $($script:ep)
  5. Close the WHOLE Windows Terminal window with its X button (confirm closing all tabs).
  6. From a console that is not that window (Win+R, cmd) run:
       $sc -Phase Verify -Endpoint $($script:ep)
"@
    if ($Attach) {
        Step "Attaching this tab to $Terminal (Ctrl+B d detaches; for the check, close the window instead)"
        & $script:exe --endpoint $script:ep attach $Terminal
    }
}

function Invoke-Record {
    $state = Read-State
    Step "Record Claude Code in $Terminal (prepared $($state.prepared_at))"
    $null = Test-Recorded $state 'before the window closes,'
    $claude = @(Get-ClaudeProcesses ([int]$state.shell.pid))
    Check ($claude.Count -gt 0) ("Claude Code runs in the pane: " + (($claude | ForEach-Object { Format-Record $_ }) -join ', '))
    $st = Get-HostStatus
    $clients = @($st.clients.list | Where-Object { $_.attached -and $_.kind -eq 'cli' } | ForEach-Object { Get-ProcessRecord ([int]$_.pid) } | Where-Object { $_ })
    Check ($clients.Count -gt 0) ("a client is attached: " + (($clients | ForEach-Object { Format-Record $_ }) -join ', '))
    $n = Get-MarkerCount $Terminal $state.marker
    Check ($n -ge 2) "the marker code $($state.marker) is on the pane's screen ($n occurrences: question and answer)"
    $state.claude = $claude
    $state.clients = $clients
    $state | Add-Member -NotePropertyName recorded_at -NotePropertyValue (Stamp) -Force
    $state | Add-Member -NotePropertyName marker_count_recorded -NotePropertyValue $n -Force
    Save-State $state
    Write-Host "`nNext: close the WHOLE Windows Terminal window with its X button, then from another console run -Phase Verify."
}

function Invoke-Verify {
    $state = Read-State
    Step "Verify after the Windows Terminal window was closed (prepared $($state.prepared_at))"
    Note "verified at $(Stamp)"
    if ($env:WT_SESSION -and $env:WT_SESSION -eq $state.wt_session) {
        Fail 'Verify runs in the same Windows Terminal tab as Prepare: close that window first and use another console'
    }
    if (-not $state.claude) { Fail 'Record was not run: the Claude Code process ids are unknown' }
    $tab = @($state.tab | Select-Object -First 1)
    if ($tab.Count -gt 0) {
        Check (-not (Test-SameProcess $tab[0])) "the tab's shell is gone (the window was closed): $(Format-Record $tab[0])"
    }
    foreach ($c in @($state.clients)) {
        if ($c) { Check (-not (Test-SameProcess $c)) "the window's attach client is gone: $(Format-Record $c)" }
    }
    $st = Test-Recorded $state 'after the window closed,'
    if ($st) { Note "clients attached now: $($st.clients.attached)" }
    $n = Get-MarkerCount $Terminal $state.marker
    Check ($n -ge [int]$state.marker_count_recorded -and $n -gt 0) "the conversation is still on the pane's screen (marker $($state.marker): $n occurrences)"
    # The follow-up answer (`<marker>-again`) can only appear after the reattach.
    $again = Get-MarkerCount $Terminal "$($state.marker)-again"
    Note "the follow-up answer $($state.marker)-again is on the screen $again time(s) before the reattach"
    $state | Add-Member -NotePropertyName verified_at -NotePropertyValue (Stamp) -Force
    $state | Add-Member -NotePropertyName marker_count_verified -NotePropertyValue $n -Force
    $state | Add-Member -NotePropertyName again_count_verified -NotePropertyValue $again -Force
    Save-State $state
    $sc = "powershell -NoProfile -ExecutionPolicy Bypass -File `"$PSCommandPath`""
    Write-Host @"

Next:
  1. Open a NEW Windows Terminal window and run:  subak --endpoint $($script:ep) attach $Terminal
  2. Check that the conversation is still shown, then ask Claude Code (do not type the code):
       What code did I ask you to remember? Reply with only that code followed by -again
  3. When it answered (stay attached or press Ctrl+B d), from any console run:
       $sc -Phase Finish -Endpoint $($script:ep)        (add -Cleanup to stop the Host afterwards)
"@
}

function Invoke-Finish {
    $state = Read-State
    Step "Finish: the reattached client talked to the same Claude Code process"
    $null = Test-Recorded $state 'after reattaching,'
    # A full-screen program may clear and repaint the screen (and history) when the new window
    # resizes the pane, so the check is a token only the answer after the reattach contains.
    $again = Get-MarkerCount $Terminal "$($state.marker)-again"
    Check ($again -gt [int]$state.again_count_verified) "Claude Code answered the follow-up after the reattach, remembering the code ($($state.marker)-again: $($state.again_count_verified) -> $again occurrences)"
    $n = Get-MarkerCount $Terminal $state.marker
    Note "marker $($state.marker): $($state.marker_count_verified) occurrences at Verify, $n now"
    Note "finished at $(Stamp); Host $($state.host.pid), shell $($state.shell.pid), Claude Code $((@($state.claude) | ForEach-Object { $_.pid }) -join ', ')"
    if ($Cleanup) {
        Step 'Cleanup: stop the private Host (this ends that Claude Code session)'
        $r = Invoke-SubakJson @('host', 'stop', '--kill-terminals', '--yes')
        Check ($r.Code -eq 0) "host stop --kill-terminals --yes (exit $($r.Code))"
        Start-Sleep -Milliseconds 500
        Check (-not (Test-SameProcess $state.host)) 'the Host is gone'
        Remove-Item $script:stateFile -ErrorAction SilentlyContinue
    } else {
        Write-Host "`nThe Host keeps running. Stop it with: subak --endpoint $($script:ep) host stop --kill-terminals --yes"
    }
}

# ---------------------------------------------------------------------------------------------

$script:exe = Find-Subak
if (-not $Endpoint) {
    if ($Phase -eq 'Auto') { $Endpoint = 'subak-mila-smoke-' + [guid]::NewGuid().ToString('N').Substring(0, 8) }
    else { $Endpoint = 'subak-milestone-a' }
}
$script:ep = $Endpoint
New-Item -ItemType Directory -Force -Path $EvidenceDir | Out-Null
$script:stateFile = Join-Path $EvidenceDir "$Endpoint.json"
$script:evidence = Join-Path $EvidenceDir "$Endpoint-evidence.md"
if (-not (Test-Path $script:evidence)) {
    Set-Content -Path $script:evidence -Encoding UTF8 -Value "### Subak Milestone A smoke evidence (endpoint ``$Endpoint``)"
}
Write-Evidence "`n#### Phase $Phase at $(Stamp)"
Write-Host ("subak: {0} ({1})" -f (Hide-User $script:exe), (& $script:exe --version))
Write-Host ("Windows: {0}  user session: {1}" -f [Environment]::OSVersion.VersionString, (Get-Process -Id $PID).SessionId)
Write-Evidence ("- subak ``{0}`` ({1}); Windows {2}; session {3}" -f (Hide-User $script:exe), (& $script:exe --version),
    [Environment]::OSVersion.Version, (Get-Process -Id $PID).SessionId)

try {
    switch ($Phase) {
        'Auto' { Invoke-Auto }
        'Prepare' { Invoke-Prepare }
        'Record' { Invoke-Record }
        'Verify' { Invoke-Verify }
        'Finish' { Invoke-Finish }
    }
} catch {
    Fail "aborted: $($_.Exception.Message)"
} finally {
    foreach ($p in $script:clients) { if (-not $p.HasExited) { $p.Kill() } }
    if ($Phase -eq 'Auto') {
        # Leave nothing behind: the smoke Host is always stopped.
        if (Get-HostStatus) {
            & $script:exe --endpoint $script:ep host stop --kill-terminals --yes | Out-Null
            Write-Host "cleanup: stopped the smoke Host on $($script:ep)"
        }
    }
}

Write-Host "`nevidence: $(Hide-User $script:evidence)"
if ($script:failures -gt 0) {
    Write-Evidence "- result: **$($script:failures) check(s) failed**"
    Write-Host "$($script:failures) check(s) failed" -ForegroundColor Red
    exit 1
}
Write-Evidence '- result: all checks passed'
Write-Host 'all checks passed' -ForegroundColor Green
exit 0
