# Ddalgak and AutoHotkey v2: feature matrix

Status: **current** (inventory) · **Matrix v2, 2026-11-04** (Step 68: remap rows; Step 69: hotstring and sending rows) · Reference: AutoHotkey **v2.0.31**
(the newest v2 entry of the official changelog, dated 2026-10-25) · Owner: mouv · Applies to:
Ddalgak · Created in Step 65

This matrix compares the behaviors people use AutoHotkey v2 for with what Ddalgak plans to
offer. **AutoHotkey is a behavioral reference only.** Ddalgak does not run AutoHotkey scripts,
does not embed an interpreter and does not claim compatibility with the AutoHotkey language. The
descriptions below are written in our own words from the official documentation; every row
names its source.

**Most of this matrix is not implemented yet.** A row marked `supported` is a commitment backed
by the specification in [DDALGAK_RULE_MODEL](DDALGAK_RULE_MODEL.md), to be implemented by the Step
named; it is usable today only when its Evidence names a Step that shipped it (Steps 66–68 so far:
the engine, the rule store, remaps and blocks). Every Step that implements a row updates its
Evidence, and the matrix version and date go up with every change of a status.

## Sources and method

All sources were read on **2026-11-04**. Only the official AutoHotkey v2 documentation and
Microsoft's Win32 documentation were used.

| Code | Source |
| --- | --- |
| **CL** | Changelog, https://www.autohotkey.com/docs/v2/ChangeLog.htm (newest entry v2.0.31, 2026-10-25; previous 2.0.30, 2026-10-09) |
| **H** | Hotkeys, https://www.autohotkey.com/docs/v2/Hotkeys.htm |
| **HS** | Hotstrings, https://www.autohotkey.com/docs/v2/Hotstrings.htm |
| **RM** | Remapping keys, https://www.autohotkey.com/docs/v2/misc/Remap.htm |
| **KL** | List of keys, https://www.autohotkey.com/docs/v2/KeyList.htm |
| **KH** | InstallKeybdHook, https://www.autohotkey.com/docs/v2/lib/InstallKeybdHook.htm |
| **SD** | Send, https://www.autohotkey.com/docs/v2/lib/Send.htm |
| **SL** | SendLevel, https://www.autohotkey.com/docs/v2/lib/SendLevel.htm |
| **IL** | #InputLevel, https://www.autohotkey.com/docs/v2/lib/_InputLevel.htm |
| **HI** | #HotIf, https://www.autohotkey.com/docs/v2/lib/_HotIf.htm |
| **WT** | WinTitle parameter, https://www.autohotkey.com/docs/v2/misc/WinTitle.htm |
| **W** | Win functions, https://www.autohotkey.com/docs/v2/lib/Win.htm |
| **R** | Run, https://www.autohotkey.com/docs/v2/lib/Run.htm |
| **C** | Click, https://www.autohotkey.com/docs/v2/lib/Click.htm |
| **MM** | MouseMove, https://www.autohotkey.com/docs/v2/lib/MouseMove.htm |
| **CB** | A_Clipboard, https://www.autohotkey.com/docs/v2/lib/A_Clipboard.htm |
| **ST** | SetTimer, https://www.autohotkey.com/docs/v2/lib/SetTimer.htm |
| **SU** | Suspend, https://www.autohotkey.com/docs/v2/lib/Suspend.htm |
| **IH** | InputHook, https://www.autohotkey.com/docs/v2/lib/InputHook.htm |
| **G** | Gui object, https://www.autohotkey.com/docs/v2/lib/Gui.htm |
| **CO** | Concepts and conventions, https://www.autohotkey.com/docs/v2/Concepts.htm |
| **SC** | Scripts, https://www.autohotkey.com/docs/v2/Scripts.htm |
| **P** | Using the program (UI Access, installation), https://www.autohotkey.com/docs/v2/Program.htm |
| **F** | FAQ, https://www.autohotkey.com/docs/v2/FAQ.htm |
| **SP** | Sleep, https://www.autohotkey.com/docs/v2/lib/Sleep.htm |
| **TT** | TrayTip (compared there with MsgBox and ToolTip), https://www.autohotkey.com/docs/v2/lib/TrayTip.htm |
| **M1** | Microsoft, LowLevelKeyboardProc, https://learn.microsoft.com/en-us/windows/win32/winmsg/lowlevelkeyboardproc |
| **M2** | Microsoft, SendInput, https://learn.microsoft.com/en-us/windows/win32/api/winuser/nf-winuser-sendinput |
| **M3** | Microsoft, KBDLLHOOKSTRUCT, https://learn.microsoft.com/en-us/windows/win32/api/winuser/ns-winuser-kbdllhookstruct |

Where a page did not state something, the row says so instead of filling the gap from memory.

## Legend

| Status | Meaning |
| --- | --- |
| `supported` | Ddalgak covers the behavior in its own model, specified in DDALGAK_RULE_MODEL, implemented by the Step named. |
| `partial` | Ddalgak covers a defined subset; the notes say exactly what is missing. |
| `planned` | Intended; the Step named specifies and implements it (today only the name or a placeholder exists). |
| `not intended` | Deliberately not provided; the notes say why. |

"Evidence" is `—` until a Step ships the row with tests and a real-Windows check.

## 1. Hotkeys

| # | AutoHotkey v2 behavior | Src | Ddalgak | Status | Step | Evidence |
| --- | --- | --- | --- | --- | --- | --- |
| 1.1 | Hotkeys with Ctrl, Alt and Shift via the symbols `^`, `!`, `+` | H | Canonical text `Ctrl+Alt+Shift+J` (hotkey contract syntax) | supported | 66 | — |
| 1.2 | Win-key hotkeys via `#` (`#n::`) | H | Not as triggers in v1: the hotkey contract reserves every `Win` chord. Win chords can be *sent* (`send_keys` `Win+D`). A later contract revision may allow selected ones (ADR 0022) | not intended | — | — |
| 1.3 | Left/right-specific modifiers via `<` and `>` | H, KL | `LCtrl+`, `RAlt+`, … in chords; bare `RAlt`, `LCtrl` as remap or tap triggers; side-specific remap outputs (`RCtrl+Up`) | supported | 66, 68 | Step 68: core tests (DDALGAK_ENGINE §11) |
| 1.4 | AltGr via `<^>!` | H, KL | `AltGr+` in the canonical syntax; `altgr_collision` warning for `Ctrl+Alt` chords | supported | 66 | — |
| 1.5 | Wildcard `*`: fire even with extra modifiers held | H | Remap and block triggers without modifiers are wildcards by design; other rules match exactly, and an exact chord wins over the wildcard. No general flag. Reserved chords and `Win` chords are never captured by the widening | partial | 68 | Step 68: core tests (DDALGAK_ENGINE §11) |
| 1.6 | `~`: the key keeps its native function | H | `pass_through: true`, since Step 68 also for a remap without a tap alternative (the original key and the target are both held) | supported | 66, 68 | Step 68: core tests (DDALGAK_ENGINE §11) |
| 1.7 | `$`: force the hook so `Send` cannot re-trigger the hotkey | H | Always the case: every rule uses the hook, and Ddalgak's own input never triggers a rule (RULE_MODEL §7.4) | supported | 66 | — |
| 1.8 | `UP`: fire on key release | H | `on: release` fires when the chord is released with no other key in between (a tap). A release after other keys is not a trigger in v1 | partial | 66 | — |
| 1.9 | Long-press detection (scripted with key-state waits) | H | `on: hold` with `hold_ms` 200–5000, no script | supported | 66 | — |
| 1.10 | Custom combinations `a & b`, where any key acts as a prefix | H | Real modifiers only. The main use, a layer while a key is held, is `mode` `hold` (CapsLock held → Navigation). A non-modifier prefix key in a chord is not in v1 | partial | 72 | — |
| 1.11 | Multi-key sequences (no built-in hotkey syntax; scripts use Input hooks) | H, IH | Up to four chords: `Ctrl+K Ctrl+U` | supported | 66 | — |
| 1.12 | Creating, changing, enabling and disabling hotkeys at run time with the `Hotkey` function | H | Rule CRUD in the GUI and CLI with live reload | supported | 67 | — |
| 1.13 | Calling a named function with `ThisHotkey` | H | No functions: a rule has declarative steps | not intended | — | — |
| 1.14 | Hotkey, timer and menu subroutines run as threads that can interrupt one another | CO, ST | One run per rule; a trigger during a run is `busy` | partial | 67 | — |
| 1.15 | Mouse-button hotkeys (`LButton`, `XButton1`; always through the mouse hook) | H, KL | `MouseMiddle`, `MouseX1`, `MouseX2` as triggers, with modifiers, through a low-level mouse hook that exists only while such a rule is active (permission `mouse-hook`, ADR 0031). The primary and secondary buttons are never triggers, so no rule can take away clicking | partial | 68 | Step 68: core tests; the on-demand mouse hook tested in the real test host (no mouse input injected; real buttons are a manual check) |
| 1.16 | Wheel hotkeys (`WheelUp`…, down events only) | H, KL | Wheel notches as triggers, through the same on-demand mouse hook | planned | 71 | — |
| 1.17 | Game controller buttons (`Joy1`…) | KL | — | not intended | — | — |
| 1.18 | Hotkeys by scan code or virtual key (`SC159::`, `vk24::`) | KL | Physical keys by W3C name (`code:KeyQ`) and named keys; raw numbers are never stored (`KIM-NFR-022`) | partial | 66 | — |
| 1.19 | Media and browser keys (`Media_Play_Pause`, `Volume_Up`, `Browser_Back`, `Launch_App1`) | KL | The contract's media and volume keys; browser and launch keys are an additive key-name change | partial | 66 | — |
| 1.20 | Lock keys as hotkeys; forcing a lock state on or off | KL, KH | `CapsLock` and `NumLock` (and `ScrollLock`) as triggers and outputs (a remapped CapsLock never toggles); forcing a lock state is not in v1 | partial | 68 | Step 68: core tests (DDALGAK_ENGINE §11) |
| 1.21 | `Suspend` with `#SuspendExempt` hotkeys | SU | Engine `pause`; engine-control rules (pause, resume, turn off) stay active | supported | 73 | — |
| 1.22 | Pausing running threads (`Pause`) | SU | Pausing cancels running rules; no thread suspension | partial | 73 | — |
| 1.23 | `ListHotkeys` | H, KH | Rule list, `ddalgak rule list --json`, `ddalgak hotkeys list --json` | supported | 67 | — |
| 1.24 | `KeyHistory`: the last keystrokes, for debugging | KH | Never: no keystroke is kept (`DDALGAK-SEC-001`). The playground explains decisions instead | not intended | — | — |

## 2. Hotstrings

| # | AutoHotkey v2 behavior | Src | Ddalgak | Status | Step | Evidence |
| --- | --- | --- | --- | --- | --- | --- |
| 2.1 | Auto-replace `::btw::by the way` | HS | Hotstring triggers (`trigger.hotstring`, a new trigger kind) with `type_text` or `send_keys`: the abbreviation is erased and the replacement typed | supported | 69 | Step 69: core, worker and CLI tests (DDALGAK_ENGINE §12); real typing manual |
| 2.2 | Options `*` (no end character), `?` (inside words), `C`/`C1` (case), `O` (omit end character), `B0` (no backspacing) | HS | `immediate`, `inside_words`, `case: sensitive`/`insensitive` (default `conform`), `omit_end`, `erase: false` (RULE_MODEL §3.4) | supported | 69 | Step 69: core, worker and CLI tests (DDALGAK_ENGINE §12); real typing manual |
| 2.3 | `R` raw and `T` text modes, `Kn` key delay, `SI`/`SP`/`SE` send modes | HS | Text is always literal (no key syntax), typed as Unicode or pasted; `key_delay_ms`; only `SendInput` | partial | 69 | Step 69: core, worker and CLI tests (DDALGAK_ENGINE §12); real typing manual |
| 2.4 | `X`: execute an expression instead of replacing | HS | No expressions; a hotstring can run any rule steps instead | not intended | — | — |
| 2.5 | Configurable end characters (`#Hotstring EndChars`) | HS | `end_chars` per hotstring; default AutoHotkey's set | supported | 69 | Step 69: core, worker and CLI tests (DDALGAK_ENGINE §12); real typing manual |
| 2.6 | Case conformity of the replacement | HS | `case: conform` (the default): `BTW` → upper case, `Btw` → first letter upper case | supported | 69 | Step 69: core, worker and CLI tests (DDALGAK_ENGINE §12); real typing manual |
| 2.7 | Recognizer resets on window change, navigation keys and (by default) mouse clicks | HS | Window and focus changes, navigation and non-text keys, shortcuts, password fields, IME compositions, pause, 10 s idle; a bounded buffer (longest abbreviation + 1), never stored. Mouse clicks inside the same field are not seen (no mouse hook for hotstrings) | partial | 69 | Step 69: core, worker and CLI tests (DDALGAK_ENGINE §12); real typing manual |
| 2.8 | Input method editors: the InputHook documentation states that IMEs are not supported | IH | Nothing fires during a composition or while the IME is in a native mode (Hangul); typed text bypasses the IME (Unicode events) | partial | 69 | Step 69: core and fake-probe tests; the Korean IME itself is a manual check (DDALGAK_ENGINE §12) |
| 2.9 | Abbreviations up to 40 characters; replacement up to about 5000 characters with SendInput | HS | Abbreviations 1–40 characters; `type_text` up to 4096 characters | partial | 69 | Step 69: core, worker and CLI tests (DDALGAK_ENGINE §12); real typing manual |

## 3. Remapping

| # | AutoHotkey v2 behavior | Src | Ddalgak | Status | Step | Evidence |
| --- | --- | --- | --- | --- | --- | --- |
| 3.1 | `a::b`: a key behaves as another, implemented as a down/up hotkey pair | RM | `remap`: target down while the trigger is down, repeated with it, up with it | supported | 68 | Step 68: core and worker tests; real-input smoke on obscure keys (DDALGAK_ENGINE §11) |
| 3.2 | Modifiers on the origin are released while the destination is sent; held modifiers combine with the destination | RM | Same semantics (RULE_MODEL §3.2, §5.1), restored afterwards if still held | supported | 68 | Step 68: core tests (DDALGAK_ENGINE §11) |
| 3.3 | An upper-case destination forces Shift (`a::B`) | RM | Write the chord explicitly: `to: "Shift+B"` | partial | 68 | Step 68: core tests (DDALGAK_ENGINE §11) |
| 3.4 | A key to a modifier (`CapsLock::Ctrl`) | RM | `remap` to `LCtrl`, with an optional tap alternative (dual role); the injected modifier combines with other rules (a `Ctrl+J` rule fires while CapsLock is held) | supported | 68 | Step 68: core tests (DDALGAK_ENGINE §11) |
| 3.5 | A key to a mouse button (`RCtrl::RButton`) | RM | `mouse_click` with `op: hold` on that key | supported | 71 | — |
| 3.6 | A mouse button to another button or key (`XButton1::LButton`) | RM | `remap` from `MouseMiddle`/`MouseX1`/`MouseX2` to a key, chord or any mouse button (`MouseLeft`/`MouseRight` are the logical primary and secondary buttons); a key can also act as a mouse button | supported | 68 | Step 68: core and worker tests (no mouse input injected; real buttons are a manual check) |
| 3.7 | The mouse wheel cannot be remapped | RM | Wheel as output: `mouse_scroll`. Wheel as trigger: Step 71 | partial | 71 | — |
| 3.8 | Disabling a key (`RWin::return`) | RM | `block`, for any key except reserved ones (the Win keys among them), and for the middle and X mouse buttons; a plain trigger is widened (Ctrl+Insert too) | partial | 68 | Step 68: core and worker tests; real-input smoke on obscure keys (DDALGAK_ENGINE §11) |
| 3.9 | Context-specific remaps (`#HotIf` around remaps) | RM, HI | Modes now; app conditions in Step 77 | supported | 72, 77 | — |
| 3.10 | Remaps do not trigger other hook hotkeys unless `#InputLevel` is raised | RM, IL, SL | Ddalgak's own output never triggers rules; no levels. `injected: accept` admits only other programs' input, for rules that inject nothing | partial | 66 | — |
| 3.11 | Registry `Scancode Map` as an alternative (needs a reboot, applies to all users) | RM | Ddalgak never edits system-wide keyboard settings | not intended | — | — |
| 3.12 | Tap for one key, hold for another (scripted in AutoHotkey) | RM | `remap` with `tap`, `tap_ms`: another key pressed during the hold is sent after the target, unchanged | supported | 68 | Step 68: core and worker tests; real-input smoke on obscure keys (DDALGAK_ENGINE §11) |
| 3.13 | Known issues: remapping to Alt can disturb Alt+Tab; some games ignore remaps | RM | Reserved chords pass unchanged even under a widened remap; games: see 9.3 | partial | 68 | Step 68: core tests (DDALGAK_ENGINE §11) |

## 4. Sending input and commands

| # | AutoHotkey v2 behavior | Src | Ddalgak | Status | Step | Evidence |
| --- | --- | --- | --- | --- | --- | --- |
| 4.1 | `Send` keys with modifiers (`Send "^c"`) | SD | `send_keys` with up to eight chords | supported | 69 | Step 69: core, worker and CLI tests (DDALGAK_ENGINE §12); real typing manual |
| 4.2 | Literal text (`SendText`, `{Text}`) and Unicode characters (`{U+nnnn}`) | SD | `type_text`, Unicode events independent of layout and IME; the CLI's `--escapes` reads `\u{…}` | supported | 69 | Step 69: core, worker and CLI tests (DDALGAK_ENGINE §12); real typing manual |
| 4.3 | Send modes Input (default, buffers user input), Event and Play (Play may not work under UAC or on Windows 11) | SD, F | `SendInput` only, inserted serially (M2); one call per remap change | partial | 68 | Step 68: core and worker tests; real-input smoke on obscure keys (DDALGAK_ENGINE §11) |
| 4.4 | `{Blind}`: keep physically held modifiers | SD | Fixed, documented modifier handling per action (RULE_MODEL §5.1, §5.3); no per-call flag | partial | 68 | — |
| 4.5 | `{Key down}` / `{Key up}` | SD | Only through `remap` and `hold`, which always release; no stray key-down | partial | 68 | Step 68: core and worker tests; real-input smoke on obscure keys (DDALGAK_ENGINE §11) |
| 4.6 | `SendLevel` / `#InputLevel` | SL, IL | No levels: own input never triggers rules | not intended | — | — |
| 4.7 | Key delays (`SetKeyDelay`) | SD | `key_delay_ms` (text), `step_delay_ms` (sequences) | partial | 69 | Step 69: core, worker and CLI tests (DDALGAK_ENGINE §12); real typing manual |
| 4.8 | `ControlSend`: keys to a window or control without focusing it (listed in the FAQ as a workaround for programs that ignore `Send`) | F | Not in v1; may be considered with window actions | not intended | — | — |
| 4.9 | `Click` with button, count, down/up and relative coordinates | C | `mouse_click` (`click`, `double_click`, `hold`; optional point) | supported | 71 | — |
| 4.10 | `MouseMove` absolute or relative, with a speed in Event mode | MM | `mouse_move` absolute, relative, or continuous while held with speed and acceleration | supported | 71 | — |
| 4.11 | `MouseClickDrag` | C | Hold a button while moving (`hold` + moves); a one-step drag in Step 78 | partial | 71, 78 | — |
| 4.12 | `CoordMode` (screen, window, client) | C, MM | `virtual_screen`, `primary_monitor`, `active_window`; no client-area mode | partial | 71 | — |
| 4.13 | `Run` an executable with parameters | R | `open_program` with a path or packaged-app ID and an argument list (never a shell string) | supported | 70 | — |
| 4.14 | `Run` documents, shortcuts, CLSIDs and shell verbs | R | Executables and packaged apps only in v1 | not intended | — | — |
| 4.15 | `Run` a URL in the default browser; `mailto:` links | R | `open_url` for `http` and `https`; other schemes refused | partial | 70 | — |
| 4.16 | `*RunAs`: run as administrator | R | Never elevates (`KIM-SEC-004`, `DDALGAK-SEC-007`) | not intended | — | — |
| 4.17 | `RunWait` and exit codes | R | A separately consented command action | planned | 79 | — |
| 4.18 | `Run` options `Max`, `Min`, `Hide` | R | `normal`, `maximized`, `minimized`; no hidden programs | partial | 70 | — |
| 4.19 | `A_Clipboard`, `ClipWait`, `ClipboardAll`, `OnClipboardChange` | CB | `type_text` paste with restore of every format (refused when one cannot be copied), kept out of clipboard history; clipboard actions in Step 79; no clipboard monitoring | partial | 69, 79 | Step 69: fake-clipboard tests; the real clipboard is a manual check (DDALGAK_ENGINE §12) |
| 4.20 | `SetTimer`: periodic or one-shot timers | ST | Scheduled triggers with an idle budget | planned | 79 | — |
| 4.21 | `Sleep`, rounded up to the OS timer granularity (about 10 or 15.6 ms) | SP | `delay` (1–10000 ms, 60 s per run), same granularity | supported | 67 | — |
| 4.22 | Message boxes (`MsgBox`), tooltips near the cursor, tray notifications (`TrayTip`, a toast on Windows 10 and later) | TT | `notify`: local notifications only, never modal from a hotkey | planned | 79 | — |
| 4.23 | `InputHook`: collect what the user types | IH | Never: shaped like a keylogger. Hotstrings use a bounded buffer | not intended | — | — |
| 4.24 | Key state queries and waits (`GetKeyState`, `KeyWait`) | KH | Hold triggers and the engine's own state tracking; no script queries | partial | 66 | — |
| 4.25 | `BlockInput` | C | Ddalgak never blocks the user's input | not intended | — | — |

## 5. Context-sensitive hotkeys

| # | AutoHotkey v2 behavior | Src | Ddalgak | Status | Step | Evidence |
| --- | --- | --- | --- | --- | --- | --- |
| 5.1 | `#HotIf WinActive("ahk_exe code.exe")` | HI, WT | `when.apps` by file name or path; `except_apps` | supported | 77 | — |
| 5.2 | Window title matching (exact, prefix, substring, RegEx via `SetTitleMatchMode`) | WT | `window_title` `contains` or `equals`, case-insensitive; no RegEx in v1 | partial | 77 | — |
| 5.3 | `ahk_class` | WT | A window-class matcher is an additive condition | planned | 77 | — |
| 5.4 | `ahk_group` window groups | WT | A list of app matchers in one rule | partial | 77 | — |
| 5.5 | Any Boolean expression as a condition, evaluated on the main thread when the key is pressed | HI | Fixed condition kinds only, evaluated at trigger time off the hook thread | not intended | — | — |
| 5.6 | Precedence: the topmost eligible variant wins; the global variant has the lowest precedence | HI | Deterministic precedence: mode-scoped, app, title, priority, order (RULE_MODEL §7.1), shown by `explain` | supported | 77 | — |
| 5.7 | Context from script variables (`#HotIf MyMode`) | HI | User-defined modes | supported | 72 | — |

## 6. Window actions

| # | AutoHotkey v2 behavior | Src | Ddalgak | Status | Step | Evidence |
| --- | --- | --- | --- | --- | --- | --- |
| 6.1 | Activate, minimize, maximize, restore, move, always-on-top (`WinActivate`, `WinMove`, …) | W | `window` steps under `window-control` | planned | 79 | — |
| 6.2 | `WinClose` | W | With `window` steps, confirmed like any destructive effect | planned | 79 | — |
| 6.3 | `WinKill` | W | Ddalgak does not end other programs' processes | not intended | — | — |
| 6.4 | Transparency, styles, regions (`WinSetTransparent`, `WinSetStyle`, …) | W | — | not intended | — | — |
| 6.5 | Waiting for and querying windows (`WinWait`, `WinGet…`) | W | Conditions read the foreground window at trigger time only | not intended | — | — |

## 7. GUI

| # | AutoHotkey v2 behavior | Src | Ddalgak | Status | Step | Evidence |
| --- | --- | --- | --- | --- | --- | --- |
| 7.1 | Scripts build their own windows with the `Gui` object (controls, events, menus) | G | Ddalgak's own editor manages rules; users do not build windows | not intended | — | — |
| 7.2 | A script's tray icon, with suspend and pause | SU | Ddalgak's tray icon with the engine states and the rule editor | supported | 73 | — |
| 7.3 | Writing hotkeys requires a text editor and the script syntax | SC | A visual editor with key capture, effect tiles and a test playground | supported | 74, 76 | — |

## 8. Scripting language

| # | AutoHotkey v2 behavior | Src | Ddalgak | Status | Step | Evidence |
| --- | --- | --- | --- | --- | --- | --- |
| 8.1 | Expressions, variables, functions, objects and classes | CO | Rules are declarative data. Bounded variables in macros are planned (Step 78), not a language | not intended | — | — |
| 8.2 | `DllCall`, COM, file reading and writing | CO | — | not intended | — | — |
| 8.3 | `#Include` libraries | SC | — | not intended | — | — |
| 8.4 | Compiling scripts into executables (Ahk2Exe) | SC | — | not intended | — | — |
| 8.5 | Running `.ahk` scripts | SC | **Never.** An importer converts a declarative subset into disabled rules with a coverage report, without executing anything | partial | 80 | — |

## 9. Limitations and environment

| # | AutoHotkey v2 behavior | Src | Ddalgak | Status | Step | Evidence |
| --- | --- | --- | --- | --- | --- | --- |
| 9.1 | A non-elevated script cannot send to or receive hotkeys from elevated windows (UAC/UIPI); `SendInput` fails silently | F, M2 | Same OS rule. Ddalgak detects an elevated foreground window, skips injecting rules and says so (`skipped_elevated_target`) | supported | 66 | — |
| 9.2 | Workarounds: "Run with UI access" (signed `_UIA.exe` in Program Files) or running as administrator | P, F | No `UIAccess`, no elevation (`DDALGAK-SEC-007`) | not intended | — | — |
| 9.3 | Games with anti-cheat (GameGuard, Hackshield) are likely to block AutoHotkey; some games ignore sent input | F | Same limitation, documented; Ddalgak never hides from or evades anti-cheat (`DDALGAK-SEC-012`) | supported | 82 | — |
| 9.4 | The OS does not allow simulating Ctrl+Alt+Del | SD | The Secure Attention Sequence is refused as an output (schema `output_reserved`; the CLI refuses it with `rule_invalid`) | supported | 67, 68 | Step 68: fixture `remap-to-emergency-chord`, CLI refusal (e2e scenario A) |
| 9.5 | Win hotkeys delay the L key to avoid locking the PC | H | Win chords are not triggers; nothing to work around | not intended | — | — |
| 9.6 | Input method editors are not supported by InputHook | IH | IME guard (no rule during a composition) and layout-independent Unicode typing | supported | 66, 69 | — |
| 9.7 | Keyboard hook timeout: Windows removes a hook that does not answer within `LowLevelHooksTimeout` (at most 1000 ms since Windows 10 1709), silently on Windows 7 and later | M1 | The hook callback only classifies the event and returns (< 1 ms, `DDALGAK-NFR-002`); rules run on a worker thread | supported | 66 | — |
| 9.8 | Injected input is flagged (`LLKHF_INJECTED`, `LLKHF_LOWER_IL_INJECTED`) and can carry extra information | M3 | Used to ignore Ddalgak's own input and to apply `injected: accept` | supported | 66 | — |
| 9.9 | A hotkey that is not on the active layout may be invalid | H, KL | Logical keys follow the layout's virtual keys; physical `code:` keys work on every layout | supported | 66 | — |
| 9.10 | Installation for the current user without administrator rights | P | Per-user install through Kim only, never elevated | supported | 82 | — |

## 10. Ddalgak additions without an AutoHotkey built-in

| # | Capability | Ddalgak | Step |
| --- | --- | --- | --- |
| 10.1 | Visual rule editor with key capture, effect tiles and validation | [GUI_JOURNEYS](DDALGAK_GUI_JOURNEYS.md) | 74 |
| 10.2 | Test playground: simulate without side effects, live test confined to the playground | RULE_MODEL §11 | 76 |
| 10.3 | Explicit, per-rule consent for injection, launching and clipboard; fingerprinted launch targets | RULE_MODEL §6 | 67–70 |
| 10.4 | A non-remappable emergency chord that removes the hook and releases every held key | [DDALGAK_SAFETY §7](DDALGAK_SAFETY.md) | 73 |
| 10.5 | Conflict reports across Kim tools and reserved keys, with non-destructive offers | HOTKEY_CONTRACT §9 | 67, 81 |
| 10.6 | Profiles with history, rollback, export and safe import | RULE_MODEL §10 | 75 |
| 10.7 | Versioned JSON rules, `--json` CLI and machine-checked schemas | RULE_MODEL §13–§14 | 67, 81 |
| 10.8 | Activity list without keys or text | RULE_MODEL §12 | 66 |

## Audit history

| Matrix | Date | AutoHotkey reference | Change |
| --- | --- | --- | --- |
| v1 | 2026-11-04 | v2.0.31 | First inventory (Step 65). Nothing implemented. |
