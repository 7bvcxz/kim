# Subak CLI reference

Status: **current** (implemented in Step 12; `capture`, `history`, `clear-history` and
`new --history-limit` in Step 13; `hotkeys` in Step 17; Windows only, Linux with M4 — the
`hotkeys` commands are portable) · Decisions:
[ADR 0027](adr/0027-subak-cli-structure-and-headless-control.md),
[ADR 0030](adr/0030-host-terminal-model-scrollback-and-replay.md) · Contract:
[CLI_CONTRACT.md](CLI_CONTRACT.md) v1, schema
[subak-cli-v1](contracts/schemas/subak-cli-v1.schema.json) · Requirements: `SUBAK-FR-002`,
`SUBAK-FR-003`, `SUBAK-FR-004`, `SUBAK-FR-011`…`SUBAK-FR-015`, `SUBAK-FR-033`, `SUBAK-FR-034`,
`SUBAK-FR-060`, `SUBAK-SEC-010`, `KIM-FR-010`…`KIM-FR-012`, `KIM-FR-041`…`KIM-FR-046`,
`KIM-SEC-013`, `KIM-SEC-023`

`subak` is the headless command line of Subak, the persistent terminal. Every command is a
short-lived client of your per-user **Host**, which owns the **Terminals** (named workspaces,
each with one window and one pane until Steps 21–22) and their processes. Closing, detaching or
killing a client never stops anything in the Host. What a Terminal is, its states and what each
verb does to processes are in [SESSION_MODEL.md](SESSION_MODEL.md); what runs in a pane is in
[PTY_PROCESSES.md](PTY_PROCESSES.md); what the Host keeps of their output, and how a reattach
reproduces it, is in [SCROLLBACK.md](SCROLLBACK.md); the wire protocol is
[IPC_PROTOCOL.md](contracts/IPC_PROTOCOL.md).

Pre-alpha: there are no tabs, no split, no persistence and no recovery yet; the window is the
separate program `subak-gui` ([SUBAK_GUI](SUBAK_GUI.md)), which no `subak` command starts. No command opens a
window, installs anything or uses the network, and the Host listens only on a local named pipe
that only your user can open.

## 1. Commands at a glance

| Command | Effect | What it does |
| --- | --- | --- |
| `subak new [<name>] [-- <program> [args…] \| --command-line <line>]` | write | Create a Terminal; starts a Host on demand |
| `subak ls` (also `list`) | read | Every Terminal with state, pid and clients |
| `subak status` | read | The Host (or that none runs), every Terminal, every client |
| `subak info <target>` | read | One Terminal with its window and pane; exit 3 if none |
| `subak attach [<target>]` | write | Connect this terminal to a Terminal's pane |
| `subak detach [<target>] [--client <id>]` | write | Detach clients from outside; stops nothing |
| `subak rename <target> <new-name>` | write | Rename; the id never changes |
| `subak stop <target>` | destructive | End the processes, keep the Terminal as `stopped` |
| `subak kill <target>` | destructive | End the processes at once and remove the Terminal |
| `subak rm <target> [--force]` | destructive | Remove a Terminal that runs nothing |
| `subak capture [<target>]` (also `capture-pane`) | read | Lines of the screen and history the Host keeps; secrets shown as `***` |
| `subak history [<target>]` | read | How much output the Host keeps (counters and limits, never content) |
| `subak clear-history [<target>]` | destructive | Delete the kept history; the screen stays |
| `subak host start\|status\|health\|attach\|stop\|serve` | — | The Host itself ([§3.11](#311-the-host-subak-host-)) |
| `subak hotkeys list\|show\|set\|disable\|enable\|reset\|check\|export\|import` | read / write | The window's keyboard shortcuts ([§3.12](#312-keyboard-shortcuts-subak-hotkeys-)); never contacts a Host |
| `subak capabilities` | read | The machine-readable command list (CLI_CONTRACT §9) |

**Structure.** The Terminal verbs are top level and named like tmux's
([TMUX_COMPATIBILITY](TMUX_COMPATIBILITY.md)). `subak terminal new|list|ls|info|rename|attach|
stop|kill|rm` (Steps 10–11) still works as a **deprecated alias**: same command, same options,
same output and command name, plus a warning (`warnings` in `--json`, one stderr line otherwise).
Bare `subak` prints help; nothing starts implicitly.

**Targets** are a Terminal name (exact, ignoring case and Unicode composition) or its id
(`t-` + 12 hex digits). `attach` and `detach` also take `<terminal>:<window>[.<pane>]` and window
or pane ids ([SESSION_MODEL §3](SESSION_MODEL.md)).

**Endpoints.** Every command except `hotkeys` takes `--endpoint <name>`; the default is your per-user,
per-Windows-session pipe `\\.\pipe\mouv-subak-<SID>-s<session>`. Use your own name for
experiments: it never touches the default Host.

## 2. Output, exit codes, consent

The rules of [CLI_CONTRACT.md](CLI_CONTRACT.md) apply to every command:

- Results on **stdout**, notices and errors on **stderr**. `--json` prints exactly one document on
  stdout, also for failures (the `data` of each command is typed by
  [subak-cli-v1](contracts/schemas/subak-cli-v1.schema.json); examples from real runs are in
  [contracts/examples/subak-cli-v1/](contracts/examples/subak-cli-v1/)). `attach` streams the
  terminal, so `--json` works there only with `--dry-run`.
- **Exit codes**: 0 ok · 1 I/O, internal, `spawn_failed` · 2 usage, `invalid_name`,
  `ambiguous_target` · 3 `host_not_running`, `target_not_found` · 4 `protocol_error`,
  `host_identity_mismatch` · 5 `unsupported_platform`, `host_too_old|new` · 6
  `host_disconnected`, `host_unresponsive`, `timeout` · 7 `host_already_running`,
  `name_conflict`, `invalid_state`, `terminals_running`, `confirmation_required` · 8 `cancelled`.
  `error.exit_code` always equals the process exit code; the registry is
  [CLI_CONTRACT §6](CLI_CONTRACT.md).
- **Destructive** commands (`stop`, `kill`, `rm`, `host stop`) ask on the terminal, or need
  `--yes`. They never ask — and fail with `confirmation_required` (exit 7) — with `--json`,
  `--no-input`, the environment variable `KIM_NO_INPUT` (any value but empty, `0`, `false`), or
  when stdin or stderr is not a terminal. `--dry-run` shows the plan and changes nothing.
- **Secrets** never appear: `--env NAME=VALUE` values are used once and never shown (outputs name
  the variables only); secret-looking arguments (`--token=…`, the value after `--password`) and
  words of a verbatim command line are shown as `***` ([CLI_CONTRACT §10](CLI_CONTRACT.md)).
- `-q` suppresses notices and warnings, `-v` adds diagnostics (endpoint, error codes) on stderr.

## 3. Command reference

### 3.1 `subak new`

```text
subak new [<name>] [--cwd <dir>] [--env NAME[=VALUE]]... [--unset-env NAME]...
          [--size <cols>x<rows>] [--remain-on-exit] [--keep-on-failure] [--no-start]
          [--history-limit <lines>] [--attach [--read-only]] [--dry-run] [--json]
          [-- <program> [args...] | --command-line <line>]
```

Creates a Terminal and returns at once (detached, like `tmux new -d`); `--attach` attaches this
terminal right after. Without a name it gets the lowest free number (`1`, `2`, …); a name in use
(ignoring case) is refused with `name_conflict`, never suffixed.

- **Host on demand.** If no Host serves the endpoint, `new` starts one (detached, with your logon
  environment, owning no Terminal of its own) under the per-endpoint start lock, so concurrent
  `new` calls share one Host (measured with 6 at once). `--no-start` never starts one and fails
  with `host_not_running` instead (tmux `-N`).
- **Program.** `-- <program> [args…]` runs the program with every argument as one word, quoted for
  Windows, never through a shell; a bare name is looked up on the pane's `PATH`. Batch and
  PowerShell scripts are refused with a hint to name their interpreter. `--command-line <line>`
  passes one verbatim line instead ([§6](#6-verbatim-command-lines)). Default: `%ComSpec%`.
- `--cwd` (absolute; default your profile folder), `--env NAME=VALUE`, `--env NAME` (copies this
  terminal's value, so a token is never typed), `--unset-env NAME`, `--size` (default 120x30),
  `--remain-on-exit` (keep it as `stopped` when its program exits), `--keep-on-failure` (keep a
  failed start as `failed`), `--history-limit` (history rows the Host keeps for the pane,
  0–100 000, default 2 000; tmux `history-limit`).
- `--dry-run` makes every check the real run makes (name, working directory, program on the
  pane's `PATH`, scripts) and reports the resolved `executable`; without a Host it starts none.

```console
> subak new build -- cmd.exe /d
Terminal `build` (t-3633c7e09d6d) created: pid 61928 (C:\Windows\system32\cmd.exe); started Host pid 62452
attach with: subak attach t-3633c7e09d6d
> subak new api --cwd C:\src\api --env RUST_LOG=debug --env GITHUB_TOKEN -- cargo run
> subak new dev --attach
```

`--json`: the `terminal.create` result (`terminal_id`, `window_id`, `pane_id`, `name`, the whole
`terminal`) plus `host: {pid, started}`; a dry run is `{dry_run, would_create, would_start_host}`.

### 3.2 `subak ls`

Every Terminal in creation order: id, name, state, pid, attached clients, program, `(busy)` when
it runs more than an idle shell. Without a Host it lists nothing (exit 0) and starts nothing.
`--json`: `{terminals: [summary…], host: {pid} | null}`.

```console
> subak ls
ID              NAME                  STATE         PID  CLIENTS  PROGRAM
t-3633c7e09d6d  build                 running     61928        0  C:\Windows\system32\cmd.exe
```

### 3.3 `subak status`

One answer for "what is running?": the Host (pid, version, uptime, idle grace), every Terminal
and every client with its **client id** (`subak detach --client` takes it). Without a Host it
says so and exits 0 (`running: false`). It starts nothing. The list includes the `status`
command's own connection (kind `cli`, not attached). `--json`: `{running, endpoint, host,
terminals, clients}`.

### 3.4 `subak info <target>`

One Terminal: state, window, pane, the command (or `… (verbatim)` for a verbatim line), the
resolved executable, pid, process count, size, clients, working directory, the names of its
environment overrides and how it last ended. Exit 3 (`target_not_found`) when no Terminal matches,
so `subak info work >NUL 2>&1 && …` tests for existence (tmux `has-session`).
`--json`: `{terminal}` (the IPC Terminal object).

### 3.5 `subak attach [<target>]`

```text
subak attach [<target>] [--size <cols>x<rows>] [--read-only] [--dry-run [--json]]
```

Connects this terminal to the Terminal's pane: you see its screen as it is — also a full-screen
program — with the earlier lines in this window's scrollback (once; [SCROLLBACK §2](SCROLLBACK.md)),
then live output, and your keys go to its program. Without a target it attaches to the Host's only running
Terminal (`ambiguous_target` with the candidates when there are several; `subak host attach` is
the same). The pane takes this window's size and follows it. `--read-only` watches without
sending keys or resizing. Keys, signals and what happens to your console are in [§5](#5-attach-and-detach).
It exits 0 when detached or when the program ended, with a notice on stderr:

```text
[subak: attached to `build` (t-3633c7e09d6d) in Host pid 62452, pid 61928; Ctrl+B d detaches]
...
[subak: detached; the shell keeps running in Host pid 62452 (Terminal `build`)]
```

### 3.6 `subak detach [<target>] [--client <id>]`

Detaches clients **from outside**: from another terminal, a script or an agent.

| Form | Detaches |
| --- | --- |
| `subak detach <target>` | every client attached to that Terminal (tmux `detach-client -s`) |
| `subak detach --client <id>` | that one client, wherever it is attached (`subak status` lists ids) |
| `subak detach <target> --client <id>` | that client's attachment to that Terminal only |
| `subak detach` inside a pane | every client of the pane's own Terminal (`SUBAK_TERMINAL_ID`) |

Nothing else changes: no process, no Terminal, no connection is ended, and nothing asks for
consent. Each detached `subak attach` exits 0 with `[subak: detached from `<name>` by another
client (pid …, cli); it keeps running in Host pid …]` and puts its console back. With nobody
attached the answer is an empty list (exit 0); an unknown client id is `target_not_found`.
`--dry-run` lists what would be detached. `--json`: `{detached: [{client, stream_id, pane_id,
terminal_id, interactive}], terminal, host}`.

### 3.7 `subak rename <target> <new-name>`

Renames a Terminal; its id, processes and attachments stay. Changing only the case of its own name
is allowed; another Terminal's name is `name_conflict`. `--dry-run` checks without renaming.

### 3.8 `subak stop | kill | rm <target>`

| Command | Processes | Terminal | Consent |
| --- | --- | --- | --- |
| `stop` | closed gracefully (`CTRL_CLOSE_EVENT`), whatever is left after 5 s ended | kept as `stopped` | ask or `--yes` while it runs |
| `kill` | ended at once | removed | ask or `--yes`, always |
| `rm` | refused while it runs (`invalid_state`); `--force` = `kill` | removed | ask or `--yes` |

The CLI confirms with the user, then acts on the Terminal **by id**, so a rename in between
cannot redirect it. `--dry-run` shows `busy`, the processes and whether it ends processes or
removes the Terminal ([SESSION_MODEL §4](SESSION_MODEL.md)).

### 3.9 `subak capture | history | clear-history [<target>]`

```text
subak capture [<target>] [--start <line>|-] [--end <line>|-] [--history] [--vt] [--join]
              [--no-redact] [--json]
subak history [<target>] [--json]
subak clear-history [<target>] [--yes] [--dry-run] [--json]
```

The Host keeps every pane's screen and the rows that scrolled off it (its history), in memory
only ([SCROLLBACK.md](SCROLLBACK.md)). Without a target, inside a pane these commands use that
pane (`SUBAK_PANE_ID`), else the Host's only Terminal. A `stopped` Terminal can still be read.

- **`capture`** (tmux `capture-pane -p`) prints the screen rows (`--json`: `lines` plus the
  pane's `screen` and `history` counters). Lines are numbered like tmux: screen rows from 0,
  history lines from -1 (the newest) down; `--start`/`--end` take a number or `-` (the oldest
  history line / the last screen row); `--history` is `--start -`. `--vt` keeps colours as escape
  sequences (`-e`), `--join` joins wrapped rows (`-J`). **Secret-looking values are shown as
  `***`** (tokens, passwords, keys; [SCROLLBACK §6](SCROLLBACK.md)) and stderr says how many;
  `--no-redact` shows the exact text. A long range is fetched in several requests.
- **`history`** reports the counters only: rows and bytes kept, the limits, rows evicted and
  cleared, and the screen size, alternate screen and cursor. It never prints content.
- **`clear-history`** (tmux `clear-history`) deletes the Host's copy of the history; the screen,
  the processes and what other terminals already show stay. It deletes data, so it asks, or needs
  `--yes` when it cannot ask (`confirmation_required`, exit 7); `--dry-run` shows how much.

```console
> subak capture build --history | findstr /C:"error"
> subak history build
Terminal `build` (t-3633c7e09d6d) — running: 1834 line(s) of history (152.3 KiB) kept in the Host's memory, limit 2000 lines / 4.0 MiB; 0 evicted, 0 cleared, 1834 in total; never written to disk
screen 120x30, cursor at row 29 column 15
> subak clear-history build --yes
cleared 1834 line(s) (152.3 KiB) of history of Terminal `build`; its screen and processes are unchanged
```

### 3.10 `subak capabilities`

The capabilities document ([CLI_CONTRACT §9](CLI_CONTRACT.md)): every command with usage,
effect, `dry_run` and `requires_yes`. Cheap, starts nothing.

### 3.11 The Host (`subak host …`)

`host start` (explicit start; `new` does this on demand), `host status` (pid, protocol, job,
environment, Terminals, clients with ids), `host health` (identity-checked heartbeat; exit 0 only
when healthy), `host attach` (= `subak attach` without a target), `host stop` (refused with live
Terminals unless `--kill-terminals`, then asks or `--yes`), `host serve` (run the Host in this
process). Details: [IPC_PROTOCOL §8](contracts/IPC_PROTOCOL.md), [CONPTY_PROOF](CONPTY_PROOF.md).
Every client verifies that the pipe is served by `subak.exe` of this user before it sends a byte;
a Host that does not answer is reported, never ended or replaced.

### 3.12 Keyboard shortcuts (`subak hotkeys …`)

The window's shortcuts follow the Kim hotkey contract ([HOTKEY_CONTRACT §10, §13.1](HOTKEY_CONTRACT.md),
ADR 0017, ADR 0009): the same commands, output (schema
[hotkeys-cli-v1](contracts/schemas/hotkeys-cli-v1.schema.json)) and error codes as `kim hotkeys`
and `noeul hotkeys`. They are **portable and headless**: they read and write one file, never
contact or start a Host, never open a window, never register a hotkey or install a hook.

| Command | Effect | What it does |
| --- | --- | --- |
| `subak hotkeys list [--scope <scope>] [--profile <name>]` | read | Every action with its keys, scope, source and state, plus the reserved navigation keys |
| `subak hotkeys show <action> [--profile <name>]` | read | One action and every conflict that involves it |
| `subak hotkeys set <action> <keys>... [--scope global\|app] [--trigger press\|release\|hold] [--hold-ms <ms>] [--repeat] [--profile <name>] [--replace] [--allow-conflict]` | write (`--dry-run`) | Replace an action's bindings; aliases (`ctrl+t`, `C-t`) are stored canonically |
| `subak hotkeys disable <action>\|--all\|--scope <global\|app\|context> [--profile <name>]` | write (`--dry-run`) | Disable one action, every shortcut, or one scope |
| `subak hotkeys enable --all\|--scope <family>` | write (`--dry-run`) | Turn them back on (per action: `reset`) |
| `subak hotkeys reset <action>\|--all [--yes] [--profile <name>]` | write; `--all` destructive (asks or `--yes`) | Restore the default; `--all` discards every change and keeps `hotkeys.json.bak` |
| `subak hotkeys check [--all-tools] [--min-severity error\|warning\|info] [--profile <name>]` | read | The conflict report (exit 0 even with conflicts; read `summary`) |
| `subak hotkeys export` / `import <file> [--dry-run]` | read / write | Print or replace the user layer as a `kim.hotkeys` document (import keeps a backup) |

**The actions** of this build: `terminal.new` `Ctrl+T`, `terminal.close` `Ctrl+W` (destructive:
"Close Terminal" kills it and confirms when busy, ADR 0020), `tab.next` `Ctrl+Tab`,
`tab.previous` `Ctrl+Shift+Tab`, `terminal.rename` `Ctrl+Shift+R`, `tab.hide`, `tab.move-left`,
`tab.move-right` (no default; bind them if you want keys) — all `app` scope — and, in the
`terminal` context, the Step 16 keys `clipboard.copy`, `clipboard.paste`, `view.zoom-in|out|reset`
and `scroll.page-up|page-down|line-up|line-down|top|bottom`. The prefix leader (`Ctrl+B`) and its
table (`window.*`, `pane.*`, `client.detach`, `help.show`) arrive with Step 23 and are not listed
until they work; Subak has no global action yet (`gui.show` comes with Step 18), so every
`registration` is `not_applicable`.

**The file** is `%APPDATA%\mouv\subak\hotkeys.json` (next to the window's `gui.json`). It holds
only your changes: an action you never changed follows the current default. Writes are atomic.
An open Subak window re-reads the file when it regains focus.

**Safe mode.** A file that is not valid, or was written by a newer Subak, is never rewritten
implicitly: the window uses the defaults, `list` says why (`config.state`: `invalid` or
`newer_version`, and a warning), and every change is refused (`hotkeys_invalid`, exit 4;
`hotkeys_from_newer_version`, exit 5) until you fix it or run `subak hotkeys reset --all --yes`,
which keeps the broken file as `hotkeys.json.bak` (a newer file is never replaced).
`KIM_NO_HOTKEYS=1` (or `subak-gui --no-hotkeys`) starts the window with every shortcut off
without touching the file.

**Conflicts.** A change that collides with another binding of Subak is refused (`hotkey_conflict`,
exit 7) and nothing is written; `--replace` takes the keys away from the other action and lists
both changes. A binding of yours that equals a *default* of another action wins over it
(`default_suppressed`, an info). Reserved chords (`Win+…`, `Alt+F4`, `F6`, typing keys in app
scope, IME keys) are refused with `hotkey_reserved`. `check --all-tools` adds every other
installed Kim tool by reading its installed `hotkeys.manifest.json` and its `hotkeys.json`
(files only; nothing is started).

**Keys for programs.** `Ctrl+T` and `Ctrl+W` are shortcuts of the window, so programs in a pane
do not receive them (`check` reports them as `shadows_terminal_input` infos). Until the prefix
pass-through of Step 23 (`Ctrl+B` followed by the shadowed key), disable or rebind them to give
them back to the program — for example `subak hotkeys disable terminal.close` for readline's
delete-word, or `subak hotkeys set terminal.close Ctrl+Shift+W`.

```console
> subak hotkeys set tab.next ctrl+pagedown
tab.next: Ctrl+Tab (default) → Ctrl+PageDown (user)
Saved to C:\Users\me\AppData\Roaming\mouv\subak\hotkeys.json. An open Subak window applies it when it regains focus.
> subak hotkeys set terminal.rename Ctrl+PageDown
subak: error: Ctrl+PageDown conflicts with another binding
  - duplicate_binding: Ctrl+PageDown is also bound to subak's "Next Terminal tab"; subak's "Rename Terminal" does not fire on Ctrl+PageDown.
hint: try Ctrl+Shift+PageDown or Ctrl+Alt+Shift+PageDown; --replace takes Ctrl+PageDown away from tab.next
```

## 4. Typical sessions

```console
> subak new work                 # starts the Host, creates `work`
> subak attach work              # type; Ctrl+B d detaches; close the window: still running
> subak ls
> subak rename work api
> subak detach api               # from another terminal: every client of `api` lets go
> subak kill api                 # asks; --yes in scripts
```

## 5. Attach and detach

| Key or event | Effect |
| --- | --- |
| `Ctrl+B d` | Detach. `Ctrl+B Ctrl+B` sends one `Ctrl+B` to the program; `Ctrl+B` followed by anything else sends both keys. |
| `Ctrl+C` | A key for the program (`0x03`; ConPTY raises `CTRL_C_EVENT` in the pane): it interrupts what runs there. It never ends the client. |
| `Ctrl+Break` | Detaches this client cleanly. It is not sent to the program (ConPTY has no input encoding for it; ADR 0025 §5). |
| Closing the window, killing the client | A detach as far as the Host is concerned: the Terminal keeps running. |
| `subak detach …` from elsewhere | Detach with a notice naming the other client. |
| The program exits | The client exits 0 with its exit code in the notice. |

**The detach key** is fixed in the CLI for now: `Ctrl+B d` are the defaults of the hotkey
contract's `prefix` leader and `client.detach` action ([HOTKEY_CONTRACT §13.1](HOTKEY_CONTRACT.md)).
Rebinding arrives with Subak's `hotkeys.json` in Steps 17 and 23, for the GUI and the CLI alike;
there is deliberately no CLI-only key option.

**Your console** is switched to raw mode while attached (no line editing or echo by the console,
`Ctrl+C` delivered as a key, VT input and output, UTF-8 output code page) and put back when the
attach ends. The restore runs once, from whichever comes first: the end of the attach (detach,
program end, an error), a crash (panic hook), or the console closing, logoff or shutdown (console
control handler). It first switches off what the program left on in your terminal and did not
switch off itself — alternate screen, hidden cursor, mouse reporting, bracketed paste, focus
events, application cursor keys and keypad — and only then restores the saved modes.

Measured inside an outer pseudo console (the mechanism Windows Terminal uses to host shells;
`tests/subak_cli.rs::console_attach_restores_the_console_on_every_exit_path_it_survives`), with a
probe that reads the console's state:

| Moment | Input mode | Output mode | Code page | Cursor |
| --- | --- | --- | --- | --- |
| Before the attach | `0x1F7` (cooked) | `0x7` | 65001 | visible |
| Attached (the pane's prompt hides the cursor) | `0x3E8` (raw, VT input) | `0xF` (VT) | 65001 | hidden |
| After `Ctrl+B d` | `0x1F7` | `0x7` | 65001 | visible |
| After `subak detach` from elsewhere | `0x1F7` | `0x7` | 65001 | visible |
| After `Ctrl+Break` | `0x1F7` | `0x7` | 65001 | visible |
| After the client was **killed** (`TerminateProcess`) | `0x3E8` (still raw) | `0xF` | 65001 | hidden |

A killed client runs no code, so nothing can restore the console then; the shell around it decides.
Measured (`cmd_and_windows_powershell_keep_working_after_an_attach_ends_or_is_killed`):
**cmd.exe** resets the console when the client ends and takes the next command normally (cooked,
`0x3EF`); **Windows PowerShell 5.1** (PSReadLine) also takes the next command normally, but its
console keeps `ENABLE_VIRTUAL_TERMINAL_INPUT` (`0x3E0` at the prompt, against `0x1E4` after a
clean detach). If a program started there afterwards misreads keys, open a new tab or window.
The output code page of an outer pseudo console is already 65001, so its restore is not visible
there; in a classic console window it returns to the previous value (for example 437 or 949).

**Redirected input or output.** With redirected stdin (a pipe or a file), the bytes are sent to
the program as they are (a UTF-8 byte order mark at the start is dropped) and the **end of input
detaches**; `Ctrl+C` in a console that is still attached is then a signal and detaches cleanly
(the same handler as Ctrl+Break, which is tested; this case is not tested separately).
With redirected stdout, the pane's output bytes are written unchanged, and if the reader goes
away the client detaches. Raw mode and the restore apply only to the parts that are a console. A
`--read-only` attach reads no redirected input at all and runs until it is detached, the program
ends or the Host stops: `subak attach build --read-only > build.log` records a Terminal until
`subak detach build`.

**Where it runs.** Windows Terminal, Windows PowerShell 5.1 and cmd.exe (classic console or any
ConPTY host) are supported; the automated tests run the client inside a pseudo console from
cmd.exe and from PowerShell 5.1. Not tested: Windows Terminal's own window (a GUI; tests must stay
headless — it hosts shells through the same ConPTY), PowerShell 7 (not installed on the test
machine; Subak installs nothing), and the panic path (it cannot be triggered without a test hook).

## 6. Verbatim command lines

Windows passes a program one command line, and most programs split it by the
`CommandLineToArgvW` rules, which is how `-- <program> [args…]` quotes each word. `cmd.exe /c`
reads the text after `/c` itself, with its own rules for quotes, `^`, `&`, `|` and `>`, so a word
containing `"` cannot be expressed that way. `--command-line <line>` passes one line verbatim:

```console
> subak new build --command-line "cmd.exe /d /s /c \"(echo \"a  b\"&echo ^&x)>C:\tmp\out.txt\""
```

- Only the first word is looked up on the pane's `PATH`; batch and PowerShell scripts are still
  refused (`not_executable`), so a script never runs through a shell you did not name.
- The dry run shows the exact line (`would_create.command_line`); `info` shows it with
  secret-looking words as `***` (`--token=***`).
- The line is **one argument** to `subak`, so the calling shell's quoting applies:
  - **cmd.exe** passes it unchanged; write inner quotes as `\"` as above.
  - **Windows PowerShell 5.1** splits a single-quoted line that contains `"` (legacy argument
    passing); `subak` then refuses it with `usage` and nothing starts. Use the stop-parsing token —
    `subak --% new build --command-line "cmd.exe /d /s /c \"echo \"a  b\"\""` — or write `\"`
    inside single quotes. Both were measured.
  - **PowerShell 7.3+** passes `'cmd.exe /d /s /c "echo "a  b""'` correctly (not tested here).
  - **Agents and programs** pass it as one argument of their process API (Rust `Command::arg`,
    Python `subprocess` with a list), which quotes it correctly.

Measured (`verbatim_command_lines_reach_cmd_exe_exactly_as_written`): cmd.exe received
`cmd.exe /d /s /c "(echo "a  b"&echo ^&x&echo two)>"…\out.txt""` unchanged and wrote the three
lines `"a  b"`, `&x` and `two` (exit 0); the same text given as separate words reached cmd with
`\"` and wrote nothing (exit 1).

## 7. Recipes for agents and scripts

Always pass `--json` (and `--no-input`, or set `KIM_NO_INPUT=1`), close stdin, start `subak`
without a console window, and branch on the exit code or `error.code`, never on message text.

| Task | Command |
| --- | --- |
| Is anything running? | `subak status --json` → `data.running`, `data.terminals` |
| Does `build` exist? | `subak info build --json` → exit 0, or 3 with `target_not_found` |
| Create, or reuse when it exists | `subak new build --json -- …`; on exit 7 `name_conflict`, `subak info build --json` |
| Create only if a Host already runs | `subak new build --no-start --json -- …` (exit 3 otherwise) |
| Check a program before starting it | `subak new --dry-run --json -- pwsh` → `would_create.executable`, or `spawn_failed` |
| Run cmd.exe text exactly | `subak new job --json --remain-on-exit --command-line "cmd.exe /d /s /c \"…\""` |
| Wait for a program to finish | poll `subak info job --json` until `state` is `stopped` (with `--remain-on-exit`), then read `last_exit.exit_code` |
| Feed input non-interactively | pipe bytes into `subak attach job`; the end of input detaches |
| Record output | `subak attach job --read-only > job.log`, later `subak detach job` |
| Read what a pane shows, or its whole history | `subak capture job --json` → `data.lines`; `subak capture job --history --json` (redacted; `--no-redact` only with the user's consent) |
| Read the output of a finished program | create it with `--remain-on-exit`, wait for `stopped`, then `subak capture job --history --json` |
| Make sure nobody is watching | `subak detach job --json` |
| Stop work, with the user's consent | `subak stop job --yes --json` (only after the user confirmed in your UI; CLI_CONTRACT §9.6) |
| Remove everything and the Host | `subak host stop --kill-terminals --yes --json` (destructive) |

A command that would need consent and cannot ask fails with `confirmation_required` (exit 7) and
says what `--yes` would confirm. Nothing an agent runs this way opens a window or prompts.

## 8. tmux at a glance

| tmux | Subak |
| --- | --- |
| `tmux new -s work [-d]` | `subak new work` (detached by default; `--attach` attaches) |
| `tmux new -A -s work` | `subak info work` then `subak attach work` or `subak new work --attach` |
| `tmux -N` | `subak new --no-start` |
| `tmux ls` | `subak ls` |
| `tmux has -t work` | `subak info work` (exit 0 / 3) |
| `tmux attach -t work [-r]` | `subak attach work [--read-only]` |
| `tmux detach` (`C-b d`) | `Ctrl+B d` |
| `tmux detach -s work` / `-t client` | `subak detach work` / `subak detach --client <id>` |
| `tmux lsc` | `subak status` (clients with ids) |
| `tmux rename -t work api` | `subak rename work api` |
| `tmux kill-session -t work` | `subak kill work` |
| `tmux capture-pane -p [-S -] [-e] [-J] -t work` | `subak capture work [--history] [--vt] [--join]` |
| `tmux clear-history -t work` | `subak clear-history work --yes` |
| `set -g history-limit 5000` | `subak new work --history-limit 5000` (per Terminal) |
| `tmux start` | `subak host start` (rarely needed) |

The full matrix with status and evidence is [TMUX_COMPATIBILITY.md](TMUX_COMPATIBILITY.md).

## 9. Evidence

Automated on Windows 10 Pro 19045 with real `subak.exe` processes on private endpoints
(`crates/subak/tests/subak_cli.rs`; parser and output unit tests in `crates/subak/src`; the e2e
scenario `e2e/tests/step12_subak_cli.rs`):

| Test | Shows |
| --- | --- |
| `every_command_answers_one_schema_valid_document_with_stable_exit_codes` | every command's `--json` validated against `subak-cli-v1`, golden files, exit codes, consent (`--no-input`, `KIM_NO_INPUT`), the deprecated alias, secrets never printed |
| `a_user_creates_attaches_detaches_lists_renames_and_kills_with_the_cli_only` | the acceptance journey with no window: new (Host on demand), attach, detach by Terminal and by client id, read-only, rename while attached, `Ctrl+B d`, kill |
| `client_exit_during_attach_leaves_host_work_running` | a killed client and a client whose reader vanished leave `ping` running in the Host; a new client sees it continue |
| `client_detach_over_ipc_ends_only_attachments_and_tells_the_detached_client` | IPC `client.detach` by target and by client id, `pane.detached` before the output stops, connections and the shell untouched, refusals (`invalid_params`, `target_not_found`, MCP `unauthorized`); every message validated against `ipc-v1` |
| `concurrent_commands_share_one_host_and_never_duplicate_a_name` | 6 concurrent `new` → one Host, one Terminal, five `name_conflict`; mixed concurrent creates, lists and status calls; concurrent kills |
| `verbatim_command_lines_reach_cmd_exe_exactly_as_written` | §6 |
| `console_attach_restores_the_console_on_every_exit_path_it_survives` | §5 table |
| `cmd_and_windows_powershell_keep_working_after_an_attach_ends_or_is_killed` | §5, killed clients under cmd.exe and PowerShell 5.1 |
| `the_cli_schema_embeds_the_ipc_and_capabilities_definitions_unchanged` | `subak-cli-v1` shares the IPC and capabilities definitions verbatim |
| `scrollback_replay.rs` (Step 13) | `capture`, `history`, `clear-history` and `--history-limit` against real panes; reattach coherence, concurrency, resize, eviction, resync, idle cost, redaction ([SCROLLBACK §8](SCROLLBACK.md)) |
| `hotkeys_cli.rs` (Step 17) | every `subak hotkeys` command in a sandboxed profile, validated against `hotkeys-cli-v1`: the shipped defaults, dry runs that write nothing, refused reserved, invalid, unknown and conflicting changes that write nothing, `--replace`, switches, `reset --all` needing `--yes` and keeping a backup, safe mode for a broken and a newer file, export/import keeping unknown fields, `check --all-tools` reading a simulated Noeul install without starting it (also `e2e/tests/step17_subak_hotkeys.rs`) |
| `milestone_a.rs` (Step 14) | real `subak attach` clients killed, detached by `Ctrl+B d`, by the end of input and by `subak detach`, reattached to the same programs with their state; 60 cycles without leaks; a reattached client's output equals the Host's history and screen; clients exit 6 when the Host is killed ([MILESTONE_A_VALIDATION](MILESTONE_A_VALIDATION.md)) |

## 10. Not implemented yet

- `pane.send_keys` / `subak send-keys` (Step 24); the CLI writes input only through an attach.
  `attach` does not reconnect by itself.
- Persisted history (Steps 29+, opt-in) and search in history (GUI, Steps 16–25).
- Rebinding the console detach key (`Ctrl+B d` of `subak attach`; Step 23 makes the prefix a
  hotkey-contract leader), `new -A`, `attach -d`, format strings (`-F`,
  Step 25), windows and panes (Steps 21–22).
- Persistence and recovery (Steps 29–30): Terminals end with their Host.
- Linux (M4): every command answers `unsupported_platform`.
