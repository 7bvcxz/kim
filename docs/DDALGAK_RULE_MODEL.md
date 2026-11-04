# Ddalgak rule model

Status: **current** · Rules document **v1** · Step 65 (specification; implemented from Steps 66–67
on, see §14) · Decision: [ADR 0022](adr/0022-ddalgak-rule-model-and-safety.md) · Schemas:
[ddalgak-rules-v1](contracts/schemas/ddalgak-rules-v1.schema.json),
[ddalgak-simulation-v1](contracts/schemas/ddalgak-simulation-v1.schema.json) · Requirements:
`DDALGAK-FR-005…042`, `DDALGAK-SEC-001…013`

This document defines what a Ddalgak rule is: its trigger, the seven core action types, modes,
conditions, profiles, consent, precedence, validation, the test playground, runtime outcomes and
the stored, versioned format. It builds on the [hotkey contract](HOTKEY_CONTRACT.md) (key
syntax, reserved keys, conflict checks) and the [CLI contract](CLI_CONTRACT.md). The safety
reasoning behind many rules is in [DDALGAK_SAFETY](DDALGAK_SAFETY.md).

Words: **MUST**/**MUST NOT** are requirements; **SHOULD** is a strong default that needs a
written reason to break. Windows behavior from Microsoft documentation is marked *documented*.
Nothing in this document has been run against real Windows input yet (§14).

## 1. Overview and terms

| Term | Meaning |
| --- | --- |
| **Rule** | *When* a trigger happens (in these modes, under these conditions) *do* these steps. Identified by a stable `id`. |
| **Trigger** | The keys that start a rule, with how they fire (`press`, `release`, `hold`) and options (§3). |
| **Step** | One action in a rule's ordered list. A rule with one step is what the GUI calls "the action". |
| **Action type** | The kind of a step: `remap`, `block`, `send_keys`, `type_text`, `open_program`, `open_url`, `mouse_move`, `mouse_scroll`, `mouse_click`, `mode`, `engine`, `delay` (§5). |
| **Mode** | A user-defined named state of a profile (Base, Navigation, …). Exactly one mode is active; rules may be limited to modes (§7.3). |
| **Profile** | A complete, switchable set of modes and rules ("Work", "Gaming"). Exactly one profile is active. |
| **Engine** | The part of `ddalgak-gui.exe` that owns the keyboard hook and runs rules: `off`, `running` or `paused` (`DDALGAK-FR-002`). |
| **Run** | One execution of a rule's steps after its trigger fired. |
| **Consent** | The user's explicit permission, recorded on a rule, for a powerful effect (§6). |
| **Injected input** | Keyboard or mouse events synthesized by a program (`SendInput`) instead of a device. Windows flags them for hooks (`LLKHF_INJECTED`, *documented*). |

Three documents use this model:

| Document | Where | Schema |
| --- | --- | --- |
| Rules file | `%APPDATA%\mouv\ddalgak\rules.json` | `ddalgak.rules` v1 |
| Export | any file the user chooses | `ddalgak.rules` v1 with an `exported` block (§10) |
| Playground result | GUI, `ddalgak test --json`, `ddalgak explain --json` | `ddalgak.simulation` v1 (§11) |

Ddalgak's own GUI shortcuts (turn the engine on, open the editor, …) are **not** rules. They are
hotkey-contract actions stored in `hotkeys.json`
([manifest](contracts/examples/kim-hotkey-manifest-v1/ddalgak.json), [HOTKEY_CONTRACT §13.4](HOTKEY_CONTRACT.md)).

## 2. The rules file

```json
{
  "schema": "ddalgak.rules", "schema_version": 1, "hotkey_contract": 1,
  "revision": 7, "active_profile": "default",
  "settings": { "engine_on_start": "off", "activity": { "persist": false }, "max_runs_per_second": 20 },
  "profiles": [ {
    "id": "default", "title": "Everyday", "default_mode": "base",
    "modes": [ { "id": "base", "title": "Base" } ],
    "rules": [ {
      "id": "caps-to-ctrl", "title": "CapsLock acts as Ctrl, a tap is Escape", "enabled": true,
      "trigger": { "keys": "CapsLock" },
      "steps": [ { "type": "remap", "to": "LCtrl", "tap": "Escape", "tap_ms": 200 } ],
      "consent": { "input-injection": { "granted_at": "2026-11-04T09:00:00Z", "via": "gui" } }
    } ]
  } ]
}
```

| Field | Rule |
| --- | --- |
| `revision` | Incremented by every write. A writer passes the revision it read; if the file changed meanwhile the write fails with `rule_conflict` (exit 7) and nothing is written (`DDALGAK-FR-005`). |
| `active_profile` | Must name a profile of the document (`unknown_profile`). |
| `settings.engine_on_start` | `off` (default) or `on`. `on` is ignored after an unclean exit or an emergency off (`DDALGAK-FR-003`). |
| `settings.activity` | `persist: false` (default) keeps the activity list in memory only; `true` keeps it in a capped local file (§12). |
| `settings.max_runs_per_second` | Per-rule rate limit, default 20, at most 100 (§7.4). |
| `profiles[]` | 1–64 profiles, each with a `default_mode`, 1–32 `modes` and up to 1000 `rules`. |

**Storage.** Paths come from the `Paths` interface ([ADR 0011](adr/0011-portability-boundary.md));
Linux (D7) uses `$XDG_CONFIG_HOME/mouv/ddalgak/rules.json`. Writes are atomic (temporary file,
flush, replace; [ADR 0006](adr/0006-state-and-configuration-storage.md)). Before every write the
previous version goes to `rules.json.bak`; applied changes are also kept as snapshots in
`history/` (§10). The folder is in the user's profile and inherits its per-user ACL
(`DDALGAK-SEC-011`).

**Loading.** A missing file means one empty profile `default` with mode `base`. A file that
fails JSON parsing or the schema is never loaded: the engine cannot be turned on with it, a
running engine keeps its previous rules, the GUI and `ddalgak status` say why (`rules_invalid`),
and the file is never rewritten automatically. A newer `schema_version` is read-only
(`rules_from_newer_version`). A schema-valid file with semantic errors (§8) loads; the affected
rules are inactive and listed with their codes.

## 3. Triggers

```json
{ "keys": "Ctrl+Shift+J", "on": "press", "repeat": false, "pass_through": false, "injected": "ignore" }
```

### 3.1 Key syntax

Rule triggers are global and use the canonical syntax of [HOTKEY_CONTRACT §5](HOTKEY_CONTRACT.md)
(the `hook` backend's capabilities: left/right modifiers, physical `code:` keys, sequences, key
release and hold), with these differences:

| Difference | Why |
| --- | --- |
| **Lock keys** `CapsLock` and `NumLock` are keys, alone or with modifiers (`Shift+CapsLock`). | The contract leaves them to Ddalgak (§5.2 there); CapsLock is the most remapped key there is. |
| **Bare side-specific modifiers** `LCtrl`, `RCtrl`, `LAlt`, `RAlt`, `LShift`, `RShift` are single-step triggers. | Right Alt as 한/영, a tap of Right Ctrl. Only with `remap` or `on: release` (§3.3). |
| **No `char:` keys.** | Translating keys to characters inside the hook changes dead-key state in the focused application. Hotstrings (Step 69) get their own trigger kind. |
| **No leaders (`@…`).** | Rules have no prefix tables; a sequence such as `Ctrl+K Ctrl+U` is written out. |
| Typing and navigation keys (`A`, `Space`, `Escape`, `code:KeyH`) are allowed. | `kim_reserved` (contract §7.2) protects Kim GUIs' own controls; remapping plain keys is Ddalgak's purpose. `Alt+F4` and the emergency chord stay reserved. |

**Reserved for triggers** (schema `trigger_keys`, `DDALGAK-SEC-009`): every chord the contract
reserves in all scopes (Secure Attention Sequence, any `Win` chord, `Ctrl+Shift+Escape`,
`Ctrl+Escape`, `Alt+Tab` variants, `Alt+Escape`, `Alt+Shift+PrintScreen`, `Alt+F4`, the IME mode
keys) plus the emergency chord `Ctrl+Alt+Shift+Escape` with any sides, plus `F12` with any
modifiers (reserved in global scope). The schema keeps its `reserved_free` identical to the
hotkey schemas'; `schema-check` enforces that. A bare `LWin`/`RWin` is not in the trigger syntax:
`Win` triggers are not in v1 ([ADR 0022](adr/0022-ddalgak-rule-model-and-safety.md)).

**Warnings** (the rule stays active, the GUI shows the warning with its text): `altgr_collision`
for `Ctrl+Alt` with a letter, digit or punctuation key; `ime_toggle` for `Ctrl+Space`,
`Shift+Space`, `Alt+code:Backquote`; `os_shortcut` for `PrintScreen`, `Alt+PrintScreen`,
`Alt+Space`; `hook_overrides_global` when another Kim tool registers the same global chord
(contract §9.3).

### 3.2 Matching

- A chord matches when its key matches and exactly its modifiers are down (contract §5.5).
  Generic modifiers accept either side; `LCtrl`/`RCtrl` need that side.
- **A trigger without modifiers used by `remap` or `block` matches with any modifiers held**, and
  the held modifiers combine with the output (Shift+CapsLock with CapsLock → Escape gives
  Shift+Escape). Other action types match exactly.
- **Reserved chords win over that widening.** When the held modifiers and the key form a chord
  reserved for triggers (§3.1), no rule matches and the event passes unchanged: with `Tab`
  remapped, `Alt+Tab` still switches windows; with `Escape` remapped, `Ctrl+Shift+Escape` still
  opens Task Manager; with `F4` blocked, `Alt+F4` still closes the window.
- Logical keys (`J`) follow the active layout's virtual keys; physical keys (`code:KeyJ`) follow
  the scan code position; both as in contract §6.
- **Consumed down, consumed up.** When a rule consumes a key-down, its key-up is consumed too;
  when a key-down passed through, its key-up passes. Applications never see an unmatched key-up
  or key-down (`DDALGAK-SEC-005`).

### 3.3 Fields

| Field | Values | Rule |
| --- | --- | --- |
| `keys` | trigger syntax | Required. Canonical form only; the GUI and CLI accept the contract's aliases and store the canonical form. |
| `on` | `press` (default), `release`, `hold` | `release`: the chord was released with no other key in between (a tap); for a bare modifier it is the only way to fire a non-remap rule, so the modifier still works in chords. `hold`: held for `hold_ms` (200–5000, default 500). Sequences fire on `press` only. |
| `repeat` | `false` (default) | `true` (with `press`): auto-repeat fires the rule again. |
| `pass_through` | `false` (default) | `true`: the original key also reaches the application (like AutoHotkey's `~`). Not allowed with steps that last while the trigger is held. |
| `injected` | `ignore` (default), `accept` | `accept`: input injected by *other* programs (an on-screen keyboard, remote-control software) may trigger the rule. Ddalgak's own injected input never triggers any rule. A rule with `accept` may not contain injecting steps (§7.4). |

**Steps that last while the trigger is held** (`remap`, `mode` with `op: hold`, `mouse_click`
with `op: hold`, `mouse_move` with `while_held`) are the rule's only step, on a single chord with
`on: press`, no `repeat` and no `pass_through` (schema `holds_trigger`).

## 4. Rules

| Field | Rule |
| --- | --- |
| `id` | `^[a-z][a-z0-9-]{1,47}$`, unique in the document (`duplicate_rule_id`), never reused. It forms the hotkey action ID `rule.<id>`. Generated IDs are `r-<uuid>`; the GUI shows titles. |
| `title`, `description` | 1–80 and up to 500 characters. Shown in the list, the tray menu and conflict reports. |
| `enabled` | Required. A disabled rule is kept, listed and never runs. |
| `trigger` | §3. |
| `modes` | Optional list of mode IDs of the same profile (`unknown_mode`). Omitted: active in every mode. |
| `when` | Optional conditions (§7.2). Evaluated from Step 77; until then a rule with `when` is **inactive** (`condition_unsupported`), never unconditional (`DDALGAK-FR-021`). |
| `priority` | −1000…1000, default 0 (§7.1). |
| `steps` | 1–32 steps run in order (§5). Total `delay` at most 60 s (`run_too_long`). |
| `on_error` | `stop` (default): a failing step ends the run. `continue`: later steps still run. |
| `consent` | Grants per permission (§6). Required for an *enabled* rule with powerful steps. |
| `created_at`, `updated_at` | Optional timestamps, written by the GUI and CLI. |

## 5. Action types

Each action type lists its **inputs** (the step's fields), its **behavior**, its **outputs** (the
playground preview of §11 and, live, what reaches the system), its **errors** (validation and
runtime codes) and its **safety** rules. Permissions are those of §6. Every injecting action
uses `SendInput` and tags its events with Ddalgak's per-process marker in `dwExtraInfo`, so the
hook recognizes and ignores its own input (§7.4).

### 5.1 `remap`: a key acts as another key or chord

| Input | Values |
| --- | --- |
| `to` | Required. One output key or chord: a named or `code:` key, optionally with modifiers (`Ctrl+C`), or a bare modifier including `LWin`/`RWin`. IME keys are allowed (`HangulMode`). Never the Secure Attention Sequence or the emergency chord. |
| `tap` | Optional dual role: a press and release of the trigger alone within `tap_ms` sends this key or chord instead. |
| `tap_ms` | 100–1000, default 200; only with `tap`. |

- **Behavior.** When the trigger goes down, the target goes down; when the trigger goes up, the
  target goes up. Auto-repeat of the trigger repeats the target's key-down. If the trigger has
  modifiers (`Ctrl+Shift+J` → `Down`), those modifiers are logically released while the target
  is held and restored afterwards if still physically down. A plain-key trigger keeps the other
  held modifiers (§3.2). With `tap`, the target is pressed as soon as another key is pressed
  during the hold or `tap_ms` passes; otherwise the release sends the `tap` output.
- **Outputs.** Preview `{ "holds": "LCtrl", "tap": "Escape", "released_modifiers": [...] }`. Live:
  key events for the target only.
- **Errors.** Validation: `output_is_trigger` (`F13` → `F13`), `output_reserved`. Runtime:
  `skipped_elevated_target`, `skipped_password_field`, `injection_failed`.
- **Safety.** `input-injection`. The target is released on every path of `DDALGAK-SEC-005`.
  Remapping keys never hides the emergency chord: it is detected on the physical keys first
  (`DDALGAK-SEC-003`).

### 5.2 `block`: disable a key or chord

No inputs. The trigger is consumed and nothing happens (AutoHotkey's `Key::return`). Preview
`{}`. Not injecting, needs no consent. Blocking a key never blocks the emergency chord.

### 5.3 `send_keys`: press a combination or sequence

| Input | Values |
| --- | --- |
| `keys` | Required. One to eight output steps separated by spaces (`Ctrl+K Ctrl+V`, `Win+D`), each tapped in order. Never the Secure Attention Sequence (Windows does not allow synthesizing it, *documented* for AutoHotkey's `Send`) or the emergency chord. |
| `step_delay_ms` | 0–1000 between steps, default 0. |

- **Behavior.** Before tapping, modifiers of the trigger that are physically held are logically
  released so that `Ctrl+Shift+F1` → `Ctrl+C` really sends Ctrl+C; they are restored afterwards
  if still held. `SendInput` inserts the events serially, not interleaved with user input
  (*documented*).
- **Outputs.** Preview `{ "keys": "Ctrl+K Ctrl+V" }`.
- **Errors.** `output_is_trigger`, `output_reserved`; runtime `skipped_*`, `injection_failed`.
- **Safety.** `input-injection`.

### 5.4 `type_text`: type a Unicode string

| Input | Values |
| --- | --- |
| `text` | Required, 1–4096 characters. `\n` types Enter and `\t` types Tab; other control characters are refused. Hangul, emoji and characters outside the Basic Multilingual Plane are allowed. |
| `method` | `unicode` (default): one Unicode key event per UTF-16 unit (`KEYEVENTF_UNICODE`), independent of layout and IME. `paste`: put the text on the clipboard, send `Ctrl+V`, restore the previous clipboard contents. |
| `key_delay_ms` | 0–200 between characters, default 0, for applications that drop fast input. |

- **Behavior.** Before typing, held trigger modifiers are logically released (as for
  `send_keys`). Typing pauses the user's own keystrokes only for the duration of one `SendInput`
  call (*documented* serial insertion). An IME composition in progress is never committed or
  cancelled by Ddalgak; the rule is skipped with `skipped_ime_composition`.
- **Outputs.** Preview `{ "characters": 13, "lines": 2, "method": "unicode" }`: counts only. The
  text appears in the playground's own field, never in JSON output or logs (`DDALGAK-SEC-011`).
- **Errors.** Runtime `skipped_password_field` (never typed into a password field,
  `DDALGAK-SEC-002`), `skipped_elevated_target`, `clipboard_busy` (paste: another program holds
  the clipboard), `injection_failed`.
- **Safety.** `input-injection`, plus `clipboard` for `paste`. The rules file stores the text in
  plain form: it is for snippets, not for passwords, and the GUI says so.

### 5.5 `open_program`: open an executable

| Input | Values |
| --- | --- |
| `path` | An absolute path to an `.exe`, on a drive (`C:\…`) or under a known-folder token: `%ProgramFiles%`, `%ProgramFiles(x86)%`, `%LocalAppData%`, `%AppData%`, `%UserProfile%`, `%SystemRoot%`. No other environment variables, no relative paths, no documents, no shortcuts. |
| `app_id` | Instead of `path`: the AppUserModelID of a packaged app (`Microsoft.WindowsCalculator_8wekyb3d8bbwe!App`). No `args` or `working_dir` with it. |
| `args` | Up to 32 separate arguments, each quoted by Ddalgak by the Windows command-line rules. Never one shell string. |
| `working_dir` | Absolute folder (same tokens); default: the program's folder. |
| `window` | `normal` (default), `maximized`, `minimized`. |
| `if_running` | `start_new` (default) or `activate`: bring an existing window of the same program to the front instead. |

- **Behavior.** `CreateProcessW` with the resolved path and the quoted argument list, as the
  current user, without shell verbs; packaged apps through the shell's activation API. A
  program whose manifest requires elevation fails with `program_requires_elevation`; Ddalgak
  never asks for elevation (`DDALGAK-SEC-007`, `KIM-SEC-004`).
- **Outputs.** Preview `{ "program": "notepad.exe", "found": true, "arguments": 0 }`: file name,
  whether it exists, the number of arguments. Never the full path or the arguments.
- **Errors.** Validation (schema): relative path, non-`.exe`, command interpreters and script
  hosts (`cmd.exe`, `powershell.exe`, `pwsh.exe`, `wscript.exe`, `cscript.exe`, `mshta.exe`,
  `rundll32.exe`, `regsvr32.exe`, `wsl.exe`, `bash.exe`, `conhost.exe`), which belong to Step 79's
  separately consented command action. Semantic: `consent_stale` when the target changed after
  consent. Runtime: `program_not_found`, `program_requires_elevation`, `launch_failed`.
- **Safety.** `launch-programs` with a fingerprint (§6.2). An imported rule arrives disabled and
  without consent (`DDALGAK-SEC-013`).

### 5.6 `open_url`: open a website

| Input | Values |
| --- | --- |
| `url` | Required. `http://` or `https://`, a host, optional port, path, query and fragment; no spaces, no user name or password; at most 2048 characters. |

- **Behavior.** The URL is handed to the user's default browser through the shell's `open`
  association for `http`/`https` only. Ddalgak itself makes no network connection
  (`DDALGAK-SEC-014`).
- **Outputs.** Preview `{ "scheme": "https", "host": "github.com" }`. The path and query are never
  shown or logged: they can carry tokens.
- **Errors.** Validation: other schemes (`file:`, `javascript:`, `ms-settings:`, custom
  protocols) and credentials. Runtime: `browser_unavailable`, `launch_failed`.
- **Safety.** `launch-programs` with a fingerprint (§6.2).

### 5.7 `mouse_move`

| Input | Values |
| --- | --- |
| `mode` | `relative` (with `dx`, `dy`, each −10000…10000 logical pixels) or `absolute` (with `to`). |
| `to` | `{ "x", "y", "relative_to" }`; `relative_to` is `virtual_screen` (default; all monitors, may be negative), `primary_monitor` or `active_window`. |
| `while_held` | Relative only: `{ "speed": 50–5000 px/s, "acceleration": 0–10 }`. The pointer moves in the `dx`/`dy` direction for as long as the trigger is held. |

- **Behavior.** Coordinates are logical pixels scaled by the target monitor's DPI. Targets
  outside the visible desktop are clamped to its edge (`clamped: true`). Continuous movement
  stops on release, pause, turn-off and emergency.
- **Outputs.** Preview `{ "from", "to", "clamped", "while_held", "speed" }`.
- **Errors.** Runtime `skipped_elevated_target` (pointer injection over an elevated window is
  blocked by UIPI), `injection_failed`.
- **Safety.** `input-injection`. No pointer tracking: Ddalgak reads the pointer position only
  when a rule runs.

### 5.8 `mouse_scroll`

`direction` (`up`, `down`, `left`, `right`) and `notches` (1–50, default 1; one notch is 120
wheel units). Preview `{ "direction", "notches" }`. `input-injection`.

### 5.9 `mouse_click`

| Input | Values |
| --- | --- |
| `button` | `left`, `right`, `middle`, `x1`, `x2`. `left` and `right` are the logical primary and secondary buttons (they follow the "swap buttons" setting). |
| `op` | `click` (default), `double_click` (two clicks within the system double-click time) or `hold` (the button is down exactly as long as the trigger). |
| `at` | Optional point (as in §5.7) to move to first. |

Preview `{ "button", "op", "at" }`. A held button is released on every path of
`DDALGAK-SEC-005`. There is no bare "button down" without "up": `hold` is the only way to keep a
button pressed. `input-injection`.

### 5.10 `mode`: switch or toggle a user-defined mode

| `op` | Inputs | Effect |
| --- | --- | --- |
| `set` | `mode` | The mode becomes active. |
| `toggle` | `mode` | If it is active, return to the previous mode; otherwise make it active. |
| `cycle` | `modes` (2–32) | The next mode in the list after the active one (the first if the active one is not in the list). |
| `hold` | `mode` | Active while the trigger is held, then the previous mode returns (a temporary layer). |
| `previous` | — | Back to the mode that was active before the last change. |
| `default` | — | Back to the profile's `default_mode`. |

- Mode IDs must exist in the rule's profile (`unknown_mode`). Mode changes never inject input
  and need no consent. Keys held when the mode changes stay with the rule that pressed them
  until they are released.
- **Outputs.** Preview `{ "from", "to", "until_release" }`; the mode indicator and
  `ddalgak status` show the new mode.
- The active mode is runtime state, not stored in `rules.json`. At engine start, after a pause,
  a profile switch or an emergency off the profile's `default_mode` is active (Step 72 may add
  an opt-in "restore last mode").

### 5.11 `engine`: pause or turn off Ddalgak

| `op` | Effect |
| --- | --- |
| `pause` | All rules stop except engine-control rules; held keys are released; the hook stays. |
| `resume` | Rules run again. |
| `toggle_pause` | Pause if running, resume if paused. |
| `disable` | The engine turns off: the hook is removed, held keys are released, runs are cancelled. |

**Engine-control rules**, whose steps are all `engine` steps, keep working while paused (like
AutoHotkey's `#SuspendExempt`), so a paused engine can still be resumed or turned off by key. **No rule turns the engine on**: without the hook no rule can
see a key. Turning on is done from the tray, the GUI, the `engine.toggle` shortcut (registered
with `RegisterHotKey`, independent of the hook) or the CLI (`DDALGAK-FR-004`). Preview
`{ "from", "to" }`. No consent needed.

### 5.12 `delay` and ordered steps

`delay` waits `ms` (1–10000). A rule's steps run one after another on the engine's worker
thread, never in the hook callback. A run ends when its last step finishes, a step fails with
`on_error: stop`, or it is cancelled (pause, turn-off, emergency, profile switch). Delays of one
run total at most 60 s. A trigger of a rule that is still running is dropped with `busy` (one run
per rule, like AutoHotkey's default of one thread per hotkey).

### 5.13 Future step types

These names are reserved for additive versions of the schema. A v1 reader keeps such a step but
the rule is inactive (`step_unsupported`, §9).

| Type | Planned in | Permission |
| --- | --- | --- |
| `window` (activate, minimize, maximize, move, resize, always on top) | Step 79 | `window-control` |
| `clipboard` (set, paste, transform) | Step 79 | `clipboard` |
| `run_command` (an allowed local command with an explicit argument list) | Step 79 | its own consent per command |
| `notify` (a local notification) | Step 79 | — |
| `repeat`, `if`, variables | Step 78 | — |
| Recorded macros (steps produced by the recorder) | Step 78 | as their steps |

Trigger kinds planned additively: hotstrings (Step 69) and mouse buttons (Step 68, needs a mouse
hook that a catalog update discloses first).

## 6. Permissions and consent

### 6.1 Permissions

The IDs are the Kim catalog's permission IDs ([CATALOG_FORMAT §2.1](CATALOG_FORMAT.md)), which Kim
discloses before installing Ddalgak.

| Permission | Needed by | Recorded |
| --- | --- | --- |
| `keyboard-hook` | the engine itself | Implicit in turning the engine on; never per rule |
| `input-injection` | `remap`, `send_keys`, `type_text`, `mouse_move`, `mouse_scroll`, `mouse_click` | Per rule |
| `launch-programs` | `open_program`, `open_url` | Per rule, with a fingerprint of the exact targets |
| `clipboard` | `type_text` with `method: paste` | Per rule |
| `window-control` | future `window` steps | Per rule |

The schema rejects an **enabled** rule that lacks a consent it needs. A disabled rule may lack
consent: that is the state of every imported rule until the user reviews it.

### 6.2 Launch fingerprints

`launch-programs` consent carries `fingerprint`: `sha256:` followed by the lowercase hex SHA-256
of the rule's `open_program` and `open_url` steps, in order, as a JSON array in RFC 8785
canonical form (keys sorted, no whitespace). Changing a path, an argument, a working folder or
a URL changes the fingerprint; the rule becomes inactive (`consent_stale`) until the user
approves the new target. `cargo xtask schema-check` recomputes every fingerprint in the examples.

### 6.3 Granting and withdrawing

- **GUI**: the first time a rule needs a permission, a dialog explains it in one sentence ("Let
  this rule type text into other programs?"), names the target for launches, and offers
  **Allow** or **Cancel** (Cancel is the default focus). Allow records `via: "gui"`.
- **CLI**: `ddalgak rule add … --allow input-injection --allow launch-programs`; without the flag
  the rule is saved disabled and the command says which `--allow` it needs (`consent_required`,
  exit 7). Records `via: "cli"`.
- **Never**: by import, by an MCP client, by a migration, or by copying a rule (duplicating a
  rule copies its consent only for unchanged targets).
- **Withdraw**: `ddalgak rule revoke <id> <permission>` or the rule's permission chips in the GUI.
  The rule stays, inactive.

## 7. Selection and precedence

### 7.1 Pipeline for one input event

1. **Emergency chord** on physical key state: always first, never configurable
   (`DDALGAK-SEC-003`).
2. **Own input**: events carrying Ddalgak's marker pass through untouched.
3. **Stepping aside**: secure desktop, an elevated foreground window (for injecting rules), a
   password field, an IME composition. The affected rules are skipped and the key passes
   unchanged (`skipped_*`).
4. **Paused**: only engine-control rules remain.
5. **Candidates**: enabled, valid rules of the active profile whose trigger matches, whose
   `modes` include the active mode (or that have none), and whose `when` holds.
6. **Precedence** among candidates, first difference wins:
   1. mode-scoped rules before rules without `modes`;
   2. rules with `when.apps` before rules without;
   3. rules with `when.window_title` before rules without;
   4. higher `priority`;
   5. earlier in the profile's list.
7. The winner runs; the key is consumed unless `pass_through`. Without a candidate the key
   reaches the application unchanged (the fallthrough).

Two enabled rules with the same trigger, the same `modes`, the same `when` and the same
`priority` are a `duplicate_trigger` error (the later one is inactive), so step 6.5 only decides
between rules whose conditions differ but both hold at that moment. The playground and
`ddalgak explain` show the full candidate list with each result (`DDALGAK-FR-023`).

### 7.2 Conditions (`when`)

| Field | Matches |
| --- | --- |
| `apps` | The focused window's process: `{ "exe": "Code.exe" }` (file name, case-insensitive) or `{ "path": "C:\\…\\Code.exe" }`; any entry matches. |
| `except_apps` | Same matchers; the rule does not apply if any matches. |
| `window_title` | `{ "contains": "…" }` or `{ "equals": "…" }`, case-insensitive. |

Conditions are read at trigger time only, from the foreground window; titles are never logged
or stored (`DDALGAK-SEC-011`). Windows of elevated processes and the secure desktop never match.

### 7.3 Modes

Exactly one mode of the active profile is active. Rules without `modes` work in every mode;
mode-scoped rules beat them on the same trigger (§7.1). Keys without a matching rule reach the
application in every mode. An option to swallow unbound typing keys in a mode is left to Step 72
and needs a guaranteed way out of that mode.

### 7.4 Recursion guard

1. **Own input is invisible to rules.** Every event Ddalgak injects carries its marker in
   `dwExtraInfo` and has `LLKHF_INJECTED` set (*documented*); the hook passes such events on
   without matching. Two remaps pointing at each other (CapsLock → Escape, Escape → CapsLock)
   are therefore a swap, not a loop (example `01-remap.json`, profile `swap`).
2. **`injected: accept` rules never inject.** Accepting other programs' synthetic input is safe
   only for rules that cannot produce input themselves; the schema enforces it.
3. **`output_is_trigger`.** A `remap` or `send_keys` whose output equals its own trigger is
   refused: it does nothing in Ddalgak and is the classic infinite loop elsewhere.
4. **Rate limit.** A rule that fires more than `max_runs_per_second` times in one second is
   dropped with `rate_limited`, which breaks loops through other automation tools.

### 7.5 Interplay with the hotkey contract

- Each enabled rule's trigger appears in `ddalgak hotkeys list --json` as a read-only action
  `rule.<id>` (scope `global`, `source: "user"`). `kim hotkeys check --all-tools` then reports
  rule triggers that equal another Kim tool's global chord as `hook_overrides_global`
  (contract §9.3, §13.4). The `rule.` prefix is reserved for this; Ddalgak's GUI actions use
  `rules.*`, `engine.*`, `editor.*` and `playground.*`.
- A rule trigger equal to one of Ddalgak's own bound GUI shortcuts is a `duplicate_binding`
  conflict; the shortcut wins and the rule is inactive.
- Under `KIM-SEC-020` the hook passes on chords that another program registered globally unless a
  rule explicitly uses that chord.

## 8. Validation

`ddalgak rule check` (and the GUI's problem badges) report:

| Code | Severity | Found by | When |
| --- | --- | --- | --- |
| `rules_invalid` | error (file) | schema | The document fails the schema; nothing loads (§2). |
| `trigger_reserved` | error | schema | A reserved trigger (§3.1). CLI exit: `hotkey_reserved` (2). |
| `trigger_invalid` | error | schema | Not the trigger syntax. CLI exit: `hotkey_invalid` (2). |
| `output_reserved` | error | schema | An output contains the Secure Attention Sequence or the emergency chord. |
| `consent_missing` | error | schema | An enabled rule lacks a consent of §6.1. |
| `recursion_risk` | error | schema | `injected: accept` with an injecting step (§7.4). |
| `step_invalid` | error | schema | A step's inputs break §5 (relative path, shell host, URL scheme, missing mode target, …). |
| `duplicate_profile_id`, `duplicate_mode_id`, `duplicate_rule_id` | error | semantic | An ID appears twice. |
| `unknown_profile`, `unknown_mode` | error | semantic | A reference to a profile or mode that does not exist. |
| `output_is_trigger` | error | semantic | §7.4 3. |
| `duplicate_trigger` | error | semantic | §7.1. The later rule is the subject. |
| `consent_stale` | error | semantic | A launch fingerprint no longer matches (§6.2). |
| `run_too_long` | error | semantic | Delays of one run exceed 60 s. |
| `step_unsupported`, `condition_unsupported`, `rule_unsupported` | info | runtime | Inactive in this version: an unknown step type, a condition not evaluated yet, or an unknown field inside the rule (§9). |
| `altgr_collision`, `ime_toggle`, `os_shortcut`, `hook_overrides_global` | warning | semantic | §3.1. |

Errors make the rule inactive; warnings and infos do not change behavior. Like hotkey conflicts,
reports come with non-destructive offers (change the trigger, disable, keep for warnings) that
are never applied automatically. The **semantic** rules are implemented for the fixtures by
`xtask/src/ddalgak.rs`; Step 67 implements them in Ddalgak and must agree with the same files.

## 9. Forward compatibility, versions and migrations

- **Unknown fields are preserved.** Ddalgak keeps fields it does not know when it rewrites the
  file, at every level.
- **Never broader.** An unknown step type, or an unknown field inside a rule's `trigger`, `when`
  or a step, makes that rule inactive (`step_unsupported` or `rule_unsupported`) rather than
  ignored, because an unknown field may restrict when or how the rule acts. Unknown fields at the
  document, settings, profile or mode level are ignored.
- **Additive changes keep v1**: new step types, trigger kinds, condition fields, settings and
  codes. Readers treat unknown codes by severity.
- **Incompatible changes** bump `schema_version` and add a pure migration `vN → vN+1`. The first
  migrated write keeps `rules.json.bak` and a history snapshot; an older Ddalgak never rewrites a
  newer file. A change of meaning to an existing field is never additive.

## 10. Profiles, export, import and history

- **Switch.** `ddalgak profile apply <id>` or the profile menu: runs are cancelled, held keys
  released, the new profile's `default_mode` becomes active, then its rules apply. Nothing of
  the old profile fires after the switch (`DDALGAK-FR-030`).
- **History.** Each applied change writes a snapshot (`history/<UTC time>-r<revision>.json`, the
  last 50 kept). Restore applies a snapshot as a new change with a new revision; it is itself
  undoable (`DDALGAK-FR-031`).
- **Export** writes a `ddalgak.rules` document with an `exported` block (`exported_at`,
  `by_version`), the chosen profiles (or rules wrapped in one profile), **without** `consent`,
  `settings` or `revision` (`09-export.json`).
- **Import** validates the file, shows a diff against the current rules, renames clashing IDs
  (`<id>-2`), and adds the profiles with every rule `enabled: false` and no consent. It never
  changes `active_profile` or `settings`. Enabling an imported injecting or launching rule goes
  through the consent of §6.3, showing its exact targets (`DDALGAK-SEC-013`).

## 11. Test playground and simulation

The playground is where users try rules safely ([GUI_JOURNEYS J5](DDALGAK_GUI_JOURNEYS.md)).
The same logic answers `ddalgak test --dry-run --json` and `ddalgak explain --json`.

**Inputs**

| Input | Values |
| --- | --- |
| profile, revision | A saved profile and its revision, or an unsaved draft (`revision: null`). |
| context | `mode`; `engine` (`running` or `paused`); `focus` (`normal`, `password_field`, `elevated_window`, `secure_desktop`); `app` (`exe`, optional `window_title`, or `null`); `ime_composing`. Chosen by the user, never read from other windows in simulation. |
| event | `keys` (trigger syntax), `on` (`press`, `release`, `hold`, `repeat`), `source` (`physical` or `injected_by_other_program`). |

**Outputs** ([ddalgak-simulation-v1](contracts/schemas/ddalgak-simulation-v1.schema.json)):
`decision` (`outcome` `fired`/`skipped`/`no_match`, `code`, `rule`, `key_reaches_app`,
`candidates` with `winner`/`lower_precedence`/`mode_mismatch`/`condition_false`/`inactive`), one
`effect` per step with a type-specific `preview` (§5), `injected_events` and `opened`.

| | `simulate` (default) | `live` |
| --- | --- | --- |
| Input injected | none (`injected_events: 0`, every `performed: false`) | only into the playground's own test field (`live_target: "playground"`) |
| Programs and websites | never opened (`opened: []`) | each opened only after its own confirmation (`confirmed: true`); otherwise `needs_confirmation` |
| Modes and engine state | simulated; the real engine is untouched | simulated; the real engine is untouched |
| Personal content | summarized (§5 previews) | summarized; the typed text is visible in the test field only |

**Reset** clears the test field, the simulated mode and engine state, and cancels a live run;
active rules and the real engine are never affected (`DDALGAK-FR-041`). The schema itself
enforces the table's safety properties: a `simulate` result with a performed effect, an
injected event or an opened program is invalid, as is a live launch without confirmation or a
live target other than the playground. Twenty-three examples cover all seven action categories,
stepping aside, pausing, precedence and live tests; `schema-check` also proves that each traces
rules that exist in the rules example it names.

## 12. Runtime outcomes, errors and activity

Every trigger event that reaches a candidate produces one outcome:

| Code | Meaning |
| --- | --- |
| `ok` | The run finished. |
| `skipped_paused` | The engine is paused. |
| `skipped_password_field`, `skipped_secure_desktop`, `skipped_elevated_target`, `skipped_ime_composition` | Ddalgak stepped aside; the key passed unchanged. |
| `skipped_injected_input` | Injected input and the rule accepts only physical input. |
| `busy` | The rule is still running. |
| `rate_limited` | §7.4 4. |
| `injection_failed` | `SendInput` inserted fewer events than requested. UIPI failures are silent (*documented*), so the elevated-target check runs first. |
| `program_not_found`, `program_requires_elevation`, `launch_failed`, `browser_unavailable`, `clipboard_busy` | §5. |
| `cancelled` | Pause, turn-off, emergency or a profile switch ended the run. |
| `timeout` | A step did not finish in its time limit. |

**Activity (audit).** The app keeps the last `max_entries` (default 200, in memory) outcomes:
`{ "at", "rule", "profile", "mode", "outcome", "step", "duration_ms" }`. Entries never contain
keys, typed text, window titles, paths, arguments or URLs (`DDALGAK-SEC-001`, `DDALGAK-SEC-011`).
With `activity.persist: true` they are also appended to a capped, rotated local file. Changes to
rules are audited by the history snapshots of §10, each recording the revision, the time and
the origin (`gui`, `cli`, `import`, `restore`).

## 13. CLI surface (preview)

The command list Steps 67 and 81 implement under the CLI contract (`--json`, `--dry-run`,
`--yes`, exit codes). Names may still be refined there; behavior may not get weaker.

| Command | Effect |
| --- | --- |
| `ddalgak rule list [--profile p] [--mode m]` · `rule show <id>` · `rule check` | read |
| `ddalgak rule add --trigger <keys> --action <type> [inputs] [--allow <permission>]…` · `rule edit <id> …` · `rule duplicate <id>` · `rule enable/disable <id>…` · `rule move <id> --before <id>` | write, `--dry-run` |
| `ddalgak rule delete <id>…` | destructive: `--dry-run`, `--yes` |
| `ddalgak rule revoke <id> <permission>` | write |
| `ddalgak profile list/show/add/rename/duplicate/delete/apply/history/restore/export/import` | read / write / destructive as named |
| `ddalgak mode list/add/rename/delete/set` | read / write (`mode set` asks the running app) |
| `ddalgak test --keys <keys> [--profile p] [--mode m] [--app exe] [--focus kind] --dry-run` | read: a `ddalgak.simulation` result; there is no headless live test |
| `ddalgak explain <keys> [context flags]` | read: the decision and candidates only |
| `ddalgak status` | read: engine state, active profile and mode, problems, recent activity |
| `ddalgak engine on/off` · `ddalgak pause` · `ddalgak resume` | write, through the running app's local IPC; `engine on` fails with `engine_not_running` (3) when the app is not running, and never starts it |
| `ddalgak hotkeys …` · `ddalgak capabilities` | the hotkey and CLI contracts' commands |

**Proposed error codes** (registered in [CLI_CONTRACT §6](CLI_CONTRACT.md) by the Step that
implements them; existing codes such as `hotkey_invalid`, `hotkey_reserved`, `usage`,
`confirmation_required` keep their meaning):

| Code | Exit | Meaning |
| --- | --- | --- |
| `rule_not_found`, `profile_not_found`, `mode_not_found` | 3 | No such ID. |
| `engine_not_running` | 3 | The command needs the running app; the CLI never starts it. |
| `rules_invalid` | 4 | `rules.json` is malformed or fails the schema. |
| `rule_invalid` | 2 | The requested rule breaks the schema or a semantic rule; nothing written. |
| `rules_from_newer_version` | 5 | The file was written by a newer Ddalgak; read-only. |
| `rule_conflict` | 7 | `duplicate_trigger`, or the file changed since it was read (`revision`). |
| `consent_required` | 7 | The change needs `--allow <permission>`; the rule was saved disabled or not at all. |

## 14. Machine-checked fixtures and verification status

| Artifact | Pins down |
| --- | --- |
| [ddalgak-rules-v1](contracts/schemas/ddalgak-rules-v1.schema.json) + `contracts/examples/ddalgak-rules-v1/` | Ten valid documents (remaps and swaps, text, programs, websites, mouse, modes, engine control, profiles and conditions, an export, a future step type). Thirty-nine invalid ones, including recursive remaps (`recursive-remap-accepts-injected`, `recursive-send-keys-accepts-injected`), the emergency chord as trigger and output, reserved OS chords (`Win+L`, `Alt+Tab`, `Ctrl+Shift+Escape`, the Secure Attention Sequence), missing consent for programs, websites, injection and clipboard, shell hosts, `file:` URLs and newer versions. |
| `contracts/examples/ddalgak-rules-v1/semantic-invalid/` | Eleven schema-valid documents named `<code>--<what>.json`, each producing exactly that semantic error: `output_is_trigger` (the recursive remap `F13` → `F13`, and `send_keys` to its own trigger), duplicate IDs and triggers, unknown modes and profiles, a stale launch consent, a run over 60 s. |
| [ddalgak-simulation-v1](contracts/schemas/ddalgak-simulation-v1.schema.json) + examples | Twenty-three playground results named `<rules example>--<what>.json` and fourteen invalid ones (effects performed or input injected in `simulate`, live launches without confirmation, live input outside the playground, previews that echo text, paths, arguments or URL paths). |
| [kim-hotkey-manifest-v1/ddalgak.json](contracts/examples/kim-hotkey-manifest-v1/ddalgak.json) | Ddalgak's final GUI actions: four global ones (matching the Kim catalog) and the `rules` and `playground` contexts. |

**Not tested on Windows in Step 65.** Everything about real input (hook timing, `SendInput`,
UIPI, password-field detection, IME, layouts, stuck keys, the emergency chord) is specified, not
measured. The owners are Steps 66 (engine), 68–72 (actions), 73 (emergency), 76 (playground) and
82 (hardening and the real-Windows acceptance matrix); [DDALGAK_SAFETY §11](DDALGAK_SAFETY.md)
lists each manual check.
