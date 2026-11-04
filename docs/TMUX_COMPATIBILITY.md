# tmux Compatibility Matrix

Status: **current** (inventory) · Reference: tmux 3.8 · Matrix version: 1 (2026-11-03) · Owner:
mouv · Tracks [SUBAK-FR-060](PRODUCT_REQUIREMENTS.md)

> **Only the session basics are implemented, partially** (Step 10: named Terminals in the Host
> and `subak terminal new|list|info|rename|attach|stop|kill|rm`; Step 11: pane programs and
> environment). Four environment rows are `implemented` and eleven are `partial`, all with
> automated evidence; every other row is `planned`, `deferred` or `unsupported`. This document
> is an inventory and a plan, not a compatibility claim.

## 0. How to read this matrix

**Reference.** The inventory follows the official tmux manual page of the **tmux 3.8 release**
(git tag `3.8`, commit `7f2a35ad3321f9ba57a1062ca73b1f3ff26aca53`; `tmux.1` revision 1.1173,
`$Mdocdate: September 23 2026`), fetched from
<https://raw.githubusercontent.com/tmux/tmux/3.8/tmux.1>. Where the manual page is incomplete,
the same tag's sources were used: commands and aliases from `cmd-*.c` (92 commands), default
key bindings from `key-bindings.c`, option names from `options-table.c`. tmux `master` at commit `a73616d781f2` (2026-10-30) was also read; its
unreleased changes are listed in §24 and are **not** part of the matrix.

**Terms.** Subak's object model is Terminal → internal Window → Pane → process
([SUBAK_PHILOSOPHY.md](SUBAK_PHILOSOPHY.md) §2). A Subak **Terminal** is a tmux **session** and
appears as one GUI tab. A Subak internal **Window** is a tmux **window**; it is *not* a GUI tab.

**Columns.**

| Column | Meaning |
| --- | --- |
| tmux feature | What tmux does, in a few words |
| tmux reference | Command (alias), key, option, format or syntax, as in tmux 3.8 |
| Subak equivalent (planned) | How Subak intends to provide it; for `deferred`/`unsupported` the *Rationale* |
| Status | One of the five values below |
| Target Step | Step issue expected to deliver it (`10, 12` = Host part in 10, CLI part in 12) |
| Evidence | Automated test name/path and date (see §26); `—` until then |

**Status values.** `planned` — intended, not built · `partial` — some documented behaviour has
automated evidence · `implemented` — the whole row has automated evidence · `deferred` — not
planned before Step 26; may be revisited · `unsupported` — will not be provided, by design.
`implemented` is used since Step 11, for four environment rows.

**Target Steps.**

| Step | Scope |
| --- | --- |
| 10 | Session/Terminal CRUD in the Host |
| 11 | Pane programs and their environment ([PTY_PROCESSES](PTY_PROCESSES.md)) |
| 12 | Subak CLI: `new`, `ls`, `info`, `attach`, `detach`, `rename`, `kill`, `host status` |
| 13 | Scrollback and replay (`capture-pane`, `clear-history`) |
| 21 | Internal Windows and Panes (create, list, select, rename, kill) |
| 22 | Splitting and layout (split, resize, swap, zoom, rotate, basic layouts) |
| 23 | Prefix and key-binding engine; default key table; `Ctrl+B Ctrl+B` literal prefix |
| 24 | Command prompt, target syntax, copy mode, paste buffers, join/break-pane, detach |
| 25 | Advanced: layout presets, linked windows, synchronize-panes, command chains, hooks, options, status line, alerts, `switch-client`, `if-shell`/`run-shell`, `display-panes`, `choose-tree`, environment, `source-file` |
| 26 | Full validation and the published compatibility statement |
| post-26 | Deferred; no Step assigned |

**Conventions.** "Host op" = an operation of the Host's IPC API (Step 10), later exposed by the
CLI and GUI. "CLI verb TBD" = the CLI spelling is decided in the listed Step; fixed names are the Step 12
verbs above and `subak capture` (ADR 0008). Keys written `Ctrl+B x` are Subak GUI keys; `C-b x` are tmux keys.
"tmux-syntax commands" = tmux command lines accepted by Subak's prompt (`Ctrl+B :`) and by
`source-file`; a CLI entry point for them is TBD.

## 1. Invocation (`tmux` command-line flags)

| tmux feature | tmux reference | Subak equivalent (planned) | Status | Target Step | Evidence |
| --- | --- | --- | --- | --- | --- |
| Run a command against the running server | `tmux <command> [flags]` | `subak <verb>` CLI with human and `--json` output (KIM-FR-010); tmux-syntax entry point TBD | planned | 12 | — |
| Run the default command when none is given | `tmux` (no args) → `default-client-command` (`new-session`) | Behaviour of bare `subak` decided in Step 12; `subak new` is explicit | planned | 12 | — |
| Shortest unambiguous command prefix | e.g. `tmux list-se` | Accepted by the tmux-syntax parser (prompt, `source-file`) | planned | 24 | — |
| Force 256 colours | `-2` | Always full colour in the GUI; no flag needed | planned | 12 | — |
| UTF-8 output | `-u` | Always UTF-8 (KIM-NFR-013); no flag needed | planned | 12 | — |
| Control mode | `-C`, `-CC` | *Rationale:* control mode is deferred (§19); Subak's own IPC and `--json` CLI serve integrations | deferred | post-26 | — |
| Run a shell command as a login shell | `-c shell-command` | *Rationale:* tmux-as-login-shell has no Windows analogue | unsupported | — | — |
| Behave as a login shell | `-l` | *Rationale:* no-op compatibility flag for login shells; no Windows analogue | unsupported | — | — |
| Do not daemonize the server | `-D` | `subak host` in the foreground for debugging (flag TBD) | planned | 10 | — |
| Alternative configuration file | `-f file` | Subak config path option (TBD); a tmux-syntax file is read with `source-file` | planned | 25 | — |
| Help | `-h` | `subak --help` (KIM-FR-011) | planned | 12 | — |
| Named or explicit server socket | `-L socket-name`, `-S socket-path` | *Rationale:* no socket compatibility; one Host per user over local, same-user IPC (SUBAK-SEC-010) | unsupported | — | — |
| Do not start the server | `-N` | CLI option that never starts the Host (name TBD) | planned | 12 | — |
| Declare outer-terminal features | `-T features` | *Rationale:* applies to an outer terminal; the GUI renders natively. Revisit if a console-rendering client is built | deferred | post-26 | — |
| Print version | `-V` | `subak --version` | planned | 12 | — |
| Verbose logging | `-v`, `-vv` | `--verbose` (KIM-FR-011); local Host log without terminal content (KIM-SEC-012) | planned | 12 | — |
| Exit status and error messages | exit codes, `no server running`, etc. | Documented exit codes and stable error codes (KIM-FR-011) | planned | 12 | — |

## 2. Commands (all 92, with aliases)

Grouped by the manual page section that documents them. `set-window-option` and
`show-window-options` are registered in `cmd-*.c` but no longer documented in `tmux.1`.

### 2.1 Clients and sessions

| tmux feature | tmux reference | Subak equivalent (planned) | Status | Target Step | Evidence |
| --- | --- | --- | --- | --- | --- |
| Attach a client to a session | `attach-session` (`attach`) | `subak attach <terminal>`; GUI opens or focuses the Terminal's tab. *Partial:* `subak terminal attach <terminal>` (Step 10); the short form, `-d`/`-r` and the GUI later | partial | 12 | `crates/subak/tests/session_lifecycle.rs::detach_disconnect_and_client_crash_never_stop_a_terminal` (2026-11-04) |
| Detach a client | `detach-client` (`detach`) | `subak detach`; closing the GUI is a detach (SUBAK-FR-015) | planned | 12 | — |
| Test whether a session exists | `has-session` (`has`) | `subak info <terminal>` exit code. *Partial:* `subak terminal info <terminal>` exits 0, or 3 (`target_not_found`) | partial | 12 | `crates/subak/tests/session_lifecycle.rs::names_are_unique_ignoring_case_and_composition_and_renames_keep_ids` (2026-11-04) |
| Kill the server and every session | `kill-server` | Kill every Terminal (explicit, confirmed when busy; SUBAK-FR-014); the Host then exits (SUBAK-FR-002). CLI verb TBD | planned | 25 | — |
| Kill a session | `kill-session` | Host delete op; `subak kill <terminal>` with `--dry-run` and `--force` (SUBAK-FR-014). *Partial:* `terminal.kill` and `subak terminal kill <terminal>` (`--dry-run`, consent or `--yes`; busy Terminals need it); `-a`, `-C` not yet | partial | 10, 12 | `crates/subak/tests/session_lifecycle.rs::stop_kill_and_rm_have_distinct_explicit_guarded_meanings` (2026-11-04) |
| List clients | `list-clients` (`lsc`) | `subak host status` lists attached clients | planned | 12 | — |
| List commands | `list-commands` (`lscm`) | List supported tmux-syntax commands with usage | planned | 24 | — |
| List sessions | `list-sessions` (`ls`) | `subak ls` (human and `--json`; SUBAK-FR-012). *Partial:* `terminal.list` and `subak terminal list` (human and `--json`); Claude status (Step 28) and `-F`/`-f` not yet | partial | 12 | `crates/subak/tests/session_lifecycle.rs::zero_one_and_many_terminals_have_isolated_process_trees` (2026-11-04) |
| Lock a client | `lock-client` (`lockc`) | *Rationale:* the OS lock screen protects the desktop; Subak never locks a session | unsupported | — | — |
| Lock the server | `lock-server` (`lock`) | *Rationale:* as `lock-client` | unsupported | — | — |
| Lock a session | `lock-session` (`locks`) | *Rationale:* as `lock-client` | unsupported | — | — |
| Create a session | `new-session` (`new`) | Host create op from a profile; `subak new` and GUI `Ctrl+T` (SUBAK-FR-011). *Partial:* `terminal.create` and `subak terminal new [<name>] [-- <command>]`; profiles (Step 33) and `Ctrl+T` (Step 16) not yet | partial | 10, 12 | `crates/subak/tests/session_lifecycle.rs::zero_one_and_many_terminals_have_isolated_process_trees` (2026-11-04) |
| Redraw, resize or pan a client | `refresh-client` (`refresh`) | Redraw and client flags; control-mode parts (`-A`, `-B`, `-C`) deferred with §19 | planned | 25 | — |
| Rename a session | `rename-session` (`rename`) | Host rename op; `subak rename` and GUI tab rename. *Partial:* `terminal.rename` and `subak terminal rename`; the GUI later | partial | 10, 12 | `crates/subak/tests/session_lifecycle.rs::names_are_unique_ignoring_case_and_composition_and_renames_keep_ids` (2026-11-04) |
| Grant other users access | `server-access` | *Rationale:* the Host is per-user by design; IPC is restricted to the same user (SUBAK-SEC-010) | unsupported | — | — |
| Show the message log | `show-messages` (`showmsgs`) | Host message log, local only | planned | 25 | — |
| Run commands from a file | `source-file` (`source`) | Parse a tmux-syntax file (§17) | planned | 25 | — |
| Start the server | `start-server` (`start`) | The Host starts on demand (SUBAK-FR-002); explicit start verb TBD. *Partial:* `subak terminal new` starts it on demand and `subak host start` explicitly | partial | 10 | `crates/subak/tests/session_lifecycle.rs::exits_restarts_and_reconnects_leave_no_stale_state` (2026-11-04) |
| Suspend a client | `suspend-client` (`suspendc`) | *Rationale:* no SIGTSTP or job control on Windows; minimizing the GUI is the analogue | unsupported | — | — |
| Switch a client to another session | `switch-client` (`switchc`) | Show another Terminal in the current client; `-T` (key table) is part of Step 23 | planned | 25 | — |

### 2.2 Windows and panes

| tmux feature | tmux reference | Subak equivalent (planned) | Status | Target Step | Evidence |
| --- | --- | --- | --- | --- | --- |
| Enter copy mode | `copy-mode` | Copy mode with vi and emacs tables (§7) | planned | 24 | — |
| Break a pane out into a new window | `break-pane` (`breakp`) | Move a Pane into a new internal Window | planned | 24 | — |
| Capture pane contents | `capture-pane` (`capturep`) | `subak capture`: screen/scrollback as text or JSON (SUBAK-FR-012, ADR 0008) | planned | 13 | — |
| Choose a client from a list | `choose-client` | Client chooser overlay | planned | 25 | — |
| Choose a session, window or pane from a tree | `choose-tree` | Tree chooser overlay; the GUI session manager covers Terminals (SUBAK-FR-032) | planned | 25 | — |
| Customize options interactively | `customize-mode` | *Rationale:* a native settings UI is the natural home; revisit after the options engine | deferred | post-26 | — |
| Fuzzy session/window switcher (new in 3.8) | `switch-mode` | Fuzzy Terminal/Window switcher overlay | planned | 25 | — |
| Show pane numbers | `display-panes` (`displayp`) | Pane-number overlay; typing a number focuses that Pane | planned | 25 | — |
| Find a window by name, title or content | `find-window` (`findw`) | Search Windows by name, title or content | planned | 25 | — |
| Join a pane into another window | `join-pane` (`joinp`) | Move a Pane into another Window as a split | planned | 24 | — |
| Kill a pane | `kill-pane` (`killp`) | Close the Pane and end its process tree (SUBAK-FR-016) | planned | 21 | — |
| Kill a window | `kill-window` (`killw`) | Close the Window and end its Panes' processes | planned | 21 | — |
| Select the previous pane | `last-pane` (`lastp`) | Focus the previously active Pane | planned | 21 | — |
| Select the previous window | `last-window` (`last`) | Focus the previously current Window | planned | 21 | — |
| Link a window into another session | `link-window` (`linkw`) | Show one Window in several Terminals | planned | 25 | — |
| List panes | `list-panes` (`lsp`) | Host op and CLI listing with stable IDs (SUBAK-FR-010) | planned | 21 | — |
| List windows | `list-windows` (`lsw`) | Host op and CLI listing with stable IDs | planned | 21 | — |
| Move a pane | `move-pane` (`movep`) | Move a Pane between Windows (like `join-pane`); floating-pane positions (`-P`, `-M`, `-X`, `-Y`) deferred with `new-pane` | planned | 24 | — |
| Move or renumber a window | `move-window` (`movew`) | Reorder or move a Window; `-r` renumbers | planned | 25 | — |
| Create a window | `new-window` (`neww`) | New internal Window in the current Terminal (not a GUI tab) | planned | 21 | — |
| Create a floating pane (new in 3.8) | `new-pane` (`newp`) | *Rationale:* floating and modal panes are new in tmux 3.8; the GUI overlay design needs an ADR; tiled panes come first | deferred | post-26 | — |
| Select the next window | `next-window` (`next`) | Focus the next Window; `-a` (next with alert) in Step 25 | planned | 21 | — |
| Select the next layout | `next-layout` (`nextl`) | Cycle layout presets | planned | 25 | — |
| Pipe pane output to a command | `pipe-pane` (`pipep`) | The Host streams pane output (`-O`) or input (`-I`) to a command | planned | 25 | — |
| Select the previous layout | `previous-layout` (`prevl`) | Cycle layout presets backwards | planned | 25 | — |
| Select the previous window | `previous-window` (`prev`) | Focus the previous Window | planned | 21 | — |
| Rename a window | `rename-window` (`renamew`) | Rename the internal Window (SUBAK-FR-010) | planned | 21 | — |
| Resize or zoom a pane | `resize-pane` (`resizep`) | Resize a split by cells or `%`; `-Z` zoom; `-M` mouse drag | planned | 22 | — |
| Resize a window | `resize-window` (`resizew`) | Manual Window size (with `window-size`) | planned | 25 | — |
| Restart a pane's process | `respawn-pane` (`respawnp`) | Start a new process in a dead Pane; `-k` kills a live one (confirmed) | planned | 25 | — |
| Restart a window's process | `respawn-window` (`respawnw`) | As `respawn-pane` for a Window | planned | 25 | — |
| Rotate panes | `rotate-window` (`rotatew`) | Rotate Pane positions (`-U`, `-D`) | planned | 22 | — |
| Select a layout | `select-layout` (`selectl`) | Basic layouts in 22; presets, layout strings and `-o` undo in 25 | planned | 22, 25 | — |
| Select a pane | `select-pane` (`selectp`) | Focus a Pane, incl. `-L`/`-R`/`-U`/`-D`; `-T` title; `-m`/`-M` mark (24); `-d`/`-e` input off/on (25) | planned | 21 | — |
| Select a window | `select-window` (`selectw`) | Focus a Window by index, ID or name; `-T` toggles to last | planned | 21 | — |
| Split a pane | `split-window` (`splitw`) | Split `-h`/`-v`, `-l` size or `%`, `-b` before, `-f` full width; GUI split (SUBAK-FR-035) | planned | 22 | — |
| Swap panes | `swap-pane` (`swapp`) | Swap Pane positions (`-U`, `-D`, `-s`/`-t`) | planned | 22 | — |
| Swap windows | `swap-window` (`swapw`) | Swap Window indexes | planned | 25 | — |
| Unlink a window | `unlink-window` (`unlinkw`) | Remove a linked Window from one Terminal | planned | 25 | — |

### 2.3 Key bindings

| tmux feature | tmux reference | Subak equivalent (planned) | Status | Target Step | Evidence |
| --- | --- | --- | --- | --- | --- |
| Bind a key | `bind-key` (`bind`) | Key-binding engine: `-T` table, `-n` root table, `-r` repeatable, `-N` note | planned | 23 | — |
| List key bindings | `list-keys` (`lsk`) | List bindings and notes (`-N`, `-T`, `-1`, `-a`, `-P`) | planned | 23 | — |
| Send keys to a pane | `send-keys` (`send`) | Host send-input op (SUBAK-FR-013) with tmux key names; `-l`, `-H`, `-N`, `-R`; `-X` copy-mode commands in 24; `-M` mouse in 25 | planned | 23 | — |
| Send the prefix key | `send-prefix` | Send `Ctrl+B` (or `prefix2` with `-2`) to the Pane | planned | 23 | — |
| Unbind a key | `unbind-key` (`unbind`) | Remove a binding (`-a` all, `-n`, `-T`, `-q`) | planned | 23 | — |

### 2.4 Options, hooks and environment

| tmux feature | tmux reference | Subak equivalent (planned) | Status | Target Step | Evidence |
| --- | --- | --- | --- | --- | --- |
| Set an option | `set-option` (`set`) | Options engine with server/session/window/pane scopes (§9) | planned | 25 | — |
| Show options | `show-options` (`show`) | Show options and inheritance (`-A`), value only (`-v`) | planned | 25 | — |
| Set a window option (legacy) | `set-window-option` (`setw`) | Alias of `set-option -w` | planned | 25 | — |
| Show window options (legacy) | `show-window-options` (`showw`) | Alias of `show-options -w` | planned | 25 | — |
| Set a hook | `set-hook` | Hook engine (§12) | planned | 25 | — |
| Show hooks | `show-hooks` | List hooks and subscriptions | planned | 25 | — |
| Set an environment variable | `set-environment` (`setenv`) | Terminal or global environment (§15) | planned | 25 | — |
| Show the environment | `show-environment` (`showenv`) | Show variables; `-s` shell syntax; `-h` hidden | planned | 25 | — |

### 2.5 Status line, prompts and menus

| tmux feature | tmux reference | Subak equivalent (planned) | Status | Target Step | Evidence |
| --- | --- | --- | --- | --- | --- |
| Clear prompt history | `clear-prompt-history` (`clearphist`) | Clear command-prompt history (per type with `-T`) | planned | 24 | — |
| Command prompt | `command-prompt` | `Ctrl+B :` prompt for tmux-syntax commands; `-I`, `-p`, `-1`, `-k`, `-N`, `-T` | planned | 24 | — |
| Confirm before running a command | `confirm-before` (`confirm`) | Confirmation prompt (`-p`, `-y`, `-c`); the `x`/`&` confirmation exists from Step 23 | planned | 24 | — |
| Show a menu | `display-menu` (`menu`) | *Rationale:* GUI context menus are designed natively first; tmux menu definitions later | deferred | post-26 | — |
| Show a message or print a format | `display-message` (`display`) | Message line; `-p` prints an expanded format to stdout | planned | 25 | — |
| Show a popup | `display-popup` (`popup`) | *Rationale:* a popup overlay needs a GUI design and ADR | deferred | post-26 | — |
| Show prompt history | `show-prompt-history` (`showphist`) | Show command-prompt history | planned | 24 | — |

### 2.6 Buffers

| tmux feature | tmux reference | Subak equivalent (planned) | Status | Target Step | Evidence |
| --- | --- | --- | --- | --- | --- |
| Choose a paste buffer | `choose-buffer` | Buffer chooser overlay | planned | 24 | — |
| Clear pane history | `clear-history` (`clearhist`) | Clear the Pane's scrollback; `-H` also clears hyperlinks | planned | 13 | — |
| Delete a paste buffer | `delete-buffer` (`deleteb`) | Delete a buffer | planned | 24 | — |
| List paste buffers | `list-buffers` (`lsb`) | List buffers (`-F`, `-f`, `-O`) | planned | 24 | — |
| Load a buffer from a file | `load-buffer` (`loadb`) | Load a file or stdin (`-`); `-w` also sets the clipboard | planned | 24 | — |
| Paste a buffer | `paste-buffer` (`pasteb`) | Paste into a Pane; `-p` bracketed, `-r`, `-s`, `-d` | planned | 24 | — |
| Save a buffer to a file | `save-buffer` (`saveb`) | Write a buffer to a file or stdout; `-a` appends | planned | 24 | — |
| Set a paste buffer | `set-buffer` (`setb`) | Set, append (`-a`) or rename (`-n`) a buffer; `-w` clipboard | planned | 24 | — |
| Show a paste buffer | `show-buffer` (`showb`) | Print a buffer | planned | 24 | — |

### 2.7 Miscellaneous

| tmux feature | tmux reference | Subak equivalent (planned) | Status | Target Step | Evidence |
| --- | --- | --- | --- | --- | --- |
| Show a big clock in a pane | `clock-mode` | *Rationale:* novelty with low value; the OS clock is visible | deferred | post-26 | — |
| Run a command conditionally | `if-shell` (`if`) | Shell or format (`-F`) condition, then a tmux-syntax command; uses the profile shell, not `/bin/sh` (§18) | planned | 25 | — |
| Run a shell command | `run-shell` (`run`) | The Host runs a command (`-b`, `-d` delay, `-C` tmux command, `-c` cwd, `-t` output pane) | planned | 25 | — |
| Wait for, signal or lock a channel | `wait-for` (`wait`) | Named channels: wait, `-S` signal, `-L` lock, `-U` unlock | planned | 25 | — |

## 3. Notable command flags

| tmux feature | tmux reference | Subak equivalent (planned) | Status | Target Step | Evidence |
| --- | --- | --- | --- | --- | --- |
| Create or attach if it exists | `new-session -A` | Attach when the named Terminal exists (flag TBD) | planned | 12 | — |
| Create without attaching | `new-session -d` | Detached creation (the CLI default) | planned | 12 | — |
| Session and first window names | `new-session -s name -n name` | Terminal name and first Window name | planned | 12 | — |
| Start directory, environment, size | `new-session -c`, `-e VAR=val`, `-x`, `-y` | Profile cwd and environment (SUBAK-FR-011); default size. *Partial:* `--cwd`, `--size` and `--env NAME=VALUE` / `--env NAME` / `--unset-env NAME` (Step 11, [PTY_PROCESSES §3](PTY_PROCESSES.md)); profiles with Step 33 | partial | 11 | `crates/subak/tests/session_lifecycle.rs::names_are_unique_ignoring_case_and_composition_and_renames_keep_ids`, `crates/subak/tests/pty_shells.rs::explicit_programs_get_their_arguments_exactly_and_report_exit_codes` (2026-11-04) |
| Print the created object | `new-session -P -F format`, `new-window -P` | `--json` output (KIM-FR-010); `-F` formats in Step 25 | planned | 12 | — |
| Command given as argv (no shell) | `new-window vi file`, `split-window cmd args` | Start the program directly without a shell | planned | 21 | — |
| Session groups | `new-session -t target-session` | *Rationale:* grouped sessions share windows with independent current windows; linked windows (Step 25) come first | deferred | post-26 | — |
| Detach other clients | `attach-session -d`, `new-session -D` | Detach every other client of the Terminal | planned | 12 | — |
| Detach all others or a whole session | `detach-client -a`, `-s target-session` | Detach selected clients | planned | 12 | — |
| Send SIGHUP to the client's parent | `attach-session -x`, `new-session -X`, `detach-client -P` | *Rationale:* no SIGHUP on Windows | unsupported | — | — |
| Skip `update-environment` | `attach-session -E`, `new-session -E`, `new-window -E` | Same flag semantics (§15) | planned | 25 | — |
| Kill all other sessions, windows or panes | `kill-session -a`, `kill-window -a`, `kill-pane -a` | Multi-target delete, confirmed when busy | planned | 21 | — |
| Clear alert flags | `kill-session -C` | Clear bell/activity/silence flags | planned | 25 | — |
| Window placement and reuse | `new-window -a`, `-b`, `-d`, `-k`, `-S` | Insert after/before, do not focus, replace, select existing | planned | 21 | — |
| Split size, side and span | `split-window -l N`, `-l N%`, `-b`, `-f`, `-d` | Split options | planned | 22 | — |
| Keep zoom while navigating | `-Z` on `select-pane`, `last-pane`, `swap-pane`, `rotate-window`, `split-window` | Zoom-preserving navigation | planned | 22 | — |
| Capture options | `capture-pane -p -e -J -N -S -E -a -b -C -M -P -q -T` | Text (`-p`), styles (`-e`), joined lines (`-J`), ranges (`-S`/`-E`), alternate screen (`-a`) | planned | 13 | — |
| Copy-mode entry options | `copy-mode -u -d -e -H -M -q -S -s -k` | Scroll on entry, exit at bottom, mouse entry, quit all modes | planned | 24 | — |
| List filters and sort order | `list-* -f filter -O order -r` | CLI filter and sort; format filters in Step 25 | planned | 25 | — |
| Send keys to a client's key table | `send-keys -K -c target-client` | Inject keys into a client's key handling | planned | 23 | — |

## 4. Default key bindings: prefix table (`C-b`)

Complete list from `key-bindings.c` of tmux 3.8 (the manual page lists only part of it). Keys in
the basic table of SUBAK-FR-033 are marked *basic*. A key can only work once the Step 23 engine
exists, so no key row targets a Step before 23.

| tmux feature | tmux reference | Subak equivalent (planned) | Status | Target Step | Evidence |
| --- | --- | --- | --- | --- | --- |
| Send the prefix key to the program | `C-b C-b` → `send-prefix` | `Ctrl+B Ctrl+B` sends a literal `Ctrl+B` (*basic*) | planned | 23 | — |
| Rotate panes forwards | `C-b C-o` → `rotate-window` | Same key | planned | 23 | — |
| Suspend the client | `C-b C-z` → `suspend-client` | *Rationale:* no SIGTSTP on Windows; the key stays unbound | unsupported | — | — |
| Next layout preset | `C-b Space` → `next-layout` | Same key | planned | 25 | — |
| Break pane to a new window | `C-b !` → `break-pane` | Same key | planned | 24 | — |
| Split top/bottom | `C-b "` → `split-window` | Same key (*basic*) | planned | 23 | — |
| List paste buffers | `C-b #` → `list-buffers` | Same key | planned | 24 | — |
| Rename the session | `C-b $` → `command-prompt -I'#S' { rename-session }` | Rename the Terminal (and its tab) (*basic*); full prompt in 24 | planned | 23 | — |
| Split left/right | `C-b %` → `split-window -h` | Same key (*basic*) | planned | 23 | — |
| Kill the window (confirm) | `C-b &` → `confirm-before kill-window` | Same key with confirmation (*basic*; SUBAK_PHILOSOPHY §6.1) | planned | 23 | — |
| Prompt for a window index | `C-b '` → `command-prompt -pindex` | Same key | planned | 24 | — |
| New floating pane | `C-b *` → `new-pane` | *Rationale:* floating panes deferred (see `new-pane`) | deferred | post-26 | — |
| Toggle floating/tiled pane | `C-b @` → `join-pane` / `break-pane -W` | *Rationale:* floating panes deferred | deferred | post-26 | — |
| Previous session | `C-b (` → `switch-client -p` | Previous Terminal in the current client (GUI analogue: `Ctrl+Shift+Tab`) | planned | 25 | — |
| Next session | `C-b )` → `switch-client -n` | Next Terminal (GUI analogue: `Ctrl+Tab`) | planned | 25 | — |
| Rename the window | `C-b ,` → `command-prompt -I'#W' { rename-window }` | Rename the internal Window (*basic*); full prompt in 24 | planned | 23 | — |
| Delete the latest paste buffer | `C-b -` → `delete-buffer` | Same key | planned | 24 | — |
| Move the window (prompt) | `C-b .` → `command-prompt { move-window }` | Same key | planned | 25 | — |
| Describe a key binding | `C-b /` → `list-keys -1N` | Same key | planned | 23 | — |
| Select window 0–9 | `C-b 0` … `C-b 9` → `select-window -t:=N` | Same keys (*basic*) | planned | 23 | — |
| Command prompt | `C-b :` → `command-prompt` | Same key | planned | 24 | — |
| Previously active pane | `C-b ;` → `last-pane` | Same key | planned | 23 | — |
| Choose a paste buffer | `C-b =` → `choose-buffer -Z` | Same key | planned | 24 | — |
| List key bindings | `C-b ?` → `list-keys -N` | Same key | planned | 23 | — |
| Choose a client to detach | `C-b D` → `choose-client -Z` | Same key | planned | 25 | — |
| Spread panes out evenly | `C-b E` → `select-layout -E` | Same key | planned | 23 | — |
| Last session | `C-b L` → `switch-client -l` | Same key | planned | 25 | — |
| Clear the marked pane | `C-b M` → `select-pane -M` | Same key | planned | 24 | — |
| Set the pane title | `C-b T` → `command-prompt -I'#T' { select-pane -T }` | Same key | planned | 24 | — |
| Enter copy mode | `C-b [` → `copy-mode` | Same key | planned | 24 | — |
| Paste the latest buffer | `C-b ]` → `paste-buffer -p` | Same key | planned | 24 | — |
| New window | `C-b c` → `new-window` | New internal Window, not a tab (*basic*) | planned | 23 | — |
| Detach the client | `C-b d` → `detach-client` | Same key; processes keep running (*basic*; SUBAK-FR-015) | planned | 24 | — |
| Find a window (prompt) | `C-b f` → `command-prompt { find-window -Z }` | Same key | planned | 25 | — |
| Show window information | `C-b i` → `display-message` | Same key | planned | 25 | — |
| Last window | `C-b l` → `last-window` | Same key | planned | 23 | — |
| Mark the pane | `C-b m` → `select-pane -m` | Same key | planned | 24 | — |
| Next window | `C-b n` → `next-window` | Same key (*basic*) | planned | 23 | — |
| Next pane | `C-b o` → `select-pane -t:.+` | Same key | planned | 23 | — |
| Customize options | `C-b C` → `customize-mode -Z` | *Rationale:* `customize-mode` deferred | deferred | post-26 | — |
| Previous window | `C-b p` → `previous-window` | Same key (*basic*) | planned | 23 | — |
| Show pane numbers | `C-b q` → `display-panes` | Same key | planned | 25 | — |
| Redraw the client | `C-b r` → `refresh-client` | Same key | planned | 23 | — |
| Choose a session | `C-b s` → `choose-tree -Zs` | Same key | planned | 25 | — |
| Show a clock | `C-b t` → `clock-mode` | *Rationale:* `clock-mode` deferred | deferred | post-26 | — |
| Fuzzy-switch window | `C-b Tab` → `new-pane …; switch-mode -wk` | Same key opens the Window switcher (without a floating pane) | planned | 25 | — |
| Fuzzy-switch session | `C-b BTab` → `new-pane …; switch-mode -sk` | Same key opens the Terminal switcher | planned | 25 | — |
| Choose a window | `C-b w` → `choose-tree -Zw` | Same key | planned | 25 | — |
| Kill the pane (confirm) | `C-b x` → `confirm-before kill-pane` | Same key with confirmation (*basic*) | planned | 23 | — |
| Zoom the pane | `C-b z` → `resize-pane -Z` | Same key (*basic*) | planned | 23 | — |
| Swap with the previous pane | `C-b {` → `swap-pane -U` | Same key | planned | 23 | — |
| Swap with the next pane | `C-b }` → `swap-pane -D` | Same key | planned | 23 | — |
| Show messages | `C-b ~` → `show-messages` | Same key | planned | 25 | — |
| Copy mode, one page up | `C-b PPage` → `copy-mode -u` | Same key | planned | 24 | — |
| Select pane by direction (repeatable) | `C-b Up`, `Down`, `Left`, `Right` → `select-pane -U/-D/-L/-R` | Same keys (*basic*) | planned | 23 | — |
| Even-horizontal layout | `C-b M-1` → `select-layout even-horizontal` | Same key | planned | 23 | — |
| Even-vertical layout | `C-b M-2` → `select-layout even-vertical` | Same key | planned | 23 | — |
| Main-horizontal layout | `C-b M-3` → `select-layout main-horizontal` | Same key | planned | 25 | — |
| Main-vertical layout | `C-b M-4` → `select-layout main-vertical` | Same key | planned | 25 | — |
| Tiled layout | `C-b M-5` → `select-layout tiled` | Same key | planned | 23 | — |
| Main-horizontal-mirrored layout | `C-b M-6` → `select-layout main-horizontal-mirrored` | Same key | planned | 25 | — |
| Main-vertical-mirrored layout | `C-b M-7` → `select-layout main-vertical-mirrored` | Same key | planned | 25 | — |
| Next window with an alert | `C-b M-n` → `next-window -a` | Same key | planned | 25 | — |
| Rotate panes backwards | `C-b M-o` → `rotate-window -D` | Same key | planned | 23 | — |
| Previous window with an alert | `C-b M-p` → `previous-window -a` | Same key | planned | 25 | — |
| Pan the visible part of a larger window | `C-b S-Up`, `S-Down`, `S-Left`, `S-Right` → `refresh-client -U/-D/-L/-R 10` | *Rationale:* only matters when a client is smaller than its window; revisit with multi-client sizing | deferred | post-26 | — |
| Follow the cursor again | `C-b DC` → `refresh-client -c` | *Rationale:* as above | deferred | post-26 | — |
| Resize the pane by 5 (repeatable) | `C-b M-Up`, `M-Down`, `M-Left`, `M-Right` → `resize-pane … 5` | Same keys | planned | 23 | — |
| Resize the pane by 1 (repeatable) | `C-b C-Up`, `C-Down`, `C-Left`, `C-Right` → `resize-pane` | Same keys | planned | 23 | — |
| Move a floating pane (`move` table) | `C-b g` then `1`–`4`, `M-1`–`M-4`, arrows, `M-` arrows, `0`, `,`, `.` | *Rationale:* floating panes deferred | deferred | post-26 | — |
| Window menu | `C-b <` → `display-menu` (window) | *Rationale:* `display-menu` deferred | deferred | post-26 | — |
| Pane menu | `C-b >` → `display-menu` (pane) | *Rationale:* `display-menu` deferred | deferred | post-26 | — |

## 5. Default key bindings: root table (mouse)

The root table has no default keyboard bindings; all defaults are mouse events and take effect
when `mouse` is on.

| tmux feature | tmux reference | Subak equivalent (planned) | Status | Target Step | Evidence |
| --- | --- | --- | --- | --- | --- |
| Click focuses a pane; forwarded if the app uses the mouse | `MouseDown1Pane` → `select-pane -t=; send -M` | Click focuses the Pane; mouse reporting forwarded when the program requests it | planned | 22 | — |
| Drag selects text | `MouseDrag1Pane` → `copy-mode -M` (or `send -M`) | Drag selection in copy mode | planned | 24 | — |
| Ctrl-drag creates a floating pane | `C-MouseDrag1Pane`, `C-MouseDrag1Empty` → `new-pane -M` | *Rationale:* floating panes deferred | deferred | post-26 | — |
| Alt-drag moves a floating pane | `M-MouseDrag1Pane`, `M-MouseDrag1Border` → `move-pane -M` | *Rationale:* floating panes deferred | deferred | post-26 | — |
| Wheel up scrolls history | `WheelUpPane` → `copy-mode -e` (or `send -M`) | Wheel scrolls scrollback; forwarded in the alternate screen or when the program uses the mouse | planned | 13 | — |
| Middle-click pastes | `MouseDown2Pane` → `paste -p` | Middle-click pastes the latest buffer | planned | 24 | — |
| Double-click copies a word | `DoubleClick1Pane` → `select-word; copy-pipe-and-cancel` | Same gesture | planned | 24 | — |
| Triple-click copies a line | `TripleClick1Pane` → `select-line; copy-pipe-and-cancel` | Same gesture | planned | 24 | — |
| Click a border selects the pane | `MouseDown1Border` → `select-pane -t=` | Same gesture | planned | 22 | — |
| Drag a border resizes | `MouseDrag1Border` → `resize-pane -M` | Drag the split divider | planned | 22 | — |
| Click the window list selects a window | `MouseDown1Status` → `switch-client -t=` | Click a Window in the in-tab window list | planned | 25 | — |
| Ctrl-click the window list swaps windows | `C-MouseDown1Status` → `swap-window -t@` | Same gesture | planned | 25 | — |
| Wheel over the window list changes window | `WheelUpStatus`, `WheelDownStatus` → `previous-window`, `next-window` | Same gesture | planned | 25 | — |
| Pane control buttons (new in 3.8) | `MouseDown1Control9` (kill menu), `Control8` (zoom), `Control7` (float toggle) | *Rationale:* depend on `display-menu` and floating panes | deferred | post-26 | — |
| Session menu on the status-left | `MouseDown3StatusLeft`, `M-MouseDown3StatusLeft` → `display-menu` | *Rationale:* `display-menu` deferred | deferred | post-26 | — |
| Window menu on the window list | `MouseDown3Status`, `M-MouseDown3Status` → `display-menu` | *Rationale:* `display-menu` deferred | deferred | post-26 | — |
| Pane menu | `MouseDown3Pane`, `M-MouseDown3Pane` → `display-menu` | *Rationale:* `display-menu` deferred; right-click forwarding to mouse-aware programs is planned with `MouseDown1Pane` | deferred | post-26 | — |
| Empty-area menu | `MouseDown3Empty`, `M-MouseDown3Empty` → `display-menu` | *Rationale:* `display-menu` deferred | deferred | post-26 | — |
| Pane scrollbar | `MouseDown1ScrollbarUp`, `MouseDown1ScrollbarDown`, `MouseDrag1ScrollbarSlider` | Native GUI scrollbar over the scrollback | planned | 13 | — |

## 6. Default key tables overview

| tmux feature | tmux reference | Subak equivalent (planned) | Status | Target Step | Evidence |
| --- | --- | --- | --- | --- | --- |
| Prefix table | `prefix` (§4) | Subak prefix table behind `Ctrl+B` | planned | 23 | — |
| Root table | `root` (§5) | Root bindings; GUI shortcuts (`Ctrl+T` etc.) are handled before it (§23) | planned | 23 | — |
| Copy-mode tables | `copy-mode` (emacs), `copy-mode-vi` (§7) | Both tables | planned | 24 | — |
| Floating-pane move table | `move` | *Rationale:* floating panes deferred | deferred | post-26 | — |
| Custom key tables | `bind -T name`, `switch-client -T name`, `key-table` option | User-defined tables | planned | 23 | — |
| Key names | `C-`, `^`, `S-`, `M-`, `Up`, `BSpace`, `BTab`, `DC`, `End`, `Enter`, `Escape`, `F1`–`F12`, `Home`, `IC`, `NPage`/`PageDown`/`PgDn`, `PPage`/`PageUp`/`PgUp`, `Space`, `Tab` | Same names in bindings and `send-keys` | planned | 23 | — |
| Catch-all binding | `Any` key | Same | planned | 23 | — |
| Repeatable keys | `bind -r`, `repeat-time`, `initial-repeat-time` | Same | planned | 23 | — |

## 7. Copy mode (vi and emacs)

Keys come from the `copy-mode-vi` and `copy-mode` tables in `key-bindings.c`. Every copy-mode
command (`send-keys -X <command>`) documented in tmux 3.8 is named in this table.

| tmux feature | tmux reference | Subak equivalent (planned) | Status | Target Step | Evidence |
| --- | --- | --- | --- | --- | --- |
| Choose vi or emacs keys | `mode-keys` option; default from `VISUAL`/`EDITOR` | Same option and default | planned | 24 | — |
| Send a copy-mode command | `send-keys -X command`, `-N count` | Same | planned | 24 | — |
| Exit copy mode | vi `q`, `C-c`; emacs `q`, `Escape`, `C-[`, `C-c` → `cancel` | Same keys | planned | 24 | — |
| Cursor movement | vi `h` `j` `k` `l`, `BSpace`, `C-h`, arrows; emacs `C-b` `C-f` `C-p` `C-n`, arrows → `cursor-left`, `cursor-right`, `cursor-up`, `cursor-down`, `cursor-down-and-cancel` | Same keys | planned | 24 | — |
| Word movement | vi `w` `b` `e` `W` `B` `E`; emacs `M-f` `M-b` → `next-word`, `next-word-end`, `previous-word`, `next-space`, `next-space-end`, `previous-space` | Same keys; `word-separators` honoured | planned | 24 | — |
| Line positions | vi `0` `^` `$` `Home` `End`; emacs `C-a` `C-e` `M-m` `Home` `End` → `start-of-line`, `end-of-line`, `back-to-indentation` | Same keys | planned | 24 | — |
| Screen positions | vi `H` `M` `L`; emacs `M-R` `M-r` `M-l` → `top-line`, `middle-line`, `bottom-line`, `cursor-centre-vertical`, `cursor-centre-horizontal` | Same keys | planned | 24 | — |
| Paging | vi `C-b` `C-f` `PPage` `NPage`; emacs `M-v` `C-v` `Space` `PPage` `NPage` → `page-up`, `page-down`, `page-down-and-cancel` | Same keys | planned | 24 | — |
| Half pages | vi `C-u` `C-d`; emacs `M-Up` `M-Down` → `halfpage-up`, `halfpage-down`, `halfpage-down-and-cancel` | Same keys | planned | 24 | — |
| Line scrolling | vi `C-y` `C-e` `K` `J` `C-Up` `C-Down` `z`; emacs `C-Up` `C-Down` `C-l` → `scroll-up`, `scroll-down`, `scroll-down-and-cancel`, `scroll-middle`, `scroll-top`, `scroll-bottom`, `recentre-top-bottom` | Same keys | planned | 24 | — |
| Exit when scrolled to the bottom | `scroll-exit-on`, `scroll-exit-off`, `scroll-exit-toggle`; `copy-mode -e` | Same | planned | 24 | — |
| History top and bottom | vi `g` `G`; emacs `M-<` `M->` → `history-top`, `history-bottom` | Same keys | planned | 24 | — |
| Paragraphs and brackets | vi `{` `}` `%`; emacs `M-{` `M-}` `C-M-f` `C-M-b` → `previous-paragraph`, `next-paragraph`, `next-matching-bracket`, `previous-matching-bracket` | Same keys | planned | 24 | — |
| Jump to a character | vi and emacs `f` `F` `t` `T` `;` `,` → `jump-forward`, `jump-backward`, `jump-to-forward`, `jump-to-backward`, `jump-again`, `jump-reverse` | Same keys | planned | 24 | — |
| Search | vi `/` `?` `n` `N`, `*` `#` (word under cursor); emacs `C-s` `C-r` `n` `N` → `search-forward`, `search-backward`, `search-forward-text`, `search-backward-text`, `search-forward-incremental`, `search-backward-incremental`, `search-again`, `search-reverse` | Same keys; regex and plain text; `wrap-search` | planned | 24 | — |
| Go to a line | vi `:`; emacs `g` → `goto-line` | Same keys | planned | 24 | — |
| Selection | vi `Space` `V` `o` `Escape`; emacs `C-Space` `C-g` → `begin-selection`, `select-line`, `select-word`, `other-end`, `clear-selection`, `stop-selection`, `selection-mode` | Same keys | planned | 24 | — |
| Rectangle selection | vi `v` `C-v`; emacs `R` → `rectangle-toggle`, `rectangle-on`, `rectangle-off` | Same keys | planned | 24 | — |
| Copy and append | vi `Enter` `C-j` `D` `A`; emacs `M-w` `C-w` `C-k` → `copy-selection`, `copy-selection-and-cancel`, `copy-selection-no-clear`, `copy-line`, `copy-line-and-cancel`, `copy-end-of-line`, `copy-end-of-line-and-cancel`, `append-selection`, `append-selection-and-cancel` | Same keys; copies to a paste buffer and the Windows clipboard (§16) | planned | 24 | — |
| Copy through an external command | `copy-pipe`, `copy-pipe-and-cancel`, `copy-pipe-no-clear`, `copy-pipe-line`, `copy-pipe-line-and-cancel`, `copy-pipe-end-of-line`, `copy-pipe-end-of-line-and-cancel`, `pipe`, `pipe-and-cancel`, `pipe-no-clear`; `copy-command` option | Same commands; the command runs in the profile shell | planned | 24 | — |
| Marks | vi and emacs `X`, `M-x` → `set-mark`, `jump-to-mark` | Same keys | planned | 24 | — |
| Repeat count | vi `1`–`9`; emacs `M-1`–`M-9` | Same keys | planned | 24 | — |
| Shell-prompt navigation (OSC 133) | emacs `M-C-Up` `M-C-Down` → `previous-prompt`, `next-prompt` (`-o`) | Same keys; needs OSC 133 marks from the shell | planned | 24 | — |
| Line numbers | emacs `L` → `line-numbers-toggle`, `line-numbers-on`, `line-numbers-off`; `copy-mode-line-numbers` option | Same | planned | 24 | — |
| Position indicator | `P` → `toggle-position`; `copy-mode-position-format` | Same | planned | 24 | — |
| Refresh from the live pane | `r` → `refresh-now`; `refresh-on`, `refresh-off`, `refresh-toggle` | Same | planned | 24 | — |
| Mouse in copy mode | `MouseDrag1Pane` (select), `MouseDragEnd1Pane` (copy), `WheelUpPane`/`WheelDownPane` (`send -N5 -X scroll-up/scroll-down`), double/triple click, `scroll-to-mouse` | Same gestures | planned | 24 | — |
| View mode | Output of a command run from a key binding (e.g. `list-keys`) shown read-only | Read-only view of command output | planned | 24 | — |
| Copy-mode appearance | `mode-style`, `copy-mode-*-style` options | Styles from the options engine | planned | 25 | — |

## 8. Other pane modes

| tmux feature | tmux reference | Subak equivalent (planned) | Status | Target Step | Evidence |
| --- | --- | --- | --- | --- | --- |
| Tree mode | `choose-tree` (`-s`, `-w`, `-Z`, `-f filter`, `-O order`, `-F`, `-G`) | Tree chooser overlay | planned | 25 | — |
| Chooser keys | `Enter`, `Up`/`Down`, `x`/`X` kill, `t`/`T`/`C-t` tag, `f` filter, `O` sort, `r` reverse, `v` preview, `:` command, `q` exit | Same keys in all choosers | planned | 25 | — |
| Client mode | `choose-client` | Client chooser | planned | 25 | — |
| Buffer mode | `choose-buffer` (`e` edits with `editor`) | Buffer chooser; editing deferred with `editor` | planned | 24 | — |
| Switch mode | `switch-mode` (fuzzy match; `Enter`, `Up`, `Down`, `Escape`) | Fuzzy switcher | planned | 25 | — |
| Customize mode | `customize-mode` | *Rationale:* see §2.2 | deferred | post-26 | — |
| Clock mode | `clock-mode` | *Rationale:* see §2.7 | deferred | post-26 | — |

## 9. Options

### 9.1 Families and mechanics

| tmux feature | tmux reference | Subak equivalent (planned) | Status | Target Step | Evidence |
| --- | --- | --- | --- | --- | --- |
| Server options | `set -s`, `show -s` | Host-wide settings | planned | 25 | — |
| Session options | `set`, `set -g`, `show` | Per-Terminal settings with global defaults | planned | 25 | — |
| Window options | `set -w`, `setw`, `show -w` | Per-Window settings | planned | 25 | — |
| Pane options | `set -p`, `show -p` | Per-Pane settings | planned | 25 | — |
| Global defaults and inheritance | `-g`; `show -A` (inherited values marked `*`) | Same inheritance chain: pane → window → session → global | planned | 25 | — |
| User options | `@name` | Same; usable in formats | planned | 25 | — |
| Array options | `name[N]`, `set -a` | Same | planned | 25 | — |
| Format-expanded values | `set -F` | Same | planned | 25 | — |
| Set-only-if-unset, quiet, unset | `set -o`, `-q`, `-u`, `-U` (unset in all panes) | Same | planned | 25 | — |
| Show modifiers | `show -v`, `-H` (hooks), `-q` | Same | planned | 25 | — |
| Durable configuration | tmux options live in server memory only | Subak config is human-readable, versioned, preserves unknown keys (SUBAK-FR-046); how runtime `set` persists is decided in Step 25 | planned | 25 | — |

### 9.2 Server options

| tmux feature | tmux reference | Subak equivalent (planned) | Status | Target Step | Evidence |
| --- | --- | --- | --- | --- | --- |
| Key sent for Backspace | `backspace` | Same | planned | 23 | — |
| Maximum automatic buffers | `buffer-limit` | Same | planned | 24 | — |
| Clear the outer terminal on attach | `clear-on-attach` | *Rationale:* no outer terminal; the GUI redraws from Host state | unsupported | — | — |
| Unicode width overrides | `codepoint-widths`, `variation-selector-always-wide` | Same; important for Korean/CJK (KIM-NFR-013) | planned | 25 | — |
| Custom command aliases | `command-alias` | Same | planned | 25 | — |
| Command for copy-pipe | `copy-command` | Same; default is the Windows clipboard | planned | 24 | — |
| Command run by a bare `tmux` | `default-client-command` | Bare `subak` behaviour (Step 12); option later | planned | 25 | — |
| `TERM` for new panes | `default-terminal` | `TERM` given to pane processes | planned | 25 | — |
| Editor command | `editor` | *Rationale:* only used by buffer editing; revisit with buffer mode extras | deferred | post-26 | — |
| Escape-key delay | `escape-time` | *Rationale:* the GUI receives key events, not an escape byte stream | unsupported | — | — |
| Exit when no sessions remain | `exit-empty` | Fixed behaviour: the Host exits when it owns no Terminals and no client is attached (SUBAK-FR-002) | planned | 10 | — |
| Exit when no clients are attached | `exit-unattached` | *Rationale:* contradicts Subak's core promise that Terminals outlive clients (SUBAK-FR-003) | unsupported | — | — |
| Extended keys to programs | `extended-keys`, `extended-keys-format` | *Rationale:* Subak sends xterm encoding or win32-input-mode (ADR 0009); CSI u extended keys revisited after Step 23 | deferred | post-26 | — |
| Focus events to programs | `focus-events` | Same | planned | 25 | — |
| OSC 52 clipboard | `set-clipboard`, `get-clipboard` | Windows clipboard; reading is opt-in (privacy) | planned | 24 | — |
| Prompt history file | `history-file`, `prompt-history-limit` | Same, stored in the user profile | planned | 24 | — |
| Escape-sequence size limit | `input-buffer-size` | Same | planned | 25 | — |
| Message log size | `message-limit` | Same | planned | 25 | — |
| Prefix timeout | `prefix-timeout` | Same | planned | 23 | — |
| Outer-terminal capabilities | `terminal-features`, `terminal-overrides` | *Rationale:* apply to an outer terminal; the GUI renders natively. Revisit for a console client | deferred | post-26 | — |
| Theme detection and palettes | `theme`, `dark-theme-black`, `dark-theme-blue`, `dark-theme-cyan`, `dark-theme-dark-grey`, `dark-theme-green`, `dark-theme-light-grey`, `dark-theme-magenta`, `dark-theme-red`, `dark-theme-white`, `dark-theme-yellow`, `light-theme-black`, `light-theme-blue`, `light-theme-cyan`, `light-theme-dark-grey`, `light-theme-green`, `light-theme-light-grey`, `light-theme-magenta`, `light-theme-red`, `light-theme-white`, `light-theme-yellow` | *Rationale:* colours and light/dark follow the Kim design system and the OS; tmux palette options revisited later | deferred | post-26 | — |
| User-defined outer-terminal keys | `user-keys` | *Rationale:* as `terminal-overrides` | deferred | post-26 | — |

### 9.3 Session options

| tmux feature | tmux reference | Subak equivalent (planned) | Status | Target Step | Evidence |
| --- | --- | --- | --- | --- | --- |
| Alert actions | `activity-action`, `bell-action`, `silence-action` | Same; GUI tab indicators | planned | 25 | — |
| Paste detection by timing | `assume-paste-time` | *Rationale:* the GUI knows when the user pastes; no timing heuristic needed | unsupported | — | — |
| First window index | `base-index` | Same | planned | 21 | — |
| Shell and command for new panes | `default-shell`, `default-command` | Profile shell and command (SUBAK-FR-011). *Partial:* the default is `%ComSpec%`; any program with its arguments via `terminal new -- <program> [args…]`, resolved on the pane's `PATH` (cmd, `powershell`, `pwsh` if installed) | partial | 11 | `crates/subak/tests/pty_shells.rs::powershell_and_cmd_run_interactively_in_independent_trees_under_the_host` (2026-11-04) |
| Size of detached sessions | `default-size` | Same | planned | 10 | — |
| Destroy a session with no clients | `destroy-unattached` | Opt-in option, off by default | planned | 25 | — |
| Behaviour when the session is destroyed | `detach-on-destroy` | What the client shows next (GUI: close the tab or switch) | planned | 25 | — |
| Message display time | `display-time` | Same | planned | 25 | — |
| Focus follows mouse | `focus-follows-mouse` | Same | planned | 25 | — |
| Scrollback length | `history-limit` | Same | planned | 13 | — |
| Repeat timing | `repeat-time`, `initial-repeat-time` | Same | planned | 23 | — |
| Default key table | `key-table` | Same | planned | 23 | — |
| Automatic locking | `lock-after-time`, `lock-command` | *Rationale:* as `lock-client` | unsupported | — | — |
| Message and prompt look | `message-style`, `message-command-style`, `message-format`, `message-line` | Same | planned | 25 | — |
| Mouse support | `mouse` | Same; the GUI always receives the mouse, the option controls tmux-style handling | planned | 25 | — |
| Prefix keys | `prefix`, `prefix2` | Same; default `Ctrl+B` | planned | 23 | — |
| Prompt cursor | `prompt-cursor-colour`, `prompt-cursor-style`, `prompt-command-cursor-colour`, `prompt-command-cursor-style` | Same | planned | 24 | — |
| Renumber windows on close | `renumber-windows` | Same | planned | 25 | — |
| Outer window title | `set-titles`, `set-titles-string` | GUI window/tab title | planned | 25 | — |
| Status line | `status`, `status-bg`, `status-fg`, `status-format`, `status-interval`, `status-justify`, `status-left`, `status-left-length`, `status-left-style`, `status-position`, `status-right`, `status-right-length`, `status-right-style`, `status-style` | In-tab status line (§11) | planned | 25 | — |
| Prompt key style | `status-keys` | vi or emacs editing in the prompt | planned | 24 | — |
| Environment refreshed on attach | `update-environment` | Same (§15) | planned | 25 | — |
| Visual alerts | `visual-activity`, `visual-bell`, `visual-silence` | GUI message and tab indicator | planned | 25 | — |
| Word separators | `word-separators` | Same | planned | 24 | — |

### 9.4 Window options

| tmux feature | tmux reference | Subak equivalent (planned) | Status | Target Step | Evidence |
| --- | --- | --- | --- | --- | --- |
| Size to the largest active client | `aggressive-resize` | Same | planned | 25 | — |
| Automatic window names | `automatic-rename`, `automatic-rename-format` | Same | planned | 25 | — |
| Clock appearance | `clock-mode-colour`, `clock-mode-style` | *Rationale:* `clock-mode` deferred | deferred | post-26 | — |
| Copy-mode styles | `copy-mode-current-line-number-style`, `copy-mode-current-line-style`, `copy-mode-current-match-style`, `copy-mode-line-number-style`, `copy-mode-mark-style`, `copy-mode-match-style`, `copy-mode-position-style`, `copy-mode-selection-style`, `mode-style` | Same | planned | 25 | — |
| Copy-mode line numbers | `copy-mode-line-numbers` | Same | planned | 24 | — |
| Pane-number overlay | `display-panes-active-colour`, `display-panes-border-style`, `display-panes-colour`, `display-panes-format`, `display-panes-time` | Same | planned | 25 | — |
| Fill character outside a smaller window | `fill-character` | Same | planned | 25 | — |
| Preset layout sizes | `main-pane-height`, `main-pane-width`, `other-pane-height`, `other-pane-width` | Same | planned | 25 | — |
| Tiled layout columns | `tiled-layout-max-columns` | Same | planned | 22 | — |
| Menu appearance | `menu-border-lines`, `menu-border-style`, `menu-selected-style`, `menu-style` | *Rationale:* `display-menu` deferred | deferred | post-26 | — |
| Popup appearance | `popup-border-lines`, `popup-border-style`, `popup-style` | *Rationale:* `display-popup` deferred | deferred | post-26 | — |
| Copy-mode key style | `mode-keys` | Same | planned | 24 | — |
| Alert monitoring | `monitor-activity`, `monitor-bell`, `monitor-silence` | Same | planned | 25 | — |
| First pane index | `pane-base-index` | Same | planned | 21 | — |
| Active-pane border indicators | `pane-border-indicators` | Same | planned | 22 | — |
| Pane scrollbars | `pane-scrollbars`, `pane-scrollbars-position`, `pane-scrollbars-timeout` | Native GUI scrollbar | planned | 13 | — |
| Per-object status styles and pane lists (new in 3.8) | `pane-status-style`, `pane-status-current-style`, `session-status-style`, `session-status-current-style`, `window-pane-status-format`, `window-pane-current-status-format` | Same | planned | 25 | — |
| Tree-mode appearance | `tree-mode-border-style`, `tree-mode-preview-style`, `tree-mode-selection-style` | Same | planned | 25 | — |
| Window size policy | `window-size` (`largest`, `smallest`, `manual`, `latest`) | Fixed `latest` first (ADR 0008); the other policies with the option | planned | 25 | — |
| Window list entries | `window-status-format`, `window-status-current-format`, `window-status-separator`, `window-status-style`, `window-status-current-style`, `window-status-last-style`, `window-status-activity-style`, `window-status-bell-style` | Same | planned | 25 | — |
| Search wrap-around | `wrap-search` | Same | planned | 24 | — |
| xterm-style modified keys | `xterm-keys` | Same | planned | 23 | — |

### 9.5 Pane options (window or pane scope)

| tmux feature | tmux reference | Subak equivalent (planned) | Status | Target Step | Evidence |
| --- | --- | --- | --- | --- | --- |
| Escape-sequence passthrough | `allow-passthrough` | *Rationale:* passthrough targets an outer terminal; the GUI must decide per sequence (images, notifications) | deferred | post-26 | — |
| Programs may rename the window | `allow-rename` | Same | planned | 25 | — |
| Programs may set the pane title | `allow-set-title` | Same | planned | 25 | — |
| Alternate screen | `alternate-screen` | Same | planned | 13 | — |
| Copy-mode position text | `copy-mode-position-format` | Same | planned | 24 | — |
| Cursor colour and style | `cursor-colour`, `cursor-style` | Same | planned | 25 | — |
| Pane borders | `pane-border-style`, `pane-active-border-style`, `pane-border-lines` | Native dividers with equivalent styles | planned | 22 | — |
| Pane status lines | `pane-border-status`, `pane-border-format` | Same | planned | 25 | — |
| Pane colour palette | `pane-colours` | Same | planned | 25 | — |
| Scrollbar style | `pane-scrollbars-style` | Same | planned | 13 | — |
| Keep a pane after its process exits | `remain-on-exit`, `remain-on-exit-format` | "Keep pane open on exit" (SUBAK_PHILOSOPHY §6.1) | planned | 25 | — |
| Scroll on clear | `scroll-on-clear` | Same | planned | 13 | — |
| Switch-mode match style | `switch-mode-match-style` | Same | planned | 25 | — |
| Synchronize input | `synchronize-panes` | Same | planned | 25 | — |
| Tree-mode preview format | `tree-mode-preview-format` | Same | planned | 25 | — |
| Pane background/foreground | `window-style`, `window-active-style` | Same | planned | 25 | — |

## 10. Formats

### 10.1 Syntax and modifiers

| tmux feature | tmux reference | Subak equivalent (planned) | Status | Target Step | Evidence |
| --- | --- | --- | --- | --- | --- |
| Variable expansion | `#{name}`, option names, environment names | Format engine shared by `-F`, status line and hooks | planned | 25 | — |
| Short aliases | `#H`, `#h`, `#D`, `#F`, `#I`, `#P`, `#S`, `#T`, `#W` | Same | planned | 25 | — |
| Escapes | `##`, `#,`, `#}` | Same | planned | 25 | — |
| Conditionals | `#{?cond,a,b}`, chained pairs | Same | planned | 25 | — |
| Comparisons | `#{==:a,b}`, `!=`, `<`, `>`, `<=`, `>=` | Same | planned | 25 | — |
| Boolean logic | `#{\|\|:a,b}`, `#{&&:a,b}`, `#{!:a}`, `#{!!:a}` | Same | planned | 25 | — |
| Pattern match | `#{m:glob,str}`, `m/r` regex, `m/i`, `m/z` fuzzy, `m/p` positions | Same | planned | 25 | — |
| Search pane content | `#{C:pattern}`, `C/r`, `C/i` | Same | planned | 25 | — |
| Arithmetic | `#{e\|+\|:a,b}`, `-`, `*`, `/`, `m`, `%%`, comparisons, `f` float with precision | Same | planned | 25 | — |
| Truncate | `#{=5:v}`, `#{=-5:v}`, `#{=/5/…:v}` | Same | planned | 25 | — |
| Pad | `#{p10:v}`, `#{p-10:v}` | Same | planned | 25 | — |
| Length and width | `#{n:v}`, `#{w:v}` | Same; width follows Unicode/CJK rules | planned | 25 | — |
| Repeat | `#{R:a,3}` | Same | planned | 25 | — |
| Time | `#{t:v}`, `t/p`, `t/r`, `t/d`, `t/f/strftime` | Same | planned | 25 | — |
| Client environment | `#{I/e:VAR}` | Same | planned | 25 | — |
| Outer-terminal interrogation | `#{I/f:feature}`, `#{I/c:capability}` | *Rationale:* no outer terminal or terminfo; GUI capabilities are fixed | unsupported | — | — |
| Basename and dirname | `#{b:v}`, `#{d:v}` | Same; Windows paths | planned | 25 | — |
| Quoting | `#{q:v}`, `q/h`, `q/a`, `q/s` | Same, plus quoting for the profile shell (TBD) | planned | 25 | — |
| Double expansion | `#{E:v}`, `#{T:v}` (with strftime) | Same | planned | 25 | — |
| Loops | `#{S:…}`, `#{W:…}`, `#{P:…}`, `#{L:…}` with sort flags `/i`, `/n`, `/t`, `/r`, `/z` | Same | planned | 25 | — |
| Neighbour user options in window loops | `next_@name`, `prev_@name` | Same | planned | 25 | — |
| Option and environment loops | `#{O:…}` (`s`, `w`, `p`, `v`, `g`), `#{V:…}` (`s`, `g`, `c`) | Same | planned | 25 | — |
| Name exists | `#{N:name}`, `N/w`, `N/s` | Same | planned | 25 | — |
| Substitution | `#{s/re/repl/:v}`, `/i`, custom delimiter | Same | planned | 25 | — |
| Literal | `#{l:text}` | Same | planned | 25 | — |
| Animation frames | `#{A/count:frame,frame,…}` | *Rationale:* decorative motion; must first respect the system reduced-motion setting (KIM-NFR-012) | deferred | post-26 | — |
| ASCII and colour helpers | `#{a:98}`, `#{c:colour}` | Same | planned | 25 | — |
| Chained modifiers | `#{T;=10:status-left}` | Same | planned | 25 | — |
| Shell output | `#(command)` | Same, run asynchronously in the profile shell | planned | 25 | — |
| Inline styles | `#[fg=…,bg=…]` (§11) | Same | planned | 25 | — |

### 10.2 Variables

275 variables are documented in tmux 3.8. Planned as families; per-variable rows are added when
a family is scheduled in detail.

| tmux feature | tmux reference | Subak equivalent (planned) | Status | Target Step | Evidence |
| --- | --- | --- | --- | --- | --- |
| Session variables (25) | `session_*` (e.g. `session_id`, `session_name`, `session_windows`, `session_attached`) | Terminal fields; `session_id` is the Terminal's stable ID (SUBAK-FR-010) | planned | 25 | — |
| Window variables (37) | `window_*` (e.g. `window_id`, `window_index`, `window_name`, `window_flags`, `window_layout`, `window_zoomed_flag`) | Window fields | planned | 25 | — |
| Pane variables (62) | `pane_*` (e.g. `pane_id`, `pane_index`, `pane_title`, `pane_current_command`, `pane_current_path`, `pane_pid`, `pane_dead`, `pane_in_mode`, `pane_floating_flag`) | Pane fields; `pane_pid` is the Windows PID | planned | 25 | — |
| Client variables (25) | `client_*` (e.g. `client_name`, `client_tty`, `client_session`, `client_readonly`, `client_width`) | Client fields; `client_tty` has no Windows meaning (empty) | planned | 25 | — |
| Buffer variables (5) | `buffer_name`, `buffer_sample`, `buffer_size`, `buffer_created`, `buffer_full` | Same | planned | 24 | — |
| Copy, selection and search variables | `copy_*`, `selection_*`, `search_*`, `scroll_position`, `rectangle_toggle` | Same | planned | 24 | — |
| Cursor and mouse variables | `cursor_*`, `mouse_*` | Same | planned | 25 | — |
| History and screen state | `history_*`, `alternate_*`, `insert_flag`, `keypad_*`, `origin_flag`, `wrap_flag`, `scroll_region_*`, `bracket_paste_flag`, `synchronized_output_flag`, `sixel_support` | Same where the renderer tracks the state | planned | 25 | — |
| Hook variables | `hook`, `hook_client`, `hook_pane`, `hook_session`, `hook_window`, `hook_*_name` | Same | planned | 25 | — |
| Key and command-list variables | `key_*`, `command`, `command_list_*` | Same, for `list-keys -F` and `list-commands -F` | planned | 24 | — |
| Option variables (customize mode) | `option_*` | *Rationale:* used by `customize-mode`, deferred | deferred | post-26 | — |
| Server and host variables | `host`, `host_short`, `pid`, `version`, `start_time`, `uid`, `user`, `config_files`, `current_file`, `server_sessions`, `next_session_id`, `line`, `loop_index`, `loop_last_flag` | Host fields; `pid` is the Host PID | planned | 25 | — |
| Socket path | `socket_path` | *Rationale:* no tmux socket (SUBAK-SEC-010) | unsupported | — | — |
| Window neighbours | `active_window_index`, `last_window_index`, `next_window_*`, `prev_window_*` | Same | planned | 25 | — |

## 11. Status line and styles

| tmux feature | tmux reference | Subak equivalent (planned) | Status | Target Step | Evidence |
| --- | --- | --- | --- | --- | --- |
| Status line on/off and height | `status on\|off\|2…5` | Optional status line inside the Terminal tab; GUI tabs list Terminals | planned | 25 | — |
| Left and right sections | `status-left`, `status-right`, lengths and styles | Same | planned | 25 | — |
| Window list | `window-status-format`, `window-status-current-format`, `window-status-separator` | Internal Window list inside the tab | planned | 25 | — |
| Window flags | `*` current, `-` last, `#` activity, `!` bell, `~` silence, `M` marked, `Z` zoomed | Same flags and GUI indicators | planned | 25 | — |
| Full line formats | `status-format[]` | Same | planned | 25 | — |
| Refresh interval | `status-interval` | Same | planned | 25 | — |
| Alignment and position | `status-justify`, `status-position` | Same | planned | 25 | — |
| Message line | `display-message`, `message-line`, `display-time` | Same | planned | 25 | — |
| Command prompt on the status line | `command-prompt`, `status-keys` | Prompt overlay | planned | 24 | — |
| Pane status lines | `pane-border-status`, `pane-border-format` | Same | planned | 25 | — |
| Colours | names, `colour0`–`colour255`, `#RRGGBB`, `default`, `terminal` | Same | planned | 25 | — |
| Attributes | `bold`, `dim`, `italics`, `underscore` (and double/curly/dotted/dashed), `blink`, `reverse`, `hidden`, `overline`, `strikethrough`, `noattr`, `none` | Same | planned | 25 | — |
| Style keywords | `fg=`, `bg=`, `us=`, `fill=`, `dim=percentage`, `width=N`, `acs` | Same | planned | 25 | — |
| Alignment and lists | `align=left\|centre\|right\|absolute-centre`, `list=on\|focus\|left-marker\|right-marker\|nolist` | Same | planned | 25 | — |
| Clickable ranges | `range=left\|right\|session\|window\|pane\|control\|user` | Clickable regions in the status line | planned | 25 | — |
| Style stack | `push-default`, `pop-default`, `set-default` | Same | planned | 25 | — |
| Hyperlinks in styles | `link=uri` | Same | planned | 25 | — |

## 12. Hooks

| tmux feature | tmux reference | Subak equivalent (planned) | Status | Target Step | Evidence |
| --- | --- | --- | --- | --- | --- |
| After-command hooks | `after-<command>` (e.g. `after-split-window`) | Same | planned | 25 | — |
| Hooks are array options with scopes | `set-hook -a`, `-g`, `-p`, `-w`, `-u`, `hook[N]` | Same | planned | 25 | — |
| Run a hook now | `set-hook -R` | Same | planned | 25 | — |
| User events | `set-hook -E @name` | Same | planned | 25 | — |
| Format subscriptions as hooks | `set-hook -B name:what:format`, `show-hooks -B` | Same | planned | 25 | — |
| Conditional hooks | `set-hook -T format` | Same | planned | 25 | — |
| Command failed | `command-error` | Same | planned | 25 | — |
| Window activity | `alert-activity` | Same | planned | 25 | — |
| Window bell | `alert-bell` | Same | planned | 25 | — |
| Window silence | `alert-silence` | Same | planned | 25 | — |
| Client became latest active | `client-active` | Same | planned | 25 | — |
| Client attached | `client-attached` | Same | planned | 25 | — |
| Client closed | `client-closed` | Same | planned | 25 | — |
| Client created | `client-created` | Same | planned | 25 | — |
| Client detached | `client-detached` | Same | planned | 25 | — |
| Client focus in | `client-focus-in` | GUI window focus | planned | 25 | — |
| Client focus out | `client-focus-out` | GUI window focus | planned | 25 | — |
| Client resized | `client-resized` | Same | planned | 25 | — |
| Client switched session | `client-session-changed` | Same | planned | 25 | — |
| Client switched to light theme | `client-light-theme` | *Rationale:* theme options deferred (§9.2) | deferred | post-26 | — |
| Client switched to dark theme | `client-dark-theme` | *Rationale:* theme options deferred (§9.2) | deferred | post-26 | — |
| Marked pane changed | `marked-pane-changed` | Same | planned | 25 | — |
| Pane output | `pane-activity` | Same | planned | 25 | — |
| Pane bell | `pane-bell` | Same | planned | 25 | — |
| OSC 133 command finished | `pane-command-finished` | Same | planned | 25 | — |
| OSC 133 command started | `pane-command-started` | Same | planned | 25 | — |
| Pane created or respawned | `pane-created` | Same | planned | 25 | — |
| Pane process exited, pane kept | `pane-died` | Same | planned | 25 | — |
| Pane process exited | `pane-exited` | Same | planned | 25 | — |
| Pane focus in | `pane-focus-in` | Same | planned | 25 | — |
| Pane focus out | `pane-focus-out` | Same | planned | 25 | — |
| Pane mode changed | `pane-mode-changed` | Same | planned | 25 | — |
| Pane entered a mode | `pane-mode-entered` | Same | planned | 25 | — |
| Pane exited a mode | `pane-mode-exited` | Same | planned | 25 | — |
| Pane moved to another window | `pane-moved` | Same | planned | 25 | — |
| Prompt in a pane closed | `pane-prompt-closed` | Same | planned | 25 | — |
| Prompt in a pane opened | `pane-prompt-opened` | Same | planned | 25 | — |
| Pane resized | `pane-resized` | Same | planned | 25 | — |
| Clipboard set by OSC 52 | `pane-set-clipboard` | Same | planned | 25 | — |
| OSC 133 shell prompt | `pane-shell-prompt` | Same | planned | 25 | — |
| Pane title changed | `pane-title-changed` | Same | planned | 25 | — |
| Session closed | `session-closed` | Same | planned | 25 | — |
| Session created | `session-created` | Same | planned | 25 | — |
| Session added to a group | `session-added-to-group` | *Rationale:* session groups deferred (§3) | deferred | post-26 | — |
| Session removed from a group | `session-removed-from-group` | *Rationale:* session groups deferred (§3) | deferred | post-26 | — |
| Session renamed | `session-renamed` | Same | planned | 25 | — |
| Session changed active window | `session-window-changed` | Same | planned | 25 | — |
| Window closed | `window-closed` | Same | planned | 25 | — |
| Window created | `window-created` | Same | planned | 25 | — |
| Window layout changed | `window-layout-changed` | Same | planned | 25 | — |
| Window linked into a session | `window-linked` | Same | planned | 25 | — |
| Window changed active pane | `window-pane-changed` | Same | planned | 25 | — |
| Window renamed | `window-renamed` | Same | planned | 25 | — |
| Window resized | `window-resized` | Same | planned | 25 | — |
| Window unzoomed | `window-unzoomed` | Same | planned | 25 | — |
| Window zoomed | `window-zoomed` | Same | planned | 25 | — |
| Window unlinked from a session | `window-unlinked` | Same | planned | 25 | — |

## 13. Target syntax

| tmux feature | tmux reference | Subak equivalent (planned) | Status | Target Step | Evidence |
| --- | --- | --- | --- | --- | --- |
| Target and source flags | `-t target`, `-s source`; client, session, window, pane target types | Same resolver for prompt, `source-file` and CLI | planned | 24 | — |
| Client target | `target-client` (tty name, e.g. `/dev/ttyp1`) | Subak client ID (no tty on Windows) | planned | 24 | — |
| Session by ID | `$1` | Terminal ID `t-` + 12 hex (ADR 0007); the prompt also maps tmux `$n` forms | planned | 24 | — |
| Session by exact name, prefix or glob | `mysess`, `mysession`, `my*` | Same match order in the prompt; the headless CLI accepts exact names and IDs only (ADR 0007) | planned | 24 | — |
| Exact-match prefix | `=mysess` | Same | planned | 24 | — |
| Default session | omitted → current, else most recently used | Same | planned | 24 | — |
| Window target | `session:window` | `terminal:window` | planned | 24 | — |
| Window by index | `mysession:1` | Same | planned | 24 | — |
| Window by ID | `@1` | Window ID `w-` + 12 hex; tmux `@n` mapped by the prompt | planned | 24 | — |
| Window by exact name, prefix or glob | `:mywindow`, `=` exact | Same | planned | 24 | — |
| Next free index | `session:` (empty) for `new-window`, `link-window` | Same | planned | 24 | — |
| Window tokens | `{start}` `^`, `{end}` `$`, `{last}` `!`, `{next}` `+`, `{previous}` `-`, `{current}` `@` | Same | planned | 24 | — |
| Offsets | `+2`, `-1` (e.g. `select-window -t:+2`) | Same | planned | 24 | — |
| Pane target | `session:window.pane` | `terminal:window.pane` | planned | 24 | — |
| Pane by ID | `%1` | Pane ID `p-` + 12 hex; tmux `%n` mapped by the prompt | planned | 24 | — |
| Pane by index | `.1` | Same | planned | 24 | — |
| Pane relative tokens | `{last}` `!`, `{next}` `+`, `{previous}` `-`, `{active}` `@` | Same | planned | 24 | — |
| Pane position tokens | `{top}`, `{bottom}`, `{left}`, `{right}`, `{top-left}`, `{top-right}`, `{bottom-left}`, `{bottom-right}` | Same | planned | 24 | — |
| Pane neighbour tokens | `{up-of}`, `{down-of}`, `{left-of}`, `{right-of}` | Same | planned | 24 | — |
| Marked pane | `{marked}` `~` | Same | planned | 24 | — |
| Mouse target | `{mouse}` `=` | Same | planned | 25 | — |
| Best-guess resolution of unqualified targets | e.g. `-t 1` as pane, window or session | Same heuristics | planned | 24 | — |
| IDs unique for the object's life | `$`, `@`, `%` IDs unique per server | Stronger: random IDs, never reused, kept across Host restarts and restore (SUBAK-FR-010, ADR 0007) | planned | 10 | — |
| Pane ID in the pane environment | `TMUX_PANE` | `SUBAK_PANE_ID=p-…` and `SUBAK_TERMINAL_ID=t-…` in every pane (ADR 0010 §1); an override cannot change them | implemented | 11 | `crates/subak/tests/pty_shells.rs::explicit_programs_get_their_arguments_exactly_and_report_exit_codes` (2026-11-04) |

## 14. Mouse

| tmux feature | tmux reference | Subak equivalent (planned) | Status | Target Step | Evidence |
| --- | --- | --- | --- | --- | --- |
| Enable mouse handling | `mouse` option | Same | planned | 25 | — |
| Mouse events as keys | `MouseDown1`–`3`, `MouseUp1`–`3`, `MouseDrag1`–`3`, `MouseDragEnd1`–`3`, `WheelUp`, `WheelDown`, `SecondClick1`–`3`, `DoubleClick1`–`3`, `TripleClick1`–`3` | Bindable mouse events | planned | 25 | — |
| Event locations | `Pane`, `Border`, `Status`, `StatusLeft`, `StatusRight`, `StatusDefault`, `ScrollbarSlider`, `ScrollbarUp`, `ScrollbarDown`, `Empty`, `ControlN` | Same suffixes | planned | 25 | — |
| Modifiers on mouse keys | `C-`, `M-`, `S-` (e.g. `M-MouseDown3Pane`) | Same | planned | 25 | — |
| Forward a mouse event to the program | `send-keys -M`, `mouse_any_flag` | Mouse reporting passed through ConPTY | planned | 25 | — |
| Mouse target token | `{mouse}`, `=` | Same (§13) | planned | 25 | — |
| Focus follows mouse | `focus-follows-mouse` | Same | planned | 25 | — |

## 15. Environment

| tmux feature | tmux reference | Subak equivalent (planned) | Status | Target Step | Evidence |
| --- | --- | --- | --- | --- | --- |
| Global environment copied at server start | global environment | *Differs on purpose:* every new pane gets the user's logon environment built fresh (`CreateEnvironmentBlock`), never the server's or a launcher's, so a variable set with `setx` reaches the next pane ([PTY_PROCESSES §3](PTY_PROCESSES.md)) | implemented | 11 | `crates/subak/tests/pty_shells.rs::each_new_pane_gets_a_fresh_logon_environment` (2026-11-04) (manual), `crates/subak/tests/pty_shells.rs::the_pane_environment_keeps_the_users_locations_and_secrets_never_leak` (2026-11-04) |
| Session environment | per-session environment | Per-Terminal environment from the profile (SUBAK-FR-011). *Partial:* per-pane overrides given at creation (`env` of `terminal.create`; names shown, values never); profiles with Step 33 | partial | 11 | `crates/subak/tests/pty_shells.rs::the_pane_environment_keeps_the_users_locations_and_secrets_never_leak` (2026-11-04) |
| Merge for new processes | session overrides global | Overrides (set or unset, case-insensitive names) on top of the logon environment, then the Host's `SUBAK_*` variables | implemented | 11 | `crates/subak/src/win/mod.rs::tests::overrides_replace_remove_and_add_case_insensitively` (2026-11-04) |
| Refresh from the client on attach | `update-environment` (e.g. `DISPLAY`, `SSH_AUTH_SOCK`) | Same | planned | 25 | — |
| Hidden variables | `set-environment -h`, `%hidden` | Hidden variables; variables marked secret are not persisted (SUBAK-SEC-011) | planned | 25 | — |
| Set, unset, remove, format value | `set-environment -u`, `-r`, `-F`, `-g` | Same | planned | 25 | — |
| Show in shell syntax | `show-environment -s` | Bourne syntax as in tmux; PowerShell syntax TBD | planned | 25 | — |
| In-tmux marker variable | `TMUX` | `SUBAK_PANE_ID` / `SUBAK_TERMINAL_ID` mark a Subak pane; no socket path | implemented | 11 | `crates/subak/tests/pty_shells.rs::explicit_programs_get_their_arguments_exactly_and_report_exit_codes` (2026-11-04) |
| Terminal type in panes | `TERM` (`default-terminal`) | Same | planned | 25 | — |
| vi keys from the editor | `VISUAL`, `EDITOR` | Same default for `mode-keys`/`status-keys` | planned | 24 | — |
| Home directory | `HOME` | Windows user profile directory | planned | 10 | — |
| Locale for output | `LC_CTYPE` | Always UTF-8 (KIM-NFR-013) | planned | 12 | — |
| Locale for times | `LC_TIME` | Windows locale for time formats | planned | 25 | — |
| Working directory | `PWD` | Profile or invoking directory | planned | 10 | — |
| Default shell | `SHELL` | Profile shell; Windows has no `SHELL` | planned | 10 | — |
| Socket directory | `TMUX_TMPDIR` | *Rationale:* no tmux socket (SUBAK-SEC-010) | unsupported | — | — |

## 16. Buffers and clipboard

| tmux feature | tmux reference | Subak equivalent (planned) | Status | Target Step | Evidence |
| --- | --- | --- | --- | --- | --- |
| Named and automatic buffers | `buffer0001`…, explicit names via `set-buffer -n`, `load-buffer -b` | Same | planned | 24 | — |
| Automatic buffer limit | `buffer-limit` (explicit names exempt) | Same | planned | 24 | — |
| Default buffer | most recent automatic buffer | Same | planned | 24 | — |
| Bracketed paste | `paste-buffer -p` | Same | planned | 24 | — |
| Line-ending handling on paste | `paste-buffer -r`, `-s separator` | Same; CRLF handling for Windows programs TBD | planned | 24 | — |
| System clipboard | `set-clipboard`, `set-buffer -w`, `load-buffer -w`, OSC 52 | Windows clipboard | planned | 24 | — |
| Clipboard read by programs | `get-clipboard` | Opt-in only (privacy) | planned | 24 | — |
| Per-pane history buffer | `history-limit` | Scrollback | planned | 13 | — |

## 17. Configuration file and parsing syntax

| tmux feature | tmux reference | Subak equivalent (planned) | Status | Target Step | Evidence |
| --- | --- | --- | --- | --- | --- |
| Load a user config at server start | `~/.tmux.conf`, `$XDG_CONFIG_HOME/tmux/tmux.conf`, `~/.config/tmux/tmux.conf` | *Rationale:* Subak's own config (SUBAK-FR-046) is primary; loading a tmux file automatically needs an ADR (opt-in import is the likely form) | deferred | post-26 | — |
| System-wide config | `@SYSCONFDIR@/tmux.conf` | *Rationale:* no system-wide tmux file on Windows; per-user installs (KIM-SEC-004) | unsupported | — | — |
| Load a tmux-syntax file on demand | `source-file path`, glob patterns | Same | planned | 25 | — |
| Parse only, quiet, verbose, format path | `source-file -n`, `-q`, `-v`, `-F` | Same | planned | 25 | — |
| Reload binding | `bind R source-file …` | Same | planned | 25 | — |
| Commands end at newline or `;` | `neww ; splitw` | Same | planned | 25 | — |
| Comments | `#` to end of line | Same | planned | 25 | — |
| Line continuation | trailing `\` | Same | planned | 25 | — |
| Quoting | `'…'`, `"…"` | Same | planned | 25 | — |
| Command lists in braces | `{ … }` (nestable, multi-line) | Same | planned | 25 | — |
| Replacements | `$VAR`, `~`, `~user`, `\uXXXX`, `\e`, `\r`, `\n`, `\t`, `\\` | Same; `~` is the Windows profile directory | planned | 25 | — |
| Conditional parsing | `%if`, `%elif`, `%else`, `%endif` | Same | planned | 25 | — |
| Environment assignment | `NAME=value`, `%hidden NAME=value` | Same | planned | 25 | — |
| Error reporting | config errors, `show-messages`, `%config-error` | Clear errors; invalid config falls back to defaults (SUBAK-FR-046) | planned | 25 | — |

## 18. Command chaining and scripting

| tmux feature | tmux reference | Subak equivalent (planned) | Status | Target Step | Evidence |
| --- | --- | --- | --- | --- | --- |
| Command sequences | `cmd1 ; cmd2`; stop at the first error | Same | planned | 25 | — |
| Separator escaping from a shell | `\;`, `';'` | Same; PowerShell and cmd need their own quoting (`';'`) | planned | 25 | — |
| Command queue semantics | `if-shell`/`confirm-before` insert commands after themselves | Same ordering | planned | 25 | — |
| Conditional on a shell command | `if-shell 'cmd' { … } { … }`, `-b` | Same; condition runs in the profile shell | planned | 25 | — |
| Conditional on a format | `if-shell -F '#{…}'` | Same | planned | 25 | — |
| Run a shell command | `run-shell`, `-b`, `-d`, `-c`, `-t` | Same | planned | 25 | — |
| Run a tmux command after a delay | `run-shell -C`, `run -d 0.3` | Same | planned | 25 | — |
| Synchronize scripts | `wait-for`, `-S`, `-L`, `-U` | Same | planned | 25 | — |
| Command aliases | `command-alias[N] name=value` | Same | planned | 25 | — |
| Format arguments | `-F` on `display-message`, `list-*`, `new-*`, `set-option` | Same | planned | 25 | — |
| Placeholders in prompts | `%%`, `%1`…`%9`, `%%%` | Same | planned | 24 | — |
| Shell for commands | `/bin/sh -c` (or `default-shell`) | Profile shell (PowerShell, cmd, Git Bash, WSL); quoting differs. *Differs:* `terminal new` runs the program with its arguments directly, never through a shell; scripts need their interpreter named (Step 11, [PTY_PROCESSES §2](PTY_PROCESSES.md)) | planned | 25 | — |

## 19. Control mode

| tmux feature | tmux reference | Subak equivalent (planned) | Status | Target Step | Evidence |
| --- | --- | --- | --- | --- | --- |
| Text protocol client | `tmux -C`, `tmux -CC` | *Rationale:* deferred; Subak's versioned IPC (KIM-NFR-014), `--json` CLI and the planned MCP server (SUBAK-FR-062) serve integrators first | deferred | post-26 | — |
| Output blocks | `%begin`, `%end`, `%error` | *Rationale:* as above | deferred | post-26 | — |
| Output notifications | `%output`, `%extended-output`, `%pause`, `%continue` | *Rationale:* as above | deferred | post-26 | — |
| Structure notifications | `%layout-change`, `%window-add`, `%window-close`, `%window-renamed`, `%window-pane-changed`, `%unlinked-window-add`, `%unlinked-window-close`, `%unlinked-window-renamed`, `%pane-mode-changed` | *Rationale:* as above | deferred | post-26 | — |
| Session and client notifications | `%sessions-changed`, `%session-changed`, `%session-renamed`, `%session-window-changed`, `%client-session-changed`, `%client-detached`, `%exit` | *Rationale:* as above | deferred | post-26 | — |
| Other notifications | `%message`, `%config-error`, `%paste-buffer-changed`, `%paste-buffer-deleted`, `%subscription-changed` | *Rationale:* as above | deferred | post-26 | — |
| Control client size and flow control | `refresh-client -C WxH`, `-A pane:on\|off\|pause\|continue`, `-f no-output,pause-after=N,wait-exit` | *Rationale:* as above | deferred | post-26 | — |
| Format subscriptions | `refresh-client -B name:what:format` | *Rationale:* as above | deferred | post-26 | — |

## 20. Multiple clients and access control

| tmux feature | tmux reference | Subak equivalent (planned) | Status | Target Step | Evidence |
| --- | --- | --- | --- | --- | --- |
| Several clients on one session | multiple `attach-session` | GUI, CLI and agents attached together (SUBAK-FR-004) | planned | 12 | — |
| List clients | `list-clients` | `subak host status` | planned | 12 | — |
| Window size with several clients | `window-size`, `aggressive-resize` | Same policies | planned | 25 | — |
| Client does not affect size | `ignore-size` client flag | Same | planned | 25 | — |
| Read-only clients | `attach-session -r`, `read-only` flag, `switch-client -r` | Same; input ignored except detach/switch | planned | 25 | — |
| Per-client active pane | `active-pane` client flag | *Rationale:* complicates the shared object model; revisit after Step 25 | deferred | post-26 | — |
| Keep client when its session is destroyed | `no-detach-on-destroy` client flag | Same | planned | 25 | — |
| Switch sessions | `switch-client -t`, `-l`, `-n`, `-p`, `-Z` | Same | planned | 25 | — |
| Choose a client | `choose-client` | Same | planned | 25 | — |
| Session groups | `new-session -t` | *Rationale:* see §3 | deferred | post-26 | — |
| Multi-user access | `server-access -a`, `-d`, `-r`, `-w`, `-g`, `-l` | *Rationale:* the Host is per-user; IPC is same-user only (SUBAK-SEC-010, SUBAK-SEC-012) | unsupported | — | — |
| Socket file permissions | socket mode bits | *Rationale:* no tmux socket; access control is the IPC's same-user restriction | unsupported | — | — |
| Locking | `lock-client`, `lock-session`, `lock-server` | *Rationale:* see §2.1 | unsupported | — | — |
| Suspending | `suspend-client` | *Rationale:* see §2.1 | unsupported | — | — |

## 21. Layouts, names, alerts and pane lifecycle

| tmux feature | tmux reference | Subak equivalent (planned) | Status | Target Step | Evidence |
| --- | --- | --- | --- | --- | --- |
| Even-horizontal layout | `even-horizontal` | Same | planned | 22 | — |
| Even-vertical layout | `even-vertical` | Same | planned | 22 | — |
| Tiled layout | `tiled` | Same | planned | 22 | — |
| Main-horizontal layout | `main-horizontal` | Same | planned | 25 | — |
| Main-horizontal-mirrored layout | `main-horizontal-mirrored` | Same | planned | 25 | — |
| Main-vertical layout | `main-vertical` | Same | planned | 25 | — |
| Main-vertical-mirrored layout | `main-vertical-mirrored` | Same | planned | 25 | — |
| Spread panes evenly | `select-layout -E` | Same | planned | 22 | — |
| Undo the last layout change | `select-layout -o` | Same | planned | 25 | — |
| Layout strings | `window_layout` format, `select-layout <string>` | Same string format, so layouts can be copied from tmux | planned | 25 | — |
| Zoom a pane | `resize-pane -Z`, `Z` flag | Same | planned | 22 | — |
| Session names | `new-session -s`, `rename-session` | Terminal names (SUBAK-FR-010) | planned | 10 | — |
| Window names | `new-window -n`, `rename-window`, `automatic-rename`, `\033k…\033\\` | Same | planned | 25 | — |
| Pane titles | `select-pane -T`, OSC 2, `allow-set-title` | Same | planned | 21 | — |
| Marked pane | `select-pane -m`, `-M`, `{marked}` | Same | planned | 24 | — |
| Disable/enable pane input | `select-pane -d`, `-e` | Same | planned | 25 | — |
| Synchronized input | `synchronize-panes` | Same | planned | 25 | — |
| Linked windows | `link-window`, `unlink-window` | Same | planned | 25 | — |
| Activity, bell and silence alerts | `monitor-*`, `visual-*`, `*-action`, window flags | Alerts on the in-tab window list and the GUI tab | planned | 25 | — |
| Dead panes | `remain-on-exit`, `pane_dead`, `respawn-pane` | Same | planned | 25 | — |
| Floating and modal panes (new in 3.8) | `new-pane`, `move-pane -P`, `-O` modal, z-order | *Rationale:* see `new-pane` (§2.2) | deferred | post-26 | — |
| Shell integration marks | OSC 133 (`next-prompt`, `pane-shell-prompt`) | Same | planned | 24 | — |
| Hyperlinks | OSC 8, `clear-history -H` | Clickable links in the GUI | planned | 25 | — |

## 22. Terminal integration and exit messages

| tmux feature | tmux reference | Subak equivalent (planned) | Status | Target Step | Evidence |
| --- | --- | --- | --- | --- | --- |
| Outer-terminal terminfo extensions | `AX`, `Bidi`, `Cs`/`Cr`, `Cmg`/`Clmg`/`Dsmg`/`Enmg`, `Dsbp`/`Enbp`, `Dseks`/`Eneks`, `Dsesc`/`Enesc`, `Dsfcs`/`Enfcs`, `Hls`, `Nobr`, `Rect`, `Smol`, `Smulx`, `Setulc`/`Setulc1`/`ol`, `Ss`/`Se`, `Spb`, `Swd`, `Sxl`, `Sync`, `Tc`, `Ms`, `XT` | *Rationale:* describe an outer terminal; the GUI renders natively. Revisit for a console client | deferred | post-26 | — |
| Pane terminal emulation | VT/xterm sequences inside panes | ConPTY output rendered by the GUI (SUBAK-FR-016) | planned | 13 | — |
| Client exit messages | `[detached (from session …)]`, `[detached and SIGHUP]`, `[lost tty]`, `[terminated]`, `[too far behind]`, `[exited]`, `[server exited]`, `[server exited unexpectedly]` | Equivalent CLI messages and exit codes; no SIGHUP or tty variants | planned | 12 | — |

## 23. Subak-specific differences

- **GUI tabs are Terminals, not tmux windows.** One GUI tab = one Terminal = one tmux session.
  `Ctrl+B c` creates an internal Window *inside* the current tab; internal Windows are listed in
  the tab (status line, Step 25), never as extra GUI tabs.
- **GUI-level shortcuts sit in front of the prefix table.** `Ctrl+T` (new Terminal),
  `Ctrl+W` (close = delete the Terminal, confirmed when busy) and `Ctrl+Tab` /
  `Ctrl+Shift+Tab` (next/previous tab) are Subak GUI shortcuts with no tmux binding
  (SUBAK-FR-033). Their nearest tmux analogues are `new-session`, `kill-session` and
  `switch-client -n`/`-p`. All are rebindable (SUBAK-FR-034).
- **Shadowed keys pass through via the prefix.** `Ctrl+B Ctrl+W`, `Ctrl+B Ctrl+T` (and
  `Ctrl+B Ctrl+Tab`) send the literal shadowed key to the program (readline, Vim), as decided in
  ADR 0009 (SUBAK-FR-034). tmux leaves these prefix keys unbound, so no tmux binding is lost.
  `Ctrl+B Ctrl+B` sends a literal `Ctrl+B`, exactly like tmux `send-prefix`.
- **The Host is not a tmux server.** There is no socket or protocol compatibility: no `-L`/`-S`,
  no `TMUX_TMPDIR`, no socket path in the marker variable, and the real `tmux` binary cannot
  attach to a Subak Host (or the reverse). tmux muscle memory carries over through the key
  tables; tmux scripts carry over through tmux-syntax commands (prompt, `source-file`, CLI form
  TBD).
- **Persistence goes further than tmux.** tmux sessions die with the server. The Subak Host
  persists structure and offers restore after a reboot, with new processes marked `restored`
  (SUBAK-FR-040…043). tmux has no built-in equivalent.
- **IDs are stronger.** tmux `$n`/`@n`/`%n` are unique for one server's life; Subak IDs are
  stable and never reused (SUBAK-FR-010).
- **Windows, not POSIX.** No SIGHUP, SIGTSTP or tty names; panes run under ConPTY with
  Job Objects (SUBAK-FR-016). Shell commands (`if-shell`, `run-shell`, `#()`, `copy-pipe`) run in
  the profile shell, so quoting differs from `/bin/sh`.
- **Closing vs killing is the same as tmux for panes and windows** (`Ctrl+B x`, `Ctrl+B &` kill
  processes), but closing the GUI is always a detach (SUBAK-FR-015).
- **Subak-only features** such as the Claude indicator (SUBAK-FR-050) have no tmux row.

## 24. Unreleased changes on tmux master (not counted)

Seen at `master` commit `a73616d781f2` (`tmux.1` revision 1.1195, 2026-10-29) and **not** in 3.8.
They get rows only once released:

- `display-popup` (`popup`) and the `popup-style`, `popup-border-style`, `popup-border-lines`
  options are removed from the manual page (floating panes take over that role).
- New options: `image-size-limit`, `image-total-count-limit`, `image-total-size-limit`, `utmp`,
  `side-status`, `side-status-format`, `side-status-style`, `side-status-width`,
  `copy-mode-keep-column`, `window-default-command`; a new `nl` list entry.
- The list of layout names is restructured in the manual page.

## 25. Related decisions and open inputs

Rows follow the Step 03 decision records in `docs/adr/`: ADR 0007 (IDs, names, targets),
ADR 0008 (attach, replay, size arbitration) and ADR 0009 (GUI keyboard dispatch and
pass-through). Still open: the CLI contract (Step 07) and the Kim design system (Step 04).
Rows that say "TBD" are updated when those land.

## 26. How evidence is recorded

- **Format.** An Evidence cell lists one or more automated tests and the date they first
  passed, e.g. `e2e/tests/step21_windows.rs::new_window_creates_internal_window (2027-01-15)` or
  a unit/integration test path plus test name. Dates are ISO 8601.
- **`implemented` needs automated evidence** that exercises the tmux-visible behaviour of the
  whole row (command, flags named in the row, key, or option effect). A manual check alone is
  never enough; it can support `partial` at most, and must say *manual* with steps.
- **`partial`** names what is covered in the Subak column (e.g. "`-h`, `-v` only").
- **Traceability.** Tests cite SUBAK-FR-060 and the tmux reference of the row in their doc
  comment (see the e2e conventions in [e2e/LEDGER.md](../e2e/LEDGER.md)).
- **Same-change updates.** Changing a Status updates Evidence and §27 in the same commit.
- **Reference upgrades.** Moving to a newer tmux release bumps the matrix version, adds new rows
  as `planned` (or `deferred`/`unsupported` with a rationale), and marks rows for removed tmux
  features. Step 26 re-validates every row and publishes the compatibility statement.

## 27. Summary counts

Computed from every matrix row in §1–§22 (matrix version 1); §24 is not counted. Recount
whenever a row or Status changes.

| Status | Rows |
| --- | --- |
| `planned` | 520 |
| `partial` | 8 |
| `implemented` | 0 |
| `deferred` | 55 |
| `unsupported` | 23 |
| **Total** | **606** |

| Section | planned | partial | deferred | unsupported | Total |
| --- | --- | --- | --- | --- | --- |
| §1 Invocation | 12 | 0 | 2 | 3 | 17 |
| §2 Commands | 75 | 7 | 5 | 5 | 92 |
| §3 Notable command flags | 17 | 1 | 1 | 1 | 20 |
| §4 Prefix table | 62 | 0 | 9 | 1 | 72 |
| §5 Root table (mouse) | 12 | 0 | 7 | 0 | 19 |
| §6 Key tables overview | 7 | 0 | 1 | 0 | 8 |
| §7 Copy mode | 29 | 0 | 0 | 0 | 29 |
| §8 Other pane modes | 5 | 0 | 2 | 0 | 7 |
| §9 Options | 81 | 0 | 9 | 5 | 95 |
| §10 Formats | 40 | 0 | 2 | 2 | 44 |
| §11 Status line and styles | 17 | 0 | 0 | 0 | 17 |
| §12 Hooks | 53 | 0 | 4 | 0 | 57 |
| §13 Target syntax | 24 | 0 | 0 | 0 | 24 |
| §14 Mouse | 7 | 0 | 0 | 0 | 7 |
| §15 Environment | 15 | 0 | 0 | 1 | 16 |
| §16 Buffers and clipboard | 8 | 0 | 0 | 0 | 8 |
| §17 Configuration file and parsing | 12 | 0 | 1 | 1 | 14 |
| §18 Command chaining and scripting | 12 | 0 | 0 | 0 | 12 |
| §19 Control mode | 0 | 0 | 8 | 0 | 8 |
| §20 Multiple clients and access control | 8 | 0 | 2 | 4 | 14 |
| §21 Layouts, names, alerts, pane lifecycle | 22 | 0 | 1 | 0 | 23 |
| §22 Terminal integration and exit messages | 2 | 0 | 1 | 0 | 3 |
| **Total** | **520** | **8** | **55** | **23** | **606** |

Coverage checks for matrix version 1: all 92 commands and their aliases, all 184 options in
`options-table.c`, all 51 named hooks and all 99 copy-mode commands of tmux 3.8 appear in the
tables above.
