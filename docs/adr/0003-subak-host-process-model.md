# ADR 0003 — Subak Host process model

Status: **Accepted** · Date: 2026-11-03 · Step 03 · Validated by Step 08, implemented in Step 09

## Context

Subak sessions must survive every client closing — including the Windows Terminal or console
window the CLI ran in (`SUBAK-FR-003`). The project forbids unnecessary always-on daemons,
Windows Services and SYSTEM processes (KIM philosophy §3), and requires the Host to run only
while it is useful (`SUBAK-FR-002`).

## Decision

1. **One Host per user per Windows logon session.** Its identity is
   `(user SID, logon session ID)`. A second RDP session of the same user gets its own Host,
   because each logon session has its own desktop, environment and lifetime.
2. **Same executable, hidden subcommand.** The Host is `subak.exe host serve`. It is
   console-subsystem, but it is spawned **detached**: `DETACHED_PROCESS |
   CREATE_NEW_PROCESS_GROUP | CREATE_NO_WINDOW`, with no inherited handles and the working
   directory set to the user profile. It also tries `CREATE_BREAKAWAY_FROM_JOB`, so a job that
   contains the launching terminal cannot take the Host down with it. If breakaway is denied,
   the Host still starts and reports `host.job_breakaway = false` in `subak host status`
   (risk R-03).
3. **Start on demand.** Any client that needs the Host connects to the well-known pipe
   ([ADR 0005](0005-local-ipc.md)). If nobody is listening, the client takes the per-session
   start lock (a named mutex), spawns the Host, and waits up to 5 s for the pipe. Exactly one
   Host wins (`FILE_FLAG_FIRST_PIPE_INSTANCE`); losers exit quietly.
4. **Lifetime.** The Host runs while it owns ≥ 1 live Terminal **or** has ≥ 1 connected client.
   When both drop to zero it waits a short idle grace period (default 10 s, configurable) and
   exits. It has no timers or polling while idle.
5. **Start at login is opt-in**, via a per-user `Run` registry value or a shortcut that the user
   enables explicitly. No Windows Service, no scheduled task, no SYSTEM component.
6. **Environment.** The Host captures the *user's* environment, not the launching terminal's.
   At spawn time the client passes nothing sensitive. The Host rebuilds a clean user
   environment with `CreateEnvironmentBlock` for the current token, so `HOME`/`USERPROFILE`,
   `PATH`, and the Git/SSH/Claude config locations match a fresh login. Pane environment
   overrides are applied on top (Step 11).
7. **Stop.** `subak host stop` refuses while live Terminals exist, unless the user passes
   `--kill-terminals` (interactive confirmation or `--yes`). A graceful stop persists state
   first. Kim's updater never stops a Host on its own (`KIM-FR-034`).

## Consequences

- Closing any window never stops the Host; only an explicit stop, logoff/shutdown, or a crash
  does.
- Logoff or shutdown ends the Host and all its processes. Restore handles that case
  ([SUBAK_PHILOSOPHY §6.2](../SUBAK_PHILOSOPHY.md)).
- Replacing `subak.exe` while a Host runs is blocked by Windows file locking. The updater must
  detect a running Host and let the user decide ([ADR 0013](0013-kim-manager-architecture.md)).
- Live upgrade by handing ConPTY handles to a new Host (`DuplicateHandle`) is theoretically
  possible. It is deferred and tracked as risk R-10.

## Alternatives considered

- **Windows Service / SYSTEM:** forbidden. It also runs in session 0 with the wrong token and
  environment, which complicates credentials.
- **A separate `subak-host.exe`:** gives no isolation benefit and means two binaries to version.
  It may be revisited if the Host's dependency set diverges from the CLI's.
- **Host inside the GUI process:** closing the GUI would kill sessions, which violates
  `SUBAK-FR-003`.

## Requirements

`SUBAK-FR-001`, `SUBAK-FR-002`, `SUBAK-FR-003`, `SUBAK-FR-015`, `KIM-FR-034`,
`KIM-NFR-001`, `SUBAK-SEC-010`.

## Notes

- 2026-11-04 (Step 08): validated on Windows 10 19045 by the Step 08 prototype; see
  [ADR 0016](0016-conpty-host-validation.md) and [CONPTY_PROOF](../CONPTY_PROOF.md). The detached
  spawn of §2 holds, with three refinements. (a) `CREATE_NEW_PROCESS_GROUP` makes the Host and
  everything it starts ignore Ctrl+C, so the Host clears that flag before spawning panes.
  (b) "No inherited handles" means `NUL` stdio plus `PROC_THREAD_ATTRIBUTE_HANDLE_LIST`.
  (c) `host status` reports `in_job` next to `job_breakaway`, because breakaway is decided per job
  level. Risk R-03 is confirmed: inside a launcher job that forbids breakaway, the Host dies when
  that job is closed. The prototype Host lives as long as its single shell; the idle grace of §4
  arrives with the session model.
