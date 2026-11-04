# Risk register

Status: **current** · Step 03 · Reviewed at every milestone validation (Steps 14, 20, 26, 31, 39)

Likelihood (L) and impact (I): **H**igh / **M**edium / **L**ow. "Owner step" is where the risk
is retired or re-assessed with evidence.

## Win32 ConPTY

| ID | Risk | L | I | Mitigation | Owner step |
| --- | --- | --- | --- | --- | --- |
| R-01 | Inbox ConPTY differs across Windows 10 1809 → 11 builds (resize reflow artifacts, missing passthrough, mouse input, cursor inheritance) | H | M | Detect OS build; conformance tests per build; consider shipping `conpty.dll`/`OpenConsole.exe` (MIT) if inbox gaps hurt. **Step 08:** I/O, resize, Ctrl+C and teardown verified on 19045; CI on Server 2025 (26100) showed `ClosePseudoConsole` not ending the shell (job kill required) and a win32-input-mode request that clients must not pass to their console ([CONPTY_PROOF](CONPTY_PROOF.md) §6); builds in between untested. **Step 11:** on 19045 the inbox ConPTY keeps one handle per pseudo console after `ClosePseudoConsole` (50 bare pairs: +53), ignores Ctrl+Break sent as a win32-input-mode key, and delivers output in ~83-byte chunks at ~1 MiB/s; the Host refuses most failed starts before a pseudo console exists and bounds send queues by bytes ([PTY_PROCESSES](PTY_PROCESSES.md) §4, §7, §9). **Step 14:** the whole Milestone A suite (survival, concurrency, cycles, scrollback, bursts, Host failures) passes on 19045 and on the CI runner (26100) ([MILESTONE_A_VALIDATION](MILESTONE_A_VALIDATION.md)) | 08, 11, 14, 15 |
| R-02 | ConPTY output is a re-rendered stream, not the program's raw bytes (e.g. it may repaint, reorder or drop some sequences) | M | M | Treat ConPTY as the terminal source of truth; replay via VT snapshot (ADR 0008) rather than raw byte fidelity. Step 13 (ADR 0030): measured on Windows 10 that ConPTY repaints instead of passing `?1049h` and soft wraps; the Host's model replays what ConPTY renders, and late clients equal continuous ones ([SCROLLBACK §8](SCROLLBACK.md)) | 13, 15 |
| R-03 | Host started from a terminal that lives in a Job without breakaway permission dies with that terminal | M | H | `CREATE_BREAKAWAY_FROM_JOB` attempt; report `job_breakaway=false` and `in_job`; warn on `host start`. **Step 08 measured:** confirmed (closing such a kill-on-close job ends Host, shell and conhost); breakaway is per job level; conhost window close is safe. **Step 09 measured** (ADR 0019 §10): a launcher job with silent breakaway frees the Host; in nested jobs the Host leaves only the levels that allow breakaway and dies with an outer kill-on-close job; status reports the Host's immediate job limits and `host start` the launcher's. Read-only on this machine: Windows Terminal 1.24 and its OpenConsole are in a job, but its tab shells are not, so a Host started from a tab is in no job. **Step 14 measured:** a Host that `subak new` started on demand from a process tree under a Windows Terminal 1.24 tab reported `job_breakaway: true`, `in_job: false`; a hidden console window that started the Host and ran its client was closed with `WM_CLOSE` and the Host, its panes and a grandchild survived ([MILESTONE_A_VALIDATION](MILESTONE_A_VALIDATION.md) §3). Closing a real WT window with a live Claude Code session: the maintainer's manual run (MILESTONE_A_VALIDATION §6; the script records `job_breakaway`/`in_job` and fails visibly if the Host dies). VS Code terminal and SSH: still not verified | 08, 09, 14 |

## Process trees and Job Objects

| ID | Risk | L | I | Mitigation | Owner step |
| --- | --- | --- | --- | --- | --- |
| R-04 | Programs that create their own jobs or break away escape kill-pane | M | M | Disallow breakaway in pane jobs by default; document; enumerate escaped processes in diagnostics. **Step 11 measured:** a pane's job refuses `CREATE_BREAKAWAY_FROM_JOB` (access denied, from a real child); a process started on the user's behalf by another service (Task Scheduler, WMI, a COM server) is outside any pane job by nature and is not tracked; enumerating escapes is not implemented | 11, 21 |
| R-05 | Kill-on-close means a Host crash ends every session | M | H | Panic containment per request/pane; crash tests; restore path; never run untrusted plugins in the Host. **Step 09:** `catch_unwind` per request and connection (counted in `host.stats`); 300 fuzzed connections gave 0 panics; a killed Host is replaced without stale state. **Step 14 measured end to end:** killing the Host ended every pane process, a grandchild development server (its port closed) and every pseudo console host within milliseconds, attached clients exited 6 (`host_disconnected`), and the next `new` started a fresh Host at once with none of the old Terminals; a hung Host is reported `host_unresponsive` and never killed. The impact stays high until recovery (Steps 29–31) | 09, 14, 30, 39 |
| R-06 | Graceful stop: some programs ignore `CTRL_CLOSE_EVENT`, others need longer than 5 s to flush | M | L | Configurable grace period; forced termination clearly reported | 10 |

## IPC and security boundary

| ID | Risk | L | I | Mitigation | Owner step |
| --- | --- | --- | --- | --- | --- |
| R-07 | Pipe-name squatting by another user to impersonate the Host | L | H | Client verifies server PID token SID + image path; `FIRST_PIPE_INSTANCE`. **Step 09 implemented and measured** with a same-user squatter: every client refuses it (`host_identity_mismatch`) with 0 bytes sent, and `host serve` reports it. Another user's squatter: covered by the same SID check, not verified (needs a second account; IPC_PROTOCOL §8) | 09 |
| R-08 | Malformed/oversized/flooding messages from a buggy same-user client | M | M | Frame limits, handshake timeout, per-connection isolation, fuzz tests. **Step 09:** limits checked before allocation, 5 s handshake and 10 s frame timeouts, 64-connection cap, byte-bounded queues; malformed, truncated, oversized and fuzzed input closes only that connection (tests/host_ipc.rs) | 09, 39 |
| R-09 | Same-user malware can drive Subak (inject input, read output) | M | H | Inherent to per-user trust; documented; MCP adds explicit capability grants; no network exposure. **Step 09:** `hook`/`mcp` kinds cannot attach, type or stop; a low-integrity (sandboxed) process of the same user cannot open the pipe (measured); medium-integrity same-user code stays trusted | 09, 37 |

## Updates and versions

| ID | Risk | L | I | Mitigation | Owner step |
| --- | --- | --- | --- | --- | --- |
| R-10 | Updating Subak requires restarting the Host, which ends sessions | H | M | Kim detects live Host, offers defer/stop-with-snapshot; live handoff of ConPTY handles evaluated later | 06, 40 |
| R-11 | Client/Host protocol skew after partial updates | M | M | Version negotiation; `host_too_old` hint; tests with N-1 Host. **Step 09:** negotiation implemented (highest common major); `host_too_old`/`host_too_new` never restart the Host; the Step 08 prototype (major 0) is refused cleanly. A real N-1 Host test needs a second release (Step 40) | 09, 40 |

## GUI rendering

| ID | Risk | L | I | Mitigation | Owner step |
| --- | --- | --- | --- | --- | --- |
| R-12 | Glyph quality (ClearType expectations), CJK/emoji fallback and wide-char alignment in a non-native renderer | H | M | **Step 04 evidence:** egui text has no color emoji and no cell grid → dedicated terminal renderer (ADR 0014); fallback A = DirectWrite surface | 15 |
| R-13 | Korean IME composition breaks shortcuts or duplicates characters | M | H | IME guard in key dispatch (ADR 0009); manual IME test matrix | 16, 23 |
| R-14 | GPU-based UI keeps high idle memory/CPU or fails on RDP/VMs | M | M | **Step 04 measured:** 0 idle CPU; glow ~69 MiB vs wgpu ~300 MiB → glow chosen; WARP/CPU fallbacks defined (ADR 0014); RDP/VM untested | 16 |
| R-15 | Accessibility (screen readers) weak in custom-rendered UIs | M | M | Toolkit with UI Automation support (e.g. AccessKit); manual Narrator checks | 04, 20 |

## SSH and credentials

| ID | Risk | L | I | Mitigation | Owner step |
| --- | --- | --- | --- | --- | --- |
| R-16 | Secrets leak via logs, JSON, state or crash dumps | M | H | Secret types that redact on `Debug`/`Display`; Credential Manager/DPAPI; tests grepping outputs. **Step 11:** pane environment overrides are used once and never stored or shown (names only); a test greps 17 outputs (JSON, events, `-v` diagnostics) for an explicit and a copied value; panes never inherit a launcher's tokens ([PTY_PROCESSES §3](PTY_PROCESSES.md)) | 11, 34, 39 |
| R-17 | Host-key changes accepted silently | L | H | Strict known_hosts; explicit warning UI; tests with changed keys | 34 |
| R-18 | Pure-Rust SSH stack gaps (agents, algorithms, jump hosts) vs OpenSSH | M | M | Evaluate `russh` vs invoking Windows OpenSSH; record ADR | 34 |

## tmux interaction

| ID | Risk | L | I | Mitigation | Owner step |
| --- | --- | --- | --- | --- | --- |
| R-19 | Nested multiplexers (tmux inside WSL/SSH also on `Ctrl+B`) | H | L | `Ctrl+B Ctrl+B` sends literal prefix; per-profile prefix; documented | 23 |
| R-20 | `Ctrl+W`/`Ctrl+T` shadow readline/Vim keys | H | M | Pass-through via prefix, rebinding, terminal-first mode (ADR 0009) | 17, 23 |
| R-21 | Matrix coverage mistaken for implementation | M | M | Evidence column required for `implemented`; docs review in Step 26 | 26 |

## Session recreation and Claude state

| ID | Risk | L | I | Mitigation | Owner step |
| --- | --- | --- | --- | --- | --- |
| R-22 | Restore re-runs side-effectful commands or duplicates Terminals | M | H | Never auto-run arbitrary commands; opt-in restart commands; idempotent restore with markers; live-Host check (`SUBAK-SEC-001`) | 29, 30 |
| R-23 | Missing cwd/executable after reboot | M | L | Fall back to profile default cwd with a visible notice; `failed` state with reason | 30 |
| R-24 | Claude Code hook schema/events change between versions | M | M | Version detection; degrade to `unknown`; contract tests on recorded hook payloads | 27, 28 |
| R-25 | Wrong session resumed (several Claude sessions in one Terminal, moved/deleted sessions) | M | M | Per-pane session IDs; validate existence before resume; one resume per restore | 30 |
| R-26 | Hook installation clobbers user Claude settings | L | H | Merge with backup; uninstall removes only Subak entries; tests on fixtures | 27 |

## Distribution

| ID | Risk | L | I | Mitigation | Owner step |
| --- | --- | --- | --- | --- | --- |
| R-27 | Unsigned binaries flagged by SmartScreen/antivirus | H | M | Code signing when a legitimate certificate exists; never fake signatures; document | 40 |
| R-28 | Signing key compromise for catalog/releases | L | H | Offline key, rotation procedure, pinned keys with rollover list | 06, 40 |
