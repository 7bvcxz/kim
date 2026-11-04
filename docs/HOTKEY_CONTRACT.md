# Kim hotkey contract

Status: **current** · Hotkey contract **v1** · Step 45 (specification) · Step 46: implemented by the
shared `kim-hotkeys` library and by `kim` / Kim Manager (§17) ·
Decision: [ADR 0017](adr/0017-universal-customizable-hotkeys.md) · Requirements: `KIM-FR-040…046`,
`KIM-NFR-020…022`, `KIM-SEC-020…023`, `SUBAK-FR-033`, `SUBAK-FR-034`, `KIM-NFR-012`

Every installable Kim tool that has a GUI follows this contract for its keyboard shortcuts:
Kim Manager, Subak's GUI, and later tools such as Noeul and Ddalgak. It defines what a shortcut
is, how it is written and stored, how conflicts are found and resolved, the headless `hotkeys`
commands, and the safety rules no tool may break. The CLI rules it builds on are in
[CLI_CONTRACT.md](CLI_CONTRACT.md); the keyboard rules of the GUIs are in
[DESIGN_SYSTEM §4.1](DESIGN_SYSTEM.md).

Words: **MUST**/**MUST NOT** are requirements; **SHOULD** is a strong default that needs a
written reason to break. Windows behavior cited from Microsoft documentation is marked
*documented*; Step 45 ran nothing against real hotkey APIs (§14 lists who verifies what and
what Step 46 ran).

## 1. Scope and conformance

- **In scope:** the shortcuts of a tool's own GUI operations: which actions exist, which keys
  invoke them, where they are active, and how the user sees, changes, disables and resets them.
- **Not every CLI command needs a hotkey.** Shortcuts serve GUI operations. A tool without a
  GUI needs no shortcuts at all, and agents reach the same operations through the CLI
  (`KIM-FR-013`).
- **Out of scope:**
  - Encoding keys for a terminal program (stage 4 of [ADR 0009](adr/0009-gui-clients-and-keyboard-dispatch.md)).
  - Ddalgak's user-defined automation rules. They have their own rule model (Step 65); §13.4
    says how they take part in conflict checks.
  - Mouse gestures (Noeul's double-click to pin). They are listed next to shortcuts but are
    configured by the tool (§11.1).
- **Conformance:** a tool conforms to hotkey contract v1 when it ships a hotkey manifest (§3.4),
  stores user bindings as in §8, implements the `hotkeys` commands of §10 and follows §9, §11 and
  §12. It announces the commands through its capabilities document like any other command
  (CLI_CONTRACT §9). No change to the capabilities schema is needed.

### 1.1 Current state of the tools (audit, Step 45)

| Tool | GUI shortcuts today | Contract status |
| --- | --- | --- |
| Kim Manager (`kim`) | Step 46: `catalog.refresh` `F5`, `help.show` `F1` (shortcut list), `tool.install`/`tool.update`/`tool.uninstall` unbound by default, plus the reserved navigation keys of §7.2. | **Conforms** since Step 46: compiled-in manifest (`crates/kim/src/hotkeys.manifest.json`), `kim hotkeys` commands, `focus` backend (§13.2, §17). The binding editor of §11.2 is not implemented yet. |
| Subak (`subak`) | Not implemented yet. Defaults are specified: `Ctrl+T`, `Ctrl+W`, `Ctrl+Tab`, `Ctrl+Shift+Tab`, the `Ctrl+B` prefix table (`SUBAK-FR-033`), and `F1` / `Ctrl+B ?` help ([DESIGN_SYSTEM §4.1](DESIGN_SYSTEM.md)). | Steps 17 and 23 implement them on this contract; §13.1 maps ADR 0009 onto it. |
| Noeul (`noeul`) | Planned (Steps 48+). `F1` region capture and `F3` floating paste are requested defaults. | Steps 48, 61 and 62 follow §13.3. Listed in the Kim catalog with this shortcut summary since Step 47. |
| Ddalgak (`ddalgak`) | Planned (Steps 65+). An automation engine with its own rule model. | Steps 66, 73 and 81 follow §13.4. Listed in the Kim catalog with this shortcut summary since Step 47. |

## 2. Terms

| Term | Meaning |
| --- | --- |
| **Action** | One GUI operation a tool offers for a shortcut, named by an action ID (§3.1). |
| **Chord** | One key with zero or more modifiers pressed together: `Ctrl+Shift+T`. |
| **Sequence** | Up to four chords pressed one after another: `Ctrl+K Ctrl+S`, `Ctrl+B c`. |
| **Leader** | A named chord that starts a key table, such as Subak's prefix `Ctrl+B`. Sequences refer to it as `@prefix`, so changing the leader moves the whole table. |
| **Binding** | A sequence assigned to an action, with a scope and a trigger. An action can have zero, one or several bindings. |
| **Scope** | Where a binding is active: `global`, `app` or a named `context` (§4). |
| **Default layer** | The bindings a tool ships (its manifest). |
| **User layer** | The user's overrides, stored in `hotkeys.json` (§8). It contains only what the user changed. |
| **Effective binding** | What is active after layering, disabling and conflict handling. |
| **Backend** | How a tool receives keys: `focus`, `register` or `hook` (§4.2). |

## 3. Actions, contexts and leaders

### 3.1 Action IDs

- An action ID is lowercase, dotted, **without** the tool name: `terminal.new`, `tab.next`,
  `capture.region`. Pattern: `^[a-z][a-z0-9-]*(\.[a-z][a-z0-9-]*){1,3}$` (two to four segments,
  `group.verb` style).
- The tool is always known from context (the document's `tool` field, the CLI that is running).
  Cross-tool reports carry `tool` and `action` as separate fields.
- Action IDs are **stable**. They are never renamed or reused. A removed action keeps its ID
  reserved, and user overrides for it are kept and reported as `unknown_action` (§9.3).
- In reports, a leader appears in the `action` field as `@<leader>` (`@prefix`).

### 3.2 Contexts

A context is a named part of a tool's UI with its own bindings: Subak's `copy-mode`, Noeul's
`capture-overlay` and `pin` windows. A context is declared in the manifest with:

- `id` (`^[a-z][a-z0-9-]{0,31}$`) and a human `title`;
- `passthrough: true` when keys that no binding uses go to a program (Subak's `terminal`).
  Section 9.3 uses this flag for `shadows_terminal_input`.

### 3.3 Leaders

A leader is declared in the manifest with an `id`, a `title`, a `default` chord and its scope
(always `app` in v1). The user may change a leader's chord (§8.2). A sequence step `@<id>`
stands for the leader's current chord. Rules:

- A leader is a single chord: no `char:` key, no trigger other than `press`.
- After a leader, the tool waits for the next key without a timer (no idle wake-ups); `Esc`,
  a mouse click or focus loss cancels it. An unbound key after a leader is swallowed and a short
  hint is shown (tmux behavior, ADR 0009).
- A leader is never suppressed by another binding. Any binding that collides with a leader is a
  conflict (§9.3).

### 3.4 Hotkey manifest

Every conforming tool declares its actions in a **hotkey manifest**
([kim-hotkey-manifest-v1](contracts/schemas/kim-hotkey-manifest-v1.schema.json)). The manifest is
compiled into the tool; its actions, scopes and defaults appear in `<tool> hotkeys list --json`.
It SHOULD also be shipped as `hotkeys.manifest.json` next to `capabilities.json` in the
package. Kim can then show a tool's shortcuts and check cross-tool defaults **without starting
the tool**, as with capabilities (CLI_CONTRACT §9.2). Before a tool is installed, the Kim
catalog carries a summary of its `global` and `app` defaults (`tools[].shortcuts`,
[CATALOG_FORMAT §2.1](CATALOG_FORMAT.md), Step 47); once it is installed, Kim shows the
installed manifest and the user's `hotkeys.json` instead.

```json
{
  "schema": "kim.hotkey-manifest", "schema_version": 1, "hotkey_contract": 1,
  "tool": "subak", "version": "0.1.0", "backend": "register",
  "contexts": [ { "id": "terminal", "title": "Terminal pane", "passthrough": true } ],
  "leaders": [ { "id": "prefix", "title": "Prefix key", "default": "Ctrl+B" } ],
  "actions": [
    { "id": "terminal.new", "title": "New Terminal", "scopes": ["app"], "defaults": ["Ctrl+T"] },
    { "id": "window.new", "title": "New window", "scopes": ["app"], "defaults": ["@prefix char:c"] },
    { "id": "gui.show", "title": "Show Subak", "scopes": ["global"], "defaults": [] }
  ]
}
```

| Field | Rule |
| --- | --- |
| `backend` | The strongest input mechanism the tool may use (§4.2). |
| `actions[].scopes` | The scopes a user may choose for this action, default first: `global` and/or `app`. |
| `actions[].context` | Instead of `scopes`: the action only exists inside this context. |
| `actions[].defaults` | Default bindings (possibly none). Rules for defaults: §7.5. |
| `actions[].bindable` | `false` for actions whose keys are fixed by this contract or by the tool's safety design (e.g. Ddalgak's emergency chord). Listed, never rebindable. Default `true`. |
| `actions[].destructive` | `true` when the action kills, deletes or discards (Subak `terminal.close`). Such an action is confirmed by the tool exactly as when invoked from a menu; a shortcut never skips a confirmation. |

### 3.5 Reserved navigation actions

Every Kim GUI has these actions. Tools do not declare them; `hotkeys list` reports them with
`bindable: false` and `source: "reserved"` so the list is complete (`KIM-FR-041`):

| Action | Keys | From |
| --- | --- | --- |
| `nav.next-control` / `nav.previous-control` | `Tab` / `Shift+Tab` | DESIGN_SYSTEM §4.1 |
| `nav.next-region` / `nav.previous-region` | `F6` / `Shift+F6` | DESIGN_SYSTEM §4.1 |
| `nav.activate` | `Enter`, `Space` | DESIGN_SYSTEM §4.1 |
| `nav.cancel` | `Escape` | DESIGN_SYSTEM §4.1 |
| `nav.context-menu` | `Shift+F10`, `ContextMenu` | DESIGN_SYSTEM §4.1 |
| `window.close` | `Alt+F4` | Windows convention; a detach in Subak (ADR 0009) |

## 4. Scopes and input mechanisms

### 4.1 Scopes

| Scope | Active when | Typical use |
| --- | --- | --- |
| `global` | Anywhere on the user's desktop, while the tool runs and its global shortcuts are enabled | A tool's primary entry action: Noeul `F1` capture, `F3` floating paste |
| `app` | One of the tool's own windows has keyboard focus | Subak `Ctrl+T`, Kim Manager `F5` |
| `context` | A named context of the tool is active (§3.2) | Noeul `Escape` in the capture overlay; Subak copy mode |

A global binding works only while its tool runs. Shortcuts never start a tool that is not
running; that would need a resident process (`KIM-NFR-020`).

### 4.2 Backends

A tool declares the strongest mechanism it uses. Each tool owns its mechanism; there is **no
shared Kim hook, hotkey service or daemon** (`KIM-NFR-020`, ADR 0017).

| Backend | Mechanism on Windows | Can do | Used by |
| --- | --- | --- | --- |
| `focus` | The tool's own window events (winit/egui key events) | `app` and `context` scopes | Kim Manager |
| `register` | `focus`, plus `RegisterHotKey` on a message-only window for `global` bindings | Global single chords with generic modifiers, press only | Subak (`gui.show`), Noeul |
| `hook` | `register`, plus a low-level keyboard hook (`WH_KEYBOARD_LL`) installed only while the user has the engine enabled | Everything in §5, including global sequences, left/right modifiers, physical keys, release and hold | Ddalgak only |

What `register` cannot do is *documented* by Microsoft for `RegisterHotKey`: no left/right
modifier distinction, no key-up event, no sequences, and `Win` chords and `F12` are reserved for
the OS. A binding that needs more than the tool's backend is rejected with reason
`backend_unsupported` (§9.3). A tool MUST NOT add a hook just to support such a binding.

## 5. Key syntax

### 5.1 Canonical form

Bindings are written as text, in one canonical form. Tools accept the aliases of §5.6 as input
and always store and print the canonical form.

```text
binding  = step *3( " " step )                 ; one to four steps
step     = leader / chord / character
leader   = "@" leader-id                        ; @prefix
chord    = modifiers key
modifiers = [ ctrl "+" ] [ alt "+" ] [ shift "+" ] [ win "+" ]
          / "AltGr+" [ shift "+" ] [ win "+" ]  ; AltGr never combines with Ctrl or Alt
ctrl     = "Ctrl" / "LCtrl" / "RCtrl"
alt      = "Alt" / "LAlt" / "RAlt"
shift    = "Shift" / "LShift" / "RShift"
win      = "Win" / "LWin" / "RWin"
key      = named-key / "code:" physical-key
character = "char:" one-character               ; no modifiers
```

- Modifiers appear in the fixed order **Ctrl, Alt (or AltGr), Shift, Win**, each at most once.
  `Ctrl` matches either Ctrl key; `LCtrl`/`RCtrl` match only that side (likewise Alt, Shift,
  Win).
- The schemas carry the exact regular expression (`$defs.binding_syntax`). `cargo xtask
  schema-check` proves that it accepts every valid example and rejects the invalid ones.

### 5.2 Key names

| Kind | Names | Matched by |
| --- | --- | --- |
| Letters and digits | `A`–`Z`, `0`–`9` | **Logical key** (§6.1) |
| Function keys | `F1`–`F24` | Logical key |
| Editing and navigation | `Escape`, `Tab`, `Enter`, `Space`, `Backspace`, `Insert`, `Delete`, `Home`, `End`, `PageUp`, `PageDown`, `Up`, `Down`, `Left`, `Right` | Logical key |
| Layout-stable punctuation | `Plus`, `Minus`, `Comma`, `Period` (Windows defines these four virtual keys for every layout) | Logical key |
| Numeric keypad | `Numpad0`–`Numpad9`, `NumpadAdd`, `NumpadSubtract`, `NumpadMultiply`, `NumpadDivide`, `NumpadDecimal`, `NumpadEnter` | Logical key |
| Other keys | `PrintScreen`, `Pause`, `ScrollLock`, `ContextMenu`, `MediaPlayPause`, `MediaStop`, `MediaNext`, `MediaPrevious`, `VolumeUp`, `VolumeDown`, `VolumeMute` | Logical key |
| IME keys | `HangulMode`, `Hanja`, `KanaMode`, `Convert`, `NonConvert` | Always reserved (§7.3); named only so they can be rejected precisely |
| Physical keys | `code:` + a W3C UI Events `code` value: `KeyA`–`KeyZ`, `Digit0`–`Digit9`, `Minus`, `Equal`, `BracketLeft`, `BracketRight`, `Backslash`, `Semicolon`, `Quote`, `Backquote`, `Comma`, `Period`, `Slash`, `IntlBackslash`, `IntlRo`, `IntlYen`, `Lang1`–`Lang5` | **Physical position** (§6.1) |
| Characters | `char:` + one character: `char:?`, `char:%`, `char:"`, `char:D` | **Produced character** (§6.1) |

- Lock keys (`CapsLock`, `NumLock`) and bare modifiers are not keys in v1: they change keyboard
  state. Ddalgak's rule model may use them (§13.4).
- New key names are an additive change (§16).

### 5.3 Sequences

- Up to four steps. Each step after the first is pressed after all keys of the previous step
  were released.
- Sequences are allowed in `app` and `context` scope. In `global` scope only with the `hook`
  backend.
- While a sequence is pending, the tool waits without a timer. `Esc`, a click or focus loss
  cancels. A pending step that matches no binding is swallowed and a hint is shown.
- A binding that is a strict prefix of another binding in an overlapping scope is a conflict
  (`sequence_prefix`, §9.3); the longer one could never fire, or the shorter one would fire too
  early.

### 5.4 Triggers

A binding fires on one trigger. The text form always means `press`; the object form (§8.2) can
choose another.

| Trigger | Fires | Backends |
| --- | --- | --- |
| `press` (default) | When the last key of the last step goes down. Auto-repeat is ignored unless `repeat: true`. | all |
| `release` | When the chord is released, if no other key was pressed in between | `focus` (app/context), `hook` |
| `hold` | When the chord stays down for `hold_ms` (200–5000, default 500); an earlier release does nothing | `focus` (app/context), `hook` |

- `release` and `hold` apply to single-chord bindings only.
- A hold timer runs only while the chord is down; there is no timer when idle.
- `repeat: true` (press only) lets auto-repeat fire the action again, as for Subak's
  pane-resize keys after the prefix (tmux `-r`, ADR 0009 `repeat-time`).

### 5.5 Matching rules

- A chord matches when its key matches and exactly its modifiers are down. Generic modifiers
  accept either side; side-specific ones need that side.
- `Shift` is significant for named and physical keys (`Ctrl+Shift+T` ≠ `Ctrl+T`). It is
  implied for `char:` keys (`char:?` is whatever produces `?`).
- `char:` steps carry no modifiers and are allowed only in `context` scope or after a first step
  (for example after a leader). As the first step in `app` or `global` scope they are typing keys
  (§7.2).

### 5.6 Input aliases

The CLI and the binding editor accept these aliases and store the canonical form:

| Alias | Canonical |
| --- | --- |
| `Control`, `Ctl`, `C-` (tmux) | `Ctrl` |
| `Option`, `Meta`, `M-` (tmux) | `Alt` |
| `S-` (tmux) | `Shift` |
| `Windows`, `Super`, `Cmd` | `Win` |
| `Esc`, `Return`, `Del`, `Ins`, `PgUp`, `PgDn`, `ArrowUp`, … | `Escape`, `Enter`, `Delete`, `Insert`, `PageUp`, `PageDown`, `Up`, … |
| lower-case letters in a chord (`ctrl+t`) | upper case (`Ctrl+T`) |
| modifiers in any order (`Shift+Ctrl+T`) | canonical order (`Ctrl+Shift+T`) |

A single printable character after a leader (`Ctrl+B c`) is read as `char:c`. Unknown names
fail with `hotkey_invalid`; a tool never guesses.

## 6. Keyboard layouts and input methods

### 6.1 Logical, physical and character keys

| Kind | Identity | Windows mapping | Example on German QWERTZ | Allowed scopes |
| --- | --- | --- | --- | --- |
| Logical (`Z`, `F1`, `Plus`) | The key the active layout assigns to that virtual key | Virtual-key code (`VK_Z`, `VK_F1`, `VK_OEM_PLUS`) | `Ctrl+Z` is the key labeled Z | all |
| Physical (`code:KeyZ`) | A position on the keyboard, whatever the layout | Scan code (`MapVirtualKeyEx`) | `Ctrl+code:KeyZ` is the key labeled Y | `app`, `context`; `global` only with `hook` |
| Character (`char:?`) | The character the keystroke produces on the active layout | `WM_CHAR` / `ToUnicodeEx` | `char:?` is `Shift+ß` | `context`, and steps after the first |

- Letters follow Windows' virtual-key assignment (*documented*): on Latin layouts the labeled
  letter (QWERTZ `Z`, AZERTY `A`); on non-Latin layouts the US position (Korean 2-set `Ctrl+Z`
  is the key with ㅋ, Russian `Ctrl+Z` the key with Я). This matches how standard Windows
  shortcuts such as `Ctrl+Z` behave.
- Punctuation other than `Plus`, `Minus`, `Comma` and `Period` has no layout-stable virtual
  key, so it is written as `char:` (meaning) or `code:` (position).
- A layout switch changes nothing in the stored file. Logical bindings keep their meaning;
  physical bindings keep their position.
- `AltGr`: on layouts that have it, Windows reports AltGr as `LCtrl+RAlt` (*documented*). Kim
  writes it `AltGr`. Any `Ctrl+Alt+<key>` binding can collide with an AltGr character on some
  layout (German `AltGr+Q` is `@`, Polish `AltGr+A` is `ą`). Defaults MUST NOT use `Ctrl+Alt`
  with letter, digit or punctuation keys, and user bindings get the warning `altgr_collision`.

### 6.2 Input method editors (Korean first)

1. **Composition guard** (ADR 0009): while an IME composition is active, no binding fires; keys
   belong to the IME. The tool commits or cancels nothing on its own.
2. **IME mode keys are reserved** (§7.3): `HangulMode` (한/영), `Hanja` (한자), `KanaMode`,
   `Convert`, `NonConvert` and their physical forms `code:Lang1`–`code:Lang5`. Binding them
   would break text input.
3. **IME toggle chords** `Ctrl+Space`, `Shift+Space` and `Alt+code:Backquote` switch input modes
   on some systems, so they get the warning `ime_toggle`.
4. **Keys after a leader** are matched by their original virtual key even in Hangul mode. When
   the IME reports `VK_PROCESSKEY`, the tool reads the original key with `ImmGetVirtualKey`
   (*documented*) and does not start a composition for it. `char:` steps are matched against
   the character of the underlying keyboard layout, not the IME. With Korean input on,
   `Ctrl+B c` still opens a window.
5. **Global bindings** are matched by virtual key and do not depend on the IME mode
   (*documented* for `RegisterHotKey`).
6. **Capture** (§11.2) records chords, not text. A key that starts a composition is not
   captured; the editor asks the user to switch the IME off or to type the chord as text.

## 7. Reserved keys

Reserved keys can never be bound by a user, and defaults never use them. The `kim-hotkeys-v1`
and `kim-hotkey-manifest-v1` schemas reject the scope-independent ones (§7.1, §7.3, §7.4) and
the reference checker rejects the rest. On the CLI, a reserved chord fails with
`hotkey_reserved`.

### 7.1 Operating system (`os_reserved`)

| Chord | Owner | Scopes |
| --- | --- | --- |
| `Ctrl+Alt+Delete`, `Ctrl+Alt+NumpadDecimal` (also with Shift and side variants) | Secure Attention Sequence: always the OS, on the secure desktop | all |
| Any chord with `Win`, `LWin` or `RWin` (`Win+L` lock, `Win+D`, `Win+Space`, `Ctrl+Shift+Win+B`, …; written in canonical order) | Windows shell; reserved for the OS by the `RegisterHotKey` documentation | all |
| `Ctrl+Shift+Escape` | Task Manager, the user's emergency exit | all |
| `Ctrl+Escape` | Start menu | all |
| `Alt+Tab`, `Alt+Shift+Tab`, `Ctrl+Alt+Tab` (and with Shift) | Task switching | all |
| `Alt+Escape`, `Alt+Shift+Escape` | Window cycling | all |
| `Alt+Shift+PrintScreen` | High Contrast toggle (accessibility) | all |
| `F12` with any modifiers | Reserved for debuggers by the `RegisterHotKey` documentation | `global` |

Sticky Keys (five Shift presses), Filter Keys (holding right Shift) and other accessibility
gestures are not chords. They are never intercepted either (§12.3).

### 7.2 Kim GUI keys (`kim_reserved`)

| Keys | Why | Scopes |
| --- | --- | --- |
| `Alt+F4` | Closes the window (§3.5) | all |
| `Tab`, `Shift+Tab`, `Escape`, `Enter`, `Space`, `F6`, `Shift+F6`, `Shift+F10`, `ContextMenu` | Reserved navigation (§3.5) | `app`, `global` |
| Navigation and editing keys without Ctrl, Alt or Win: `Up`, `Down`, `Left`, `Right`, `Home`, `End`, `PageUp`, `PageDown`, `Backspace`, `Insert`, `Delete` | Move focus and edit text in every control | `app`, `global` |
| **Typing keys**: a first step without Ctrl, Alt or Win whose key types text (letters, digits, `Plus`, `Minus`, `Comma`, `Period`, numeric keypad, `code:` keys, any `char:` key) | Would make typing impossible | `app`, `global` |
| `Ctrl+Alt+Shift+Escape` | Ddalgak's emergency chord (§12.5), reserved for every tool | all |

A **context** may bind navigation and typing keys when it is designed for it (Noeul's capture
overlay uses `Escape` and arrow keys; Subak's copy mode uses letters). `Alt+F4` and the
emergency chord stay reserved everywhere. Keys without modifiers that remain allowed in `app`
and `global` scope are `F1`–`F24` (except `F6`), `PrintScreen`, `Pause`, `ScrollLock` and media
keys.

### 7.3 Input method keys (`ime_reserved`)

`HangulMode`, `Hanja`, `KanaMode`, `Convert`, `NonConvert`, `code:Lang1`–`code:Lang5`, with
any modifiers, in all scopes (§6.2).

### 7.4 Emergency chord

`Ctrl+Alt+Shift+Escape` belongs to Ddalgak's built-in emergency switch (§12.5). It is reserved
in every tool so that no Kim binding can hide it. This choice is **provisional**: Step 73
verifies it on real Windows; a change is recorded in an ADR 0017 note and a contract revision.

### 7.5 Rules for defaults

A tool's defaults MUST:

1. avoid every reserved chord (§7.1–§7.4);
2. use `global` scope only for the tool's primary entry action (`KIM-FR-042`). Today these are
   Noeul's `F1` and `F3`. Every other global action ships unbound;
3. not use the same global chord as another Kim tool's global default. Step 47 checks this
   across the catalog with the manifests;
4. not use `Ctrl+Alt` with letter, digit or punctuation keys (§6.1) or an IME toggle chord;
5. never be the only way to reach an action: every action is also in a menu, button or the
   command list (`KIM-NFR-012`).

## 8. Configuration and storage

### 8.1 Files

| File | Where (Windows) | Written by |
| --- | --- | --- |
| User layer | `%APPDATA%\mouv\<tool>\hotkeys.json` | the tool's GUI and `<tool> hotkeys set/disable/enable/reset/import` |
| Backup | `%APPDATA%\mouv\<tool>\hotkeys.json.bak` (last version before a reset, an import or a migration) | the same commands |
| Manifest | compiled in; optionally `hotkeys.manifest.json` in the installed version (§3.4) | release packaging |

Paths come from the `Paths` interface (ADR 0011); the Linux mapping (M4) is
`$XDG_CONFIG_HOME/mouv/<tool>/hotkeys.json`. Writes are atomic (write a temporary file, flush,
replace; [ADR 0006](adr/0006-state-and-configuration-storage.md)). The file holds no secrets
and no typed text.

### 8.2 Document (`kim.hotkeys`, version 1)

Schema: [kim-hotkeys-v1](contracts/schemas/kim-hotkeys-v1.schema.json). It holds **only the
user's changes**. An action without an override uses the tool's current defaults, so an update
can improve defaults for actions the user never touched, while the user's own choices stay
(`KIM-FR-044`).

```json
{
  "schema": "kim.hotkeys",
  "schema_version": 1,
  "tool": "subak",
  "enabled": true,
  "disabled_scopes": [],
  "leaders": { "prefix": "Ctrl+A" },
  "overrides": [
    { "action": "terminal.close", "disabled": true },
    { "action": "tab.next", "bindings": ["Ctrl+PageDown", "Ctrl+Tab"] },
    { "action": "pane.resize-up", "bindings": [ { "keys": "@prefix Ctrl+Up", "repeat": true } ] }
  ],
  "profiles": {
    "wsl": { "overrides": [ { "action": "terminal.new", "disabled": true } ] }
  }
}
```

| Field | Rule |
| --- | --- |
| `enabled` | `false` is the tool-wide switch: no binding is active, and no global hotkey is registered. Default `true`. |
| `disabled_scopes` | Turns off whole scopes, e.g. `["global"]` keeps app shortcuts but registers nothing system-wide. |
| `leaders` | Leader ID → chord; overrides the manifest default. |
| `overrides[]` | At most one entry per action. Either `bindings` (a non-empty list that **replaces** the default list) or `disabled: true` (the action has no binding). Resetting an action removes its entry. |
| binding | A string in canonical form, or an object `{ "keys", "scope", "trigger", "hold_ms", "repeat" }`. `scope` must be one of the action's `scopes`; context actions take no `scope`. |
| `profiles` | Named overlays (Subak's per-profile bindings, ADR 0009). The tool says which profile is active; profile overrides apply over the user layer. |

**Layering:** `default` → `user` → `profile` (later wins per action), then `disabled_scopes`
and `enabled`. Validation, conflict detection (§9) and remap offers run on the result.

**Unknown fields** are ignored and preserved when the tool rewrites the file (like the catalog,
CATALOG_FORMAT §2). Unknown **actions** are preserved and reported (`unknown_action`). The
manifest and configuration schemas are therefore open; the CLI output schema is closed
(CLI_CONTRACT §8).

### 8.3 Loading, safe mode and reload

1. **Missing file:** defaults apply.
2. **Unreadable or invalid file** (bad JSON, schema violation, reserved chord, duplicate
   override): the tool starts in **safe mode**. No global hotkey is registered, app and context
   defaults apply, and the GUI and `hotkeys list` say why (`hotkeys_invalid`). The file is never
   overwritten automatically; the user repairs it, or runs `hotkeys reset --all --yes`, which
   keeps a backup. A disabled global hotkey is therefore never re-enabled by a broken file
   (`KIM-SEC-023`).
3. **Newer `schema_version`:** read-only, as in safe mode, with `hotkeys_from_newer_version`.
   An older tool never rewrites a newer file.
4. **Start flags:** `<tool> --no-hotkeys` and the environment variable `KIM_NO_HOTKEYS` (any
   value except empty, `0` or `false`) start the GUI with every shortcut off, without touching
   the file.
5. **Reload while running:** the CLI writes the file; the running tool notices it through a
   file-change notification (`ReadDirectoryChangesW` on the folder, a blocking wait with no
   polling) or through its own IPC where it has one (Subak). It re-validates the file. A valid
   file is applied, so global hotkeys are released or registered at once. An invalid file is
   reported and the **previous effective bindings stay**.
6. **Fixed safety keys are not configuration.** `enabled: false`, `disabled_scopes`, safe mode
   and the start flags never turn off reserved navigation (§3.5) or Ddalgak's emergency chord
   (§12.5) while its hook is active.

### 8.4 Versioning and migrations

- Additive changes keep `schema_version` 1: new optional fields, new key names, new triggers,
  new conflict codes. Readers ignore unknown fields and treat unknown key names as invalid
  bindings, never as a crash.
- An incompatible change bumps `schema_version`. Migration is a chain of pure functions
  `vN → vN+1`. The tool backs the file up (`hotkeys.json.bak`) before the first migrated write,
  never migrates without writing a backup, and never rewrites a newer version (§8.3).
- Renamed actions are migrated by the tool (`old.id → new.id`); the old ID stays reserved.
- Subak's draft `config.toml` `[keys]` table (STATE_SCHEMA §2) is superseded by this file
  before it shipped, so no migration is needed. Behavior settings stay in `config.toml`
  (`close_terminal_shortcut`).

## 9. Conflicts and precedence

### 9.1 Who gets a keystroke

From first to last:

1. **The OS:** the secure desktop and its Secure Attention Sequence, and OS-reserved chords.
   Kim never sees these keys and never tries to (§12.1).
2. **Global hotkeys:** the first program that registered a chord gets it (*documented*
   `RegisterHotKey` behavior). Kim never unregisters, re-registers over or hooks around another
   program's registration (`KIM-SEC-020`).
3. **A hook backend** (Ddalgak) sees keys before they reach windows. Under `KIM-SEC-020` it
   passes on every chord that another program or Kim tool registered globally, unless the user
   explicitly bound that chord in Ddalgak. A check then reports `hook_overrides_global` (§13.4).
4. **The focused Kim tool**, in this order (ADR 0009):
   1. IME composition guard (§6.2);
   2. a pending sequence or leader;
   3. bindings of the innermost active context, then outer contexts;
   4. `app` bindings;
   5. the focused control or, in a passthrough context, the program.

Inside one tool, a binding that is reserved or invalid is inactive, and so is the **subject**
(§9.4) of a `duplicate_binding` or `sequence_prefix` conflict, while the other binding keeps
working. A user binding beats a default (`default_suppressed`), and nothing beats a leader.

### 9.2 What is checked

| Source | How it is read | Daemon? |
| --- | --- | --- |
| The tool's own defaults and user layer | in-process | no |
| Other installed Kim tools | `kim hotkeys check --all-tools` reads every installed tool's manifest and `hotkeys.json` (files only, no tool is started). A tool asks Kim through the CLI (`read` effect, CLI_CONTRACT §9.5–§9.6); without Kim installed, it skips this and warns. | no |
| Reserved keys | this contract (§7) | no |
| Other programs | Only at registration time: `RegisterHotKey` fails with `ERROR_HOTKEY_ALREADY_REGISTERED` (*documented*). Who owns the chord cannot be known and is never guessed. | no |

A check never registers a hotkey, never installs a hook, never starts a tool and never opens a
window (`KIM-FR-045`).

### 9.3 Conflict codes

| Code | Severity | When | Effect |
| --- | --- | --- | --- |
| `os_reserved` | error | A binding uses an OS-reserved chord (§7.1) | Refused; never registered |
| `kim_reserved` | error | A binding uses a Kim GUI key or typing key in a scope where it is reserved (§7.2) | Refused |
| `ime_reserved` | error | A binding uses an IME mode key (§7.3) | Refused |
| `invalid_binding` | error | Valid syntax, but not for this action or tool. `reason` is one of: `scope_not_allowed`, `backend_unsupported`, `trigger_unsupported`, `unknown_leader`, `not_bindable` | Inactive |
| `duplicate_binding` | error | Two bindings of one tool have the same keys in overlapping scopes, or a binding equals a leader | The subject is inactive |
| `sequence_prefix` | error | One binding is a strict prefix of another in overlapping scopes, or starts with a leader's chord without using `@leader` | The subject is inactive |
| `cross_tool_global` | error | Two Kim tools bind the same global chord | At run time the first tool to register wins; the other reports `taken` |
| `global_shadows_app` | warning | One tool's global chord equals another Kim tool's `app` or `context` binding (or its first step) | The shadowed binding never fires while the other tool runs |
| `taken` | warning | Registering a global chord failed because another program owns it | Inactive until the user changes it or restarts the tool after the other program released it; never retried in the background |
| `hook_overrides_global` | warning | A user-bound Ddalgak trigger equals another tool's global chord | Ddalgak's rule sees the key first (§9.1) |
| `os_shortcut` | warning | A well-known OS default that programs can take: `PrintScreen`, `Alt+PrintScreen`, `Alt+Space` | Works, but replaces the OS behavior while the tool runs |
| `altgr_collision` | warning | `Ctrl+Alt` (or `LCtrl+RAlt`) with a letter, digit, punctuation or `code:` key | May block an AltGr character on some layouts (§6.1) |
| `ime_toggle` | warning | `Ctrl+Space`, `Shift+Space`, `Alt+code:Backquote` | May block switching the input method |
| `unknown_action` | warning | An override names an action this tool version does not have | Ignored, kept in the file |
| `context_override` | info | A context binding has the same keys as an `app` binding of the same tool | Inside the context the context binding wins (intended) |
| `default_suppressed` | info | A default has the same keys as a user binding of another action of the same tool | The user binding wins; the default is inactive |
| `shadows_terminal_input` | info | In a tool with a passthrough context, an `app` chord (a leader included) of exactly `Ctrl` or `Alt` plus a letter | The program does not receive that chord; `@leader <chord>` sends it (ADR 0009) |

**Overlapping scopes** (within one tool): `global` overlaps every scope; `app` overlaps `app`;
a context overlaps itself. An `app` binding and a context binding with the same keys are a
`context_override`, not a duplicate. Sequences that start with `@leader` are compared with each
other in that leader's table; the leader's chord is compared with all other first steps.
Across tools, only `global` bindings can collide.

### 9.4 Subject, other side and order

Each conflict is one entry in the report, never two mirrored entries:

- **`tool`, `action`, `keys`, `scope`, `source`** describe the **subject**: the binding the
  report is about and that the remap offer changes.
- **`with`** describes the other side: another Kim binding `{tool, action, keys, scope}`, a
  reserved chord `{reserved: os|kim|ime|emergency, keys, name}`, another program
  `{other_program: true, keys}`, or `null` for single-binding warnings.
- **Choosing the subject:** for `global_shadows_app`, the shadowed binding; for
  `context_override`, the hidden `app` binding; for `default_suppressed`, the suppressed
  default. Otherwise the binding with the **higher layer** (profile, then user, then default);
  a leader is never the subject of a conflict with another binding of its own tool; on a tie,
  the shorter binding for `sequence_prefix`, else the one whose `tool`, then `action`, sorts
  last.
- **Order:** errors, then warnings, then infos; within a severity by `tool`, `action`, `keys`
  and `code`. Reports are deterministic, so fixtures and tests can compare them exactly.

### 9.5 Remap offers (non-destructive)

Every error and warning comes with `offers`, in this order:

1. up to two `rebind` suggestions for a single-chord subject: the subject's chord with `Shift`,
   then `Ctrl+Shift`, then `Alt+Shift` added, keeping only candidates that are valid for the
   action and backend, not reserved, not used by any other binding of the same tool in an
   overlapping scope (so no other action loses its key), and free of any error or warning
   against other tools' bindings and every chord known to be taken. Suggestions only add
   modifiers, so a chord whose problem is a modifier it already has (`altgr_collision`) gets
   none;
2. `disable` (not for a leader: a leader is changed, never disabled);
3. `keep`, for warnings only: the user accepts the conflict;
4. `reset` instead of the above for `unknown_action` and `invalid_binding`.

Infos carry no offers. **An offer is never applied by itself.** The GUI shows the offers and
the user picks one; the CLI prints them as hints (§10.5).

### 9.6 The override workflow

When the user sets a binding (`hotkeys set`, or the binding editor):

1. The tool canonicalizes the input (§5.6) and rejects bad syntax (`hotkey_invalid`), unknown
   actions (`action_not_found`), reserved chords (`hotkey_reserved`) and bindings beyond its
   backend (`hotkey_unsupported`).
2. It builds the new user layer and runs the full check (§9.2), including other Kim tools when
   it can.
3. **Error conflicts involving the action refuse the change** (`hotkey_conflict`, exit 7) and
   nothing is written. The user resolves it explicitly:
   - `--replace` (same tool only): the conflicting binding is taken away from the other action
     of **this** tool (that action is disabled if nothing is left). The change lists both
     actions. Leaders are never replaced this way; change the leader itself.
   - `--allow-conflict` (cross-tool global only): the user accepts that the first tool to
     register wins. The report keeps the error.
4. Warnings and infos never block. They are printed (CLI) or shown with their offers (GUI).
5. `--dry-run` performs steps 1–4 and writes nothing.
6. **A tool never edits another tool's configuration.** For a cross-tool conflict the hint
   names the exact command for the other tool (`noeul hotkeys set capture.region Shift+F1`).
7. A running tool picks the change up as in §8.3.

## 10. Headless `hotkeys` commands

Every conforming tool implements this command group following CLI_CONTRACT v1: help, streams,
`--json` envelope, exit codes, `--dry-run`, `--yes`, `--no-input`. Data shapes and error codes
are in [hotkeys-cli-v1](contracts/schemas/hotkeys-cli-v1.schema.json) (closed schema, golden
examples in `contracts/examples/hotkeys-cli-v1/`). **No `hotkeys` command opens a window,
registers a hotkey, installs a hook or starts the GUI** (`KIM-FR-012`, `KIM-FR-045`).

### 10.1 Commands

| Command | Effect | Dry run | Needs `--yes` | What it does |
| --- | --- | --- | --- | --- |
| `<tool> hotkeys list [--scope <s>]` | read | — | — | Every action with its effective bindings, defaults, source and state; leaders; the reserved navigation actions |
| `<tool> hotkeys show <action>` | read | — | — | One action with its bindings and every conflict that involves it |
| `<tool> hotkeys set <action> <keys>… [--scope <s>] [--trigger <t>] [--hold-ms <n>] [--repeat] [--profile <p>] [--replace] [--allow-conflict]` | write | yes | — | Replace the action's bindings (§9.6). `<action>` may be `@<leader>` to change a leader. |
| `<tool> hotkeys disable <action>` / `--all` / `--scope <s>` | write | yes | — | Disable one action, the whole tool (`enabled: false`) or a scope |
| `<tool> hotkeys enable --all` / `--scope <s>` | write | yes | — | Turn the tool-wide switch or a scope back on (per action: `reset`) |
| `<tool> hotkeys reset <action>` | write | yes | — | Remove the user's override; the default applies again |
| `<tool> hotkeys reset --all` | destructive | yes | yes | Discard all user bindings; keeps `hotkeys.json.bak` |
| `<tool> hotkeys check [--all-tools] [--min-severity <s>]` | read | — | — | The conflict report (§9). `--all-tools` includes other installed Kim tools. |
| `<tool> hotkeys export` | read | — | — | Print the user layer (`kim.hotkeys` document) |
| `<tool> hotkeys import <file>` | write | yes | — | Validate and replace the user layer; keeps a backup |
| `kim hotkeys check --all-tools` | read | — | — | The cross-tool report for every installed tool, from files only (§9.2) |

**New verbs and why** (CLI_CONTRACT §2 asks for a reason): `disable`/`enable` name the switch
users look for (`KIM-FR-044`); `set <action> --none` would hide it. `reset` restores defaults,
which `set` cannot express without repeating them. `export`/`import` move a document as a whole,
unlike `show`/`set`, which work per action.

### 10.2 Output (`--json` data)

```json
{ "ok": true, "command": "hotkeys.list", "schema_version": 1, "warnings": [],
  "data": { "tool": "subak", "enabled": true, "config": { "path": "…\\hotkeys.json", "state": "user" },
            "disabled_scopes": [], "leaders": [ { "id": "prefix", "keys": "Ctrl+B", "source": "default" } ],
            "actions": [ { "id": "terminal.new", "title": "New Terminal", "scope": "app",
                           "bindable": true, "source": "default", "defaults": ["Ctrl+T"],
                           "bindings": [ { "keys": "Ctrl+T", "scope": "app", "trigger": "press",
                                           "active": true, "registration": "not_applicable" } ] } ] } }
```

- `config.state`: `default` (no file), `user`, `invalid` (safe mode), `newer_version`.
- `source`: `default`, `user`, `profile`, `disabled`, `unbound`, `reserved`.
- `registration` (global bindings): `registered`, `taken`, `not_running` or `unknown`. Only a
  tool that can ask its running instance reports `registered`/`taken` (Subak through IPC);
  others say `unknown`. Non-global bindings say `not_applicable`.
- `hotkeys.check` data is the conflict report: `{ "tools": [...], "conflicts": [...],
  "summary": { "errors", "warnings", "infos" } }`, with conflicts as in §9.3–§9.5.
- Changing commands return `{ "tool", "dry_run", "written", "path", "changes": [ { "action",
  "before", "after" } ], "conflicts": [...] }`.

### 10.3 Exit codes

A successful `check` exits 0 even when it finds conflicts; scripts read `summary`. A refused
change exits with its error's class (CLI_CONTRACT §5).

### 10.4 Error codes for the `hotkeys` commands

These codes are registered in CLI_CONTRACT §6 for the `hotkeys` command group. `kim`
implements them since Step 46 (`ErrorCode` in `crates/kim/src/error.rs`).

| Code | Exit | Meaning |
| --- | --- | --- |
| `hotkey_invalid` | 2 | The keys do not parse, use an unknown key or modifier, or use a scope or leader the action does not have. |
| `hotkey_reserved` | 2 | The chord is reserved by the OS, by Kim's GUI rules or by the IME (§7), or the action is not bindable. |
| `action_not_found` | 3 | The tool has no action with this ID. |
| `hotkeys_invalid` | 4 | The stored `hotkeys.json` is malformed or invalid; the tool runs in safe mode (§8.3). |
| `hotkey_unsupported` | 5 | The binding needs a capability this tool's backend does not have (§4.2). |
| `hotkeys_from_newer_version` | 5 | `hotkeys.json` was written by a newer version; it is read-only. |
| `hotkey_conflict` | 7 | The change conflicts with another binding; resolve with `--replace`, `--allow-conflict`, or other keys (§9.6). |

Existing codes keep their meaning: `confirmation_required` (7) for `reset --all` without `--yes`,
`io` and `permission_denied` (1) for file problems, `not_installed` (3) for
`kim hotkeys check --all-tools` on a tool that disappeared.

### 10.5 Human output

`hotkeys list` prints a table (action, keys, scope, state). A refused `set` prints, on stderr:

```text
noeul: error: F1 conflicts with another binding
  - cross_tool_global: ddalgak engine.toggle uses F1 (global)
hint: try Shift+F1 or Ctrl+Shift+F1, or run `ddalgak hotkeys set engine.toggle <keys>`;
      --allow-conflict keeps both (the first tool to register wins)
```

## 11. GUI behavior

### 11.1 The shortcut list (`KIM-FR-041`)

- Reachable from the tool's settings and from the keyboard help overlay (`F1` where `F1` is the
  tool's help, `Ctrl+B ?` in Subak; DESIGN_SYSTEM §4.1).
- Shows every action, grouped by scope and context, with its current keys, source (default,
  user, disabled, unbound), state and conflicts (by code and sentence, not by color alone).
  Reserved navigation actions are shown, marked as fixed.
- Mouse gestures (Noeul's double-click to pin) are listed in the same view, with their keyboard
  alternative.
- Each row offers **Change**, **Disable** and **Reset**; the view offers **Reset all** (with
  confirmation) and the tool-wide **Keyboard shortcuts: on/off** switch.

### 11.2 The binding editor: keyboard-only capture (`KIM-NFR-021`)

1. Focus a row and press `Enter` (or activate **Change**). The row reads "Press the new
   shortcut…" and screen readers announce it.
2. While capturing, **the tool dispatches nothing**: its app and context bindings are off and
   `WM_HOTKEY` for its own global hotkeys is ignored, so the action being captured never fires.
3. The first complete chord (a key plus the held modifiers) is recorded on key down and shown
   in canonical form. **Add step** records the next chord of a sequence.
4. **Escape cancels**; `Tab`/`Shift+Tab` leave the row without change. Focus is never trapped,
   so these keys cannot be captured. They are reserved anyway, and a context binding that needs
   `Escape` is entered as text.
5. **Type instead** opens a text field for the canonical syntax (with aliases). It is the
   alternative for chords the OS or another program intercepts before the tool can see them,
   for IME users and for screen-reader users.
6. The result is validated and checked at once (§9.6). Conflicts are announced with their
   offers; **Save** stays unavailable while an error conflict is unresolved.
7. Capture records chords, never text. It is impossible while a password field has focus and
   stops when the window loses focus.

### 11.3 Focus safety

- A global action never steals focus unless its purpose is to show a window (Noeul's capture
  overlay, `gui.show`). It never sends keys to another application. Only Ddalgak does that, and
  only for the user's own rules.
- A tool that shows a window from a global hotkey does not show it while the session is locked
  or the secure desktop is active.

## 12. Safety

### 12.1 Secure desktop, UAC and sign-in (`KIM-SEC-021`)

- The secure desktop (the Ctrl+Alt+Del screen, the UAC consent prompt, sign-in and the lock
  screen) is a separate desktop. Hotkeys and hooks of the user's desktop do not receive its
  input (*documented*). Kim MUST NOT try to change that: no service, no SYSTEM component, no
  `UIAccess` manifest, no process on the Winlogon desktop, no credential-provider hooks.
- Windows of elevated programs may not deliver keys to a non-elevated tool's hook or window
  (UIPI, *documented*). Kim never elevates to get around this (`KIM-SEC-004`). The GUI says
  that shortcuts can be unavailable while an administrator window has focus.

### 12.2 Password fields

- In a Kim tool's own password fields (Subak's SSH prompt, Step 34) only reserved navigation
  and paste work; no binding with `char:` or typing keys matches, and capture is impossible.
- A hook backend passes keys through unchanged while the focused element is a password field
  (`ES_PASSWORD` edit style or UI Automation `IsPassword`, best effort; *documented*, verified
  by Ddalgak's Steps 66 and 82). Only the emergency chord stays active. Text expansion never
  types into a password field.

### 12.3 No keylogging (`KIM-SEC-022`)

- `focus` and `register` backends only receive bound chords or the tool's own window input.
  They store no keystrokes.
- A `hook` backend inspects each key only to decide whether a rule matches. It never logs,
  stores or transmits keys or text. Diagnostics name action and rule IDs, never typed keys.
- Logs and `--json` never contain typed text, captured passwords or clipboard contents
  (`KIM-SEC-013`).
- Accessibility gestures (Sticky Keys, Filter Keys, Narrator, Magnifier) are never intercepted
  or swallowed.

### 12.4 Full disable switch (`KIM-FR-044`, `KIM-SEC-023`)

The same switch is reachable everywhere:

- GUI: **Keyboard shortcuts: on/off** in settings, and in the tray menu where the tool has one;
- CLI: `<tool> hotkeys disable --all` (a running tool releases its global hotkeys within one
  file-change notification, §8.3);
- start: `--no-hotkeys` or `KIM_NO_HOTKEYS`;
- `hotkeys.json` with `"enabled": false`.

### 12.5 Emergency recovery

| Situation | Recovery |
| --- | --- |
| A global binding fires unwanted actions | `<tool> hotkeys disable <action>` or `--all` from any console; or end the tool (the OS releases its hotkeys when the process ends, *documented*) |
| The configuration is broken | Safe mode (§8.3): no global hotkeys; fix it, or `hotkeys reset --all --yes` |
| The tool cannot be reached (frozen window) | `Ctrl+Shift+Escape` (reserved, §7.1) opens Task Manager to end it |
| A hook backend misbehaves (Ddalgak) | The non-remappable emergency chord `Ctrl+Alt+Shift+Escape` (§7.4) removes the hook, releases held keys, cancels running actions and pauses the engine before any rule runs. Then `ddalgak hotkeys disable --all`, `--no-hotkeys`, or ending the process. Windows also removes a low-level hook that stops responding (*documented* `LowLevelHooksTimeout` behavior); Ddalgak never relies on that alone. Step 73 specifies and verifies it. |

A tool never re-enables bindings the user disabled. Not after a crash, a reboot, an update, an
invalid configuration or a failed registration.

## 13. Tool mappings and the design distinction

### 13.1 Subak (ADR 0009)

Subak's keys map onto the contract without changing ADR 0009's dispatch order:

| ADR 0009 | Contract |
| --- | --- |
| `Ctrl+T`, `Ctrl+W`, `Ctrl+Tab`, `Ctrl+Shift+Tab` | `app` actions `terminal.new`, `terminal.close` (destructive), `tab.next`, `tab.previous` |
| `Ctrl+B` prefix | leader `prefix`; the table is `@prefix …` sequences: `window.new` `@prefix char:c`, `window.next` `@prefix char:n`, `window.previous` `@prefix char:p`, `window.select-0…9` `@prefix 0`…`@prefix 9`, `pane.split-right` `@prefix char:%`, `pane.split-down` `@prefix char:"`, `pane.focus-up/down/left/right` `@prefix Up/Down/Left/Right`, `pane.kill` `@prefix char:x`, `window.kill` `@prefix char:&`, `pane.zoom` `@prefix char:z`, `client.detach` `@prefix char:d`, `window.rename` `@prefix char:,`, `terminal.rename` `@prefix char:$`, `help.show` `F1` and `@prefix char:?` |
| `Ctrl+B Ctrl+B` sends a literal `Ctrl+B` | action `input.send-prefix` = `@prefix @prefix`; it follows the leader when the user changes it |
| `Ctrl+B Ctrl+W/Ctrl+T/Ctrl+Tab` send the shadowed key | Pass-through rule: `@prefix` followed by the current keys of any `app` binding sends those keys to the program. It is derived from the bindings, so it needs no configuration and follows rebinds. |
| Unbound key after the prefix: swallowed, hint shown | §3.3 |
| Every shortcut rebindable or disabled (`SUBAK-FR-034`) | §8, §10 |
| "Terminal-first mode" | A preset that writes `disabled` overrides for the four single-chord `app` actions. It is visible in the list and undone by `reset`. |
| Bindings per profile | `profiles` overlay (§8.2) |
| `repeat-time` 500 ms for resize keys | `repeat: true` on those bindings (§5.4) |
| Context `terminal` | `passthrough: true`: unbound keys go to the PTY (stage 4) |

Subak has **no global default**. `gui.show` (show the GUI or the floating icon) is a global
action without a default; the user may bind it. The `[keys]` table of the draft `config.toml`
is superseded by `hotkeys.json` (§8.4). Floating icon and session manager actions (Step 18) are
added as actions in the same way.

**Hand-off (Step 46 → Steps 17 and 23).** Step 46 did not change `crates/subak`. Subak's GUI
Steps use the shared `kim-hotkeys` crate exactly as Kim does: ship the manifest
(`docs/contracts/examples/kim-hotkey-manifest-v1/subak.json` is the starting point) as
`hotkeys.manifest.json`, implement `subak hotkeys …` with `kim_hotkeys::{store, edit, view,
check}` (see `crates/kim/src/hotkeys_cmd.rs`), match keys with `keys::steps_overlap` (see
`crates/kim-manager/src/shortcuts.rs`, including sequences, the IME guard and reload), and
register `gui.show` with `register::GlobalHotkeys` only while the GUI runs and the binding is
enabled. Leaders, the pass-through rule and `copy-mode` dispatch remain Subak's own code.

### 13.2 Kim Manager

Implemented in Step 46 (§17). The minimum for conformance:
`catalog.refresh` `F5` (app), `help.show` `F1` (app, shows the shortcut list), and
`tool.install`, `tool.update`, `tool.uninstall` without defaults (`destructive` where it
applies). Each opens the same confirmation as its button; a shortcut is an explicit user action,
but it never skips consent (`KIM-SEC-001`). The tool ID for its shortcuts is `kim`
(`kim hotkeys …`).

### 13.3 Noeul: shortcuts are tool operations

- Noeul's shortcuts invoke Noeul's own operations: `capture.region` `F1` (global) starts
  rectangle selection; `paste.float` `F3` (global) pins the clipboard image as a floating
  window. These are its primary entry actions, the only allowed kind of global default (§7.5).
- Overlay and pin keys are **contexts** (`capture-overlay`, `pin`): `Escape` cancels, `Ctrl+C`
  copies, `Ctrl+S` saves, arrow keys nudge. They exist only while that window is focused.
- Backend `register`: no hook, nothing resident beyond Noeul itself while it runs, nothing at
  all when it is not running. Double-click to pin is a gesture with a keyboard alternative
  (Steps 48, 51, 61 decide the exact keys).
- `F1` as a global default makes `F1` unavailable to every other program while Noeul runs,
  including Subak's help. `hotkeys check --all-tools` reports exactly that
  (`global_shadows_app`; scenario 02). The user chooses; Subak's help stays reachable through
  `Ctrl+B ?`.

### 13.4 Ddalgak: a user-defined automation engine

- Ddalgak is not a set of fixed tool operations. The user defines rules (triggers → actions:
  remaps, text, launches, macros), so it needs a **more capable input backend** (`hook`):
  left/right modifiers, physical keys, release and hold, global sequences, injected-input
  detection.
- That capability belongs to Ddalgak alone and is **opt-in**: the hook exists only while the
  user has the engine enabled, it is visible (tray or status), and it follows §12 entirely.
  Other tools never require it, call into it, or run inside it.
- Ddalgak's own GUI operations (`engine.toggle`, `engine.pause`, `editor.open`; all global
  without defaults) are ordinary actions under this contract. The emergency switch
  `engine.emergency-off` is `bindable: false` and fixed to `Ctrl+Alt+Shift+Escape` (§7.4).
- **Rules are not stored in `hotkeys.json`.** For conflict checks, Ddalgak lists each enabled
  rule trigger in `hotkeys list --json` as a read-only action `rule.<rule-id>` (scope `global`,
  `source: "user"`), edited through Ddalgak's own rules CLI (Step 81). Then `kim hotkeys check
  --all-tools` sees rule triggers that shadow Kim tools' global keys
  (`hook_overrides_global`). Rule triggers may use what the hook backend supports, but never
  the chords that §7.1, §7.3 and §7.4 reserve.
- **Step 65** specified the rule model ([DDALGAK_RULE_MODEL](DDALGAK_RULE_MODEL.md),
  [ADR 0022](adr/0022-ddalgak-rule-model-and-safety.md)) and finalized Ddalgak's manifest: the
  four global actions above plus the `rules` and `playground` contexts of its editor. Rule
  triggers add lock keys and bare side-specific modifiers to §5, use no `char:` keys or
  leaders, and also never use `Alt+F4` or `F12`. Ddalgak's GUI actions avoid the `rule.` prefix,
  which stays reserved for rule triggers.

### 13.5 No shared hook or daemon

No tool loads another tool's code, joins a shared hook, or depends on a resident Kim process
for shortcuts. The only cross-tool interaction is reading manifests and `hotkeys.json` files
during an explicit check (§9.2). A shared Rust crate for parsing and checking (Step 46) is a
library compiled into each tool: no process, no GUI dependency in headless binaries (ADR 0002,
`cargo xtask deps-check`).

## 14. Portability and verification status

| Area | Windows | Linux (M4, not claimed) | Verified by |
| --- | --- | --- | --- |
| `app`/`context` keys | winit/egui key events (logical key, physical scan code, text) | same toolkit | Step 46 (library and Kim Manager), Steps 17 and 23 (Subak) |
| `global` (`register`) | `RegisterHotKey`/`UnregisterHotKey`, `MOD_NOREPEAT`. Step 46 registers on a dedicated thread's message queue (null window handle) instead of a message-only window: no window at all, same API and semantics | X11 `XGrabKey`; Wayland only through the desktop portal's global-shortcuts interface, where the compositor and the user decide | **Step 46: tested on Windows 10 19045** (register, `taken`, release on drop, lost registration; obscure chords only); Steps 64 and 83 (Linux) |
| `hook` | `WH_KEYBOARD_LL` with `LLKHF_INJECTED` plus a `dwExtraInfo` marker to tell its own injected input apart (Step 66: `crates/ddalgak`, [DDALGAK_ENGINE](DDALGAK_ENGINE.md)) | to be designed | **Step 66: tested on Windows 10 19045** (observe-only hook, injected F13–F17, real app); emergency chord on physical keys: Step 73 |
| IME after a leader | `VK_PROCESSKEY` + `ImmGetVirtualKey` | to be designed | Step 23 |
| Layout mapping | `MapVirtualKeyEx`, `ToUnicodeEx` with the foreground layout. Step 46: app keys use the logical and physical keys winit/egui report (no own `MapVirtualKeyEx` call); global logical keys map to virtual keys, which Windows assigns per layout | XKB | Step 46 for the US layout of this machine; other layouts **not tested** (Steps 17/23 with Korean IME, Step 61) |
| Secure desktop, UIPI | not reachable by design | not applicable | inspection in each tool step, manual check in Steps 61 and 82 |

**Nothing in this table was run in Step 45.** Step 46 ran the rows it names (§17). Step 45 is a specification. Its machine checks
cover the syntax, the reserved keys, the conflict rules and the override workflow (§15), not OS
behavior. Stored bindings contain no Windows virtual-key numbers (`KIM-NFR-022`). Each platform
maps the key names of §5.2.

## 15. Machine-checkable fixtures

| Artifact | What it pins down |
| --- | --- |
| [kim-hotkeys-v1](contracts/schemas/kim-hotkeys-v1.schema.json) + `contracts/examples/kim-hotkeys-v1/` | The user layer. Valid: per-tool overrides, disabled bindings, leaders, profiles, triggers. Invalid: unknown modifier, reserved OS chord, `Win` chord, missing action ID, non-canonical order, `bindings` and `disabled` together, too many steps, `hold_ms` without `hold`, newer `schema_version`, IME key. |
| [kim-hotkey-manifest-v1](contracts/schemas/kim-hotkey-manifest-v1.schema.json) + examples | Declared actions for Kim Manager, Subak (from `SUBAK-FR-033` and ADR 0009), Noeul and Ddalgak (illustrating the issues' requested defaults; their Steps own the final lists). |
| [hotkeys-cli-v1](contracts/schemas/hotkeys-cli-v1.schema.json) + examples | The `--json` envelopes of §10 and the error codes of §10.4 with their exit codes. |
| [hotkey-scenarios-v1](contracts/schemas/hotkey-scenarios-v1.schema.json) + `contracts/examples/hotkey-scenarios-v1/` | Conflict and override **scenarios**: input manifests, user layers, chords already taken by other programs, an optional `set`/`disable`/`reset` request, and the exact expected report or outcome. |

`cargo xtask schema-check` validates every example against its schema. For the scenarios it
also validates each embedded manifest, user layer and expected report against their own
schemas. A small **reference checker** (`xtask/src/hotkeys.rs`) then recomputes every expected
report and outcome from the inputs and fails on any difference (conflict `message`s are not
compared). It also checks that the shared `$defs` of the four hotkey schemas are identical and
that its own reserved-chord rules agree with the schemas' `reserved_free`. The checker is
deliberately limited to this contract's rules: no OS calls, no `profiles`, no Ddalgak rule
triggers (so no `hook_overrides_global`), no input aliases, no `set @<leader>`, and
`char:`/named key overlap only for ASCII letters and digits. It exists so that the fixtures are
proven consistent, and it is not a library. Step 46's real implementation MUST pass the same
scenarios; once it does, Step 46 may replace the reference checker with it. **It does**: the
`kim-hotkeys` library recomputes all 17 scenarios in `crates/kim-hotkeys/tests/contract.rs`.
Both are kept on purpose, so the fixtures are checked by two independent implementations.

| Scenario | Shows |
| --- | --- |
| `01-subak-defaults` | Subak's defaults are conflict-free; the shadowed terminal keys are reported as infos with the pass-through hint |
| `02-noeul-f1-shadows-subak-help` | A global default of one tool shadows an app key of another (warning with remap offers) |
| `03-cross-tool-global-duplicate` | Two tools want the same global chord (error) |
| `04-user-binding-beats-default` | A user binding wins over a default of the same tool (`default_suppressed`) |
| `05-binding-collides-with-leader` | A binding equal to the prefix key is an error; the leader is never the subject |
| `06-taken-by-another-program` | A failed registration (`taken`) with remap offers; nothing is stolen |
| `07-layout-and-ime-warnings` | `altgr_collision` and `ime_toggle` |
| `08-invalid-reserved-and-unknown` | Scope-dependent `kim_reserved` (`F6` in app scope) and `os_reserved` (global `F12`), `invalid_binding` (`backend_unsupported`) and `unknown_action` |
| `09-disabled-switches` | `enabled: false` and `disabled_scopes` remove conflicts entirely |
| `10-context-override` | Context bindings win inside their context (`context_override`) |
| `11-set-refused-reserved` | `set` with `Win+L` is refused with `hotkey_reserved`; nothing is written |
| `12-set-refused-conflict` | `set` onto the leader is refused with `hotkey_conflict` |
| `13-set-replace-same-tool` | `--replace` takes a user binding away from another action of the same tool and lists both changes |
| `14-set-allow-cross-tool` | `--allow-conflict` keeps a cross-tool conflict; the other tool's file is untouched |
| `15-set-dry-run` | `--dry-run` writes nothing |
| `16-reset-restores-default` | `reset` removes the override, so the default is active again |
| `17-disable-one-action` | `disable` writes a disabled override for one action |

## 16. Versioning of this contract

- This is **hotkey contract v1**; manifests say `"hotkey_contract": 1`.
- Additive changes keep v1: new key names, triggers, conflict codes, reserved chords (a newly
  reserved chord makes existing user bindings report `os_reserved` and become inactive; it never
  changes them), CLI fields, commands and error codes. Consumers ignore unknown fields and
  treat unknown conflict codes by their severity.
- Incompatible changes (syntax meaning, layering, precedence) need contract v2, a new schema
  major and a migration (§8.4), recorded in an ADR.

## 17. Implementation (Step 46)

| Part | Where | Notes |
| --- | --- | --- |
| Shared library | `crates/kim-hotkeys` (headless; listed in `cargo xtask deps-check`) | Key model and aliases (§5), manifest and user layer (§3.4, §8.2) keeping unknown fields, safe mode, atomic writes, backups and a migration chain (§8.3–§8.4; v1 is the first version, so the shipped chain is empty and the mechanism is tested with a test chain), reserved keys (§7), layering with profiles, conflicts, subjects, order and offers (§9), the override workflow and switches (§9.6, §12.4), `list`/`show` views (§10.2), `RegisterHotKey` backend (§4.2) |
| `kim hotkeys …` | `crates/kim/src/hotkeys_cmd.rs`, `crates/kim/src/hotkeys.rs` | All commands of §10.1, including `export`/`import` and `--profile`; `check --all-tools` reads `hotkeys.manifest.json` of each installed version and its `hotkeys.json`, never starts a tool ([KIM_CLI §5](KIM_CLI.md#5-keyboard-shortcuts)) |
| Kim Manager | `crates/kim-manager/src/shortcuts.rs` | `focus` backend from egui key events; re-reads `hotkeys.json` when the window regains focus instead of a `ReadDirectoryChangesW` watcher (an event, no polling; a focus-only tool has no global registration to release); `--no-hotkeys`, `KIM_NO_HOTKEYS` |
| Not implemented | — | The binding editor with keyboard capture (§11.2); `hook_overrides_global` (no tool has rules yet, Step 81); `char:`/named overlap beyond ASCII letters and digits (as in the reference checker) |

**Privilege boundaries (documented, not tested).** The `register` backend cannot see the secure
desktop, UAC prompts or the lock screen, and Windows does not deliver keys from an elevated
window to a non-elevated tool (UIPI); Kim never elevates or installs a service to change that
(§12.1, `KIM-SEC-021`). Step 46 adds no hook, service, `UIAccess` manifest or elevated
component, so there is nothing to test here beyond the inspection that none exists; Steps 61 and
82 check it manually on real Windows.

**Performance.** Measured on Windows 10 19045 (debug build, `register_windows` test): starting
the registration thread about 0.15 ms, one `RegisterHotKey` about 0.2 ms, unregistering and
joining the thread on drop about 0.2 ms. The thread sleeps in `GetMessageW` (no idle wake-ups).
The library's eight behaviour tests (full checks over the example manifests) finish in about
10 ms together; `kim hotkeys check --all-tools` is a short-lived process that reads a few small
files and starts nothing.
