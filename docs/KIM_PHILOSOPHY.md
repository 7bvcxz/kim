# Kim Philosophy

Status: **current** · Owner: mouv · Applies to: Kim manager and every Kim tool

Kim is a platform of small, independent, native Rust tools that are beautiful for humans and
powerful for agents. This document states *why* Kim is built the way it is. Testable
obligations derived from it live in [PRODUCT_REQUIREMENTS.md](PRODUCT_REQUIREMENTS.md)
(IDs in brackets below).

## 1. Small, independent tools

- Each Kim tool is its own native Rust binary with its own version, release notes and
  installation. Tools are useful alone. [KIM-FR-001]
- **Kim** (the manager) is itself just one small tool: a catalog and an explicit installer. It
  does not host other tools, run them inside itself, or require them. Installing Kim installs
  nothing else. [KIM-FR-002]
- Tools interoperate through ordinary, documented interfaces — command lines, JSON on
  stdout, exit codes, files, and optionally MCP — not through a shared monolithic runtime,
  plugin host or mandatory background service. [KIM-FR-020]
- Shared code is extracted into a library only after real reuse appears; tools must not drag
  each other's UI runtimes or dependencies into their binaries.

## 2. Beautiful for humans, powerful for agents

- **For humans:** restrained, polished visuals; dense but calm layouts; predictable keyboard
  behavior; accessible focus and contrast. Beauty is a feature, not a skin.
- **For agents:** every capability that matters is reachable from a stable CLI with
  machine-readable JSON output, documented exit codes, structured error codes, discoverable
  `--help`, and no interactive prompts in non-interactive mode. [KIM-FR-010, KIM-FR-011]
- The GUI and the CLI are two clients of the same behavior. Anything a human can do in the GUI
  that changes state should be doable headlessly, and vice versa. [KIM-FR-013]
- **MCP guidance:** where a tool benefits agents beyond what a CLI offers (streaming state,
  structured tools), it may expose an MCP server. MCP is optional, opt-in, local by default,
  and never the only interface. [KIM-FR-021]

## 3. Fast and frugal

Rust does not make software fast automatically; discipline does.

- Idle cost matters as much as peak speed: near-zero CPU when idle, small resident memory,
  small binaries, no disk churn, **no network traffic unless the user asked for it**.
  [KIM-NFR-001…005]
- No always-on daemons unless the tool's purpose requires one, and then only per user, only
  while needed, and visible to the user. (Subak's Host exists because it owns live terminal
  sessions; it is not a general Kim daemon.)
- Performance claims are measured, recorded and reproducible, not assumed.

## 4. Windows first, Linux later

- Windows 10 (version 1809+) and Windows 11 on x86_64 are the first-class targets.
  [KIM-NFR-010]
- Linux is planned. Code keeps OS-specific details behind narrow, portable interfaces so a
  Linux implementation can be added, but Linux support is **never claimed before it is
  implemented and tested**. [KIM-NFR-011]

## 5. CLI, headless and GUI

- CLI and headless operation are the foundation; GUIs are optional layers.
- A headless or CLI path must never launch a GUI, tray icon, browser window or interactive
  prompt unexpectedly. [KIM-FR-012]
- GUIs are native, light, and launched only when the user starts them. Electron and embedded
  full browsers are out.

## 6. Local-first, private, secure

- No accounts, no sign-in, no ads, no tracking. Tools work fully offline once installed.
  [KIM-SEC-010, KIM-SEC-011]
- Telemetry is off by default; any diagnostic data is local, documented, contains no secrets,
  terminal content, command lines or file paths, and is only shared if the user exports it.
  [KIM-SEC-012]
- Secrets never appear in logs or JSON output. [KIM-SEC-013]
- Downloads are verified (hash and signature) before anything is installed. Least privilege;
  never silently elevate. [KIM-SEC-003, KIM-SEC-004]

## 7. Consent: nothing installs or updates by itself

This is non-negotiable.

- Kim **never** installs, updates, or removes a tool without an explicit user action for that
  specific operation. [KIM-SEC-001]
- "Update check" means *showing* that a newer version exists. It never downloads or installs,
  is optional (off by default at launch; always available on demand), and respects
  offline/privacy preferences. [KIM-SEC-002]
- No tool updates itself silently, and no tool starts another tool at login unless the user
  enabled that explicitly.
- Uninstalling preserves user data unless the user separately and clearly confirms deletion.
  [KIM-SEC-005]

## 8. Honesty in engineering

- Placeholders, mocks and planned features are labeled as such in code, help text and docs.
- Untested platforms or behaviors are explicitly marked **not tested**.
- Decisions that trade one principle against another are written down as ADRs.

## 9. Keyboard shortcuts belong to the user

- Every installable Kim tool that has a GUI lets the user see, change and switch off its
  keyboard shortcuts: a discoverable list of its actions and their current keys, defaults only
  where a default really helps, user-editable bindings, disable and reset for one action or for
  the whole tool, and a headless command to inspect and change all of it.
  [KIM-FR-040…046]
- Shortcuts serve GUI operations. Not every CLI command needs a hotkey, and a tool without a GUI
  needs none; agents reach the same operations through the CLI.
- A tool **asks** for keys and never **takes** them. Keys the operating system reserves, the
  secure desktop (Ctrl+Alt+Del, UAC, sign-in, lock screen), password fields, and shortcuts that
  other programs already own are respected. Kim detects and reports a conflict and offers a
  remap; it never steals a key by force. [KIM-SEC-020, KIM-SEC-021]
- There is no shared hotkey daemon or system-wide hook for all tools. Each tool handles its own
  shortcuts, and only while it runs. A tool installs a keyboard hook only when its purpose
  requires one (Ddalgak, an automation engine), only after the user enabled it, and never
  hidden. Shortcut handling is never keylogging. [KIM-NFR-020, KIM-SEC-022]
- There is always a way back: a switch that turns all of a tool's shortcuts off, reset to
  defaults, and an emergency path that still works when a binding or the configuration is
  broken. [KIM-SEC-023]
- Shortcuts survive keyboard layouts and input methods: Korean and other IMEs keep working,
  non-US layouts are first-class, and stored bindings are portable to Linux later.
  [KIM-NFR-021, KIM-NFR-022]
