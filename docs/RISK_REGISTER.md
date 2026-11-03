# Risk register

Status: **current** · Step 03 · Reviewed at every milestone validation (Steps 14, 20, 26, 31, 39)

Likelihood (L) and impact (I): **H**igh / **M**edium / **L**ow. "Owner step" is where the risk
is retired or re-assessed with evidence.

## Win32 ConPTY

| ID | Risk | L | I | Mitigation | Owner step |
| --- | --- | --- | --- | --- | --- |
| R-01 | Inbox ConPTY differs across Windows 10 1809 → 11 builds (resize reflow artifacts, missing passthrough, mouse input, cursor inheritance) | H | M | Detect OS build; conformance tests per build; consider shipping `conpty.dll`/`OpenConsole.exe` (MIT) if inbox gaps hurt | 08, 11, 15 |
| R-02 | ConPTY output is a re-rendered stream, not the program's raw bytes (e.g. it may repaint, reorder or drop some sequences) | M | M | Treat ConPTY as the terminal source of truth; replay via VT snapshot (ADR 0008) rather than raw byte fidelity | 13, 15 |
| R-03 | Host started from a terminal that lives in a Job without breakaway permission dies with that terminal | M | H | `CREATE_BREAKAWAY_FROM_JOB` attempt; report `job_breakaway=false`; test from Windows Terminal, conhost, VS Code terminal, SSH | 08 |

## Process trees and Job Objects

| ID | Risk | L | I | Mitigation | Owner step |
| --- | --- | --- | --- | --- | --- |
| R-04 | Programs that create their own jobs or break away escape kill-pane | M | M | Disallow breakaway in pane jobs by default; document; enumerate escaped processes in diagnostics | 11 |
| R-05 | Kill-on-close means a Host crash ends every session | M | H | Panic containment per request/pane; crash tests; restore path; never run untrusted plugins in the Host | 09, 30, 39 |
| R-06 | Graceful stop: some programs ignore `CTRL_CLOSE_EVENT`, others need longer than 5 s to flush | M | L | Configurable grace period; forced termination clearly reported | 10 |

## IPC and security boundary

| ID | Risk | L | I | Mitigation | Owner step |
| --- | --- | --- | --- | --- | --- |
| R-07 | Pipe-name squatting by another user to impersonate the Host | L | H | Client verifies server PID token SID + image path; `FIRST_PIPE_INSTANCE` | 09 |
| R-08 | Malformed/oversized/flooding messages from a buggy same-user client | M | M | Frame limits, handshake timeout, per-connection isolation, fuzz tests | 09, 39 |
| R-09 | Same-user malware can drive Subak (inject input, read output) | M | H | Inherent to per-user trust; documented; MCP adds explicit capability grants; no network exposure | 09, 37 |

## Updates and versions

| ID | Risk | L | I | Mitigation | Owner step |
| --- | --- | --- | --- | --- | --- |
| R-10 | Updating Subak requires restarting the Host, which ends sessions | H | M | Kim detects live Host, offers defer/stop-with-snapshot; live handoff of ConPTY handles evaluated later | 06, 40 |
| R-11 | Client/Host protocol skew after partial updates | M | M | Version negotiation; `host_too_old` hint; tests with N-1 Host | 09, 40 |

## GUI rendering

| ID | Risk | L | I | Mitigation | Owner step |
| --- | --- | --- | --- | --- | --- |
| R-12 | Glyph quality (ClearType expectations), CJK/emoji fallback and wide-char alignment in a non-native renderer | H | M | Toolkit evaluation with Korean/emoji fixtures; font fallback chain; screenshot tests | 04, 15 |
| R-13 | Korean IME composition breaks shortcuts or duplicates characters | M | H | IME guard in key dispatch (ADR 0009); manual IME test matrix | 16, 23 |
| R-14 | GPU-based UI keeps high idle memory/CPU or fails on RDP/VMs | M | M | Measure idle working set; reactive repaint only; software fallback | 04, 16 |
| R-15 | Accessibility (screen readers) weak in custom-rendered UIs | M | M | Toolkit with UI Automation support (e.g. AccessKit); manual Narrator checks | 04, 20 |

## SSH and credentials

| ID | Risk | L | I | Mitigation | Owner step |
| --- | --- | --- | --- | --- | --- |
| R-16 | Secrets leak via logs, JSON, state or crash dumps | M | H | Secret types that redact on `Debug`/`Display`; Credential Manager/DPAPI; tests grepping outputs | 34, 39 |
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
