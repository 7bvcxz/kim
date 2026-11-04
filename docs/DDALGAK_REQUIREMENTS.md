# Ddalgak Requirements

Status: **current** (Step 65, specification; the engine rows of D2 are implemented since Step 66
and the rule-store rows `DDALGAK-FR-005…007`, `DDALGAK-FR-009` since Step 67; remap and block
(`DDALGAK-FR-010`) and mouse-button triggers (`DDALGAK-FR-071`, middle and X buttons) since Step
68, whose real-keyboard and mouse checks are still manual; see [DDALGAK_ENGINE](DDALGAK_ENGINE.md)
§8, §10 and §11 for what was verified; the GUI half of `DDALGAK-FR-007` and undo arrive with the
editor, Step 74) · Owner: mouv ·
Derived from [DDALGAK_PHILOSOPHY](DDALGAK_PHILOSOPHY.md) · Decisions:
[ADR 0022](adr/0022-ddalgak-rule-model-and-safety.md), [ADR 0028](adr/0028-ddalgak-rule-store-validation-and-reload.md),
[ADR 0031](adr/0031-ddalgak-remap-execution-and-mouse-hook.md)

## 0. How to read this document

- **IDs** are unique and permanent: `DDALGAK-FR-ddd` (functional), `DDALGAK-NFR-ddd`
  (non-functional), `DDALGAK-SEC-ddd` (security, privacy and safety). IDs are never renumbered
  or reused; a dropped requirement is marked *withdrawn*. `cargo xtask docs-check` validates the
  rows and every reference.
- **Platform requirements still apply.** Every `KIM-*` requirement in
  [PRODUCT_REQUIREMENTS](PRODUCT_REQUIREMENTS.md) binds Ddalgak too: no automatic installs or
  updates (`KIM-SEC-001`), headless means headless (`KIM-FR-012`), no account or telemetry
  (`KIM-SEC-010…012`), secrets never in logs (`KIM-SEC-013`), the hotkey contract
  (`KIM-FR-040…046`, `KIM-SEC-020…023`). This document adds what is specific to Ddalgak.
- **Priority:** `P0` must ship in its milestone · `P1` should ship · `P2` tracked, not promised.
- **Verify:** `T` automated test · `M` manual check on real Windows · `I` inspection · `D`
  documentation exists and is accurate.
- **NON-NEGOTIABLE** requirements may not be relaxed by any ADR, setting, default, flag or test
  shortcut.

## 1. Milestones

| Milestone | Goal | Steps | Done when |
| --- | --- | --- | --- |
| **D1 – Specification** | Philosophy, requirements, rule model and schemas, GUI journeys, safety scope, AutoHotkey matrix | 65 | Docs current; `schema-check` proves the fixtures |
| **D2 – Engine core** | Hook engine, rule store and CRUD with validation, enable/pause/disable and emergency recovery | 66, 67, 73 | All `P0` D2 rows pass, including manual Windows checks |
| **D3 – Core actions** | Remap and block, send keys, Unicode text, programs and websites, mouse, modes | 68–72 | Each action passes its tests and a real-Windows check |
| **D4 – Visual editor** | Rule editor, profiles with history and export, test playground | 74–76 | A first-time user completes the journeys of [GUI_JOURNEYS](DDALGAK_GUI_JOURNEYS.md) |
| **D5 – Context and advanced automation** | App conditions and precedence, macros and recorder, window/clipboard/scheduled actions, AutoHotkey import | 77–80 | Per row |
| **D6 – Integration and first release** | Complete headless CLI, opt-in MCP, Kim integration, hardening, first Windows release through Kim | 81, 82 | Release checklist of [RELEASE_POLICY](RELEASE_POLICY.md) and all `P0` rows of D2–D6 |
| **D7 – Linux** | Linux backends where the desktop allows them | 83 | Linux rows pass on real Linux desktops |

Until D6 ships, the Kim catalog lists Ddalgak as *planned* (`KIM-FR-051`).

## 2. Functional requirements

### 2.1 Product, process and engine

| ID | Requirement | Pri | MS | Verify |
| --- | --- | --- | --- | --- |
| DDALGAK-FR-001 | Ddalgak is one independently installable package with two binaries: `ddalgak.exe` (headless CLI, links no GUI toolkit) and `ddalgak-gui.exe` (tray app that owns the engine, the editor and the playground). It needs no AutoHotkey runtime, Electron, resident Kim process, account or cloud service, and no other Kim tool. | P0 | D2 | I, T |
| DDALGAK-FR-002 | The engine has three states, `off`, `running` and `paused`, shown in the tray icon (shape and text, not color alone), the GUI header and `ddalgak status --json`. The keyboard hook exists only in `running` and `paused`. | P0 | D2 | T, M |
| DDALGAK-FR-003 | The app starts with the engine `off` unless the user chose "start with the engine on" (`engine_on_start: on`). After an unclean exit or an emergency off it always starts `off` and says why. | P0 | D2 | T |
| DDALGAK-FR-004 | The engine is turned on from the tray, the GUI, the `engine.toggle` shortcut (registered with `RegisterHotKey`, so it works while the hook is off) or `ddalgak engine on` against the running app. A rule can pause, resume or turn the engine off, never on. | P0 | D2 | T, M |
| DDALGAK-FR-005 | Rules are stored in `%APPDATA%\mouv\ddalgak\rules.json` as a `ddalgak.rules` v1 document ([ddalgak-rules-v1](contracts/schemas/ddalgak-rules-v1.schema.json)), written atomically with a backup, with a `revision` that makes concurrent GUI and CLI edits fail instead of overwriting each other. | P0 | D2 | T |
| DDALGAK-FR-006 | Validation applies the schema and the semantic rules of [RULE_MODEL §8](DDALGAK_RULE_MODEL.md) with stable codes. A rule with an error is inactive and the others keep working; a file that fails the schema is never loaded, and a running engine keeps its previous rules. | P0 | D2 | T |
| DDALGAK-FR-007 | Rule CRUD: create, show, edit, delete, duplicate, enable, disable and reorder, in the GUI and the CLI, with `--json` and `--dry-run`; delete asks for confirmation (`--yes` headless) and can be undone in the GUI. | P0 | D2 | T |
| DDALGAK-FR-008 | Triggers use the hotkey contract's key syntax with Ddalgak's extensions (bare side-specific modifiers, lock keys), up to four-chord sequences, `press`, `release` and `hold`, auto-repeat, pass-through and an injected-input policy ([RULE_MODEL §3](DDALGAK_RULE_MODEL.md)). | P0 | D2 | T |
| DDALGAK-FR-009 | A running app picks up a valid change to `rules.json` (from the GUI or the CLI) within one file-change notification, without restarting, without stuck keys and without dropping unrelated rules. | P0 | D2 | T |

### 2.2 Core action types

| ID | Requirement | Pri | MS | Verify |
| --- | --- | --- | --- | --- |
| DDALGAK-FR-010 | **Remap**: a key acts as another key or chord for exactly as long as it is held, with modifiers handled as in [RULE_MODEL §5.1](DDALGAK_RULE_MODEL.md), an optional tap alternative (dual role), and **block** to disable a key or chord. | P0 | D3 | T, M |
| DDALGAK-FR-011 | **Send keys**: a trigger taps a chord or a sequence of up to eight chords. | P0 | D3 | T, M |
| DDALGAK-FR-012 | **Type text**: Unicode text up to 4096 characters (Hangul, emoji, newlines, tabs) is typed independent of keyboard layout and IME, or pasted through the clipboard, which is restored afterwards. | P0 | D3 | T, M |
| DDALGAK-FR-013 | **Open a program** by absolute path or packaged-app ID, with an argument list, working folder, window state and "bring to front if running". | P0 | D3 | T, M |
| DDALGAK-FR-014 | **Open a website**: an `http` or `https` URL opens in the user's default browser. | P0 | D3 | T, M |
| DDALGAK-FR-015 | **Mouse**: move relative, absolute or continuously while held (speed, acceleration), scroll by notches in four directions, click, double-click or hold any of five buttons, across monitors with negative coordinates, clamped to the visible desktop. | P0 | D3 | T, M |
| DDALGAK-FR-016 | **Modes**: users create, rename, duplicate and delete modes per profile; rules can be limited to modes; actions set, toggle, cycle, hold (while the trigger is held), return to the previous and to the default mode. The active mode is shown (optional compact indicator) and readable headlessly. | P0 | D3 | T, M |
| DDALGAK-FR-017 | **Engine control** actions pause, resume, toggle pause and turn the engine off. Rules made only of engine-control steps keep working while paused. | P0 | D2 | T |
| DDALGAK-FR-018 | **Ordered steps**: a rule runs up to 32 steps in order with delays (total at most 60 s) and a stop-or-continue error policy. One run per rule at a time; a trigger during a run is reported as `busy`. | P1 | D3 | T |

### 2.3 Selection, modes and conditions

| ID | Requirement | Pri | MS | Verify |
| --- | --- | --- | --- | --- |
| DDALGAK-FR-020 | Exactly one rule wins per trigger event, chosen by the deterministic precedence of [RULE_MODEL §7](DDALGAK_RULE_MODEL.md) (mode-scoped, then condition specificity, then priority, then order). Unmatched keys reach the application unchanged. | P0 | D2 | T |
| DDALGAK-FR-021 | A rule with conditions (`when`) or a step type this version cannot evaluate is inactive with a stated reason; it is never treated as unconditional. | P0 | D2 | T |
| DDALGAK-FR-022 | App conditions: focused program by file name or path, exclusions, and window title (contains or equals); evaluated only at trigger time. | P1 | D5 | T, M |
| DDALGAK-FR-023 | `ddalgak explain <keys> --mode <m> --app <exe> --json` and the playground show every candidate with why it won or lost, without running anything. | P1 | D5 | T |

### 2.4 Profiles, history, import and export

| ID | Requirement | Pri | MS | Verify |
| --- | --- | --- | --- | --- |
| DDALGAK-FR-030 | Profiles: create, rename, duplicate, delete and switch; a switch is atomic (all rules of the old profile stop, held keys are released, then the new profile applies). | P0 | D4 | T, M |
| DDALGAK-FR-031 | History: every applied change keeps a snapshot; the user can compare and restore an earlier state, which is itself a new, undoable change. | P1 | D4 | T |
| DDALGAK-FR-032 | Export writes a portable `ddalgak.rules` document without consent or settings. Import validates, shows a diff and adds profiles with every rule disabled until reviewed; it never activates a profile or grants consent. | P0 | D4 | T |

### 2.5 Test playground

| ID | Requirement | Pri | MS | Verify |
| --- | --- | --- | --- | --- |
| DDALGAK-FR-040 | Simulation (the default): for an event and a context (profile, mode, app, focus kind, engine state) Ddalgak reports a `ddalgak-simulation-v1` trace: decision, candidates and the effect of every step. Nothing is injected, opened or changed. Available in the GUI and as `ddalgak test --dry-run --json`. | P0 | D4 | T |
| DDALGAK-FR-041 | Live test: input goes only into the playground's own test field; a program or website opens only after a confirmation for that launch; reset restores the playground and never touches active rules. | P1 | D4 | T, M |
| DDALGAK-FR-042 | Unsaved drafts can be simulated and live-tested before they are saved or activated. | P1 | D4 | T |

### 2.6 GUI

| ID | Requirement | Pri | MS | Verify |
| --- | --- | --- | --- | --- |
| DDALGAK-FR-050 | A rule can be created in four steps: capture the trigger, pick one of the effect tiles, fill one form, save; with keyboard-only capture and a "type instead" text field. | P0 | D4 | T, M |
| DDALGAK-FR-051 | The rule list offers search, filters (profile, mode, app, state), conflict and problem badges with text, multi-select enable/disable, duplicate, delete with undo, and export. | P0 | D4 | T, M |
| DDALGAK-FR-052 | The GUI never shows a rule as saved or active before the engine has loaded it; drafts, saved and active states are distinct. | P0 | D4 | T |
| DDALGAK-FR-053 | Korean and English labels; the editor renders Hangul and emoji correctly. | P1 | D4 | M |

### 2.7 Headless, hotkey contract and integration

| ID | Requirement | Pri | MS | Verify |
| --- | --- | --- | --- | --- |
| DDALGAK-FR-060 | `ddalgak` follows the Kim CLI contract for `rule`, `profile`, `mode`, `test`, `explain`, `status`, `pause`, `resume`, `engine`, `hotkeys` and `capabilities`, with `--json`. No CLI command opens a window or tray icon, starts the app or installs a hook. | P0 | D6 | T |
| DDALGAK-FR-061 | Ddalgak conforms to the hotkey contract: its manifest ([example](contracts/examples/kim-hotkey-manifest-v1/ddalgak.json)), `hotkeys.json`, the `hotkeys` commands, rule triggers listed as read-only `rule.<id>` actions and `hook_overrides_global` reported ([HOTKEY_CONTRACT §13.4](HOTKEY_CONTRACT.md)). | P0 | D2 | T |
| DDALGAK-FR-062 | An opt-in local MCP adapter offers read-only inspection, explain and simulation; changes need explicitly granted permissions. It cannot turn the engine on, run live tests or grant consent. | P2 | D6 | T |
| DDALGAK-FR-063 | An AutoHotkey v2 importer previews and converts a declarative subset into disabled rules with a line-by-line coverage report; it never executes or evaluates the script. | P2 | D5 | T |

### 2.8 Later capabilities (tracked)

| ID | Requirement | Pri | MS | Verify |
| --- | --- | --- | --- | --- |
| DDALGAK-FR-070 | Hotstrings: typed abbreviations as triggers, with word boundaries, case options and replacement, never in password fields and without keeping typing history. | P1 | D3 | T, M |
| DDALGAK-FR-071 | Mouse buttons as triggers (needs a low-level mouse hook, disclosed in the catalog before it ships). Step 68: the middle and X buttons, through a mouse hook that exists only while such a rule is active; the primary and secondary buttons are never triggers (ADR 0031). | P1 | D3 | T, M |
| DDALGAK-FR-072 | Window actions (activate, minimize, maximize, move, resize) under the `window-control` permission. | P2 | D5 | T, M |
| DDALGAK-FR-073 | Clipboard actions (set, paste, transform) under the `clipboard` permission, never logging contents. | P2 | D5 | T, M |
| DDALGAK-FR-074 | Macro recorder: explicit start and stop with a visible banner, password fields and secure contexts excluded, every recorded event reviewable and removable before saving. | P2 | D5 | T, M |
| DDALGAK-FR-075 | Richer macro control: bounded repeat, conditions and variables, with timeouts and cancellation. | P2 | D5 | T |
| DDALGAK-FR-076 | Scheduled and time-based triggers in a running app, with an idle budget and no hidden timers. | P2 | D5 | T, M |

## 3. Non-functional requirements

| ID | Requirement | Pri | MS | Verify |
| --- | --- | --- | --- | --- |
| DDALGAK-NFR-001 | **Idle cost.** With the engine running, 100 rules and no input, the app averages < 0.1 % of one core over 60 s and has no periodic timers; with the engine off it holds no hook. | P0 | D6 | M |
| DDALGAK-NFR-002 | **Hook latency.** The hook callback returns in < 1 ms (p99) for any key, matched or not; matching side effects run on a worker thread. The engine never relies on the system's `LowLevelHooksTimeout` (at most 1000 ms on Windows 10 1709 and later). | P0 | D2 | T, M |
| DDALGAK-NFR-003 | **Memory.** Tray app with the engine running and no window open: < 25 MB working set; editor open: < 80 MB. | P1 | D6 | M |
| DDALGAK-NFR-004 | **Load time.** Loading and validating 1000 rules and installing the hook takes < 100 ms on a typical machine. | P1 | D2 | T, M |
| DDALGAK-NFR-005 | **Portable rules.** Stored keys use the contract's names, never virtual-key numbers; paths may use known-folder tokens; the file is UTF-8 JSON. | P0 | D2 | T, I |
| DDALGAK-NFR-006 | **Layouts and IME.** Logical, physical and character keys behave as in [HOTKEY_CONTRACT §6](HOTKEY_CONTRACT.md); no rule fires during an IME composition; typed text is independent of the active layout. Verified with the US layout and Korean 2-set IME. | P0 | D3 | T, M |
| DDALGAK-NFR-007 | **Accessibility.** The editor and playground are fully keyboard-operable, announce captured keys and errors to screen readers and meet `KIM-NFR-012`. | P0 | D4 | T, M |
| DDALGAK-NFR-008 | **Contract stability.** The rules and simulation documents carry schema versions; additive changes keep v1; a newer file is read-only; migrations are pure `vN → vN+1` steps behind a backup. | P0 | D2 | T |
| DDALGAK-NFR-009 | **Linux is not claimed** before D7; the X11 and Wayland differences are documented ([DDALGAK_SAFETY §9](DDALGAK_SAFETY.md)). | P0 | D2 | I |
| DDALGAK-NFR-010 | **Reliability.** No handle, thread or hook leak over a 72-hour run with periodic profile switches, pauses and reloads. | P1 | D6 | M |

## 4. Security, privacy and safety

| ID | Requirement | Pri | MS | Verify |
| --- | --- | --- | --- | --- |
| DDALGAK-SEC-001 | **NON-NEGOTIABLE. No keylogging.** The hook inspects each event only to decide whether a rule matches. Keys and text are never stored, logged, transmitted or shown in diagnostics; the activity list records rule IDs, outcome codes and times only. | P0 | D2 | I, T |
| DDALGAK-SEC-002 | **NON-NEGOTIABLE. Secure desktop and password fields.** Ddalgak never intercepts or injects on the secure desktop (Ctrl+Alt+Del, UAC, sign-in, lock screen). In a password field it steps aside: rules are skipped and keys pass unchanged; text is never typed into one; the recorder never records one. | P0 | D2 | I, T, M |
| DDALGAK-SEC-003 | **NON-NEGOTIABLE. Emergency off.** `Ctrl+Alt+Shift+Escape` is detected on physical key state before any rule, cannot be remapped, hidden or produced by a rule, and within one key event removes the hook, releases every key and button Ddalgak holds, cancels runs and turns the engine off. It works with an invalid or conflicting configuration. | P0 | D2 | T, M |
| DDALGAK-SEC-004 | **No recursion.** Ddalgak's own injected input never triggers a rule; a rule that accepts other programs' injected input never injects input; a rule whose output is its own trigger is refused; each rule is rate-limited and a run-away loop through another program is stopped with `rate_limited`. | P0 | D2 | T |
| DDALGAK-SEC-005 | **No stuck keys.** Every key or button Ddalgak presses is released: on trigger release, pause, turn-off, emergency, profile or mode switch, session lock or switch to the secure desktop, a failing run and process exit. A consumed key-down always has its key-up consumed too. | P0 | D3 | T, M |
| DDALGAK-SEC-006 | **Explicit consent per permission.** `input-injection`, `launch-programs` (bound to a fingerprint of the exact targets) and `clipboard` each need consent recorded on the rule, given in the GUI or with `--allow <permission>`. Consent is never imported and never granted by an agent. | P0 | D2 | T |
| DDALGAK-SEC-007 | **No elevation or privileged components.** No administrator rights, `UIAccess`, service, driver or SYSTEM process. Injecting rules are skipped while an elevated window has focus (`skipped_elevated_target`), and the GUI explains why. | P0 | D2 | I, M |
| DDALGAK-SEC-008 | **Launch safety.** Programs start with `CreateProcess` and an argument list, never a shell string; command interpreters and script hosts are refused; URLs are `http`/`https` only and carry no credentials; nothing requests elevation; a missing target is reported, never searched for. | P0 | D3 | T, I |
| DDALGAK-SEC-009 | **Reserved chords.** Triggers never use OS-reserved, IME, emergency, `Alt+F4` or global `F12` chords; outputs never produce the Secure Attention Sequence or the emergency chord; `Win` chords are not triggers in v1. | P0 | D2 | T |
| DDALGAK-SEC-010 | **A visible hook.** The hook runs only inside `ddalgak-gui.exe` started by the user (or by opt-in sign-in start), with its tray icon showing the engine state. No headless path creates a hook or a tray icon. | P0 | D2 | I, T |
| DDALGAK-SEC-011 | **Private rule content.** Typed text, URLs, paths and arguments never appear in logs, activity entries or simulation output beyond the summaries of [RULE_MODEL §11](DDALGAK_RULE_MODEL.md); rule files are readable by the user only. | P0 | D2 | T, I |
| DDALGAK-SEC-012 | **Anti-cheat honesty.** Ddalgak never hides itself, never evades or bypasses anti-cheat or input protection, and installs no kernel component. Documentation says that games may ignore or forbid synthetic input and that using Ddalgak in such games is the user's decision. | P0 | D3 | D, I |
| DDALGAK-SEC-013 | **Imported profiles are untrusted.** They arrive disabled and without consent; each launch target and injecting rule is reviewed before it can be enabled. | P0 | D4 | T |
| DDALGAK-SEC-014 | **No network.** Ddalgak opens no network connection; "open a website" only hands the URL to the default browser. | P0 | D2 | I, T |
