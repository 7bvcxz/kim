# Subak Philosophy

Status: **current** · Owner: mouv · Applies to: Subak (first Kim tool)

Subak is a persistent, Windows-first terminal. Its sessions live in a background **Host**; any
number of **Clients** — a native tabbed GUI, a tiny floating icon, the headless CLI, an agent —
attach to and detach from them. Closing a window never loses your work. Subak follows the
[Kim philosophy](KIM_PHILOSOPHY.md); testable obligations are in
[PRODUCT_REQUIREMENTS.md](PRODUCT_REQUIREMENTS.md) (IDs in brackets).

## 1. Host and Client have separate lifecycles

| Component | Owns | Lifetime |
| --- | --- | --- |
| **Host** (`subak host`, per user) | Every Terminal, Window and Pane; the pseudo-consoles (ConPTY) and process trees; scrollback; the session state store | Starts on demand when a client needs it (or at login only if the user enabled that). Keeps running while it owns at least one live Terminal. Exits when it owns none and no client is attached. [SUBAK-FR-001, SUBAK-FR-002] |
| **Clients**: GUI, floating icon, CLI, MCP (later) | Nothing durable — only their view and input | Come and go freely. A client exiting, crashing or being closed never affects processes in the Host. [SUBAK-FR-003] |

- Only the Host starts, signals, or kills terminal processes.
- Clients talk to the Host over a local, per-user, versioned IPC channel that other users cannot
  access. No network listener by default. [SUBAK-SEC-010]
- The Host has no GUI; GUI code is never linked into headless paths. [KIM-FR-012]

## 2. Object model: Terminal → Window → Pane → process

| Level | Meaning | tmux analogue | GUI representation |
| --- | --- | --- | --- |
| **Terminal** | A named, persistent workspace with a stable ID | session | one **tab** in the tabbed GUI |
| **Window** | An internal full-size view inside a Terminal; a Terminal has ≥ 1 | window | the tab's content; switch with `Ctrl+B n/p/0-9` |
| **Pane** | A split region of a Window; a Window has ≥ 1 | pane | a split inside the tab |
| **Process** | The pane's root process (usually a shell) and its descendants, attached to one pseudo-console | pane process | text inside the pane |

Every object has a stable, opaque ID plus a human-editable name/title. IDs never get reused.
The CLI and GUI address the same objects by the same IDs. [SUBAK-FR-010]

## 3. Terminal CRUD

Terminals are first-class objects with full **C**reate / **R**ead / **U**pdate / **D**elete, in
both the GUI and the headless CLI: [SUBAK-FR-011…014]

- **Create** — new Terminal from a profile (shell, cwd, env, title); `Ctrl+T` in the GUI.
- **Read** — list Terminals with ID, name, state, windows/panes, Claude status; read pane output
  (capture) as text or JSON.
- **Update** — rename; change profile defaults; add/remove Windows and Panes; send input.
- **Delete** — kill a Terminal and its process trees. Deleting is explicit and is the *only* way
  to end a Terminal from Subak besides its processes exiting on their own.

## 4. The GUI: tiny floating icon and a native tabbed terminal

- **Floating icon.** A small, unobtrusive, draggable always-on-top icon (or tray icon, user's
  choice; both can be disabled). It shows aggregate status — notably the Claude indicator — and
  opens the GUI on click. [SUBAK-FR-030]
- **Tabbed terminal window.** One tab per Terminal; native rendering (no Electron, no
  embedded browser); restrained Kim design system; readable glyphs including Korean/CJK.
  [SUBAK-FR-031]
- **Session manager.** A view listing every Terminal the Host owns, including ones not open as
  tabs, with their state; open, rename, close, and restore from there. [SUBAK-FR-032]

### Keyboard (GUI defaults)

| Keys | Action |
| --- | --- |
| `Ctrl+T` | New Terminal (new tab) with the default profile |
| `Ctrl+W` | Close the current Terminal = **delete** it (see §6). Asks for confirmation when any pane runs something other than an idle shell. |
| `Ctrl+Tab` / `Ctrl+Shift+Tab` | Next / previous Terminal tab (positional order, wraps) |
| `Ctrl+B` *key* | tmux-style prefix table for Windows/Panes (`c`, `n`, `p`, `%`, `"`, arrows, `x`, `z`, `d`, `,`, `$`, `[`, …). `Ctrl+B Ctrl+B` sends a literal `Ctrl+B`. |

`Ctrl+T` and `Ctrl+W` shadow shell/editor bindings (readline transpose / delete-word, Vim window
commands). Subak therefore **must** let users rebind every GUI shortcut and must provide a way to
send the shadowed key through to the program. [SUBAK-FR-034] The exact pass-through mechanism is
an architecture decision (Step 03).

## 5. Compatibility aspirations (long-term, not MVP)

- **tmux.** Aspire to full compatibility with tmux's session/window/pane commands, `Ctrl+B` key
  table, and scripting surface (`send-keys`, `capture-pane`, `list-*`, format strings), so tmux
  muscle memory and scripts carry over. MVP ships a documented subset; every item is tracked as
  implemented / planned / unsupported in `docs/TMUX_COMPATIBILITY.md` (planned).
  [SUBAK-FR-060]
- **Tabby.** Aspire to functional parity with Tabby's terminal UX: profiles, split panes, SSH
  connections, file transfer, themes, search. Tracked in `docs/TABBY_FEATURE_MATRIX.md`
  (planned). Parity is a direction, not an MVP promise. [SUBAK-FR-061]

## 6. Crucial distinctions

These are the heart of Subak. Every UI label, CLI command and doc must keep them distinct.

### 6.1 Closing a GUI vs killing a session

| Action | Effect on processes |
| --- | --- |
| Close the GUI window / quit the GUI app / `Ctrl+B d` (detach) | **None.** The client disconnects; every Terminal keeps running in the Host. |
| Client crashes or is killed | **None.** Same as detach. |
| Closing a tab with its × button / "Hide tab" | **None.** That tab detaches; the Terminal keeps running in the session manager ([SESSION_MODEL §4](SESSION_MODEL.md)). |
| `subak attach` / `subak detach` from the CLI | **None.** Clients attach/detach freely. |
| `Ctrl+W` / "Close Terminal" / `subak kill <terminal>` | **Kills** that Terminal's processes. Explicit, confirmed when busy, reported. |
| Closing a pane (`Ctrl+B x`) or window (`Ctrl+B &`) | Kills that pane's / window's processes only (confirmation like tmux). |
| Process exits by itself (e.g. `exit`) | The pane closes; an empty window/Terminal closes (configurable "keep pane open on exit"). |

[SUBAK-FR-003, SUBAK-FR-014, SUBAK-FR-015]

### 6.2 Session restore vs preserving live processes across reboot

A reboot, logoff, or Host crash **ends every process**; no terminal can keep a live process
alive through an OS restart, and Subak never pretends otherwise. What Subak *can* do:

- **Restore** (re-create) the structure: Terminals, Windows, Panes, names, layout, working
  directories, profiles and non-secret environment. New processes are started for them.
- Show the **previous scrollback** as read-only history (if the user enabled scrollback saving),
  clearly separated from the new session's output.
- Never claim that a restored pane is the same process. Restored objects are marked
  `restored`. [SUBAK-FR-040…043]

### 6.3 Sleep / lock vs shutdown

- **Sleep, hibernate, screen lock, and disconnecting Remote Desktop** do not end processes. The
  Host keeps running; nothing needs restoring, and Subak must not "restore" duplicates.
- **Shutdown, restart, logoff, or a Host crash** end processes; the next Host start offers
  restore (§6.2). [SUBAK-FR-044]

### 6.4 Claude conversation resume vs continuing arbitrary commands

- A **Claude Code conversation** is stored by Claude Code itself and can be resumed after a
  reboot (`claude --resume <session-id>`). When a pane was running Claude Code with a known
  session ID, Subak can start that resume command in the restored pane — only if the user
  enabled Claude resume. [SUBAK-FR-045]
- **Arbitrary running commands** (builds, servers, scripts, editors) cannot be continued; their
  in-memory state is gone. Subak never re-runs them automatically, because re-running is not
  resuming and may be destructive or have side effects. A user may opt in per pane to a
  "restart command" that Subak runs on restore. [SUBAK-SEC-002]

## 7. Claude five-state indicator

Each pane running Claude Code has one of **five states**, aggregated to its Window, Terminal
tab, session manager and the floating icon:

| State | Meaning | Visual intent |
| --- | --- | --- |
| `idle` | Claude Code is running and waiting for a new prompt | quiet dot |
| `in-progress` | Claude is generating or running tools | animated (respects reduced motion) |
| `need-input` | Claude is blocked on the user: permission request or question | prominent accent |
| `error` | The last turn failed (API error, crash, failure reported by Claude Code) | error color |
| `done` | Claude finished its task and the user has not looked at it yet; becomes `idle` once viewed | distinct "unread" dot |

Two further values are not Claude states but honesty markers: `unknown` — Claude Code is (or
may be) present but the evidence is missing or stale, so Subak shows "unknown" instead of
guessing; and `none` — no Claude Code in the pane, nothing shown. A tool call finishing is not
`done`; only the end of Claude's turn is.

Aggregation priority: `need-input` > `error` > `done` > `in-progress` > `unknown` > `idle` >
`none`. Every indicator also has a text label/tooltip, never color alone. Detection must be
local, use official Claude Code hooks/events where possible (opt-in, installed without
overwriting the user's settings), and never read or transmit prompts, credentials or tool
output beyond what is needed to compute the state. [SUBAK-FR-050…053]

## 8. Durable configuration and state

- Configuration is human-readable, versioned, and survives updates; unknown keys are preserved,
  and invalid configs fall back to defaults with a clear error instead of crashing.
  [SUBAK-FR-046]
- Session state is written atomically, versioned with migrations, and backed up before any
  migration or restore so a bad restore can never destroy the previous good state.
  [SUBAK-SEC-003]
- Updating or uninstalling Subak while the Host owns live Terminals must not kill them
  silently: the user chooses to defer or to end them (with a restore snapshot). [KIM-FR-034]
