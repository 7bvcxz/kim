# Milestone A validation: persistent sessions and reconnection

Status: **current** (Step 14, issue #14) · Validates [ADR 0003](adr/0003-subak-host-process-model.md),
[ADR 0004](adr/0004-pty-and-process-ownership.md), [ADR 0016](adr/0016-conpty-host-validation.md),
[ADR 0019](adr/0019-secure-host-supervisor-and-ipc-v1.md),
[ADR 0020](adr/0020-terminal-session-lifecycle.md),
[ADR 0025](adr/0025-pane-programs-environment-and-backpressure.md),
[ADR 0027](adr/0027-subak-cli-structure-and-headless-control.md) and
[ADR 0030](adr/0030-host-terminal-model-scrollback-and-replay.md) · Requirements: `SUBAK-FR-001`,
`SUBAK-FR-002`, `SUBAK-FR-003`, `SUBAK-FR-004`, `SUBAK-FR-012`, `SUBAK-FR-013`, `SUBAK-FR-015`,
`SUBAK-FR-016`, `SUBAK-FR-044`, `KIM-FR-012`, `KIM-NFR-001`, `KIM-SEC-012`

> **Milestone A is not complete.** Its acceptance needs one manual run on a real desktop: a
> Windows Terminal window is closed while a live Claude Code session runs in a Subak Terminal,
> and a new client reattaches to that same process. Agents may not open windows or start an
> interactive Claude Code session on the maintainer's machine, so that run is **pending**. The
> exact procedure is §6 and its results table §6.4. Everything else of this document was
> measured on real Windows, as stated row by row.

## 1. Result

| Acceptance criterion (issue #14) | Status | Evidence |
| --- | --- | --- |
| An actual Windows Terminal close does not kill the live Claude Code session; a new client can reattach to that same running process | **not verified — manual run pending** | Procedure and script: §6. Verified headlessly instead: a console window that started the Host and ran its client closed with `WM_CLOSE`, clients killed, their console hosts killed, and a stand-in interactive program that kept its pid and its memory through all of it (§3, §4). A Host started from a Windows Terminal tab's process tree is in no job (§4) |
| CMD, PowerShell, independent sessions and reconnection under failure are tested with reproducible commands and evidence | **verified** (automated, real ConPTY) on Windows 10 19045 and on the CI runner (Windows Server 2025, 26100) | §3 and §4; the headless smoke run §3.3 |
| Baseline resource metrics and failures are documented; Milestone A is only complete if live-Windows acceptance passes | metrics and failures **documented** (§5, §7); Milestone A **not complete** | §5, §7, §6.4 |

## 2. Scope and plan

Milestone A is Subak's first milestone validation (the risk register is reviewed at Steps 14,
20, 26, 31 and 39). It covers what Steps 08–13 built — the detached Host and its ConPTY panes,
the identity-checked IPC, named Terminals, pane programs, the CLI and scrollback — and asks one
question: **does work in a Subak Terminal survive everything that happens to the terminals that
show it, and can a user or an agent come back to the same live process?**

| Layer | What it runs | Where | When |
| --- | --- | --- | --- |
| Integration suite [`crates/subak/tests/milestone_a.rs`](../crates/subak/tests/milestone_a.rs) | Real `subak.exe` Hosts on private endpoints, real ConPTY, real `subak attach` clients and raw IPC clients; headless | this machine, CI (`windows-latest`) | every `cargo test` on Windows |
| Earlier suites, carried forward | `conpty_host.rs`, `host_ipc.rs`, `session_lifecycle.rs`, `pty_shells.rs`, `subak_cli.rs`, `scrollback_replay.rs` (unchanged, passing) | same | same |
| Smoke script [`scripts/milestone-a-smoke.ps1`](../scripts/milestone-a-smoke.ps1), phase `Auto` | The CLI as a user runs it from PowerShell, headless | this machine (by hand) | on demand |
| Smoke script, phases `Prepare` `Record` `Verify` `Finish` | The real Windows Terminal close with a real Claude Code session | the maintainer's desktop | **pending** (§6) |
| E2E scenario `e2e/tests/step14_milestone_a.rs` | Release binaries, the same journeys with `tasklist` as an independent witness | `cargo xtask e2e` only | at the 15-Step cadence (AGENTS.md §4); not run in this Step |
| Measurements (`milestone_a_baseline_measurements`) | Host start, memory, idle CPU, attach latency | this machine (debug and release), CI (debug, short) | every `cargo test`; full sizes with `SUBAK_MILESTONE_A_FULL=1` |

**Stand-ins instead of Claude Code.** An interactive Claude Code session spends tokens and uses
the maintainer's account, so no test starts one; `claude --version` (non-interactive) is the only
Claude Code command run, inside a pane. The role "long-running interactive program" is played by
a stand-in that behaves like one for every purpose that matters here: it is a full-screen program
(alternate screen, a status bar, a prompt), it keeps state in memory, and it answers every line
with its turn number, its own pid and the first thing it was told —
`standin-answer-<turn>-pid-<pid>-first-<first>-said-<line>` — so a reattached client proves by
the answer that it talks to the same live process with the same memory. The suite's stand-in is
the test binary itself (`standin_helper`); the smoke script's is a PowerShell `Read-Host` loop.
A **development server** is played by a TCP listener on an ephemeral `127.0.0.1` port that counts
the requests it answers, started by typing its command into a `cmd.exe` pane (so it is a
grandchild in that pane's job, like `npm run dev`).

**"No accidental child kill"** is checked by process handle (or pid plus start time in the
script), never by pid alone: after every client-side event, every witnessed process must be the
same live process and the Host must list every Terminal as `running` with the same pane pid.

## 3. Automated evidence

### 3.1 Commands

```sh
cargo test -p subak --test milestone_a                                  # the suite (~60 s)
SUBAK_MILESTONE_A_FULL=1 cargo test -p subak --test milestone_a milestone_a_baseline_measurements
SUBAK_MILESTONE_A_FULL=1 cargo test --release -p subak --test milestone_a milestone_a_baseline_measurements
powershell -NoProfile -ExecutionPolicy Bypass -File scripts\milestone-a-smoke.ps1      # Auto phase
```

Every `EVIDENCE` line goes straight to stderr, so it is printed without `--nocapture` and also
appears in the log of a passing CI run. The suite runs its tests in parallel, except the
measurement test, which runs alone.

### 3.2 The suite

Numbers are from one parallel run of the whole suite on this machine (debug build, 2026-11-04:
32 passed in 56 s, no `subak.exe` left afterwards). The same tests pass on the CI runner; its
numbers (run 41777666722) were of the same order: the killed Host's tree ended within 11 ms, a new
Host started in 91 ms, a hung Host was reported after 5.0 s; 60 cycles left handles 101 → 101 and
threads 10 → 10 (+16 KiB); during the burst the worst `host.health` was 3 ms and the worst answer
from the other program 111 ms.

| Test | What happens | Measured here |
| --- | --- | --- |
| `a_detached_host_keeps_shells_a_dev_server_and_an_interactive_program_through_every_client_event` | A console window (created hidden) runs `subak new agent -- <stand-in>`, which starts the Host on demand, then `subak attach agent`; the stand-in is told `remember-kiwi`; the window gets `WM_CLOSE` (what its close button sends). Then `cmd.exe` and Windows PowerShell Terminals; a dev server typed into `cmd`; nine client-side events (below); reattach | window shell 100772, attach client 59680, console host 96340 all ended; Host 101472, stand-in 103272, `cmd` 102908, PowerShell 98384, dev server 86200 survived **9 events with the same pids**; the stand-in answered turn 3 still remembering turn 1; PowerShell kept `$kim`; the dev server answered requests 2 and 3 with no client attached; after `host stop` every process ended and its port was closed |
| `concurrent_sessions_and_clients_stay_independent_and_consistent` | 4 Terminals (2 `cmd`, 2 stand-ins) from 4 concurrent `new`; 16 clients (per Terminal 2 interactive and 1 read-only `subak attach`, 1 raw viewer) type and read at once; two writers drive one stand-in; then the clients of 3 Terminals are ended at the same moment in different ways | one Host for all four `new` (exactly one reported `started`); no client saw another Terminal's output; both writers and all viewers saw turn 10 of one counter; killing (c1), `Ctrl+B d` and end of input (c2) and `subak detach` (s1) at once left s2's clients and every program untouched |
| `sixty_attach_detach_cycles_keep_the_program_and_leak_nothing` | 6 warm-up cycles, then 60 cycles rotating `Ctrl+B d`, end of input, `TerminateProcess`, `subak detach`, a raw IPC connection dropped without detaching, and a killed client of another Terminal; each stand-in cycle sends a line and checks the answer | 60 cycles in 4.6 s; Host handles **100 → 100**, threads **9 → 9**, private bytes **1.62 → 1.62 MiB** (+8 KiB; min of 5 samples before and after); the stand-in answered all 56 turns as one pid |
| `scrollback_after_reattach_is_complete_ordered_and_never_repeated` | 300 lines while attached, 200 more with no CLI client, 1000 more while a client is killed mid-burst; a new `subak attach`; its stdout is fed into a terminal model, as the user's terminal would show it; then a full-screen program's client is killed and reattached | the reattached terminal equals the Host's own capture, **1513 history and screen lines**, with `sb-line-1` … `sb-line-1500` each **once and in order**, 0 evicted; the full-screen stand-in's screen came back identical to the Host's |
| `output_bursts_and_resize_storms_never_block_or_end_anything` | 20 000 lines (2.8 MiB) while one client never reads, another sends 121 `pane.resize`, and 10 read-only clients attach and are killed or detached by client id; meanwhile `host.health` and another Terminal's program are asked | every resize answered, final 97×33 reached `cmd` (`mode con`); worst `host.health` **2 ms**, worst answer from the other program **114 ms**; 0 panics, 0 dropped clients; the slow client stayed connected |
| `host_failure_ends_its_panes_as_documented_and_leaves_no_stale_endpoint` | The Host is killed with a `cmd`, a dev server grandchild, a stand-in, 2 CLI clients and a raw client; then `new` on the same endpoint; then the new Host is suspended (hung) | `cmd`, stand-in, dev server and 2 pseudo console hosts ended **within 8 ms**; both CLI clients exited **6** (`host_disconnected`, "the Host closed the connection"); the dev server's port was closed; `ls` reported no Host; the next `new` started a fresh Host in **82 ms** with only the new Terminal (the old id: `target_not_found`); the hung Host was reported `host_unresponsive` after **5.0 s** by `ls` and `new`, was neither killed nor replaced, and was intact after it resumed |
| `milestone_a_baseline_measurements` | §5 | §5 |

The nine client-side events of the journey test, each followed by the survival check:

1. the console window that started the Host and runs the attach closes (`WM_CLOSE`);
2. a client attached to the `cmd` pane running the dev server is killed (`TerminateProcess`);
3. a PowerShell client detaches by the end of its input;
4. a stand-in client detaches with `Ctrl+B d`;
5. the console host of a client in its own console window is killed (the client did not end by
   itself within 5 s; it was then killed);
6. a read-only client loses its output reader;
7. `subak detach agent` from another terminal while a client is attached;
8. three clients of three Terminals are killed at the same moment;
9. reattaching to `agent` and `ps`.

### 3.3 Headless smoke run

`scripts/milestone-a-smoke.ps1` (phase `Auto`) on this machine, 2026-11-04, debug `subak`, 15 s,
no window (abridged; the full transcript is the script's evidence file):

```text
== Create Terminals with subak new on \\.\pipe\subak-mila-smoke-bbafabcb (the Host starts on demand)
PASS  subak new started Host pid 91748
Host: subak.exe pid 91748 (started 2026-11-04T04:59:57.1595460Z); job breakaway: True, in a job: False
Terminal cmd: cmd.exe pid 31644 · ps: powershell.exe pid 98432 · dev: cmd.exe pid 72244 · agent: powershell.exe pid 100432
PASS  the dev server answers on 127.0.0.1:2905 (powershell.exe pid 101860 ...)
PASS  it is a child of the dev pane's shell (pid 72244)
PASS  after a killed client, all 6 processes are the same live processes
PASS  the dev server kept serving with no client (hits 2)
PASS  after the stand-in client was killed, all 6 processes are the same live processes
PASS  end of input detached the client (exit 0)
PASS  claude --version ran in Terminal cmd: 2.1.318 (Claude Code)
PASS  Ctrl+B d detached the client (exit 0)
PASS  subak detach agent detached one client
PASS  the dev server still counts with nobody attached (hits 3)
PASS  the stand-in answered turn 2 as pid 100432, remembering turn 1
PASS  PowerShell kept $kim
PASS  cmd.exe kept KIMSTATE
PASS  host stop without --kill-terminals is refused (exit 7)
PASS  host stop --kill-terminals --yes (exit 0)
PASS  every process ended with the Host (6 checked)
PASS  nothing listens on 127.0.0.1:2905 any more
all checks passed
```

The script's manual phases were also dry-run headlessly with a simulated Claude Code (a renamed
copy of the suite's stand-in, `claude-sim-step14.exe`), a throwaway `cmd.exe` playing the tab's
shell, and the attach client killed to play the window closing: Prepare, Record, Verify and
Finish all passed, including the marker-code checks. This checks the script, **not** Windows
Terminal or Claude Code. The existing Step 08 smoke script (`scripts/conpty-smoke.ps1`, Auto)
still passes unchanged.

### 3.4 Carried forward

The Step 08–13 evidence still holds and is not repeated here: launcher jobs with and without
breakaway, Ctrl+C, console attach and window resize ([CONPTY_PROOF §4](CONPTY_PROOF.md)); identity
checks, fuzzing, idle exit, concurrent starts ([IPC_PROTOCOL §8](contracts/IPC_PROTOCOL.md));
the Terminal state machine and isolation ([SESSION_MODEL §7](SESSION_MODEL.md)); programs,
environment and backpressure ([PTY_PROCESSES](PTY_PROCESSES.md)); console restore on every
client exit path ([SUBAK_CLI §5](SUBAK_CLI.md)); snapshot replay and resync
([SCROLLBACK §8](SCROLLBACK.md)).

## 4. Failure and concurrency matrix

| Event | Expected (documented) | Result | Evidence |
| --- | --- | --- | --- |
| Client detaches: `Ctrl+B d`, end of input, `subak detach` from outside, Ctrl+Break | nothing in the Host changes | **verified**: pids and state unchanged, client exits 0 | journey, concurrent, cycles; Ctrl+Break: `subak_cli.rs` |
| Client crashes (`TerminateProcess`), one or several at once | as a detach | **verified** | journey (events 2, 8), cycles, concurrent, scrollback, burst |
| A client's console window closes (`WM_CLOSE`), including the window that started the Host | the window's processes end; the Host and its panes do not | **verified headlessly** (hidden console) | journey (event 1); `conpty_host.rs` |
| A client's console host is killed | the Host is unaffected; the client may linger | **verified**; the client did not end by itself within 5 s (§7.3) | journey (event 5) |
| A client's output reader goes away | the client detaches | **verified** (exit 0) | journey (event 6) |
| A **Windows Terminal** window or tab closes | like a console window, if Windows Terminal does not put its shells in a kill-on-close job without breakaway | **not verified — manual** (§6). Indirect: a Host started from a WT 1.24 tab's process tree reported `job_breakaway: true`, `in_job: false` (smoke run and manual dry run, §3.3) | §6 |
| 4 Terminals, 16 clients, two writers into one program | independent output; one consistent state per program | **verified** | concurrent |
| 60 attach/detach cycles in six ways | no growth of handles, threads, memory; same program | **verified** (100 → 100 handles, 9 → 9 threads, +8 KiB) | cycles |
| Reattach after output, a killed client and a full-screen program | the same history and screen, every line once | **verified** through the real CLI client | scrollback; Step 13 |
| Output burst + resize storm + client churn + a client that never reads | nothing blocks or dies; last size wins | **verified** | burst; Step 11/13 storms |
| The Host is killed | every pane process, grandchild and pseudo console host ends (kill-on-close jobs, ADR 0004); clients report `host_disconnected` | **verified** (within 8 ms; exit 6; port closed) | host failure; `conpty_host.rs` |
| A new Host after a crash | starts at once on the same endpoint, with no stale Terminal | **verified** (82 ms; old id `target_not_found`) | host failure; `host_ipc.rs` |
| The Host hangs | reported `host_unresponsive`, never killed or replaced | **verified** (5.0 s to detect) | host failure; `host_ipc.rs` |
| Explicit `host stop` | refused without `--kill-terminals`, then needs `--yes`; ends everything | **verified** | journey, smoke |
| Launcher job without breakaway is closed | the Host dies with it (risk R-03) | **verified** (Step 08) | `conpty_host.rs` |
| Sleep, hibernate, lock, RDP disconnect | everything survives (frozen and resumed) | **not verified — manual** | [CONPTY_PROOF §7.2–7.3](CONPTY_PROOF.md) |
| Logoff, restart, shutdown | every process of the session ends; nothing can survive (restore: Steps 29–31) | **not verified — manual** (expected by design) | [CONPTY_PROOF §7.3](CONPTY_PROOF.md) |

## 5. Measurements

### 5.1 Environment

| | This machine | CI runner |
| --- | --- | --- |
| OS | Windows 10 Pro 22H2, build 19045.6466, x64, user session 1, inbox ConPTY | Windows Server 2025, build 26100.33674 (GitHub `windows-latest`) |
| CPU, memory | 13th Gen Intel Core i5-13600KF, 14 cores / 20 logical processors, 31.8 GiB | 4 logical processors; the runner's CPU varies (run 41777666722: AMD family 25 model 1; run 41776166173: Intel family 6 model 106) |
| Build | `subak` 0.0.1 of this branch, debug and release (`lto = "thin"`, `opt-level = 3`), toolchain 1.99.1 (GNU) | debug (MSVC toolchain) |
| Other software | Windows PowerShell 5.1.19041.6456; Windows Terminal 1.24.261001001 installed (not used by any test); Claude Code 2.1.318 (only `--version`) | PowerShell 7 present |
| Conditions | the maintainer's desktop in use, other build jobs possibly running: latencies include that noise; idle CPU is the Host's own CPU time and is not affected | shared runner; the measurement test runs alone in its binary |

### 5.2 Numbers

Full sizes (`SUBAK_MILESTONE_A_FULL=1`: 10 Host starts, 60 s idle windows, 100 attaches, 20 CLI
attaches) here; the short sizes (5, 15 s, 30, 10) on CI, from the log of the passing CI run
41777666722 (commit `acd49a3`). "Private" is the Host's commit charge
(`PrivateUsage`), "working set" its resident memory, both from `host.stats` (ADR 0012); neither
includes the pane programs or their pseudo console hosts (`conhost.exe`, one per pane).

| Metric | Here, release | Here, debug | CI, debug | Budget |
| --- | --- | --- | --- | --- |
| `subak host start --no-shell` (CLI start to exit; includes the starter's 50 ms polling of the new pipe) | p50 73 ms, p95 75 ms, max 75 ms | p50 74, p95 77, max 77 ms | p50 76, p95 78, max 78 ms | none |
| First `host health` after the start | p50 21 ms, max 26 ms | p50 18, max 19 ms | p50 20, max 22 ms | none |
| `subak new` with the Host started on demand (Host + `cmd.exe` pane) | p50 86 ms, p95 88 ms, max 88 ms | p50 85, p95 86, max 86 ms | p50 89, p95 93, max 93 ms | none |
| Host memory, no Terminal (median), private / working set | 1.04 / 4.96 MiB | 1.10 / 6.05 MiB | 1.07 / 6.76 MiB | none for the Host¹ |
| … 1 Terminal (`cmd`) | 1.34 / 5.56 MiB | 1.43 / 6.74 MiB | 1.34 / 7.40 MiB | none¹ |
| … 4 Terminals (2 `cmd`, PowerShell, stand-in) | 2.00 / 6.08 MiB | 2.11 / 7.27 MiB | 2.28 / 8.13 MiB | none¹ |
| … and 2 × 2 000 history rows (the terminal model reports 513 KiB) | 2.48 / 6.61 MiB, peak private 2.52 MiB | 2.59 / 7.82 MiB, peak 2.66 MiB | 2.78 / 8.66 MiB (562 KiB), peak 2.86 MiB | none¹ |
| **Idle CPU**, 4 idle Terminals, no client | **0 ms over 60 s (0.000 %)** | 0 ms over 60 s | 0 ms over 15 s | `KIM-NFR-001`: < 0.1 % of one core over 60 s — **met** |
| Idle CPU with 2 attached clients | 0 ms over 60 s | 0 ms over 60 s | 0 ms over 15 s | `KIM-NFR-001` — **met** |
| Host threads during the idle window | 13 → 12 | 13 → 12 | 13 → 13 (14 → 13 in run 41776166173) | — (no thread appeared²) |
| `pane.attach` → whole snapshot received, pane with 2 000 history rows (90 593 bytes) | p50 0.4 ms, p95 0.5 ms, max 0.6 ms | p50 1.1, p95 1.2, max 1.4 ms | p50 1.7, p95 1.9, max 2.0 ms (90 630 bytes) | none |
| … full-screen stand-in (≈250 bytes) | p50 0.1, p95 0.1, max 0.1 ms | p50 0.3, p95 0.4, max 0.5 ms | p50 0.5, p95 0.6, max 0.7 ms (307 bytes) | none |
| `subak attach` process start → the pane's screen shown | p50 27 ms, p95 27 ms, max 29 ms | p50 27, p95 27, max 29 ms | p50 27, p95 28, max 28 ms | none |
| Memory growth over 60 attach/detach cycles (min of 5 samples) | — | +8 KiB (another run: +28 KiB); handles 100 → 100; threads 9 → 9 | +16 KiB; handles 101 → 101; threads 10 → 10 | none (leak guard only³) |

1. No requirement sets a memory number for the Subak Host. `KIM-NFR-002` (Kim CLI < 30 MB, Kim
   Manager idle < 80 MB) is for the Kim manager; [ARCHITECTURE §8](ARCHITECTURE.md) cites it only
   for the measurement method. The numbers are reported, not judged.
2. A thread *ended* during the idle windows on both machines (most likely a pooled worker thread
   retiring after its idle timeout). The check is that none appeared: an idle Host has no timer
   or polling thread (KIM-NFR-001).
3. The cycle test's assertions (≤ 20 more handles, no more threads, < 4 MiB more private bytes)
   are leak detectors — one handle or one snapshot leaked per cycle would exceed them — not
   performance budgets.

**Reading them.** Starting a Host costs about 75 ms of wall time here, most of it the starter
waiting for the pipe; an idle Host with four Terminals uses no measurable CPU (below one 15.6 ms
scheduler tick in 60 s) and about 2.5 MiB of private memory; attaching costs about a millisecond
in the Host plus the CLI's own start (~27 ms). The debug and release builds differ little, because
the Host mostly waits.

## 6. Manual acceptance procedure (maintainer)

This is acceptance criterion 1 of issue #14. It needs a visible Windows Terminal window and an
interactive Claude Code session with your account, which is why no agent ran it.
`scripts/milestone-a-smoke.ps1` never starts Claude Code and never closes a window: it only
records and checks. Everything runs on a private endpoint (`subak-milestone-a`), so your default
per-user Host is never touched.

### 6.1 Before you start

1. Build `subak` (`cargo build -p subak --release`), or pass `-Subak <path>` to the script.
2. Start in the repository root. Below, `smoke` stands for
   `powershell -NoProfile -ExecutionPolicy Bypass -File scripts\milestone-a-smoke.ps1`.
3. Optional: run `smoke` (phase `Auto`) once; it must end with `all checks passed`.

### 6.2 Steps

| # | Where | What you do | What the script checks and records |
| --- | --- | --- | --- |
| 1 | A **Windows Terminal** tab (any profile) | `smoke -Phase Prepare -Attach` | Windows build, `subak` and Claude Code versions (`claude --version`), Windows Terminal version, this tab's process chain; runs `subak new claude` (a `cmd.exe` Terminal; the Host starts on demand from inside the tab) and records the Host and shell pids and start times, `job_breakaway` and `in_job`; prints a marker code such as `kim-4711`; then attaches this tab to `claude` |
| 2 | The attached pane | Type `claude` (Claude Code starts in the pane), then ask: *Remember the code kim-4711. Reply with only: OK kim-4711* — and wait for the answer | — |
| 3 | A **second tab** of the same window | `smoke -Phase Record` | The Claude Code process(es) below the pane's shell (pid and start time), the attached client, and that the marker is on the pane's screen (≥ 2 occurrences) |
| 4 | — | Close the **whole Windows Terminal window** with its X button and confirm closing all tabs | — |
| 5 | A console that is **not** that window: Win+R → `cmd` | `smoke -Phase Verify` | The tab's shell and the attach client are gone (the window really closed); the Host, the pane's shell and Claude Code are the **same live processes**; `claude` is still `running` with the same pid; the marker is still on the pane's screen |
| 6 | A **new** Windows Terminal window | `subak --endpoint subak-milestone-a attach claude`; check that the conversation is still shown; ask, without typing the code: *What code did I ask you to remember? Reply with only that code followed by -again* | — |
| 7 | Any console | `smoke -Phase Finish -Cleanup` | The same Host, shell and Claude Code processes; `kim-4711-again` is now on the pane's screen and was not at step 5 (only the remembering answer after the reattach can contain it, even if the program repainted or cleared its screen when the new window resized the pane); then stops the private Host (this ends that Claude Code session) |

**Expected:** every phase ends with `all checks passed`, and in step 6 Claude Code answers with
`kim-4711-again`. **If the Host is gone at step 5**, Windows Terminal put the tab in a kill-on-close job
without breakaway: note `job_breakaway`/`in_job` from step 1 and reopen risk R-03.

The evidence file `%TEMP%\subak-milestone-a\subak-milestone-a-evidence.md` collects every phase:
PASS/FAIL lines, pids, start times, exit codes and versions, with paths shown relative to
`%USERPROFILE%`. It never contains terminal content or conversation text (`KIM-SEC-012`): the
marker is checked by counting, inside the script. Paste the file into issue #14.

### 6.3 Variants (optional, same script)

- Close only the **tab** (not the window) at step 4; run Verify from another tab or console.
- Run step 1 from the Start menu's *Command Prompt* (a conhost window) and close it with its X.
- Start a long-running development server in the pane instead of Claude Code (for example
  `npm run dev` in a project), close the window, and check from another console that it still
  answers.
- Sleep, hibernate and logoff: [CONPTY_PROOF §7.2–7.3](CONPTY_PROOF.md) (`scripts/conpty-smoke.ps1`).

### 6.4 Results (pending — to be filled in by the maintainer)

| Check | Date | Result | Evidence |
| --- | --- | --- | --- |
| Windows Terminal window closed with a live Claude Code session; Host, shell and Claude Code pids unchanged (Verify) | — | **pending** | — |
| New Windows Terminal window reattached; the same Claude Code process answered with the code (Finish) | — | **pending** | — |
| Tab close only (variant) | — | pending | — |
| Command Prompt (conhost) window close (variant) | — | pending | — |

Milestone A becomes **complete** when the first two rows pass.

## 7. Defects and open issues

### 7.1 Found and fixed in this Step

- **Test-side:** the first CI run of the measurement test failed because the Host's thread count
  went from 14 to 13 during the idle window (a thread retiring), and the check demanded equality.
  The check now waits for a stable thread count and asserts that no thread *appeared*, which is
  what the requirement is about. Not a product defect.

### 7.2 Product defects

**None found.** The suite, the smoke run and the manual-phase dry run found no defect in the Host
or the CLI on this machine or on the CI runner: no process died on any client-side event, no
state was lost across reattach, nothing leaked over 60 cycles, and Host failures behaved as
documented.

### 7.3 Open issues and limits (not defects of this Step)

| Issue | Status | Where |
| --- | --- | --- |
| Real Windows Terminal close with a live Claude Code session (acceptance 1) | **pending manual run**; Milestone A not complete | §6 |
| Windows Terminal tab close, conhost window close, VS Code terminal, SSH sessions | not verified (manual; VS Code and SSH need their environments) | §6.3, R-03 |
| Sleep, hibernate, lock, RDP, logoff, restart | not verified (manual) | CONPTY_PROOF §7 |
| A Host crash or a logoff ends every Terminal | by design until persistence and recovery (Steps 29–31) | R-05, SESSION_MODEL §6 |
| A client whose console host is killed does not end by itself (it holds its attachment until killed or detached with `subak detach`) | known since Step 08; harmless to the Host; a real window close ends the client through `CTRL_CLOSE_EVENT` | CONPTY_PROOF §6 |
| A killed client leaves its own console in raw mode | known since Step 12; nothing can run in a killed process | SUBAK_CLI §5 |
| Detecting a hung Host takes about 5 s (the handshake timeout) | by design (ADR 0019) | IPC_PROTOCOL §8 |
| Measurements exclude the per-pane `conhost.exe` and the pane programs | stated in §5 | §5 |

## 8. Risk review

| Risk | After Step 14 |
| --- | --- |
| R-01 inbox ConPTY differences | The whole suite passes on 19045 and on the CI runner's 26100; builds in between untested |
| R-02 ConPTY re-renders output | A reattached CLI client equals the Host's model exactly (§3.2), including a full-screen program |
| R-03 Host dies with a launcher job without breakaway | Unchanged in substance: confirmed in Step 08; a Host started from a WT 1.24 tab's process tree is in no job; the real window close is §6 |
| R-04 programs escaping the pane job | Unchanged (Step 11); a dev server grandchild stayed in its pane's job and ended with it |
| R-05 a Host crash ends every session | Measured end to end (§3.2); impact stays high until Steps 29–31 |
| R-08 misbehaving clients | 16 concurrent clients, killed and detached at once, 60 cycles, a client that never reads: no effect on others |
| R-06, R-07, R-09 – R-11 | Not exercised beyond Steps 09–13 |

## 9. Reproducing

```sh
cargo test -p subak                                               # every Subak suite, incl. milestone_a
cargo test -p subak --test milestone_a                            # this Step's suite with EVIDENCE lines
SUBAK_MILESTONE_A_FULL=1 cargo test --release -p subak --test milestone_a milestone_a_baseline_measurements
powershell -NoProfile -ExecutionPolicy Bypass -File scripts\milestone-a-smoke.ps1
cargo xtask e2e                                                   # only when the ledger says it is due
```

The tests run only on Windows (`#![cfg(windows)]`); on other targets CI only compiles them
(ADR 0011).
