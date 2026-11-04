# Ddalgak GUI journeys and wireframes

Status: **current** (Step 65, design; implemented in Steps 73–76; the type-text, shortcut and hotstring forms and the text editor's data model are machine-checked since Step 69, the program picker and website field since Step 70, the Mouse form, its screen map and the playground's simulated pointer since Step 71; **the editor window — J1–J4, J7 and the rule part of J8 — is implemented since Step 74** (`ddalgak-editor.exe`, [ADR 0049](adr/0049-ddalgak-editor-process-and-toolkit.md); §7 says what is built and verified, §8 is the maintainer's manual script); **J6's profiles, history and rollback and J7's profile export and import with a diff are implemented since Step 75** (the Profiles page, §7.1, §8 steps 13–16), the playground Step 76) · Requirements:
`DDALGAK-FR-050…053`, `DDALGAK-FR-030…042`, `DDALGAK-NFR-007` · Visual language:
[DESIGN_SYSTEM](DESIGN_SYSTEM.md) · Model: [DDALGAK_RULE_MODEL](DDALGAK_RULE_MODEL.md)

These are the end-to-end journeys a first-time user must be able to complete, with original
low-fidelity wireframes. They fix the *flow, content and states*, not pixel sizes: the Kim
design system gives the tokens (near-black `bg.base`, lilac `accent` for focus and primary
actions, 28 px controls, 12–13 px text, state never shown by color alone). Wireframes use `▣`
for the focused element, `[ ]` for buttons, `( )` for options, `●`/`○` for the engine state
glyph, and `⚠`/`✕` for warnings and errors, each always with text.

## 1. Principles

1. **Say it like a sentence.** A rule reads "When I press **CapsLock** → **act as Ctrl**". The
   editor is built around that sentence, not around fields.
2. **Few clicks, no syntax.** Capture the keys, pick an effect tile, fill one small form, save.
   Advanced options (modes, apps, priority, pass-through) are folded away until asked for.
3. **Try before you trust.** Every rule can be simulated before it is saved, and nothing is
   shown as active until the engine has loaded it (`DDALGAK-FR-052`).
4. **Always a way back.** Undo after delete, history after apply, `Ctrl+Alt+Shift+Esc` for
   everything. The safe choice has default focus in every dialog.
5. **Keyboard first, screen-reader friendly.** Every journey works without a mouse; capture
   never traps focus; errors are announced with their text (`DDALGAK-NFR-007`).
6. **Korean and English.** Labels exist in both; layout leaves room for the longer one.

## 2. Window layout

The editor window (`ddalgak-editor.exe` since Step 74, its own process beside the tray app, ADR
0049; opened from the tray, the `editor.open` shortcut or the Start menu) has three regions; `F6`
cycles them (DESIGN_SYSTEM §4.1).

```text
┌──────────────────────────────────────────────────────────────────────────────────────┐
│ ● Running  [Pause]  [Turn off]      Profile: [Work ▾]      Mode: Base        ⚙  ?      │  header
├────────────┬──────────────────────────────────────────┬──────────────────────────────┤
│ Rules    ▣ │ 🔍 Search rules…            [+ New rule]  │ CapsLock acts as Ctrl         │
│ Modes      │ ───────────────────────────────────────── │ ───────────────────────────── │
│ Profiles   │ ☑ CapsLock        → act as Ctrl (tap Esc) │ When I press   [CapsLock]     │
│ Playground │ ☑ Right Alt       → 한/영                  │ Do             Act as LCtrl   │
│ Activity   │ ☑ Ctrl+Shift+F2   → type "감사합니다…"     │                tap → Escape   │
│ Settings   │ ☐ Ctrl+Shift+F8   → open tool.exe  ⚠ needs│ In modes       every mode     │
│            │                                   review  │ Permissions    ✓ type for me  │
│            │ ☑ F14             → open Notepad          │                               │
│            │                                           │ [Edit] [Test] [Duplicate] [⋯] │
├────────────┴──────────────────────────────────────────┴──────────────────────────────┤
│ 5 rules · 1 needs review · saved and active (revision 7)                              │  status line
└──────────────────────────────────────────────────────────────────────────────────────┘
```

- **Header**: engine state glyph *and* word (`● Running`, `◐ Paused`, `○ Off`), the main engine
  buttons, the profile switcher, the active mode (with its color chip and title).
- **Navigation rail**: Rules, Modes, Profiles, Playground, Activity, Settings.
- **List and detail**: the rule list (sentence per row, a checkbox to enable, badges with text)
  and the selected rule's detail. Rows show only summaries of personal content (the first
  characters of a snippet, a program's file name).
- **Status line**: counts, problems and the save state: *draft*, *saved*, *saved and active*.

## 3. Journeys

### J1. First start: safe and empty

1. The app starts with the engine **off** (`DDALGAK-FR-003`) and shows the tray icon `○`.
2. The window shows one sentence and three starter suggestions, nothing is created
   automatically:

```text
┌──────────────────────────────────────────────────────────────┐
│ ○ Off   Ddalgak does nothing until you turn it on.            │
│                                                              │
│   Make your first rule:                                      │
│   ▣ [ CapsLock acts as Ctrl ]   [ Type a phrase ]   [ Open a program ] │
│   or [+ New rule]                                            │
│                                                              │
│   Emergency off: Ctrl+Alt+Shift+Esc (always works)           │
└──────────────────────────────────────────────────────────────┘
```

3. A suggestion opens the editor pre-filled (J2 step 3). The emergency chord is shown on first
   start and in Settings → Safety.

### J2. Create a rule in a few clicks

Four steps in one panel; `Back` and `Esc` never lose the draft.

**Step 1: When I press…** (J3 has the capture details)

```text
┌ New rule ──────────────────────────────────────────── 1 Keys · 2 Effect · 3 Details · 4 Try ┐
│ When I press                                                                               │
│ ┌────────────────────────────────────────┐                                                 │
│ │ ▣ Press the keys…                       │   Type instead                                  │
│ └────────────────────────────────────────┘                                                 │
│ ( ) When pressed   ( ) When tapped alone   ( ) When held for [500] ms                      │
│ ☐ Also let the key through to the app                                                      │
│                                                                    [Cancel]  [Next ▸]     │
└────────────────────────────────────────────────────────────────────────────────────────────┘
```

**Step 2: …do this.** Seven tiles, one per core action category, each with an icon, a short
title and a one-line example:

```text
┌ New rule ─ When I press Ctrl+Shift+F2 ─────────────────────────────────────────────────────┐
│  ▣ ⌨  Act as another key    ⌘  Press a shortcut     Aa  Type text      ▢  Open a program    │
│     CapsLock → Ctrl            F13 → Ctrl+C            "감사합니다."        Notepad            │
│                                                                                            │
│    🌐 Open a website         🖱  Mouse               ◧  Switch mode     ⏻  Pause Ddalgak     │
│      github.com               move · scroll · click   Navigation          pause · resume · off│
│                                                                                            │
│  More: Block this key · Several steps…                                [◂ Back]  [Next ▸]   │
└────────────────────────────────────────────────────────────────────────────────────────────┘
```

**Step 3: details.** One small form per effect (§4). Required fields first, defaults filled.

**Step 4: try and save.**

```text
┌ New rule ─ When I press Ctrl+Shift+F2 → type text ─────────────────────────────────────────┐
│ Try it here (nothing leaves this box):                                                     │
│ ┌──────────────────────────────────────────────────────────────┐                           │
│ │ ▣ press Ctrl+Shift+F2 in this box                             │   ( ) Simulate  ( ) Live │
│ │ 감사합니다.                                                    │                           │
│ │ 김민수 드림                                                    │                           │
│ └──────────────────────────────────────────────────────────────┘                           │
│ ✓ This rule would fire: types 13 characters on 2 lines.                                    │
│ Permission: Ddalgak will type for you in other programs.  [Allow]                          │
│                                                     [◂ Back]  [Save as draft]  [Save and enable] │
└────────────────────────────────────────────────────────────────────────────────────────────┘
```

- **Save and enable** writes the rule, waits until the engine has loaded it, then shows
  "saved and active". If the engine is off, the rule is saved and the status line says
  "saved; the engine is off".
- A rule that needs a permission cannot be enabled without **Allow** (the consent dialog of
  RULE_MODEL §6.3; Cancel has default focus). Without it, **Save as draft** stores it disabled.

### J3. Pick a physical hotkey

- **Capture** records chords on key-down, exactly like the hotkey contract's binding editor
  (HOTKEY_CONTRACT §11.2): `Esc` cancels, `Tab` leaves, focus is never trapped, and the captured
  keys are announced ("Control plus Shift plus F2"). **Add step** records the next chord of a
  sequence (up to four).
- **Logical or physical.** A small toggle under the field: *the key labeled J* (logical, follows
  the layout) or *this key position* (`code:KeyJ`, the same physical key on every layout).
  Letters default to logical; the toggle explains the difference with the current layout's
  label.
- **Left or right.** Holding Right Ctrl records `RCtrl+…` only if the user ticks "only this
  side"; otherwise the generic `Ctrl+…` is stored.
- **Type instead** opens a text field accepting the canonical syntax and the contract's aliases
  (`ctrl+shift+f2`, `C-j`), for chords Windows or another program intercepts before Ddalgak can
  see them, and for IME and screen-reader users.
- **Immediate feedback**, before Next is enabled:

```text
│ When I press   [ Win+L ]                                                                   │
│ ✕ Windows reserves Win+L (lock). Ddalgak never takes keys from Windows.                    │
│   Try: [Ctrl+Shift+L]  [Ctrl+Alt+Shift+L]                                                  │
│ When I press   [ Ctrl+Alt+Q ]                                                              │
│ ⚠ On some layouts AltGr+Q types "@". This rule would block it there.  [Keep] [Change]      │
│ When I press   [ F3 ]                                                                      │
│ ⚠ Noeul uses F3 system-wide. Ddalgak would see it first while it runs.  [Keep] [Change]    │
│ When I press   [ Ctrl+Shift+V ]                                                            │
│ ✕ "Paste plain" already uses Ctrl+Shift+V in every mode.  [Change keys] [Limit to an app…] │
```

  Errors (`trigger_reserved`, `duplicate_trigger`, `output_is_trigger`) block saving; warnings
  (`altgr_collision`, `ime_toggle`, `os_shortcut`, `hook_overrides_global`) offer **Keep** or
  **Change**. Offers are suggestions only; nothing changes until the user picks one.
- **IME.** While a Korean IME composition is active the field asks the user to switch to
  English input or to type the chord as text (HOTKEY_CONTRACT §6.2).

### J4. Choose the effect: the seven forms

Each form shows its inputs (RULE_MODEL §5), a live summary sentence, and the permission it needs.

| Tile | Form | Summary sentence | Permission |
| --- | --- | --- | --- |
| Act as another key | Target key (capture or pick; a mouse button from a list), optional *Tap alone sends* key and time, *Also let the original key through* (not with a tap) | "CapsLock acts as Left Ctrl; a quick tap is Escape" | type for me |
| Press a shortcut | Up to eight chords, optional delay between them | "Presses Ctrl+K, then Ctrl+V" | type for me |
| Type text | Multi-line text box (Hangul, emoji), *Paste instead of typing*, delay | "Types 13 characters on 2 lines" | type for me (+ clipboard for paste) |
| Open a program | **Choose program…** (file picker limited to `.exe`) or **Choose an app…** (installed packaged apps), arguments as a list, start folder, window state, *bring to front if running* | "Opens Notepad" | open programs |
| Open a website | URL field with live validation (`https://` added when missing, other schemes refused) | "Opens github.com in your browser" | open programs |
| Mouse | Segmented: *Move* (by / to; once, glide while held with speed, or repeat while held; pixels or grid cells), *Scroll* (direction, notches), *Click* (button, click, double, hold) with a small screen map showing the target point across monitors | "Moves the pointer right at 600 px/s while Right is held" | type for me |
| Switch mode | Operation (switch to, toggle, cycle, hold, back, default) and the mode list with **+ New mode** | "Navigation while CapsLock is held" | — |
| Pause Ddalgak | Pause, resume, toggle pause, turn off; a note that no rule can turn Ddalgak on | "Pauses all rules" | — |

```text
┌ Open a program ──────────────────────────────────────────────┐
│ Program     [ Notepad (C:\Windows\System32\notepad.exe) ] [Choose…] │
│ Arguments   [ + Add argument ]                                │
│ Start in    [ program's folder ▾ ]                            │
│ Window      ( ) Normal  ( ) Maximized  ( ) Minimized          │
│ ☐ Bring it to the front if it is already running              │
│ ✓ Found. Ddalgak starts it as you, never as administrator.    │
└──────────────────────────────────────────────────────────────┘
```

**The "Act as another key" and "Block this key" forms** (Step 68 fixed their fields;
`crates/ddalgak/src/remap.rs` `FIELDS`, `BLOCK_FIELDS`, `summary` and `preview`, whose tests check
every field against the rules schema and every preview against the playground contract):

| Field (rule document) | Input | Required | Default | Label and help |
| --- | --- | --- | --- | --- |
| `steps[0].to` | capture, with *Type instead* | yes | — | **Acts as**: one key, chord, modifier or mouse button. Logical keys follow the layout; *this key position* stores `code:Key…` instead |
| `steps[0].tap` | capture | no | — | **A quick tap alone sends**: dual role; a press and release with no other key within the tap time sends this instead |
| `steps[0].tap_ms` | number | no | 200 | **Tap time (ms)**: 100–1000; held longer, or another key pressed meanwhile, the key acts as the target |
| `trigger.pass_through` | toggle | no | off | **Also let the original key through**: not with a tap alternative |

**The "Type text" form, hotstrings and the text editor** (Step 69 fixed their data;
`crates/ddalgak/src/text.rs` `TYPE_TEXT_FIELDS`, `SEND_KEYS_FIELDS`, `EditorDraft`, `summary` and
`preview`, whose tests check every field against the rules schema and every preview against the
playground contract; the visible editor is Step 74):

| Field (rule document) | Input | Required | Default | Label and help |
| --- | --- | --- | --- | --- |
| `title` | text | yes | — | **Snippet name**: shown in the list and the tray menu instead of the text, which stays private |
| `steps[0].text` | multi-line text | yes | — | **Text**: up to 4096 characters, Hangul, emoji, new lines, tabs; typed exactly as written; "rules.json keeps it in plain form: not for passwords" |
| `steps[0].variables` | toggle | no | off | **Fill in variables**: `{date}`, `{time}`, `{datetime}`, `{year}`…`{second}` and `{cursor}`, inserted from a menu; `{{` `}}` for braces; no environment or clipboard variables |
| `steps[0].method` | choice | no | type | **Paste instead of typing**: needs the clipboard permission; the clipboard is restored |
| `steps[0].key_delay_ms` | number | no | 0 | **Delay between characters (ms)**: 0–200 |
| `trigger.hotstring.abbreviation` | text | yes (hotstring) | — | **Abbreviation**: 1–40 characters without spaces (`btw`, `;sig`) |
| `trigger.hotstring.case`, `inside_words`, `immediate`, `end_chars`, `omit_end`, `erase` | choice, toggles, text | no | conform, off, off, AutoHotkey's set, off, on | **Case**, **Also inside words**, **Fire as soon as it is typed**, **End characters**, **Leave out the end character**, **Erase the abbreviation** (RULE_MODEL §3.4) |

- **Live preview.** Under the text box the editor shows the text as it would be typed now (the
  variables filled in) in its own field, the summary sentence ("Types 17 characters on 2 lines
  with {date}, then moves the cursor back 12 characters") and each field problem with its
  position ("character 7 names no variable of the whitelist"); the problem never quotes the text.
- **Insertion point.** `{cursor}` marks where the cursor ends up: the preview shows the number of
  Left presses; one per character after it, a new line counts.
- **Explicit save and test.** Nothing is saved or typed by itself. **Save** is enabled only for a
  changed, valid draft and writes through the rule store (revision checked; *Save as draft*
  writes `rules.draft.json`); **Test** is enabled for a valid draft and types into the
  playground's own field only (J5, Step 76).
- **Trigger choice.** *When I press* (keys, J3) or *When I type* (an abbreviation): the hotstring
  options appear under the abbreviation; the summary reads "Typing btw then a space or punctuation
  types 10 characters".

"Block this key" has no inputs ("Insert does nothing (blocked)"). Under the summary the form says
when a plain trigger combines with held modifiers ("Shift+CapsLock sends Shift+Escape") and, for
a mouse-button trigger, that Ddalgak then uses a mouse hook while the engine is on. Choosing
`MouseLeft` or `MouseRight` as the trigger is not offered.

**The "Open a program" and "Open a website" forms** (Step 70 fixed their data;
`crates/ddalgak/src/launch.rs` `OPEN_PROGRAM_FIELDS`, `OPEN_URL_FIELDS`, `ProgramDraft`,
`UrlDraft`, `summary` and the previews, whose tests check every field against the rules schema;
the icon is read from the file by `win::launch::icon_preview`; the visible editor is Step 74):

| Field (rule document) | Input | Required | Default | Label and help |
| --- | --- | --- | --- | --- |
| `steps[0].path` | program picker (`.exe` only) or typed path | one of path or app | — | **Program**: quotes from *Copy as path* are removed, `/` becomes `\`, a file in a known folder is written with its token (`%LocalAppData%\Programs\…`: portable, no user name); checked as typed (absolute, `.exe`, not cmd/PowerShell/script hosts, no `:` stream); the preview shows the icon, the file name, *found* and whether it is really a program |
| `steps[0].app_id` | app ID | one of path or app | — | **Or an app**: `PackageFamilyName!AppId`; installed apps are not listed yet (the picker needs the shell's COM enumeration), so the ID is pasted and checked by form |
| `steps[0].args` | list | no | — | **Arguments**: one per line, up to 32, each quoted by Ddalgak; the details show the exact command line |
| `steps[0].working_dir` | folder picker | no | the program's folder | **Start in** |
| `steps[0].window` | choice | no | normal | **Window**: normal, maximized, minimized; never hidden |
| `steps[0].if_running` | toggle | no | start new | **Bring it to the front if it is already running**: "Windows may only flash its taskbar button" |
| `steps[0].env` | name/value pairs (advanced) | no | — | **Environment**: for this program only; shown in the consent dialog, never in lists or logs |
| `steps[0].url` | URL field | yes | — | **Website**: `https://` added when missing, scheme and host in lower case, an international host in its `xn--` form, spaces encoded; only `http`/`https`, no user name or password; the summary reads "Opens github.com in your browser" |

- **Nothing opens by itself.** Picking a file reads it (exists, `MZ` header, icon) but never runs
  it; Save writes only a valid, changed draft after the consent dialog names the exact target;
  Test opens it only after its own confirmation (J5, RULE_MODEL §11).

**The "Mouse" form** (Step 71 fixed its data; `crates/ddalgak/src/mouse.rs` `MOUSE_FIELDS`,
`MouseDraft`, `summary`, `screen_map` and `Playground`, whose tests check every field against the
rules schema; the visible editor is Step 74):

| Field (rule document) | Input | Required | Default | Label and help |
| --- | --- | --- | --- | --- |
| `steps[0].type` | segmented | yes | move | **Mouse**: Move, Scroll or Click. Every choice works from the keyboard alone |
| `steps[0].mode` | choice | yes (Move) | by a distance | **Move**: by a distance from where the pointer is, or to a point; the pointer never leaves the visible desktop |
| `steps[0].dx`, `steps[0].dy` | numbers | yes (by) | 0 | **Right** / **Down** (negative: left / up): logical pixels, scaled by the monitor's display scale, or grid cells |
| `steps[0].unit`, `steps[0].grid` | choice, columns × rows | no | pixels | **Count in**: pixels, or cells of a virtual grid over the monitor (2–64 columns and rows); the pointer jumps from cell centre to cell centre |
| `steps[0].while_held` | choice | no | once per press | **While the keys are held**: once per press; keep moving smoothly (speed 50–5000 px/s, acceleration 0–10); or repeat the step every 20–2000 ms after a first wait (default 300 ms), independent of the keyboard's repeat settings. Grid moves repeat, never glide. A held move must be the rule's only step |
| `steps[0].to` | point | yes (to) | — | **To**: x and y on the desktop, the primary monitor or the active window's content area; the map shows the point across the monitors |
| `steps[0].direction`, `steps[0].notches` | choice, number | yes, no | down, 1 | **Scroll**: up, down, left or right; 1–50 notches |
| `steps[0].button` | choice | yes (Click) | left | **Button**: left and right follow "swap buttons"; middle, back (X1), forward (X2) |
| `steps[0].op` | choice | no | click | **Click**: click, double click, or hold exactly as long as the keys are held (released on pause, lock and every stop) |
| `steps[0].at` | point | no | — | **At**: move to this point first |

- **The screen map** draws every monitor at its place (negative coordinates included) with its
  scale, the playground's simulated pointer and the step's target, marked when it had to be
  moved onto the desktop ("The point is beside every monitor; the pointer goes to the nearest
  edge").
- **Accessible alternatives**: every action is reachable with keys only; *repeat while held*
  replaces holding a key steady at the keyboard's pace (it works with Filter Keys and slow
  repeat settings); grid cells need far fewer presses than pixels; a point on the primary monitor
  or in the active window jumps in one press. The form names Windows' own Mouse Keys and ClickLock
  settings as the system alternatives (Ddalgak offers no click-lock: a button stays down only
  while its keys are held).
- **Nothing moves by itself.** Save writes only a valid, changed draft after the consent dialog;
  Test runs the draft on the playground's simulated pointer (J5): the real pointer never moves.

**Advanced** (folded): *Only in modes…*, *Only in apps… / except in…*, *Window title contains…*,
*Priority*, *If a step fails: stop / continue*, *Accept input from other programs* (shown only
for effects that inject nothing). **Several steps…** turns the single effect into an ordered
list with *+ Add step*, *Wait*, drag handles and `Alt+Up`/`Alt+Down` to reorder.

### J5. Test in the safe playground

The Playground page tests any saved rule, a draft or a whole profile without touching the
running engine (RULE_MODEL §11).

```text
┌ Playground ─────────────────────────────────────────────────────────────────────────────┐
│ Profile [Work ▾]   Mode [Base ▾]   App [Code.exe ▾]   Focus ( ) normal ( ) password field │
│                                                    ( ) admin window   Engine ( ) paused  │
│ Mode: ( ) Simulate — nothing leaves Ddalgak    ( ) Live — types only into the box below  │
│ ┌──────────────────────────────────────────────┐ ┌──────────────────────────────────────┐ │
│ │ ▣ Press keys here, or type them: [Ctrl+Shift+V]│ │ Trace                                │ │
│ │                                                │ │ ✓ Paste plain in Code  (winner)       │ │
│ │                                                │ │   – Paste plain everywhere: lower     │ │
│ │                                                │ │     precedence (app rule is more      │ │
│ │                                                │ │     specific)                         │ │
│ └──────────────────────────────────────────────┘ │ Step 1  Press Ctrl+K, then Ctrl+V     │ │
│                                                  │ Key reaches the app: no               │ │
│ [Reset]  [Step ▸]                                 └──────────────────────────────────────┘ │
└─────────────────────────────────────────────────────────────────────────────────────────┘
```

- **Simulate** is the default: no input is injected, nothing opens, the real engine's mode and
  state do not change. The trace shows the decision, every candidate and each step's preview.
- **Live** types only into the playground's box. Launch steps stop and ask
  "Open Notepad now? [Cancel] [Open]" (Cancel focused); nothing opens without that click.
- **Focus** choices let users see Ddalgak step aside ("A password field has focus; Ddalgak never
  types into it.") without needing a real password field.
- **Step ▸** walks through multi-step rules one step at a time. **Reset** restores the
  playground; active rules are never affected.
- **Mouse steps** (Step 71) run on a **simulated pointer** over the real monitor layout (reading
  the layout moves nothing): the trace shows where the pointer would go and whether it was moved
  onto the desktop, a held move plays out while the key is held in the capture box, a held button
  shows as pressed until the key is released. Neither Simulate nor Live ever moves, scrolls or
  clicks the real pointer (`mouse::Playground`).
- The box is a key-capture surface: it binds nothing by default (manifest context
  `playground`), so every key can be tested; `Tab` leaves and `Esc` is entered via *type them*.

### J6. Save a profile, switch modes, roll back

```text
┌ Profiles ─────────────────────────────────────────────┐   ┌ History · Work ───────────────────────┐
│ ▣ Work       12 rules · 3 modes   active             │   │ r42  today 10:12  edited "Paste plain" │
│   Gaming      2 rules · 1 mode                        │   │ r41  today 09:58  imported "Writing"   │
│   Writing     3 rules (imported, needs review)        │   │ r40  yesterday    added 2 rules        │
│ [+ New] [Duplicate] [Rename] [Export…] [Import…]      │   │ ▣ [Compare with now]  [Restore r41]    │
└───────────────────────────────────────────────────────┘   └───────────────────────────────────────┘
```

- **Switch profile** from the header menu, the tray or the Profiles page. The switch is atomic:
  held keys are released, the new profile's default mode becomes active, and the header shows
  the new name only after the engine applied it.
- **Modes** page: create, rename (`F2`), color, duplicate, delete (rules that use a deleted mode
  are listed first and must be moved or deleted). Switching the active mode from the header
  menu is immediate; the optional compact mode indicator (off by default) shows title and
  color chip near the tray.
- **Step 72 data model** (machine-checked, drawn by Steps 73–74): the Modes page form is
  `mode_edit::MODE_FIELDS` — *Name* (required), *Badge* (1–8 characters, never instead of the
  name), *Color*, *Layer over* (none: exclusive; a mode: keys this mode does not change keep that
  mode's rules), *Only for the next key* (one-shot), *Return to the default mode after (ms)*;
  `ModeDraft` gives field problems (the same checks as `ddalgak mode add/edit`: a parent cycle or
  a default mode with a parent is shown under *Layer over* before Save), the summary line
  ("Navigation: a layer over Base: keys it does not change keep their rules there") and Save
  gating. Each row shows the policy, the chain, the rules limited to the mode, the rules that
  switch to it and its layers (`mode_edit::row`). Delete is refused, with the reason, for the
  default mode, a mode other modes are layers over and a mode a rule switches to; rules limited to
  it are offered for deletion. The **Switch mode** action form is `SWITCH_FIELDS` (*Switch*: to,
  toggle, cycle, hold, back, default; *Mode*; *Modes, in order*); its sentence is
  `modes::summary` ("Navigation is active while CapsLock is held, then the previous mode
  returns"). The header switcher and the tray's `Mode` submenu are `modes::menu` (every mode, the
  active one checked, "(default)" and "(layer over …)" in words) plus **Back to <default>**; the
  header and the compact indicator show `modes::Indicator` (badge and title, color chip; shown in
  the tray only for a profile with more than one mode unless `settings.mode_indicator` says
  `tray` or `off`). Every control is reachable from the keyboard; the mode is never told by color
  alone.
- **Roll back**: History lists applied changes with revision, time and origin (GUI, CLI, import,
  restore). *Compare* shows a side-by-side diff; *Restore* applies that state as a new change,
  so a restore can itself be undone.

### J7. Search, edit, delete, duplicate, export

- **Search** (`Ctrl+F` in the rule list) matches titles, keys and effect names, never the
  content of typed text unless the user opts into "search snippet text". Filters: profile,
  mode, app, enabled, needs review, has problems.
- **Edit** opens the same four-step panel at step 3. Saving creates a new revision; a change to
  a launch target asks for consent again (`consent_stale`).
- **Delete** (`Delete`, destructive): one confirmation naming the rules ("Delete 2 rules:
  CapsLock acts as Ctrl, Open Notepad? [Cancel] [Delete]"), then an **Undo** toast that stays
  until dismissed; history keeps the old revision anyway.
- **Duplicate** (`Ctrl+D`) creates "… (copy)", disabled, with a fresh ID, so it can never
  collide with the original's trigger until edited.
- **Bulk**: multi-select with `Shift`/`Ctrl` and `Space`; *Enable*, *Disable*, *Move to mode*,
  *Export*.
- **Export** writes a portable file without permissions or settings (RULE_MODEL §10). **Import**
  validates, shows what will be added, and adds rules disabled with a "needs review" badge; each
  launch target and each typing rule is approved one by one.

### J8. On, pause, off and emergency

```text
 Tray menu (right-click or Shift+F10 on the icon)
 ┌────────────────────────────────┐
 │ ● Ddalgak: running (Work, Base) │
 │   Pause all rules               │
 │   Turn off                      │
 │   Profile ▸ Work / Gaming       │
 │   Mode ▸ Base / Navigation      │
 │   Open rule editor              │
 │   Quit Ddalgak                  │
 └────────────────────────────────┘
```

- Tray icon states differ in shape, not only color: `○` off, `●` running, `◐` paused, and a
  struck-through glyph after an emergency off, each with a tooltip in words.
- **Emergency** (`Ctrl+Alt+Shift+Esc`): the hook is removed at once, held keys and buttons are
  released, running rules stop, the engine is off; a non-modal notice says what happened and
  offers *Open rule editor* and *Turn on again*. Ddalgak never turns itself back on.
- If the rules file is invalid, the editor opens on a repair view listing each problem with
  its code and line, and the engine cannot be turned on until the file loads.
- **Step 73 data model** (machine-checked, drawn by Step 74): the header and the tray show
  `indicator::Status` — the shape (ring, struck ring after an emergency, a lost hook or an unclean
  exit, disc, notched disc outside the default mode, bars), the tooltip in words and one attention
  code (`profile_disabled`, `rules_kept_previous`, `shortcut_taken`, `session_locked`, …). The
  optional floating badge (Settings → *Show a small status badge*: off, while paused, always;
  `settings.floating_indicator`) never takes focus and uses `indicator.floating.label`. The tray
  menu has *Release stuck keys*. *Turn off* is remembered after a restart until *Turn on*; the
  Profiles page and the Modes page offer *Disable*/*Enable* (`enabled`), and a disabled mode is
  not offered in the mode menu.

## 4. Keyboard map (editor)

| Keys | Where | Action |
| --- | --- | --- |
| `Ctrl+N` · `Ctrl+D` · `Delete` · `Ctrl+F` · `F5` | rule list (`rules` context) | new · duplicate · delete · search · test in playground |
| `Enter` / `F2` | rule list | edit / rename |
| `Alt+Up` / `Alt+Down` | rule list, steps | move up / down |
| `F6` / `Shift+F6` | everywhere | next / previous region |
| `Esc` | dialogs, capture | cancel |
| `F1` | everywhere | shortcut list and help |
| `Ctrl+Alt+Shift+Esc` | system-wide | emergency off (fixed) |

All of them except the fixed ones are rebindable in Settings → Keyboard shortcuts (HOTKEY_CONTRACT
§11); the list comes from Ddalgak's [hotkey manifest](contracts/examples/kim-hotkey-manifest-v1/ddalgak.json).

## 5. Labels (Korean and English)

| Concept | English | 한국어 |
| --- | --- | --- |
| Rule | Rule | 규칙 |
| When I press | When I press | 이 키를 누르면 |
| Act as another key | Act as another key | 다른 키로 바꾸기 |
| Press a shortcut | Press a shortcut | 단축키 누르기 |
| Type text | Type text | 글자 입력 |
| Open a program | Open a program | 프로그램 열기 |
| Open a website | Open a website | 웹사이트 열기 |
| Mouse | Mouse | 마우스 |
| Switch mode | Switch mode | 모드 전환 |
| Pause Ddalgak | Pause Ddalgak | 딸깍 일시 정지 |
| Playground | Playground | 연습장 |
| Simulate / Live | Simulate / Live | 시뮬레이션 / 실제 입력 |
| Profile | Profile | 프로필 |
| Emergency off | Emergency off | 긴급 끄기 |

## 6. What Step 74–76 must prove

A person who has never used Ddalgak, using only the keyboard or only the mouse, completes J1–J8
without help: a remap, a Korean text snippet, a program launch tested in the playground, a mode
switch, a profile export and import, a rollback, and an emergency off. Steps 74–76 record the
results (with `egui_kittest` tests for the flows and a manual run on Windows) in their completion
reports; Step 82 repeats them in the release acceptance matrix.

## 7. Step 74: the editor as built

`ddalgak-editor.exe` (package `crates/ddalgak-editor`,
[ADR 0049](adr/0049-ddalgak-editor-process-and-toolkit.md)) is a separate process: the tray
menu's **Open rule editor** and the `editor.open` shortcut start it beside `ddalgak-gui.exe`; a
second start brings the open window forward. It works without the tray app (the header then says
*Ddalgak is not running*). It never runs the engine, installs a hook, injects input, opens a
program or a website.

### 7.1 What each journey has now

| Journey | Built in Step 74 | Not yet (Step) |
| --- | --- | --- |
| J1 | Empty state with the sentence, three starters (*CapsLock acts as Ctrl*, *Type a phrase*, *Open a program*: a pre-filled, unsaved draft) and the emergency chord; nothing is created by itself | — |
| J2 | One panel with the steps *1 Keys · 2 Effect · 3 Details · 4 Try* (Back and Next; the step labels jump directly), *Save turned off* and *Save and enable*; the consent dialog names each permission in a sentence and every launch target exactly, Cancel focused, Allow records `via: "gui"` | *Save as draft* to `rules.draft.json` (the CLI's `--draft`) is not offered: a rule saved turned off is the editor's draft |
| J3 | Capture on key-down from the window's own events (`Esc` ends, `Tab` leaves, *Add step* for sequences up to four), the keys announced in words, **Type instead** with the contract's aliases, chips for keys the window cannot see (CapsLock, RAlt, RCtrl, LCtrl, RShift, ScrollLock, Pause, ContextMenu, NumLock, Insert), the IME guard; trigger errors (reserved, invalid) shown under the field before Next; *When pressed / When tapped alone / When held* | The *logical / this key position* toggle (letters are captured logical; `code:` keys are typed) and *only this side* (side-specific keys are typed or picked); the **Keep / Change** buttons for warnings: warnings are shown in Try with their *Use <keys>* offers |
| J4 | Eight tiles plus *Block this key*, each form on the engine's data model with a live summary, a preview and its problems in words: remap (target, tap, tap time, pass-through), shortcut (up to eight chords, delay), text (Hangul, variables, paste, delay, the text as it would be typed in the editor's own field), program (path or app ID, arguments, start folder, window, bring to front; found / not a program; the exact command line), website (normalized address), mouse (move by / to, scroll, click; grid cells, glide, repeat; the screen map with the simulated pointer), mode (switch to, toggle, cycle, hold, back, default; **+ New mode**), pause (pause, resume, toggle, turn off); *Advanced*: description, only in modes, only in apps (stays inactive, `condition_unsupported`, until Step 77), priority | *Choose program…* (a file picker) and *Choose an app…* (paths and IDs are typed or pasted); editing `env` (kept as stored); *If a step fails*, *Accept input from other programs*; *Several steps…* (78) |
| J5 | **Test** (`F5`) in the detail panel and the form's **Simulate**: what the rule would do, from the engine's previews; the mouse on a simulated pointer; nothing leaves the window | The playground page, live tests, focus and engine choices (76) |
| J6 | The **Modes** page: every mode of the active profile with its policy, layer and rule count, *Enable/Disable*, **+ New mode**. Since Step 75 the **Profiles** page (also from the header's profile name): search and sort; *Switch…* with a side-by-side diff of what each trigger does now and after (Cancel focused; the header says *switching…* until the app confirms); *Duplicate*, *Rename…*, *Delete profile* (never the active one; Undo); *History…* with a side-by-side comparison, *Restore this version*, *Roll back to the last working version* and labelled save points; *Export…*/*Export all…* (environment values only when ticked); *Import profile…* with the validation preview (renamed IDs, restored modes, permissions and exact targets, missing programs, conflicts, inactive codes), adding profiles never active, rules off, no consent | Rename, color, layers and delete of modes in the window (use `ddalgak mode …`); a header drop-down switcher (the header opens the Profiles page) |
| J7 | Search (`Ctrl+F`; titles, keys, effect names, mode titles, apps; snippet text only with *Also search snippet text*), filters (state, profile, mode, app), grouping (profile, mode, app), drag ordering and `Alt+Up/Down`, multi-select (`Ctrl`/`Shift`+click, `Shift+Up/Down`, `Space`, `Ctrl+A`) with *Enable, Disable, Duplicate, Delete, Export*, conflict/problem/warning/review/inactive badges in words, the description under each row, delete with a confirmation naming the rules (Cancel focused) and an *Undo* toast that stays, `Ctrl+Z`, **Export…** (a portable file without consent, settings or revision) and **Import…** (rules added turned off and without consent, every launch target named, nothing opened or run) | Import with a diff and whole profiles (75) |
| J8 | The header's engine state in a glyph and a word with *Turn on / Turn off* and *Pause / Resume* (the same IPC requests as the CLI, on click only), the tray's *Open rule editor* | The indicator's shapes and attention codes in the header and the optional floating badge (Step 73's model, not drawn yet); the repair view for an invalid file (the status line names the problem; the CLI repairs) |

### 7.2 Saved, loaded, active

Every change is one rule-store transaction with origin `gui` (lock, re-read, one operation,
validation before and after, atomic save; ADR 0028). The status line then says
*saved; waiting for Ddalgak to load it* until the running app answers `rules.reload`, and
*saved and active (revision N)* only when the app's active revision is N and its engine runs;
otherwise *saved and loaded; the engine is off/paused*, *saved; Ddalgak is not running*,
*saved; Ddalgak kept its previous rules: <reason>* or *saved; Ddalgak did not confirm the
reload*. A row says *active* only in the first case (*saved, not active yet* otherwise). A change
by the CLI or another program is seen through the folder watcher (no polling); the app is asked to
reload, and a form open on a rule that changed shows *The rules changed outside the editor* and
refuses to save over it (`rule_conflict`).

### 7.3 Keyboard map as built

| Keys | Action | Rebindable |
| --- | --- | --- |
| `Ctrl+N` · `Ctrl+D` · `Delete` · `Ctrl+F` · `F5` | new · duplicate · delete (asks) · search · test (simulate) | yes (`rules.*` in `hotkeys.json`; `rules.toggle` and `rules.export` ship unbound) |
| `Up` / `Down`, `Shift+Up` / `Shift+Down`, `Space`, `Ctrl+A` | select, extend, toggle, select all shown | no |
| `Enter` / `F2` | edit / edit with the name focused | no |
| `Alt+Up` / `Alt+Down` | move the selected rule | no |
| `Ctrl+Z` | undo the window's last change (outside text fields) | no |
| `F6` / `Shift+F6` | next / previous region | no |
| `Esc` | end the key capture, close a dialog or the unchanged form (a changed one asks) | no |
| `F1` | the shortcut list | no |

### 7.4 Measured

Release build, glow, on the reference machine (Windows 10 19045), the window off-screen and not
activated (`cargo run --release -p ddalgak-editor --example measure -- --rules N --idle S`), memory
read from outside:

| Rules | First frame | CPU while idle | Frames while idle | Working set | Private |
| --- | --- | --- | --- | --- | --- |
| 0 (J1) | 167 ms | 0 ms (3 s) | 0 | 76 MiB | 107 MiB |
| 100 | 163–288 ms | 0 ms (10 s and 30 s) | 0 | 96 MiB | 126 MiB |
| 1000 (a profile's limit), 4 modes | 223 ms | 0 ms (10 s) | 0 | 99 MiB | 128 MiB |
| Kim Manager's preview, same machine | — | — | — | 74 MiB | 108 MiB |

Before Step 74 moved validation to short-lived threads the window held 152 MiB with 100 rules.
`DDALGAK-NFR-003` (editor open < 80 MB) is met without rules and missed by ~20 MiB with them
(ADR 0049 consequences). The tray app is unchanged (no GUI code).

### 7.5 Verification

| Check | Status | Evidence |
| --- | --- | --- |
| Every effect form's validation, the canonical rule it saves, round trips, summaries, simulation, GUI consent with launch fingerprints | **tested** | `form::tests` |
| Rows, badges, search, filters, grouping, 1000 rules over 32 modes | **tested** | `listing::tests`, `flows::many_rules_and_modes_stay_usable` |
| Store transactions with origin `gui`, refused conflicts, undo only while unchanged elsewhere | **tested** | `repo::tests`, `flows::edit_duplicate_enable_disable_delete_and_undo` |
| J1, J2 (capture, tile, form, simulation, consent with Cancel focused), J7 (edit, duplicate, enable with consent, disable, delete and undo, search, filters, grouping, bulk, reorder by keys and by drag), keyboard-only navigation, draft safety, Korean labels, import and export, Test as simulation, the Modes page | **tested** headlessly (egui_kittest, through the accessibility tree; an unnamed control fails the tests) | `crates/ddalgak-editor/tests/flows.rs` |
| "Saved and active" only after the reload answer; engine off, not running | **tested** with an app double that reloads with the engine's own loader | `flows::a_save_is_acknowledged_only_after_the_engine_reload`, `sync::tests` |
| A CLI change seen through the real folder watcher, never overwritten by an open form | **tested** | `flows::a_cli_change_appears_and_is_never_overwritten` |
| The real tray app over the real pipe: active revision, CLI `rule disable` seen, engine buttons, import opens nothing | **e2e** (daily) | `e2e/tests/step74_ddalgak_editor.rs` |
| `editor.open` registered; the tray's *Open rule editor* starts the editor beside the app | registration **tested** (`tests/cli.rs`, `shortcuts::tests`); the real start of a visible window **not tested** (no window may open on this machine) | §8 |
| Idle CPU, startup, memory | **measured** | §7.4 |
| The visible window: focus handover to a running editor, IME in the real text fields, a screen reader, the real tray menu, mouse-only use | **not tested** (needs the maintainer) | §8 |

## 8. Manual usability script (maintainer, the real visible editor)

Use a fresh user profile or back up `%APPDATA%\mouv\ddalgak\` first. Build or install
`ddalgak-gui.exe`, `ddalgak.exe` and `ddalgak-editor.exe` into one folder. Note each step's
result (pass, fail, remark) in the Step 74 issue.

1. **J1 first start.** Start `ddalgak-gui.exe`: the tray shows `○`. Right-click the tray icon:
   **Open rule editor** opens the editor. It says *No rules yet*, shows three starters and the
   emergency chord; the status line says *nothing saved yet*. Choose **Open rule editor** again:
   the same window comes to the front, no second window opens.
2. **J2/J3 a remap by keyboard only.** Press `Ctrl+N`. Press `Ctrl+Shift+F2`: the capture box
   shows it and its words. Press `Esc`, then type `CapsLock` into **Type instead** (or press the
   *CapsLock* chip). `Tab` to **Next ▸**, pick **Act as another key**, capture `LCtrl` in *Acts as*,
   `Escape` in *A quick tap alone sends*. Go to **4 Try**, **▶ Simulate**, then **Save and enable**:
   the consent dialog appears with **Cancel** focused; press `Tab` and `Enter` on **Allow**. The
   status line first says *waiting*, then *saved and active (revision N)* once you turned the
   engine on (**Turn on** in the header). Press CapsLock in Notepad: it acts as Ctrl, a tap is Esc.
3. **J4 a Korean snippet.** **+ New rule**, type `Ctrl+Shift+F3`, **Type text**; type
   `감사합니다.` and a second line `김민수 드림` with the Korean IME (2-set): composition works in the
   text box, the preview shows both lines, the summary *Types 13 characters on 2 lines*. Save and
   enable (Allow). In Notepad, press Ctrl+Shift+F3: the text appears exactly.
4. **J4 a program and a website.** Make `Ctrl+Alt+Shift+F14` → **Open a program** with
   `C:\Windows\System32\notepad.exe` (paste it with quotes from *Copy as path*: they are removed);
   the preview says *found*; **Test** says it would open it and opens nothing. Save and enable: the
   dialog names the exact path. Make `Ctrl+Alt+Shift+F15` → **Open a website** `github.com`
   (shown as `https://github.com/`). Press both chords: Notepad and the browser open only now.
5. **J4 mouse, mode and pause.** Make a **Mouse** rule (*Move by* right 40, *Keep moving
   smoothly*): the screen map shows your monitors and the target; **Test** moves only the simulated
   pointer. Make a **Switch mode** rule with **+ New mode** `Navigation`. Make a **Pause Ddalgak**
   rule on `Ctrl+Alt+Shift+F16`. Check each summary sentence reads naturally.
6. **J7 list.** Type into the search box (`Ctrl+F`); switch *Show* to *Disabled*, *Group* to *By
   mode*; select two rules with `Ctrl`+click and **Disable** them (one change, one revision). Drag a
   rule by its `≡` handle above another, then `Alt+Up`: the order in `ddalgak rule list` follows.
   **Duplicate** a rule: the copy is off. **Delete** two rules: the dialog names them, Cancel is
   focused; delete, then **Undo** in the toast: they are back.
7. **Truth with the CLI.** With the editor open, run `ddalgak rule disable <id>` in a terminal:
   the row turns off within a second and the status line returns to *saved and active*. Open a
   rule's form, change its name, run `ddalgak rule edit <id> --title X`, then **Save** in the
   editor: it refuses (*rule_conflict*) and keeps your text. Quit the tray app: the status line
   says *Ddalgak is not running* after the next change.
8. **Drafts and closing.** Change a rule's name and click another rule: *Discard your changes?*
   with **Keep editing** focused. Close the window with the change: it asks again.
9. **Export and import.** Select two rules, **Export…** to a file; open the file: no `consent`.
   **Import…** it into a second user profile: the plan names each website and program; the rules
   arrive turned off with *needs review*; enabling one asks for consent with the exact target.
10. **Korean and accessibility.** Press **한국어**: every label is Korean (summary sentences stay
    English). With Narrator on, `Tab` through the list and the form: every control is announced
    by name; the capture box announces the keys. Shrink the window below 860 px: the navigation
    moves into the header and the form replaces the list.
11. **J8 emergency.** With rules active, press `Ctrl+Alt+Shift+Esc`: the engine turns off; the
    editor's header says *Off* after its next request (open or save something); **Turn on** works
    again.
12. **Idle.** Leave the editor open for a minute without input: Task Manager shows 0 % CPU for
    `ddalgak-editor.exe`; note its memory.
13. **Step 75 — profile browser.** Open **Profiles** (rail, or click *Profile: …* in the header).
    **+ New profile** `Gaming`, **Duplicate** your work profile, **Rename…** the copy; type in
    *Search profiles*, change *Sort*. **Delete profile** is greyed out for the active one; delete
    the copy, then **Undo** in the toast.
14. **Step 75 — switch with the diff.** Hold CapsLock (remapped to Ctrl in the work profile), and
    with the mouse choose **Switch…** on Gaming: the diff shows *Now* and *After* side by side and
    **Cancel** is focused; press **Cancel** (nothing changes), then **Switch to this profile**.
    Release CapsLock and type in Notepad: Ctrl is not stuck. The header shows *switching…* and then
    Gaming once the app confirmed; `ddalgak mode status` shows the default mode.
15. **Step 75 — export and import preview.** **Export…** the work profile (leave *Include
    environment values* off); open the file: no `consent`, rules `"enabled": false`, environment
    values empty. In a second user profile, **Import profile…**, **Check the file**: the preview
    names each program and website and *needs your consent*; **Add them, not active**: the active
    profile stays as it was. Check a broken file: refused with the reason.
16. **Step 75 — history and rollback.** Rename a rule in the work profile twice. **History…** on
    work: pick an older version, compare side by side, **Restore this version**; then **Roll back
    to the last working version**. Type a label and **Save this version**: it appears as a save
    point. Each restore is a new change (the history grows; **Undo** works).
