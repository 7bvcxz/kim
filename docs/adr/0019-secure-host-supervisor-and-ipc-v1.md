# ADR 0019 — Secure Host supervisor and IPC v1

Status: **Accepted** · Date: 2026-11-04 · Step 09 · Implements [ADR 0003](0003-subak-host-process-model.md)
and [ADR 0005](0005-local-ipc.md), refining both as described below

## Context

ADR 0003 (Host process model) and ADR 0005 (local IPC) were accepted on paper. Step 08
([ADR 0016](0016-conpty-host-validation.md)) proved the ConPTY and job primitives with a
prototype: protocol major 0, no peer identity checks, and a Host that lived exactly as long as its
one shell. Step 09 replaces the prototype with the supervisor and protocol v1 and tests them on
Windows 10 Pro 19045 and, through CI, on Windows Server 2025 (build 26100). This ADR records how
the paper decisions were realized, where reality required a refinement, and what stays open.

## Decision

1. **IPC v1 is implemented and frozen** as specified in
   [contracts/IPC_PROTOCOL.md](../contracts/IPC_PROTOCOL.md) (framing, limits, handshake,
   requests, events, the closed error set). Additive details now part of v1:
   - The connection-level error `{"type":"error"}` also explains why the Host closes a
     connection after a protocol violation, not only a refused handshake.
   - The whole `hello` must arrive within 5 s. After the handshake, a frame whose first byte
     arrived must be complete within 10 s. An idle connection has no timeout (heartbeats are
     client-driven). Frame lengths are checked before anything is allocated.
   - At most 64 connections (handshaking or established) at once; more get `busy`. Each
     connection's send queue is bounded (4096 frames, 16 MiB); a client that falls further
     behind is disconnected, never allowed to block the Host or a pane.
   - `host.health` (all kinds) is the heartbeat. `host.status` groups its result as
     `{protocol, endpoint, host, terminals, pane, clients}`.
   - Authorization is checked against the **whole** v1 method table, including methods of later
     Steps, so a kind's rights never depend on implementation progress. A permitted but
     unimplemented method answers `unknown_request`.
   - Clients send only input data frames. Input is written to a pane only for an interactively
     attached `cli`/`gui` connection; anything else is dropped and counted, never executed.

2. **The Host checks the token of the connection itself, not the client's process**
   (refines ADR 0005). After the first frame has been read, the Host calls
   `ImpersonateNamedPipeClient`, `OpenThreadToken` (with `OpenAsSelf`) and `RevertToSelf`, and
   compares the token's user SID with its own. Clients connect with `SECURITY_IDENTIFICATION`, so
   this lets the Host *read* the identity and nothing else. Compared with looking up
   `GetNamedPipeClientProcessId` and opening that process's token, it cannot be fooled by pid
   reuse or a handle passed to another process. It also works for an elevated client, whose
   process token a normal Host may not open. The pid is still recorded for `host.status`.
   Windows allows the impersonation only after data has been read from the pipe, so the check
   runs between reading `hello` and answering it. A mismatch gets `unauthorized` and the
   connection is closed.

3. **Clients verify the server before sending a single byte**, as ADR 0005 states: the server
   pid comes from `GetNamedPipeServerProcessId`. The client checks that its token user SID equals
   its own and that its image file name is `subak.exe`. The `welcome` must then name that same
   pid. A failure is the CLI error `host_identity_mismatch` (exit 4). Known limit: a
   non-elevated client cannot open an elevated Host's token, so it refuses such a Host (**not
   verified**: needs a UAC elevation).

4. **Endpoint and start lock.** The endpoint is `\\.\pipe\mouv-subak-<user SID>-s<session id>`.
   The start lock is the named mutex `Local\mouv-subak-start-<endpoint>`; `Local\` scopes it to
   the Windows session. Start on demand is: connect; if nobody serves the endpoint, take the lock
   (wait up to 20 s), connect again, and only then spawn and wait up to **10 s** for the pipe.
   ADR 0003 said 5 s; 10 s leaves room for a loaded machine and CI. A lock abandoned by a starter
   that crashed mid-start is treated as acquired and reported (`start_lock_abandoned`).

5. **Lifetime, as ADR 0003 §4 states.** The Host runs while it owns a live pane **or** has a
   connection (one still in its handshake counts). When both are gone it waits the idle grace
   period (default 10 s, `--idle-exit 0..86400`) and exits. That grace period is the only timed
   wait, and it happens only while idle. A pane whose shell exits no longer ends the Host
   directly; the Host idles out afterwards. `--no-shell` starts a Host that owns no pane, which
   is what start on demand will use from Step 10.

6. **Stale endpoints and unresponsive Hosts are never "recovered" by force.** A named pipe
   disappears with its last server handle, so a crashed Host leaves no stale endpoint. Measured:
   a new Host served the same name 84 ms after the old one was killed. What remains are three
   cases, none of which kills or replaces a process:
   - a **stopping** Host: the client waits up to 15 s for it to release the name;
   - a **hung** Host: the handshake times out or its pipe stays busy. The client reports
     `host_unresponsive` (exit 6) and starts nothing, because that Host may own live sessions.
     Measured with a suspended Host: reported after 5.0 s, healthy again once resumed, with the
     same shell;
   - a **foreign holder** of the name (a squatter): `host_identity_mismatch`.

   `host serve` on a taken name tells a genuine Host from a squatter. Measured:
   `CreateNamedPipe` with `FILE_FLAG_FIRST_PIPE_INSTANCE` fails with `ERROR_PIPE_BUSY`, not
   `ERROR_ACCESS_DENIED`, when the holder's instance limit is reached. Both mean "taken".

7. **Stop.** IPC `host.stop` refuses with `busy` while a live pane exists unless
   `kill_terminals` is set. It answers once the panes have ended (clients allow 30 s). The CLI
   keeps the Step 08 consent rule: `--yes`, or an interactive prompt. Only after consent does it
   send `kill_terminals`. Stopping a Host that owns no live pane is not destructive and needs no
   confirmation. The `--kill-terminals` flag of ADR 0003 §7 arrives with Terminals (Steps 10–12).

8. **Environment independent of the launching terminal** (ADR 0003 §6). The starter spawns the
   Host with `CreateEnvironmentBlock(token, FALSE)`, and the Host spawns its panes with the same
   block. Their working directory is that block's `USERPROFILE`. `HOME` exists only if the user
   defined it, and Git, OpenSSH and Claude Code then find their configuration under the real
   profile (`.gitconfig`, `.ssh`, `.claude`, or a user-level `CLAUDE_CONFIG_DIR`). A
   terminal-scoped `SSH_AUTH_SOCK` is deliberately not carried. The Windows OpenSSH agent uses a
   fixed pipe and is unaffected. If the block cannot be built, the Host inherits its launcher's
   environment and reports `"environment": "inherited"` with a warning.

9. **Panic containment** (ARCHITECTURE §2.1): each request and each connection runs inside
   `catch_unwind`. A panic fails that request with `internal_error` and is counted in
   `host.stats`; the Host continues. The release profile keeps unwinding for this reason.

10. **Risk R-03, measured further** (ADR 0016 §5 stands, still no broker):
    - With a kill-on-close launcher job that allows *silent* breakaway, the Host ends up outside
      that job and survives its close.
    - With nested jobs (inner job allows breakaway, outer job forbids it), the Host leaves the
      inner job, stays in the outer one, and dies when the outer job is closed.
    - The Host reports its own immediate job's limits, and `host start` reports the launcher's,
      so the warning tells the cases apart.
    - **Read-only measurement on this machine** (Windows Terminal 1.24.12741.0): Windows Terminal
      and its `OpenConsole.exe` hosts are in a job object, but the shells in its tabs are not.
      A Host started from a tab is therefore in no job.
    - Closing a Windows Terminal window, VS Code terminals and SSH sessions stay **not
      verified** (CONPTY_PROOF §7).

11. **Same-user sandboxes are kept out by the pipe itself.** The Host's pipe has no explicit
    mandatory label, so it is medium integrity with no-write-up. Measured: a thread of the same
    user impersonating a *low*-integrity copy of its token gets `ERROR_ACCESS_DENIED`; at medium
    integrity it connects.

12. **Unchanged:** no Windows Service, no SYSTEM component, no scheduled task, no TCP listener.
    Nothing starts at login (ADR 0003 §5 remains opt-in for later).

## Consequences

- Multiple clients of all four kinds share one Host per user session. A client of another user
  is stopped by the DACL, and as a second line by the token check. Same-user processes at medium
  integrity remain trusted (risk R-09); MCP capability grants come in Step 37.
- New CLI error codes in [CLI_CONTRACT §6](../CLI_CONTRACT.md): `host_identity_mismatch` (4),
  `host_too_old` (5), `host_too_new` (5), `host_unresponsive` (6), `target_not_found` (3). A
  protocol mismatch never makes a client restart the Host (risk R-11).
- **Not verified headlessly:** a second Windows user trying to connect, and an elevated Host
  with a normal client. Both need another account or a UAC prompt on the maintainer's desktop.
  Manual steps are in [IPC_PROTOCOL §8](../contracts/IPC_PROTOCOL.md).

## Alternatives considered

- **Check the client by pid, as ADR 0005 worded it.** Rejected for the reasons in item 2. For a
  non-elevated client in the same session the result is the same.
- **End or replace an unresponsive Host** to "recover" the endpoint. Rejected: the Host owns live
  sessions, and recovery never kills them (`SUBAK-SEC-001` in spirit).
- **A shared secret or token file** for client authentication. Not needed: the OS identity of
  both ends is checked, and a file readable by the user adds nothing against same-user code.
- **An async runtime.** Two threads per connection, capped at 64 connections, idle in the kernel;
  no measurement justifies more (ARCHITECTURE §2.1).

## Requirements

`SUBAK-FR-001`, `SUBAK-FR-002`, `SUBAK-FR-003`, `SUBAK-FR-004`, `SUBAK-FR-015`, `SUBAK-SEC-010`,
`SUBAK-SEC-012`, `KIM-NFR-001`, `KIM-NFR-004`, `KIM-NFR-014`.
