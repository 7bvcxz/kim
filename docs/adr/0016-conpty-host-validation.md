# ADR 0016 — ConPTY ownership and the detached Host, validated on Windows

Status: **Accepted** · Date: 2026-11-04 · Step 08 · Validates [ADR 0003](0003-subak-host-process-model.md)
and [ADR 0004](0004-pty-and-process-ownership.md), and the transport of [ADR 0005](0005-local-ipc.md),
with the corrections below

## Context

ADR 0003 (detached per-user Host) and ADR 0004 (one ConPTY plus one kill-on-close job per pane,
owned by the Host) were accepted on paper, with Step 08 as their validation. Step 08 built a
narrow prototype (`subak host serve|start|status|attach|stop`) and tested it against real ConPTY,
Job Objects, console windows and named pipes on Windows 10 Pro 22H2 (build 19045.6466). The
evidence is in [CONPTY_PROOF.md](../CONPTY_PROOF.md). This ADR records which parts held, which
needed corrections, and what remains open.

## Decision

1. **ADR 0003 and ADR 0004 stand.** A detached Host survives every client ending: clean detach,
   end of input, `TerminateProcess`, a killed console host, and `WM_CLOSE` on the console window
   that both launched the Host and ran the client. A new client reattaches to the same shell
   process with its state intact. Ending the Host ends the whole pane tree, including
   grandchildren and the pseudo console host, so nothing is orphaned. No superseding ADR is
   needed; the corrections below are additive.

2. **Ctrl+C must be re-enabled by the Host (correction to ADR 0003 §2 and ADR 0004 §6).**
   `CREATE_NEW_PROCESS_GROUP` starts the Host with Ctrl+C *ignored*, and every process it
   creates inherits that flag. Measured: `0x03` typed into a pane did not interrupt `ping`. The
   Host therefore calls `SetConsoleCtrlHandler(NULL, FALSE)` before it spawns any pane. A
   regression test fails without the call and passes with it (interrupt in ~50 ms). The flag
   stays, as ADR 0003 states; it is harmless once the Host clears the inherited state. The fix
   also covers Hosts started by any other launcher that disabled Ctrl+C.

3. **"No inherited handles" is made exact (refines ADR 0003 §2).** The Host's stdin, stdout and
   stderr are one fresh `NUL` handle, and `PROC_THREAD_ATTRIBUTE_HANDLE_LIST` allows only that
   handle to be inherited. `bInheritHandles = FALSE` with no std handles is not enough: a
   launcher with redirected output (an agent, a test, CI) would otherwise hang until the Host
   exits. `CREATE_NO_WINDOW` is ignored next to `DETACHED_PROCESS`; it is kept, harmless, as ADR
   0003 lists it.

4. **Job breakaway is per level, and the Host reports both facts (refines ADR 0003 §2).**
   `CREATE_BREAKAWAY_FROM_JOB` succeeds when the immediate job allows it. The child then leaves
   that job but stays in an outer job that forbids breakaway (measured under `cargo test`).
   Status therefore reports `job_breakaway` (the flag was accepted) **and** `in_job` (the Host is
   still in some job). `host start` and `host status` add an R-03 warning whenever `in_job` is
   true.

5. **Risk R-03 is confirmed, not retired.** When a launcher's job forbids breakaway, the Host
   still starts (the ADR 0003 fallback), reports `job_breakaway: false`, and dies together with
   its shell and pseudo console host when that kill-on-close job is closed. The prototype keeps
   the fallback and the warning. Escaping such a job through a broker (WMI `Win32_Process.Create`,
   Task Scheduler, a shell-execute via Explorer) is **not** adopted: each one adds an always-there
   component or a hidden indirection that ADR 0003 rejected. Step 09 revisits this with
   measurements in Windows Terminal, VS Code and SSH sessions.

6. **Pseudo console teardown (refines ADR 0004 §1, §5).** After `CreatePseudoConsole`, the Host
   closes its copies of the pseudo console's pipe ends; otherwise the output pipe never reports
   end of stream. On Windows 10, `ClosePseudoConsole` blocks until conhost exits, so the Host
   calls it on a helper thread while the reader keeps draining. It waits up to 5 s for the
   shell, then calls `TerminateJobObject`, and bounds every join. The pseudo console's
   `conhost.exe` is a child of the Host but **not** a member of the pane job. It ends with its
   `HPCON` or with the Host.

7. **Transport facts for ADR 0005 / Step 09.** Named-pipe handles must be overlapped on both
   ends. A synchronous handle serializes I/O per file object, so a blocked read stalls a write
   from another thread and deadlocks a duplex terminal stream. The prototype already applies the
   owner-only DACL (deny NETWORK, allow the user), `PIPE_REJECT_REMOTE_CLIENTS`,
   `FILE_FLAG_FIRST_PIPE_INSTANCE`, and `SECURITY_IDENTIFICATION` on clients. It speaks
   protocol major **0** with the IPC v1 framing, so v1 and the prototype reject each other
   cleanly. Peer identity checks remain Step 09's job.

8. **Prototype scope.** One Host owns exactly one pane. The Host lives as long as its shell
   (ADR 0003's idle grace and the session model start in Steps 09–10). Replay is raw recent output
   (64 KiB), not a VT snapshot (ADR 0008, Step 13). A client 4096 frames behind is
   disconnected, never allowed to block the pane. Redirected input that starts with a UTF-8 BOM
   (Windows PowerShell 5.1 / .NET Framework write one) has it dropped.

## Consequences

- Steps 09–12 build on proven primitives: `crates/subak/src/win/{conpty,pipe,spawn}.rs` and their
  tests carry over. Only the protocol, identity checks, session model and CLI shape grow.
- `subak host status` gains `in_job` next to `job_breakaway` (IPC `host.status` in Step 09 should
  carry both).
- Users who launch Subak from a terminal whose job forbids breakaway are warned, but they still
  lose their sessions when that terminal's job is closed. This remains open as R-03.
- **Not verified (manual run pending):** closing a Windows Terminal window or tab, sleep,
  hibernate, logoff, shutdown, lock and RDP. Starting them headlessly would need a visible window
  or a power transition on the maintainer's desktop. Procedures and the smoke script are in
  [CONPTY_PROOF §7](../CONPTY_PROOF.md). Logoff, shutdown and restart end every user process by
  design; sleep and hibernate are expected to preserve the Host.
- Only one Windows build was measured (risk R-01 stays open for 1809–11 differences).

## Alternatives considered

- **Drop `CREATE_NEW_PROCESS_GROUP`** instead of clearing the flag. Rejected: it would only fix
  launches made by `host start`. A Host started by another program with that flag, or from a
  launcher that already ignores Ctrl+C, would still break Ctrl+C in panes.
- **Rely on `bInheritHandles = FALSE`**. Rejected: the child's std handles are then undefined.
  `NUL` plus an explicit handle list makes the Host's stdio well-defined and leak-free.
- **Synchronous pipe I/O with one thread per direction on duplicated handles.** Rejected: duplicated
  handles share the file object, so the serialization (and the deadlock) remains.
- **A broker to escape non-breakaway jobs** (see item 5). Deferred to Step 09 with measurements.

## Requirements

`SUBAK-FR-001`, `SUBAK-FR-002`, `SUBAK-FR-003`, `SUBAK-FR-013`, `SUBAK-FR-015`, `SUBAK-FR-016`,
`SUBAK-FR-044`, `SUBAK-SEC-010`, `KIM-NFR-001`, `KIM-NFR-010`.
