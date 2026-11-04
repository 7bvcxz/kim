# ADR 0027 — Subak CLI structure, detach from outside, verbatim command lines and console restore

Status: **Accepted** · Date: 2026-11-04 · Step 12 · Refines [ADR 0015](0015-cli-contract-and-tool-discovery.md)
(command names), [ADR 0020](0020-terminal-session-lifecycle.md) (the CLI spelling of its verbs),
[ADR 0025](0025-pane-programs-environment-and-backpressure.md) §3 (verbatim command lines) and
[ADR 0008](0008-attach-replay-and-backpressure.md) (attach clients) · Reference:
[SUBAK_CLI.md](../SUBAK_CLI.md)

## Context

Steps 09–11 gave Subak a Host, IPC v1 and a Terminal lifecycle, reachable as `subak host …` and
`subak terminal new|list|info|rename|attach|stop|kill|rm`. Step 12 must deliver the
tmux-familiar CLI (`subak new`, `ls`, `info`/`status`, `attach`, `detach`, `rename`, `kill`) on
the same Host, with a published `--json` schema. Four questions were open:

1. Two spellings for one operation (`subak new` and `subak terminal new`) must not become two
   commands with two contracts.
2. `subak detach` run from *another* terminal has no attachment of its own; IPC v1 only had
   `pane.detach`, which detaches the caller.
3. `cmd.exe /c` parses its command line itself, so text with inner quotes cannot be expressed as
   separate, MSVC-quoted words (ADR 0025 §3 left this to Step 12).
4. An attaching client puts its console into raw mode. Before Step 12 only a normal return
   restored it; a panic, Ctrl+Break or a closed window left the console raw, and full-screen
   programs could leave the user's terminal in the alternate screen with a hidden cursor.

## Decision

1. **Terminal verbs are top level; `terminal <verb>` is a deprecated alias.** The canonical
   commands are `subak new | ls | info | status | attach | detach | rename | stop | kill | rm`
   (`list` is accepted for `ls`) and `subak host start | status | health | attach | stop |
   serve`. The Step 10 spelling `subak terminal new|list|ls|info|rename|attach|stop|kill|rm` runs
   the same command with the same options, **reports the canonical command name** in the
   envelope (`"command": "ls"`), and adds a deprecation warning (`warnings` in `--json`, one
   stderr line otherwise, suppressed by `--quiet`). Verbs that did not exist under `terminal`
   (`status`, `detach`) are not added there. Removing the alias is a breaking change and needs
   its own ADR; it stays at least until the first public release. The capabilities document
   lists the canonical commands only.
2. **`status` is the overview, `info` the detail.** `subak status` answers the Host (or that
   none runs: exit 0, `running: false`), every Terminal and every client in one document and
   starts nothing; `subak info <target>` is one Terminal (exit 3 when there is none, tmux
   `has-session`). Bare `subak` prints help: nothing starts implicitly (tmux would create and
   attach a session).
3. **Detaching from outside is an IPC method of its own.** `client.detach {target?, client_id?,
   dry_run?}` (cli and gui clients; MCP only after the Step 37 grant) ends the attachments of
   every client of the panes `target` names, of the client `client_id` (the `id` each
   `host.status` client now carries), or of that client to that target. It touches no process,
   Terminal or connection. Each detached connection subscribed to `panes` receives
   `pane.detached {pane_id, terminal_id, stream_id, by}` in the critical section that removes the
   attachment, so it arrives before the output stops; the CLI client then exits 0 with
   `[subak: detached from `<name>` by another client …]`. `subak detach` without a target inside a
   pane uses `SUBAK_TERMINAL_ID` (ADR 0010 §1); outside a pane it needs a target or `--client`.
   Detach is an effect `write` with `--dry-run`; it never asks, because it ends nothing.
   Additive in IPC v1 (features `client.detach`).
4. **Verbatim command lines.** `terminal.create` accepts `command_line` (exclusive with
   `command`); CLI `subak new --command-line <line>`. The line is passed to `CreateProcessW`
   unchanged; only its first word (by the `CommandLineToArgvW` rules) is resolved on the pane's
   `PATH`, so the ADR 0025 checks still apply (scripts refused, no `CreateProcessW` search).
   `command` becomes the line's split, for display and the busy rule. The pane reports the line
   as `command_line` with secret-looking words replaced by `***` (`session::redact_line`); the
   `--shell` Terminal of `host serve` reports its line the same way. Additive (feature
   `terminal.command_line`). A line is one argument to `subak`, so its quoting is the calling
   shell's; Windows PowerShell 5.1 needs `--%` or `\"` for inner quotes (SUBAK_CLI §6).
5. **The attaching client restores its console on every path it survives.** One idempotent
   restore (`win::console::restore`) runs from the raw-mode guard (detach, the program's end, an
   error), a panic hook, and a console control handler (`CTRL_CLOSE_EVENT`, logoff, shutdown). It
   first writes the sequences that switch off the terminal modes the pane turned on and left on —
   tracked from the output (`ModeTracker`: alternate screen, hidden cursor, mouse reporting,
   bracketed paste, focus events, application cursor keys and keypad) — then restores the saved
   input and output modes and the code page. **Ctrl+C** stays a key for the program (raw input
   delivers `0x03`; ConPTY raises `CTRL_C_EVENT` in the pane). **Ctrl+Break**, which Windows
   always raises as a signal, detaches the client cleanly instead of killing it (and is not sent
   to the program, as in ADR 0025 §5); with redirected input, Ctrl+C does the same. A client ended
   with `TerminateProcess` runs no code; what the surrounding shells do then is measured and
   documented, not assumed.
6. **Detach keys** stay `Ctrl+B d` (`Ctrl+B Ctrl+B` sends one `Ctrl+B`), the defaults of the
   hotkey contract's `prefix` leader and `client.detach` action. The CLI has no rebinding option of
   its own: rebinding comes with Subak's `hotkeys.json` (Steps 17 and 23), so there is one
   configuration, not a CLI-only one.
7. **Smaller additions:** `new --no-start` (never start a Host; tmux `-N`), `new --attach`
   (attach after creating), `attach --read-only` (IPC `interactive: false`; keys other than the
   detach keys are dropped and redirected input is not read), `attach` without a target (the only
   running Terminal, as `host attach`).
8. **`subak-cli-v1`** types the `data` of every command and Subak's error registry. Its Terminal,
   Window and Pane definitions are copied verbatim from `ipc-v1` and kept identical by a test;
   golden files are generated from real runs.

## Consequences

- Scripts written for Steps 10–11 keep working; their envelopes now say `"command": "new"`
  instead of `"terminal.new"` and carry a warning. Subak had no published CLI schema before this
  Step, so no versioned contract changes; from now on the names are fixed by `subak-cli-v1`.
- An agent can watch a Terminal (`attach --read-only` with redirected output) and end that watch
  from anywhere (`subak detach --client <id>`), without any process being touched.
- `cmd.exe /c` lines with inner quotes, `&`, `^` and redirections run exactly as written (measured
  in `tests/subak_cli.rs`); the same text as separate words fails, as ADR 0025 predicted.
- After Ctrl+B d, a remote detach and Ctrl+Break the console's input and output modes, code page
  and cursor are exactly as before the attach (measured with a probe). After a forced kill the raw
  input mode and a hidden cursor remain until the shell resets them: cmd.exe does at its next
  prompt; Windows PowerShell 5.1 (PSReadLine) keeps taking commands, but its console then still
  has `ENABLE_VIRTUAL_TERMINAL_INPUT` set (`0x3E0` at the prompt, against `0x1E4` after a clean
  detach).
- Two more IPC surface items (`client.detach`, `pane.detached`) must be served by every future
  Host of protocol 1.

## Alternatives considered

- **Keep `terminal <verb>` canonical and add short aliases**: rejected; the tmux-familiar words
  are what users type, and the dotted `terminal.*` names would remain the only ones in JSON.
- **Aliases that report their own name** (`terminal.new`): rejected; one data shape would have two
  command names, and the capabilities document could not list both honestly.
- **Detach other clients by closing their connection**: rejected; the client could not tell a
  detach from a Host crash, and its console would not be restored with a clear notice.
- **Rebuild the verbatim line from the client's own command line (`GetCommandLineW`)** after
  `--`: rejected for now; PowerShell 5.1 rebuilds that line itself, so the result would depend on
  the calling shell in ways the dry run could not show. `--command-line` is one explicit argument
  whose effect the dry run shows exactly.
- **A `--detach-keys` option** (as Docker has): rejected; it would be a second, CLI-only key
  configuration next to the hotkey contract.
- **Always send a fixed reset sequence on detach**: rejected; `?1049l` on a terminal that is not
  in the alternate screen restores a saved cursor position on some terminals. Only modes the pane
  actually switched on are switched off.

## Requirements

`SUBAK-FR-002`, `SUBAK-FR-003`, `SUBAK-FR-004`, `SUBAK-FR-011`, `SUBAK-FR-012`, `SUBAK-FR-013`,
`SUBAK-FR-014`, `SUBAK-FR-015`, `SUBAK-FR-060`, `SUBAK-SEC-010`, `KIM-FR-010`, `KIM-FR-011`,
`KIM-FR-012`, `KIM-SEC-013`.
