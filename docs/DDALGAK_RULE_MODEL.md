# Ddalgak rule model

Status: **current** · Rules document **v1** · Step 65 (specification); the engine since Step 66, the
rule store, validation and rule commands since Step 67, remaps, blocks and mouse-button triggers
since Step 68, `send_keys`, `type_text` and hotstrings since Step 69, `open_program` (with the
additive `env` overrides) and `open_url` since Step 70, `mouse_move` (with the additive grid units
and stepped holds), `mouse_scroll` and `mouse_click` since Step 71 (§14) · Decisions:
[ADR 0022](adr/0022-ddalgak-rule-model-and-safety.md), [ADR 0028](adr/0028-ddalgak-rule-store-validation-and-reload.md),
[ADR 0031](adr/0031-ddalgak-remap-execution-and-mouse-hook.md),
[ADR 0034](adr/0034-ddalgak-typing-runs-hotstrings-and-clipboard.md),
[ADR 0037](adr/0037-ddalgak-program-and-website-launches.md),
[ADR 0040](adr/0040-ddalgak-mouse-actions.md) · Schemas:
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
What has been run against real Windows input, and what has not, is listed in §14 and in
[DDALGAK_ENGINE §8, §11](DDALGAK_ENGINE.md).

## 1. Overview and terms

| Term | Meaning |
| --- | --- |
| **Rule** | *When* a trigger happens (in these modes, under these conditions) *do* these steps. Identified by a stable `id`. |
| **Trigger** | The keys that start a rule, with how they fire (`press`, `release`, `hold`) and options (§3), or (Step 69) a typed abbreviation, a **hotstring** (§3.4). |
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

**Storage** ([ADR 0028](adr/0028-ddalgak-rule-store-validation-and-reload.md)). Paths come from the
`Paths` interface ([ADR 0011](adr/0011-portability-boundary.md)); Linux (D7) uses
`$XDG_CONFIG_HOME/mouv/ddalgak/rules.json`. Writers take an exclusive OS lock on
`rules.json.lock` (released by the OS when a writer dies), re-read the file and check the revision
they started from. Writes are atomic (`rules.json.tmp`, flush, rename;
[ADR 0006](adr/0006-state-and-configuration-storage.md)): a crash leaves the old or the new
file, and a leftover temporary file is reported (`interrupted_save`) and replaced by the next
save. Before every write the previous version goes to `rules.json.bak`; every applied change is
also kept as a snapshot `history/<UTC time>-r<revision>.json` (§10, the last 50). An unsaved
draft is `rules.draft.json`, a whole document whose `revision` is the saved revision it started
from. A restore over an unusable file keeps that file as `rules.corrupt-<UTC time>.json`. The
folder is in the user's profile and inherits its per-user ACL (`DDALGAK-SEC-011`).

**Loading.** A missing file means one empty profile `default` with mode `base`. A file that is not
JSON, is not a `ddalgak.rules` document, **fails the schema outside its rules** (the document,
settings, profiles and modes, at most 1000 rules per profile), names a missing active profile or
default mode, or has a rule without a valid `id` or a boolean `enabled` is never loaded: the
engine cannot be turned on with it, a running engine keeps its previous rules, the GUI and
`ddalgak status` say why (`rules_invalid`), and the file is never rewritten automatically. A
newer `schema_version` is read-only (`rules_from_newer_version`). **Each rule is checked against
the schema on its own** (ADR 0028): a rule that fails it is inactive with the code of the part
that is wrong (§8) and kept exactly as written, and the other rules keep working
(`DDALGAK-FR-006`). Semantic errors (§8) also make only the affected rules inactive.

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
| **Mouse buttons** `MouseMiddle`, `MouseX1`, `MouseX2` (Step 68), optionally with modifiers (`Ctrl+MouseX1`), as a whole trigger (never a sequence step). `MouseLeft` and `MouseRight` are never triggers. The CLI also accepts `MButton`, `XButton1`, `XButton2`. | Side buttons as Back, Copy or a modifier are popular remaps. A rule must never take away clicking. Mouse triggers need the low-level mouse hook, which exists only while such a rule is active ([ADR 0031](adr/0031-ddalgak-remap-execution-and-mouse-hook.md), permission `mouse-hook`, §6.1). Wheel triggers are not in this version (planned without an owner Step, [ADR 0040](adr/0040-ddalgak-mouse-actions.md) §13). |
| **No `char:` keys.** | Translating keys to characters inside the hook can change dead-key state in the focused application. Hotstrings (Step 69) are their own trigger kind (§3.4), whose translation leaves the keyboard state untouched ([ADR 0034](adr/0034-ddalgak-typing-runs-hotstrings-and-clipboard.md)). |
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
  Shift+Escape). Other action types match exactly. **An exact chord wins over that widening**:
  with `J` → `Down` and a rule on `Ctrl+J`, Ctrl+J fires the `Ctrl+J` rule whatever their order
  (Step 68).
- **Reserved chords win over that widening.** When the held modifiers and the key form a chord
  reserved for triggers (§3.1), no rule matches and the event passes unchanged: with `Tab`
  remapped, `Alt+Tab` still switches windows; with `Escape` remapped, `Ctrl+Shift+Escape` still
  opens Task Manager; with `F4` blocked, `Alt+F4` still closes the window; with `CapsLock` or
  `MouseX1` remapped, any `Win` chord with them stays Windows'.
- **Modifiers as the application sees them.** A modifier whose down a rule consumed (a remapped
  `RAlt`) is not held for matching; a modifier Ddalgak injected (the `LCtrl` of `CapsLock` →
  `LCtrl`) is, so a `Ctrl+J` rule fires while CapsLock is held. Ddalgak's own output never
  triggers a rule (§7.4).
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
| `pass_through` | `false` (default) | `true`: the original key also reaches the application (like AutoHotkey's `~`). Not allowed with steps that last while the trigger is held, except a `remap` without `tap` (Step 68, ADR 0031): the original key and the target are both held, and the trigger's modifiers stay as they are. |
| `injected` | `ignore` (default), `accept` | `accept`: input injected by *other* programs (an on-screen keyboard, remote-control software) may trigger the rule. Ddalgak's own injected input never triggers any rule. A rule with `accept` may not contain injecting steps (§7.4). |

**Steps that last while the trigger is held** (`remap`, `mode` with `op: hold`, `mouse_click`
with `op: hold`, `mouse_move` with `while_held`) are the rule's only step, on a single chord with
`on: press`, no `repeat` and no `pass_through` (schema `holds_trigger`); only a `remap` without
`tap` may pass the original key through.

### 3.4 Hotstrings (Step 69)

```json
{ "hotstring": { "abbreviation": "btw", "case": "conform", "inside_words": false,
                 "immediate": false, "end_chars": " .\n", "omit_end": false, "erase": true } }
```

A hotstring trigger has `hotstring` instead of `keys` (and none of `on`, `hold_ms`, `repeat`,
`pass_through`; schema `trigger` `oneOf`). Typing the abbreviation erases it and runs the rule
(AutoHotkey's `::btw::by the way`). An older Ddalgak keeps such a rule inactive.

| Field | Values | Rule |
| --- | --- | --- |
| `abbreviation` | 1–40 characters, no spaces or control characters | Required. What the user types: letters, digits, punctuation, Hangul (`;sig`, `@@addr`, `ㄱㅅ`). |
| `case` | `conform` (default), `insensitive`, `sensitive` | `conform`: any case matches and the replacement follows it (`BTW` → all upper case, `Btw` → first letter upper case); `insensitive`: any case, replacement as written; `sensitive`: only the exact case. |
| `inside_words` | `false` (default) | `false`: only when the character before the abbreviation is not a letter or digit (or there is none); `true`: also inside a word (AutoHotkey's `?`). |
| `immediate` | `false` (default) | `true`: fires as soon as the last character is typed (AutoHotkey's `*`); otherwise an end character fires it. |
| `end_chars` | 1–32 characters | The end characters; default `-()[]{}':;"/\,.?!`, space, Tab and Enter (`\n`). Not with `immediate`. |
| `omit_end` | `false` (default) | `true`: the end character is not typed after the replacement (AutoHotkey's `O`). Not with `immediate`. |
| `erase` | `true` (default) | `false`: the abbreviation stays and the replacement follows it (AutoHotkey's `B0`). |

- **Behavior.** The key that completes the abbreviation (the end character, or the last character
  of an `immediate` hotstring that erases) is consumed; the run erases what the application shows
  of the abbreviation with Backspace, types the replacement (the rule's one `type_text` or
  `send_keys` step) and types the end character again with the same key and modifiers, unless
  `omit_end`. When nothing is typed (`busy`, a skip, a reload in between), the key is given back
  unchanged. If another key is typed before the first Backspace, the run ends with `cancelled`
  without erasing anything.
- **The buffer.** While a hotstring rule is active, the engine keeps the last characters typed
  into the focused control, at most the longest abbreviation plus one, and only for keys that
  reach the application unchanged. It is cleared by a focus change, a password field, an IME
  composition, the secure desktop, an elevated window, pause, other programs' input, navigation
  and other non-text keys, shortcuts with Ctrl, Alt or Win, IME keys, a dead key, a key a rule
  takes, a match and 10 s without typing; Backspace forgets the last character. It is never
  stored, logged or shown (`DDALGAK-SEC-001`). Characters come from the focused window's layout
  without changing its keyboard state.
- **Stepping aside.** Never in a password field or on the secure desktop; not during an IME
  composition or while the IME is in a native mode such as Hangul (`skipped_ime_composition`).
- **Limits.** A hotstring runs exactly one `type_text` or `send_keys` step in v1 (other steps:
  `step_unsupported` until Step 78); input from other programs (an on-screen keyboard) never
  feeds the buffer (`injected: accept` rules cannot inject, §7.4); a mouse click inside the same
  field does not clear the buffer (no mouse hook for hotstrings). Two enabled hotstrings with the
  same abbreviation (in any case, unless `sensitive`), modes, conditions and priority are a
  `duplicate_trigger`.

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
| `to` | Required. One output key or chord: a named or `code:` key, optionally with modifiers (`Ctrl+C`), a bare modifier including `LWin`/`RWin`, or (Step 68) a mouse button `MouseLeft`, `MouseRight`, `MouseMiddle`, `MouseX1`, `MouseX2` with modifiers (`Ctrl+MouseLeft`). IME keys are allowed (`HangulMode`). Never the Secure Attention Sequence or the emergency chord. |
| `tap` | Optional dual role: a press and release of the trigger alone within `tap_ms` sends this key or chord instead. |
| `tap_ms` | 100–1000, default 200; only with `tap`. |

What each output is: a **named key** is sent by virtual key, so the active layout decides what it
types, like the key with that label (with the layout's scan code and the extended flag for the
navigation cluster, Right Ctrl/Alt, the Windows and media keys); a **`code:` key** is sent by scan
code, the key at that position on every layout; **`MouseLeft`/`MouseRight`** are the logical
primary and secondary buttons (they follow the "swap buttons" setting); **`AltGr`** in an output is
sent as Right Alt. Media and gaming keys (`MediaPlayPause`, `VolumeUp`, `F13`–`F24`) are outputs
like any key; whether an application reacts to them is up to it.

- **Behavior.** When the trigger goes down, the target goes down; when the trigger goes up, the
  target goes up. Auto-repeat of the trigger repeats the target's key-down (a bare-modifier
  target repeats that modifier; mouse buttons do not repeat). If the trigger has modifiers
  (`Ctrl+Shift+J` → `Down`), those modifiers are logically released while the target is held and
  restored afterwards if still physically down; a modifier the user lets go of meanwhile has its
  up consumed (the application already saw it go up) and is not restored. A plain-key trigger
  keeps the other held modifiers (§3.2). With `tap`, the target is pressed as soon as another key
  is pressed during the hold or `tap_ms` passes; otherwise the release sends the `tap` output.
  The other key reaches the application after the target, unchanged (with its repeats and its
  up), so CapsLock+C is Ctrl+C even within `tap_ms`; a key with its own rule decides the dual
  role and fires its rule. Mouse clicks never decide a dual role (wait `tap_ms` before
  Ctrl+click).
- **Shared and held outputs** (Step 68). Two remaps holding the same output share it: it goes up
  when the last one lets go. A target the user already holds (a physical `LCtrl` under
  `CapsLock` → `LCtrl`) is neither pressed nor released by Ddalgak. A reload while a target is
  held keeps it until the trigger goes up; an undecided dual role is dropped by a reload.
- **Outputs.** Preview `{ "holds": "LCtrl", "tap": "Escape", "released_modifiers": [...] }`
  (`tap` and `released_modifiers` only when there are any; `crates/ddalgak/src/remap.rs` builds it
  and a test validates it against `preview_remap`). Live: key and button events for the target
  only.
- **Errors.** Validation: `output_is_trigger` (`F13` → `F13`), `output_reserved`. Runtime:
  `skipped_elevated_target`, `skipped_password_field`, `injection_failed`.
- **Safety.** `input-injection`. The target is released on every path of `DDALGAK-SEC-005`.
  Remapping keys never hides the emergency chord: it is detected on the physical keys first
  (`DDALGAK-SEC-003`).

### 5.2 `block`: disable a key or chord

No inputs. The trigger is consumed and nothing happens (AutoHotkey's `Key::return`). A trigger
without modifiers is widened like a remap's (§3.2: with `F17` blocked, `Ctrl+F17` is blocked too;
`Alt+F4` with `F4` blocked still closes the window). Mouse buttons can be blocked (`MouseX2`).
Preview `{}`. Not injecting, needs no consent, still works over an elevated window. Blocking a key
never blocks the emergency chord. A rule of `block` and `engine` steps is an engine-control rule
(the block adds nothing).

### 5.3 `send_keys`: press a combination or sequence

| Input | Values |
| --- | --- |
| `keys` | Required. One to eight output steps separated by spaces (`Ctrl+K Ctrl+V`, `Win+D`), each tapped in order. Never the Secure Attention Sequence (Windows does not allow synthesizing it, *documented* for AutoHotkey's `Send`) or the emergency chord. |
| `step_delay_ms` | 0–1000 between steps, default 0. |

- **Behavior** (runs since Step 69). Before tapping, the modifiers the application sees held are
  logically released so that `Ctrl+Shift+F1` → `Ctrl+C` really sends Ctrl+C (an Alt release is
  preceded by a masking key so no menu bar opens); Ctrl and Shift are pressed again afterwards
  if still held, Alt and Win are not (their physical release is consumed instead). Each chord
  is one `SendInput` call, inserted serially, not interleaved with user input (*documented*).
  The run is checked and cancelled like `type_text` (§5.4).
- **Outputs.** Preview `{ "keys": "Ctrl+K Ctrl+V" }`.
- **Errors.** `output_is_trigger`, `output_reserved`; runtime `skipped_*`, `focus_changed`,
  `injection_failed`, `busy`, `cancelled`.
- **Safety.** `input-injection`.

### 5.4 `type_text`: type a Unicode string

| Input | Values |
| --- | --- |
| `text` | Required, 1–4096 characters. `\n` types Enter and `\t` types Tab; other control characters are refused. Hangul, emoji and characters outside the Basic Multilingual Plane are allowed. |
| `method` | `unicode` (default): one Unicode key event per UTF-16 unit (`KEYEVENTF_UNICODE`), independent of layout and IME. `paste`: put the text on the clipboard, send `Ctrl+V`, restore the previous clipboard contents. |
| `key_delay_ms` | 0–200 between characters, default 0, for applications that drop fast input. |
| `variables` | `false` (default). `true` (Step 69): `text` is a template with a fixed whitelist: `{date}` (2026-11-04), `{time}` (14:05), `{datetime}`, `{year}`, `{month}`, `{day}`, `{hour}`, `{minute}`, `{second}` (the local time when the rule runs, fixed width) and at most one `{cursor}`, the insertion point; `{{` and `}}` type a brace. Nothing else in braces (schema `text_template`): no environment, clipboard or user-defined variables. |

- **Behavior** (runs since Step 69, [ADR 0034](adr/0034-ddalgak-typing-runs-hotstrings-and-clipboard.md)).
  Every character is typed exactly as written: one `KEYEVENTF_UNICODE` down and up per UTF-16 unit
  (an emoji is a surrogate pair, high unit first), nothing normalized (composed `가` and
  decomposed `가` arrive as written), `\n` and `\t` as the Enter and Tab keys. The text goes out
  in calls of 32 characters, or one character per call with `key_delay_ms`; the engine handles its
  other work between calls (remaps keep working). Before typing, the modifiers the application
  sees held are logically released (as for `send_keys`). With `{cursor}`, the caret moves back one
  Left per character after it (a newline counts; an application that moves over a whole emoji
  sequence or a decomposed syllable with one Left will land elsewhere — the preview gives the
  number). An IME composition in progress is never committed or cancelled by Ddalgak; the rule is
  skipped with `skipped_ime_composition`.
- **Checks before every call.** The session state, a password edit, the target's integrity
  level, an IME composition and whether the focused control is enabled and not read-only (when no
  focused control is reported, as for UWP apps, the foreground window) are checked before the first call (`skipped_*`); before every later call a different
  window or control ends the run with `focus_changed` and nothing more is typed. Pause, turning
  off, the emergency chord, a session lock and a reload end it with `cancelled`; more than 60 s,
  with `timeout`. One typing run at a time: a trigger meanwhile is `busy`.
- **Paste.** The clipboard is opened (retried for 200 ms, otherwise `clipboard_busy` and nothing
  is pasted), every format on it is copied (text, HTML, RTF, images as DIB, file lists,
  registered formats byte for byte, enhanced metafiles by their bits; formats Windows makes from
  others are left to it), and content that cannot be copied faithfully (owner-drawn, private or
  other GDI formats, more than 32 MiB) **refuses the paste with `clipboard_not_restorable`
  without touching the clipboard**. The text is put on it marked so clipboard history and the
  cloud clipboard skip it, Ctrl+V is pressed, and after 500 ms the copy is put back, unless
  another program changed the clipboard meanwhile (its newer content wins). A restore that cannot
  open the clipboard within 500 ms ends with `clipboard_restore_failed` (the snippet stays on the
  clipboard). Every way out of the run restores the clipboard first.
- **Outputs.** Preview `{ "characters": 13, "lines": 2, "method": "unicode" }` plus, with
  variables, `"variables": ["date"]` and `"cursor_back": 3`: counts and names only. The text
  appears in the playground's own field, never in JSON output, validation messages or logs
  (`DDALGAK-SEC-011`); `ddalgak rule show` replaces it with its counts unless `--show-text`.
- **Errors.** Validation: `step_invalid` for control characters, length and template errors (the
  message names the position, never the text). Runtime `skipped_password_field` (never typed
  into a password field, `DDALGAK-SEC-002`), `skipped_elevated_target`, `skipped_secure_desktop`,
  `skipped_ime_composition`, `skipped_disabled_field`, `focus_changed`, `clipboard_busy`,
  `clipboard_not_restorable`, `clipboard_restore_failed`, `injection_failed`, `busy`,
  `cancelled`, `timeout`.
- **Safety.** `input-injection`, plus `clipboard` for `paste`. The rules file stores the text in
  plain form: it is for snippets, not for passwords, and the GUI says so.

### 5.5 `open_program`: open an executable

| Input | Values |
| --- | --- |
| `path` | An absolute path to an `.exe`, on a drive (`C:\…`) or under a known-folder token: `%ProgramFiles%`, `%ProgramFiles(x86)%`, `%LocalAppData%`, `%AppData%`, `%UserProfile%`, `%SystemRoot%`. No other environment variables, no relative paths, no documents, no shortcuts. |
| `app_id` | Instead of `path`: the AppUserModelID of a packaged app (`Microsoft.WindowsCalculator_8wekyb3d8bbwe!App`). No `args` or `working_dir` with it. |
| `args` | Up to 32 separate arguments, each quoted by Ddalgak by the Windows command-line rules. Never one shell string. |
| `working_dir` | Absolute folder (same tokens); default: the program's folder. |
| `env` | Step 70, additive: explicit environment overrides for this program only, `[{ "name": "TOOL_MODE", "value": "portable" }]`; at most 32, names `[A-Za-z_][A-Za-z0-9_]*` (up to 64) and unique ignoring case, values up to 4096 characters without control characters. Not with `app_id`. Without `env` the program inherits Ddalgak's environment unchanged. |
| `window` | `normal` (default), `maximized`, `minimized`. There is no hidden state: a program the user cannot see is silent execution. |
| `if_running` | `start_new` (default) or `activate`: bring an existing window of the same program to the front instead. |

- **Behavior** (Step 70, [ADR 0037](adr/0037-ddalgak-program-and-website-launches.md)). Just
  before the start the path is expanded (the six tokens only; nothing is searched: no `PATH`, no
  current folder) and checked again: it exists and is a file, the real file behind links is an
  `.exe` and not a shell host, and there is no `:` after the drive (an alternate data stream; the
  schema's pattern allows it, so the check is a runtime `step_invalid`). Then `CreateProcessW` with
  the expanded path as the application name (never split at a space), a command line with each
  argument quoted by the `CommandLineToArgvW` rules, no inherited handles, the working folder, the
  window state and, with `env`, Ddalgak's environment with those variables replaced; as the
  current user, without shell verbs. Packaged apps go to the shell's `open` on
  `shell:AppsFolder\<app_id>`. A program whose manifest requires elevation fails with
  `program_requires_elevation`; Ddalgak never asks for elevation and never retries elevated
  (`DDALGAK-SEC-007`, `KIM-SEC-004`). `if_running: activate` looks for a visible, unowned
  top-level window of a process started from the same file, restores it and asks Windows to
  bring it to the front; without one the program starts. Whether a new or activated window comes
  to the front is decided by Windows' foreground lock (ADR 0037 §8). A launch rule runs on its own
  short-lived thread; one run per rule at a time (`busy`) and one start per rule per second
  (`rate_limited`).
- **Outputs.** Preview `{ "program": "notepad.exe", "found": true, "arguments": 0 }`: file name,
  whether it exists, the number of arguments. Never the full path, the arguments or the
  environment values.
- **Errors.** Validation (schema): relative path, non-`.exe`, command interpreters and script
  hosts (`cmd.exe`, `powershell.exe`, `pwsh.exe`, `wscript.exe`, `cscript.exe`, `mshta.exe`,
  `rundll32.exe`, `regsvr32.exe`, `wsl.exe`, `bash.exe`, `conhost.exe`), which belong to Step 79's
  separately consented command action; an `env` entry with a bad name, or `env` with `app_id`.
  Since Step 70 also at compile time (`step_invalid`, reported by `rule check`): a `:` after the
  drive, an override name given twice, a quoted command line longer than Windows' 32 766
  characters. Semantic: `consent_stale` when the target (path, argument, folder, window state,
  override) changed after consent. Runtime: `program_not_found` (missing, moved, a folder, a
  missing working folder, an unset known folder, an app that is not installed),
  `program_requires_elevation`, `launch_failed` (not a program, a shell host behind a link,
  access denied).
- **Safety.** `launch-programs` with a fingerprint (§6.2). An imported rule arrives disabled and
  without consent (`DDALGAK-SEC-013`). Loading, checking, reloading or importing rules never
  starts anything; only a fired trigger does.

### 5.6 `open_url`: open a website

| Input | Values |
| --- | --- |
| `url` | Required. `http://` or `https://`, a host, optional port, path, query and fragment; no spaces, no user name or password; at most 2048 characters. |

- **Behavior.** The URL is handed to the user's default browser through the shell's `open`
  association for `http`/`https` only (`ShellExecuteExW`, verb `open`, no error dialog; Step 70).
  Ddalgak itself makes no network connection (`DDALGAK-SEC-014`).
- **Input normalization** (the editor and the CLI, before consent; the stored URL is what the
  fingerprint covers): surrounding spaces removed; `https://` added when no scheme is given
  (`github.com`, `localhost:3000/x`); scheme and host in lower case; a non-ASCII host label in its
  `xn--` Punycode form (no full IDNA mapping); `/` added when there is no path; spaces, `"`, `<`,
  `>`, `\`, `^`, `` ` ``, `{`, `}`, `|`, non-ASCII characters and a `%` that starts no escape
  percent-encoded in the path, query and fragment.
- **Outputs.** Preview `{ "scheme": "https", "host": "github.com" }`. The path and query are never
  shown or logged: they can carry tokens. `rule show` shows `https://github.com/…` unless
  `--show-targets`.
- **Errors.** Validation: other schemes (`file:`, `javascript:`, `vbscript:`, `data:`, `ms-*:`
  such as `ms-settings:`, `search-ms:`, `shell:` and custom protocols such as `mailto:` or
  `zoommtg:` — none can be allowed in rules v1) and credentials. Since Step 70 also at compile time
  (`step_invalid`): control or whitespace characters, `"`, `<`, `>`, `\`, a port outside 1–65535,
  a malformed host. Runtime: `browser_unavailable` (no association for the scheme),
  `launch_failed`.
- **Safety.** `launch-programs` with a fingerprint (§6.2).

### 5.7 `mouse_move`

| Input | Values |
| --- | --- |
| `mode` | `relative` (with `dx`, `dy`, each −10000…10000) or `absolute` (with `to`). |
| `to` | `{ "x", "y", "relative_to" }` (each −32768…32767); `relative_to` is `virtual_screen` (default; desktop pixels across all monitors, may be negative), `primary_monitor` or `active_window` (its content area). |
| `unit` | Step 71, additive, relative only: `pixels` (default; logical pixels scaled by the display scale of the monitor the pointer is on) or `grid` (cells of `grid` on that monitor; the pointer lands on cell centres and stays on the grid). |
| `grid` | With `unit: grid`: `{ "columns": 2–64, "rows": 2–64 }` per monitor. |
| `while_held` | Relative only. Continuous: `{ "speed": 50–5000 logical px/s, "acceleration": 0–10 }`, the pointer glides in the `dx`/`dy` direction (a diagonal as fast as a straight line) while the trigger is held; the speed grows linearly over the first second to `speed × (1 + acceleration)`, at most 10 000 px/s. Stepped (Step 71, additive): `{ "repeat_ms": 20–2000, "delay_ms": 0–2000 }`, the step at once, after `delay_ms` (default 300) again, then every `repeat_ms`, whatever the keyboard's repeat settings. Grid moves are stepped only. |

- **Behavior** (Step 71, [ADR 0040](adr/0040-ddalgak-mouse-actions.md)). The engine works in
  physical desktop pixels read in per-monitor DPI mode. Rule coordinates are logical pixels of
  the monitor they refer to: `dx`/`dy` scaled by the DPI of the monitor the pointer is on, a
  `primary_monitor` point by the primary monitor's, an `active_window` point by the window's,
  from the top-left corner of its content area; a `virtual_screen` point names a physical desktop
  pixel (mixed scales give it no single logical size). Every move is sent as an absolute move to
  a checked pixel (`MOUSEEVENTF_VIRTUALDESK`, normalized coordinates); a relative move starts from
  the pointer position read just before, so the mouse speed and "enhance pointer precision"
  settings never change the distance. **Never outside the desktop**: the visible desktop is the
  union of the monitors; an absolute target outside every monitor lands on the nearest pixel of a
  monitor, and a relative move stops at the last pixel before its path leaves the desktop (it
  crosses onto an adjacent monitor), both with `clamped: true`. A move while held is ticked every
  10 ms (continuous) or at its step times only while it lasts, at most 50 ms of distance per tick,
  and stops on the trigger's release, pause, turn-off, the emergency chord, a session lock, a
  desktop switch, an elevated window taking focus, a reload and after 60 s (`timeout`).
- **Outputs.** Preview `{ "from", "to", "clamped", "while_held", "speed" }` (an absolute move
  has no `from`); a stepped hold has `repeat_ms` instead of `speed`, a grid move `"unit": "grid"`.
  The playground runs the step on a simulated pointer: `to` is where the pointer would go, the
  real pointer never moves.
- **Errors.** Validation (schema): both a distance and a point, `unit: grid` without `grid` (or
  `grid` without the unit), a continuous grid move, both `speed` and `repeat_ms`, a move while
  held without a direction, a move while held that is not the rule's only step (§3.3). Runtime
  `skipped_elevated_target` (the window that would receive the input — the capture window during
  a drag, otherwise the window at the point — belongs to an elevated process; UIPI would drop it
  silently), `skipped_secure_desktop`, `skipped_disabled_field` (`active_window` and no window is
  in the foreground), `injection_failed`; a move while held also `cancelled` and `timeout`.
- **Safety.** `input-injection`. No pointer tracking: Ddalgak reads the pointer position only
  when a rule runs, and mouse actions never install the mouse hook. No game or anti-cheat
  promise: programs that read raw relative mouse input may ignore absolute moves (ADR 0040).

### 5.8 `mouse_scroll`

`direction` (`up`, `down`, `left`, `right`) and `notches` (1–50, default 1; one notch is 120
wheel units). Since Step 71 one wheel event per notch (`MOUSEEVENTF_WHEEL`, positive away from the
user, or `MOUSEEVENTF_HWHEEL`, positive to the right) in one call, at the pointer, checked like a
click (`skipped_elevated_target`, `skipped_secure_desktop`, `injection_failed`). Preview
`{ "direction", "notches" }`. `input-injection`.

### 5.9 `mouse_click`

| Input | Values |
| --- | --- |
| `button` | `left`, `right`, `middle`, `x1`, `x2`. `left` and `right` are the logical primary and secondary buttons (they follow the "swap buttons" setting). |
| `op` | `click` (default), `double_click` (two clicks within the system double-click time) or `hold` (the button is down exactly as long as the trigger). |
| `at` | Optional point (as in §5.7) to move to first. |

Since Step 71: a click is the button's down and up in one call, a double click two clicks in one
call (so they arrive within the double-click time at the same point), with the move to `at` in
the same call. A **held** button is a remap whose target is the button (§5.1, [ADR 0040](adr/0040-ddalgak-mouse-actions.md)):
it shares reference counts with remaps that hold the same button, releases the trigger's
modifiers while held (a plain drag, not Ctrl+drag) and restores them, never auto-repeats, stays
down through a reload until its trigger goes up, and is released on every path of
`DDALGAK-SEC-005`. Errors as for `mouse_move`. Preview `{ "button", "op", "at" }` (`at` as
clamped). There is no bare "button down" without "up": `hold` is the only way to keep a button
pressed (no click-lock toggle; Windows' own ClickLock is the accessible alternative).
`input-injection`.

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

Trigger kinds planned additively: mouse wheel notches (without an owner Step since Step 71,
[ADR 0040](adr/0040-ddalgak-mouse-actions.md) §13). Mouse buttons arrived in Step 68
(§3.1; the `mouse-hook` permission, §6.1) and hotstrings in Step 69 (§3.4). `send_keys` and
`type_text` run from Step 69, each as the rule's only step; `open_program` and `open_url` from
Step 70, together and in order but not mixed with other step types; `mouse_move`,
`mouse_scroll` and `mouse_click` from Step 71 likewise (a held button or a move while held as
the rule's only step); and `delay`, with ordered multi-step runs, from Step 78
([ADR 0031](adr/0031-ddalgak-remap-execution-and-mouse-hook.md),
[ADR 0034](adr/0034-ddalgak-typing-runs-hotstrings-and-clipboard.md),
[ADR 0037](adr/0037-ddalgak-program-and-website-launches.md), ADR 0040); until then they are
`step_unsupported`.

## 6. Permissions and consent

### 6.1 Permissions

The IDs are the Kim catalog's permission IDs ([CATALOG_FORMAT §2.1](CATALOG_FORMAT.md)), which Kim
discloses before installing Ddalgak.

| Permission | Needed by | Recorded |
| --- | --- | --- |
| `keyboard-hook` | the engine itself | Implicit in turning the engine on; never per rule |
| `mouse-hook` | mouse-button triggers (Step 68) | Implicit: the low-level mouse hook exists only while the engine is on and an active rule has a mouse-button trigger; never per rule (`status` shows `engine.hook.mouse_installed`). The Kim catalog discloses it before install ([ADR 0031](adr/0031-ddalgak-remap-execution-and-mouse-hook.md) lists the catalog change) |
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
   are therefore a swap, not a loop (example `01-remap.json`, profile `swap`), and remaps do not
   chain (`F15` → `F16` and `F16` → `F17`: F15 gives F16). Only the modifier state of injected
   modifiers is seen by later physical keys (§3.2), which needs a physical key each time and so
   cannot loop.
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
| `rules_invalid` | error (file) | schema | The document fails the schema outside its rules, or cannot be read; nothing loads (§2). |
| `trigger_reserved` | error | schema | A reserved trigger (§3.1). CLI exit: `hotkey_reserved` (2). |
| `trigger_invalid` | error | schema | Not the trigger syntax. CLI exit: `hotkey_invalid` (2). |
| `output_reserved` | error | schema | An output contains the Secure Attention Sequence or the emergency chord. |
| `consent_missing` | error | schema | An enabled rule lacks a consent of §6.1. |
| `recursion_risk` | error | schema | `injected: accept` with an injecting step (§7.4). |
| `step_invalid` | error | schema | A step's inputs break §5 (relative path, shell host, URL scheme, missing mode target, …). Since Step 70 also the launch checks the schema cannot express (§5.5, §5.6: a `:` after the drive, an override name given twice, a command line over Windows' limit, a URL with characters a browser's command line could misread), found when the rule is compiled; messages never quote the target. |
| `duplicate_profile_id`, `duplicate_mode_id`, `duplicate_rule_id` | error | semantic | An ID appears twice. |
| `unknown_profile`, `unknown_mode` | error | semantic | A reference to a profile or mode that does not exist. |
| `output_is_trigger` | error | semantic | §7.4 3. |
| `duplicate_trigger` | error | semantic | §7.1. The later rule is the subject. |
| `consent_stale` | error | semantic | A launch fingerprint no longer matches (§6.2). |
| `run_too_long` | error | semantic | Delays of one run exceed 60 s; since Step 69 `key_delay_ms` between the characters of a text as written and `step_delay_ms` between sent chords count too. |
| `rule_invalid` | error | schema | Any other field of the rule breaks the schema (title, priority, modes, consent record, …). |
| `duplicate_binding` | error | runtime | The trigger equals one of Ddalgak's own bound GUI shortcuts in `hotkeys.json`; the shortcut wins (§7.5). |
| `step_unsupported`, `condition_unsupported`, `rule_unsupported` | info | runtime | Inactive in this version: an unknown step type or one a later Step implements, a condition not evaluated yet, or an unknown field inside the rule (§9). |
| `altgr_collision`, `ime_toggle`, `os_shortcut`, `hook_overrides_global` | warning | semantic | §3.1. `hook_overrides_global` needs the other Kim tools and is reported by `kim hotkeys check --all-tools` (Step 81). |
| `trigger_shadowed` | warning | runtime | Another enabled rule with the same keys and conditions wins by precedence (§7.1) in every mode this rule works in, so it never fires (a lower `priority`, or `on: release` behind `on: press`). |
| `sequence_prefix` | warning | runtime | A shorter trigger of another rule is the start of this sequence in a mode both work in; that rule fires first and this sequence never completes. |
| `target_missing` | warning | runtime | An `open_program` path or working folder does not exist on this computer (checked by the CLI, never searched for; the run would end with `program_not_found`). |
| `trigger_overridden` | info | runtime | In some modes a mode-scoped rule takes this rule's trigger first (intended layering, §7.3). |
| `output_cycle` | info | runtime | The outputs of several rules lead back to the first; Ddalgak never matches its own input, so it is a swap, not a loop (§7.4). |

Errors make the rule inactive; warnings and infos do not change behavior (the `*_unsupported`
infos keep the rule inactive in this version). Every error and warning comes with non-destructive
**offers** in the hotkey contract's style (HOTKEY_CONTRACT §9.5): up to two `rebind` suggestions
for a single-chord trigger (the chord with `Shift`, `Ctrl+Shift` or `Alt+Shift` added, keeping
only valid, unreserved chords without a warning that no enabled rule of the profile and no
Ddalgak shortcut uses), `allow <permission>` for missing or stale consent, `disable`, and `keep`
for warnings. Infos carry no offers. **An offer is never applied by itself.**

**Who checks.** The schema rows are checked with the published schema itself, in two parts (§2,
ADR 0028). The semantic rules are implemented for the fixtures by `xtask/src/ddalgak.rs` and in
Ddalgak by a line-by-line port (`crates/ddalgak/src/validate.rs`) that its tests compare with the
xtask original on every fixture and on generated mutations; a rule that fails the schema claims
no trigger, so it never makes a valid rule a `duplicate_trigger`. The runtime rows need what only
Ddalgak knows (its version, its shortcuts, the whole profile).

**Changes never add errors.** `ddalgak rule …` (and later the editor) validates the document
before and after a change and refuses one that introduces an error, with nothing written:
`rule_conflict` for `duplicate_trigger`, `duplicate_binding` or a taken ID, `hotkey_invalid` and
`hotkey_reserved` for triggers, `rule_invalid` for everything else. Problems that existed before
do not block an unrelated change.

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
| `skipped_password_field`, `skipped_secure_desktop`, `skipped_elevated_target`, `skipped_ime_composition` | Ddalgak stepped aside; the key passed unchanged (a hotstring's end key is given back). |
| `skipped_disabled_field` | Typing (Step 69): the focused control was disabled or a read-only edit (when the application reports no focused control, as UWP apps do, a disabled or missing foreground window); nothing was typed. Mouse (Step 71): a point `relative_to: active_window` and no window in the foreground; nothing was sent. |
| `focus_changed` | Typing (Step 69): another window or control took focus during the run; it stopped, and nothing was typed into the new window. |
| `skipped_injected_input` | Injected input and the rule accepts only physical input. |
| `busy` | The rule is still running (a launch rule's thread, Step 70, included). |
| `rate_limited` | §7.4 4; a launch rule also within one second of its last start (Step 70: a double press never opens a program twice). |
| `injection_failed` | `SendInput` inserted fewer events than requested. UIPI failures are silent (*documented*), so the elevated-target check runs first. |
| `program_not_found`, `program_requires_elevation`, `launch_failed`, `browser_unavailable`, `clipboard_busy` | §5. |
| `clipboard_not_restorable`, `clipboard_restore_failed` | Paste (Step 69, §5.4): the clipboard held content that cannot be copied faithfully, so nothing was pasted and it was left alone; or the previous content could not be put back after the paste. |
| `cancelled` | Pause, turn-off, emergency or a profile switch ended the run (since Step 71 also a move while held stopped by a session lock, a desktop switch, an elevated window taking focus or a reload). |
| `timeout` | A step did not finish in its time limit (a move while held: 60 s). |

**Activity (audit).** The app keeps the last `max_entries` (default 200, in memory) outcomes:
`{ "at", "rule", "profile", "mode", "outcome", "step", "duration_ms" }`. Entries never contain
keys, typed text, window titles, paths, arguments or URLs (`DDALGAK-SEC-001`, `DDALGAK-SEC-011`).
With `activity.persist: true` they are also appended to a capped, rotated local file. Changes to
rules are audited by the history snapshots of §10, each recording the revision, the time and
the origin (`gui`, `cli`, `import`, `restore`).

## 13. CLI surface

The command list Steps 67 and 81 implement under the CLI contract (`--json`, `--dry-run`,
`--yes`, exit codes). The `rule` and `rules` rows are **implemented (Step 67)**; their `--json`
documents are [ddalgak-cli-v1](contracts/schemas/ddalgak-cli-v1.schema.json) and
`ddalgak --help` is the reference. The other rows are planned; names may still be refined there,
behavior may not get weaker.

| Command | Effect |
| --- | --- |
| `ddalgak rule list [--profile p] [--mode m]` · `rule show <id> [--show-text] [--show-targets]` · `rule check` (each `[--draft]`) | read (snippet text only with `--show-text`, launch targets only with `--show-targets`) |
| `ddalgak rule add (--trigger <keys> \| --hotstring <abbreviation> [--case c] [--inside-words] [--immediate] [--end-chars s] [--omit-end] [--keep-abbreviation]) (--action <type> [inputs] \| --step <json>…) [--allow <permission>]…` (text inputs: `--text`, `--escapes`, `--variables`, `--method`, `--key-delay-ms`; Step 69; launch inputs: `--path` or `--app-id`, `--arg`…, `--working-dir`, `--window`, `--if-running`, `--env NAME=VALUE`…, `--url`; Step 70) · `rule edit <id> … [--patch <json>]` · `rule duplicate <id>` · `rule enable/disable <id>…` · `rule move <id> --before <id> \| --after <id> \| --to start\|end\|<n>` | write, `--dry-run`, `--draft`, `--if-revision <n>` |
| `ddalgak rule delete <id>…` | destructive: `--dry-run`, `--yes` |
| `ddalgak rule revoke <id> <permission>` | write (the rule is disabled when it needs the permission) |
| `ddalgak rules history` · `rules draft [show]` | read |
| `ddalgak rules backup` · `rules reload` (asks the running app) · `rules draft apply` | write, `--dry-run` |
| `ddalgak rules restore (--backup \| <snapshot>)` · `rules draft discard` | destructive: `--dry-run`, `--yes` |
| `ddalgak profile list/show/add/rename/duplicate/delete/apply/history/restore/export/import` | read / write / destructive as named |
| `ddalgak mode list/add/rename/delete/set` | read / write (`mode set` asks the running app) |
| `ddalgak test --keys <keys> [--profile p] [--mode m] [--app exe] [--focus kind] --dry-run` | read: a `ddalgak.simulation` result; there is no headless live test |
| `ddalgak explain <keys> [context flags]` | read: the decision and candidates only |
| `ddalgak status` | read: engine state, active profile and mode, problems, recent activity |
| `ddalgak engine on/off` · `ddalgak pause` · `ddalgak resume` | write, through the running app's local IPC; `engine on` fails with `engine_not_running` (3) when the app is not running, and never starts it |
| `ddalgak hotkeys …` · `ddalgak capabilities` | the hotkey and CLI contracts' commands |

**Error codes** (registered in [CLI_CONTRACT §6](CLI_CONTRACT.md) in Step 67; existing codes such
as `hotkey_invalid`, `hotkey_reserved`, `usage`, `confirmation_required`, `busy` and
`permission_denied` keep their meaning):

| Code | Exit | Meaning |
| --- | --- | --- |
| `rule_not_found`, `profile_not_found`, `mode_not_found` | 3 | No such ID. |
| `snapshot_not_found` | 3 | No such backup, history snapshot or draft. |
| `engine_not_running` | 3 | The command needs the running app; the CLI never starts it. |
| `rules_invalid` | 4 | `rules.json` is malformed or fails the schema outside its rules (§2). |
| `rule_invalid` | 2 | The requested rule breaks the schema or a semantic rule; nothing written. |
| `rules_from_newer_version` | 5 | The file was written by a newer Ddalgak; read-only. |
| `rule_conflict` | 7 | `duplicate_trigger`, `duplicate_binding`, a taken ID, or the file changed since it was read (`revision`). |
| `consent_required` | 7 | The change needs `--allow <permission>`; the rule was saved disabled or not at all. |

## 14. Machine-checked fixtures and verification status

| Artifact | Pins down |
| --- | --- |
| [ddalgak-rules-v1](contracts/schemas/ddalgak-rules-v1.schema.json) + `contracts/examples/ddalgak-rules-v1/` | Fourteen valid documents (remaps and swaps, text, programs, websites, mouse, modes, engine control, profiles and conditions, an export, a future step type, since Step 68 mouse-button remaps, pass-through and a physical-position trigger in `11-remap-mouse-and-pass-through.json`, and since Step 69 hotstrings, variables and the insertion point in `12-hotstrings-and-snippets.json`, and since Step 70 environment overrides, activation and websites in order in `13-launch-options.json`, and since Step 71 a keyboard-mouse profile with gliding, grid and stepped moves, every button, holds, the wheel in four directions and points on the desktop, the primary monitor and the active window in `14-mouse-navigation.json`). Fifty-seven invalid ones (Step 71 added a continuous grid move, a grid unit without a grid, a move while held without a direction, both a speed and a repeat, a held button with repeat; Step 70 added `javascript:`, `ms-settings:` and custom-protocol URLs, `env` with an app ID and a bad variable name; Step 69 added a hotstring with keys, with a space, `immediate` with end characters, a hotstring remap, an unknown and a second-cursor template), including recursive remaps (`recursive-remap-accepts-injected`, `recursive-send-keys-accepts-injected`), a dual role that also passes the key through (`remap-tap-with-pass-through`), the primary button and a mouse button in a sequence as triggers, the emergency chord as trigger and output, reserved OS chords (`Win+L`, `Alt+Tab`, `Ctrl+Shift+Escape`, the Secure Attention Sequence), missing consent for programs, websites, injection and clipboard, shell hosts, `file:` URLs and newer versions. |
| `contracts/examples/ddalgak-rules-v1/semantic-invalid/` | Thirteen schema-valid documents named `<code>--<what>.json`, each producing exactly that semantic error: `output_is_trigger` (the recursive remap `F13` → `F13`, and `send_keys` to its own trigger), duplicate IDs and triggers (since Step 69 also two hotstrings `btw`/`BTW`), unknown modes and profiles, a stale launch consent, a run over 60 s (delays, and since Step 69 slow typing). |
| [ddalgak-simulation-v1](contracts/schemas/ddalgak-simulation-v1.schema.json) + examples | Twenty-four playground results (Step 71 added a stepped grid move, checked against the code's own preview) named `<rules example>--<what>.json` and fourteen invalid ones (effects performed or input injected in `simulate`, live launches without confirmation, live input outside the playground, previews that echo text, paths, arguments or URL paths). |
| [kim-hotkey-manifest-v1/ddalgak.json](contracts/examples/kim-hotkey-manifest-v1/ddalgak.json) | Ddalgak's final GUI actions: four global ones (matching the Kim catalog) and the `rules` and `playground` contexts. |

**Step 66** implements the engine for engine-control rules ([DDALGAK_ENGINE](DDALGAK_ENGINE.md));
every other step type is inactive with `step_unsupported` until its Step.

**Step 71** runs `mouse_move` (§5.7), `mouse_scroll` (§5.8) and `mouse_click` (§5.9) and adds
grid units and stepped holds ([ADR 0040](adr/0040-ddalgak-mouse-actions.md),
[DDALGAK_ENGINE §14](DDALGAK_ENGINE.md)). The CLI writes them with the Step 67 syntax:
`ddalgak rule add --trigger Ctrl+Alt+Shift+Right --action mouse_move --dx 1 --dy 0 --speed 600 --acceleration 3 --allow input-injection`,
`… --action mouse_move --dx 1 --grid 16x9 --repeat-ms 150`, `… --action mouse_move --x 960 --y 540 --relative-to primary_monitor`,
`… --action mouse_scroll --direction down --notches 3`, `… --action mouse_click --button left --op hold`,
or any step as `--step '<json>'`. The editor's Mouse form, its summary and its screen map are
`crates/ddalgak/src/mouse.rs` (`MouseDraft`, `MOUSE_FIELDS`, `screen_map`), checked against this
schema; the playground's safe pointer is `mouse::Playground` over a simulated pointer
([GUI_JOURNEYS J4, J5](DDALGAK_GUI_JOURNEYS.md)).

**Step 70** runs `open_program` (§5.5) and `open_url` (§5.6) and adds the `env` overrides
([ADR 0037](adr/0037-ddalgak-program-and-website-launches.md), [DDALGAK_ENGINE §13](DDALGAK_ENGINE.md)).
The CLI writes them, normalizing a pasted path (quotes removed, `/` as `\`) and a typed address
(§5.6): `ddalgak rule add --trigger Ctrl+Shift+F6 --action open_program --path "%SystemRoot%\System32\notepad.exe" --allow launch-programs`,
`… --action open_program --path "C:\Tools\tool.exe" --arg --profile --arg "C:\My Files" --env TOOL_MODE=portable --window minimized --if-running activate`,
`… --action open_program --app-id Microsoft.WindowsCalculator_8wekyb3d8bbwe!App`,
`… --action open_url --url github.com`. `rule show` redacts launch targets unless
`--show-targets` (`targets_redacted`). The editor's program picker and website field are
`crates/ddalgak/src/launch.rs` (`ProgramDraft`, `UrlDraft`, `OPEN_PROGRAM_FIELDS`,
`OPEN_URL_FIELDS`; an icon preview read from the file on Windows), checked against this schema
([GUI_JOURNEYS J4](DDALGAK_GUI_JOURNEYS.md)). Fixture `13-launch-options.json` shows overrides,
a minimized window, activation, two websites in order with `on_error: continue` and an imported
rule waiting for review.

**Step 69** runs `send_keys` (§5.3) and `type_text` (§5.4) and adds hotstring triggers (§3.4),
the variable whitelist with the insertion point, and the paste method with clipboard restore
([ADR 0034](adr/0034-ddalgak-typing-runs-hotstrings-and-clipboard.md),
[DDALGAK_ENGINE §12](DDALGAK_ENGINE.md)). The CLI writes them:
`ddalgak rule add --hotstring btw --action type_text --text "by the way" --allow input-injection`,
`ddalgak rule add --trigger Ctrl+Shift+F2 --action type_text --text "감사합니다.\n김민수 드림" --escapes --allow input-injection`,
`ddalgak rule add --hotstring ";dd" --immediate --action type_text --text "{date}" --variables --allow input-injection`.
The text editor's form fields, live preview and save/test gating are `crates/ddalgak/src/text.rs`
(`TYPE_TEXT_FIELDS`, `EditorDraft`), checked against this schema
([GUI_JOURNEYS J4](DDALGAK_GUI_JOURNEYS.md)).

**Step 68** runs `remap` (§5.1) and `block` (§5.2) and adds mouse-button triggers and outputs
(§3.1, §5.1) with the on-demand mouse hook, pass-through for remaps (§3.3) and the "exact chord
beats widening" rule (§3.2) ([ADR 0031](adr/0031-ddalgak-remap-execution-and-mouse-hook.md),
[DDALGAK_ENGINE §11](DDALGAK_ENGINE.md)). The CLI writes them with the Step 67 syntax:
`ddalgak rule add --trigger CapsLock --action remap --to LCtrl --tap Escape --allow input-injection`,
`--pass-through`, `ddalgak rule add --trigger Insert --action block`.

**Step 67** implements the rule store and its validation in Ddalgak (ADR 0028): the typed model
(`crates/ddalgak/src/model.rs`, every fixture round-trips through it without loss), the schema
applied per rule (`schema.rs`, which refuses every `invalid/` fixture with the expected code and
gives the whole rule schema's verdict on every fixture and on mutations), the cross-references
compared with `xtask/src/ddalgak.rs` (`validate.rs`; every `semantic-invalid/` fixture produces
exactly its code), the store (`store.rs`), the operations (`edit.rs`), the `rule`/`rules`
commands, and the live reload in the running app ([DDALGAK_ENGINE §10](DDALGAK_ENGINE.md)).
Migrations are a chain of pure `vN → vN+1` steps; v1 is the first version, so the shipped chain is
empty and the mechanism is tested with a test chain. Import and export (§10) and profile and mode
commands remain with Steps 72, 75 and 81.

**Not tested on Windows in Step 65.** Everything about real input (hook timing, `SendInput`,
UIPI, password-field detection, IME, layouts, stuck keys, the emergency chord) is specified, not
measured. The owners are Steps 66 (engine), 68–72 (actions), 73 (emergency), 76 (playground) and
82 (hardening and the real-Windows acceptance matrix); [DDALGAK_SAFETY §11](DDALGAK_SAFETY.md)
lists each manual check.
