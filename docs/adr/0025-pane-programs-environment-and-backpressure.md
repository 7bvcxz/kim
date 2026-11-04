# ADR 0025 — Pane programs, their environment, and backpressure

Status: **Accepted** · Date: 2026-11-04 · Step 11 · Refines [ADR 0004](0004-pty-and-process-ownership.md),
[ADR 0003](0003-subak-host-process-model.md) §6, [ADR 0008](0008-attach-replay-and-backpressure.md),
[ADR 0011](0011-portability-boundary.md); implements [ADR 0010](0010-claude-code-integration.md) §1
· Details and evidence: [PTY_PROCESSES.md](../PTY_PROCESSES.md)

## Context

Until Step 10 the Host handed a quoted command line to `CreateProcessW` with no application
name and its start-time environment. Measurements on Windows 10 19045 showed what that implies:
`CreateProcessW` then searches the Host's own folder and working directory before `PATH`, and
uses the Host's `PATH`, not the pane's; a `.bat` program silently runs through `cmd.exe /c`; a
failed start already created a pseudo console, and the inbox ConPTY keeps one handle per pseudo
console; a new user variable never reaches a pane of a running Host; and ConPTY delivers output
in ~83-byte chunks, so the 4096-frame send queue dropped a client after about 0.35 s of lag.
Step 11 also had to add environment overrides without letting a value reach logs or JSON, and a
platform-neutral PTY interface (`KIM-NFR-011`).

## Decision

1. **The Host resolves the program itself**, on the `PATH` and `PATHEXT` of the pane's final
   environment; a path is taken as given (relative to the pane's working directory). The
   resolved file is `lpApplicationName` and is reported as the pane's `executable`. The Host's
   folder, its working directory and relative `PATH` entries are never searched.
2. **No implicit shell.** Batch and PowerShell scripts are refused with `spawn_failed`, new
   reason `not_executable`, and a hint naming the explicit interpreter form. A file this user may
   not execute, or without an `MZ` header, is refused before a pseudo console exists
   (`access_denied`, `not_executable`).
3. **Command lines** are the program as named plus every argument quoted by the
   `CommandLineToArgvW`/MSVC rules; `host serve --shell` keeps the user's line verbatim. cmd.exe's
   own parsing of quotes is documented, not worked around.
4. **Environment layers:** a fresh `CreateEnvironmentBlock` of the user for every pane (not the
   Host's start-time snapshot), then the overrides of `terminal.create` (`env: {NAME: value |
   null}`, CLI `--env NAME=VALUE`, `--env NAME` copying the caller's value, `--unset-env NAME`),
   then `SUBAK_PANE_ID` and `SUBAK_TERMINAL_ID` (ADR 0010 §1), which cannot be overridden.
   Override values are used once and never stored, logged or shown; panes report their names.
5. **Code page and signals:** the pane's console code page is left as the system sets it (ConPTY's
   own I/O is UTF-8 either way). Ctrl+C is `0x03` input. Ctrl+Break is not delivered in Step 11:
   ConPTY's input has no encoding the inbox ConPTY honors (measured), and raising
   `CTRL_BREAK_EVENT` would need the Host to attach to the pane's console.
6. **Backpressure:** a connection's send queue is bounded by bytes only (16 MiB, each frame
   counted with 64 extra bytes), no longer by a frame count; runs of small frames are written at
   once; output frames are shared by all clients of a pane; the reader takes what is already
   queued in the pipe in one read. Input stays a synchronous write by the sending connection: the
   flooding client waits, nobody else does, and the Host buffers nothing (measured).
7. **One `pty::Pty` trait per pane** covers ADR 0011's `Pty` and `ProcessTree` rows, because the
   pseudo terminal and its container are created together; Windows implements it, other targets
   get an `unsupported_platform` stub. The foreground-process query arrives with Step 21.
8. `host.stats` adds the Host's memory (`host.memory`), a local metric (ADR 0012).

## Consequences

- `terminal new -- npm …` now fails with a clear hint instead of running through cmd; scripts
  need their interpreter named. The `spawn_failed` reasons grow by `not_executable` (additive).
- A `PATH` override decides which program runs; the dry run shows the resolved file, so "is
  `pwsh` installed?" is answered without starting anything, and Subak still installs nothing.
- Setting a user variable with `setx` reaches the next new pane without restarting the Host.
- Failed starts that Subak can decide create no OS object, so they leak nothing even on Windows 10.
- A lagging client is now dropped after ~16 MiB of queued output instead of ~4096 frames;
  several lagging clients share one copy. The disconnect itself stays until Step 13's resync.
- cmd.exe `/c` text with inner quotes is not representable yet; a verbatim command line for
  `terminal new` is left to Step 12.

## Alternatives considered

- **Let `CreateProcessW` search** (null application name): rejected; it searches the Host's
  folder and working directory and ignores a `PATH` override.
- **Run batch files like `CreateProcessW` does**, with cmd-specific escaping: rejected; it adds a
  shell the user did not ask for, and its escaping has a history of injection bugs.
- **Set every pane to code page 65001:** rejected; it changes how legacy byte-oriented programs
  behave and is not what a console window does. Users can run `chcp 65001`.
- **A bounded per-pane input queue with its own writer thread:** rejected for now; conhost keeps
  accepting input (measured), so the Host would only add buffering. Revisit with Step 12's
  `send_keys`.
- **Raising the 4096-frame limit:** rejected; the bound that matters is memory, and a larger
  bounded channel would preallocate it for every connection.
- **Snapshot the environment once at Host start** (Step 09 behavior): rejected; a tool installed
  after the Host started would stay invisible in new panes.

## Requirements

`SUBAK-FR-001`, `SUBAK-FR-003`, `SUBAK-FR-011`, `SUBAK-FR-013`, `SUBAK-FR-016`, `SUBAK-FR-070`,
`KIM-SEC-013`, `KIM-NFR-011`, `KIM-NFR-013`.

## Notes

- 2026-11-04 (Step 12): the verbatim command line left open in §3 and the Consequences is
  `terminal.create` `command_line` / `subak new --command-line`
  ([ADR 0027](0027-subak-cli-structure-and-headless-control.md) §4); only its first word is
  resolved, so decisions 1 and 2 still hold.
- 2026-11-04 (Step 13): decision 6's disconnect of a lagging client is replaced by a resync
  ([ADR 0030](0030-host-terminal-model-scrollback-and-replay.md) §4): the client stays connected,
  its queued output is dropped, and it gets `pane.resync` and a snapshot once its queue drained.
  The 16 MiB bound, shared frames and synchronous input are unchanged.
