# Ddalgak GUI journeys and wireframes

Status: **current** (Step 65, design; implemented in Steps 73–76) · Requirements:
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

The editor window (`ddalgak-gui.exe`, opened from the tray, the `editor.open` shortcut or the
Start menu) has three regions; `F6` cycles them (DESIGN_SYSTEM §4.1).

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
| Mouse | Segmented: *Move* (by / to / while held with speed), *Scroll* (direction, notches), *Click* (button, click, double, hold) with a small screen map showing the target point across monitors | "Moves the pointer right while held" | type for me |
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

"Block this key" has no inputs ("Insert does nothing (blocked)"). Under the summary the form says
when a plain trigger combines with held modifiers ("Shift+CapsLock sends Shift+Escape") and, for
a mouse-button trigger, that Ddalgak then uses a mouse hook while the engine is on. Choosing
`MouseLeft` or `MouseRight` as the trigger is not offered.

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
