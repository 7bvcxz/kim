# ADR 0004 — PTY and process ownership on Windows

Status: **Accepted** · Date: 2026-11-03 · Step 03 · Validated by Step 08, implemented in Step 11

## Context

Each Pane runs a process tree attached to a terminal. On Windows the modern primitive is the
**pseudo console (ConPTY)**, available from Windows 10 1809. Killing a Pane must end its whole
tree (`SUBAK-FR-016`), but must never touch another Pane's tree.

## Decision

1. **One ConPTY per Pane, owned by the Host.** The Host calls `CreatePseudoConsole` with two
   anonymous pipes and starts the pane's root process with `PROC_THREAD_ATTRIBUTE_PSEUDOCONSOLE`
   via `CreateProcessW` (`EXTENDED_STARTUPINFO_PRESENT`). Only the Host holds the pipe ends and
   the `HPCON`.
2. **One Job Object per Pane.** The root process is created suspended, assigned to a new job
   with `JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE`, then resumed. Descendants inherit the job (nested
   jobs, Windows 8+), so:
   - *kill pane* = `TerminateJobObject` + `ClosePseudoConsole`, which ends exactly that tree;
   - *Host exits or crashes* = the job handles close and every pane tree ends. No orphaned,
     invisible, uncontrollable processes are left behind. Live sessions cannot outlive the Host
     anyway, because their ConPTY dies with it.
   - Processes that explicitly break away from the job (`CREATE_BREAKAWAY_FROM_JOB`, which our
     job allows only if configured) are outside our control. This is documented (risk R-04).
3. **I/O threads.** Each Pane has a reader thread on the ConPTY output pipe. It feeds the pane's
   terminal state and history ([ADR 0008](0008-attach-replay-and-backpressure.md)) and never
   blocks on clients. A writer path takes input from the IPC layer. Blocked threads cost no CPU
   while idle.
4. **Resize** = `ResizePseudoConsole` with the size requested by the attached client
   ([ADR 0008](0008-attach-replay-and-backpressure.md) defines which client wins).
5. **Exit detection.** The Host waits on the root process handle and the job (via
   `JOBOBJECT_ASSOCIATE_COMPLETION_PORT`, when the job is empty) and records the exit code.
   Then it drains remaining output and closes the pseudo console.
6. **Console control events.** Ctrl+C typed by the user is sent as input (`0x03`) through
   ConPTY, which generates `CTRL_C_EVENT` for the foreground process group. The Host itself is
   detached from any console, so closing a client's console window sends it nothing.
7. **Optional newer ConPTY.** Shipping `conpty.dll` + `OpenConsole.exe` (MIT, from Windows
   Terminal) for features older inbox ConPTY lacks is deferred. Step 15 decides, with
   measurements.

## Consequences

- Pane trees are isolated from each other and from clients.
- A Host crash ends sessions. This is mitigated by not crashing (unwinding panics are contained
  per request), and by the restore path.
- Requires Windows 10 1809+ (`KIM-NFR-010`).

## Alternatives considered

- **winpty / legacy console scraping:** obsolete and slow.
- **No Job Objects (track PIDs):** races with PID reuse, and misses grandchildren.
- **Job without kill-on-close:** a Host crash would leave headless orphans that consume
  resources and cannot be reattached.

## Requirements

`SUBAK-FR-001`, `SUBAK-FR-014`, `SUBAK-FR-016`, `KIM-NFR-010`, `SUBAK-SEC-001`.

## Notes

- 2026-11-04 (Step 08): validated on Windows 10 19045 by the Step 08 prototype; see
  [ADR 0016](0016-conpty-host-validation.md) and [CONPTY_PROOF](../CONPTY_PROOF.md). Suspended
  creation, job assignment, resume, continuous draining, `ResizePseudoConsole` and kill-on-close
  teardown work as decided: a Host crash ends the shell, its grandchildren and the pseudo console
  host. Refinements: the Host closes its copies of the pseudo console's pipe ends after
  `CreatePseudoConsole` (otherwise there is no EOF). `ClosePseudoConsole` blocks on Windows 10 and
  runs on a helper thread while output is drained. The pseudo console's `conhost.exe` is not in
  the pane job. Ctrl+C (§6) works only after the Host clears the "ignore Ctrl+C" flag inherited
  through `CREATE_NEW_PROCESS_GROUP`.
- 2026-11-04 (Step 11): implemented with refinements recorded in
  [ADR 0025](0025-pane-programs-environment-and-backpressure.md): the Host resolves the
  program on the pane's `PATH` and passes it as `lpApplicationName`; scripts are refused
  instead of running through an implicit `cmd.exe /c`; unusable program files are refused
  before a pseudo console exists, because Windows 10's inbox ConPTY keeps one handle per
  pseudo console after `ClosePseudoConsole` (measured). §3's "writer path" is a synchronous
  write by the sending connection. §5: exit detection waits on the root process; the job's
  completion port is not used, since teardown terminates leftovers anyway. Breakaway from a
  pane's job is refused (measured from inside a pane). See [PTY_PROCESSES](../PTY_PROCESSES.md).
