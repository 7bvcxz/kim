# Ddalgak safety: risks, scope and policies

Status: **current** (Step 65, specification; nothing here has been run on real Windows yet,
§11) · Requirements: `DDALGAK-SEC-001…014`, `DDALGAK-NFR-002`, `DDALGAK-NFR-006`,
`DDALGAK-NFR-009`, `KIM-SEC-020…023` · Decision: [ADR 0022](adr/0022-ddalgak-rule-model-and-safety.md)

A keyboard hook sees every key, and an injector can type into any window the user can reach.
This document fixes what Ddalgak may and may not do with that power, what the operating system
prevents anyway, and who verifies each claim on real Windows. Windows behavior from Microsoft
documentation is marked *documented*; the sources are listed in
[DDALGAK_AHK_MATRIX](DDALGAK_AHK_MATRIX.md#sources-and-method) (M1–M3) and in the
[hotkey contract](HOTKEY_CONTRACT.md).

## 1. Scope and trust boundary

| Ddalgak does | Ddalgak never does |
| --- | --- |
| Run as the signed-in user, without elevation, inside `ddalgak-gui.exe` that the user started (or opt-in start at sign-in) | Install a service, driver, scheduled task, `UIAccess` binary, SYSTEM or elevated component |
| Install one low-level keyboard hook (`WH_KEYBOARD_LL`) while the engine is on, and (from Step 68, after catalog disclosure) a low-level mouse hook for mouse triggers | Hook into other processes (no DLL injection), read other programs' memory, or use kernel input filters |
| Inject keyboard and mouse input with `SendInput` when the user's rule says so and the user consented | Inject on the secure desktop, into password fields, or into elevated windows by any workaround |
| Start programs and hand `http(s)` links to the browser, as the user | Run shell strings, scripts or elevated programs; open network connections itself |
| Keep rules, history and an optional outcome list in the user's profile | Keep, log or transmit keystrokes, typed text, clipboard contents, window titles or URLs |

**Trust boundary.** Like every Kim tool, Ddalgak trusts code running as the same user at medium
integrity (see `R-09` in [RISK_REGISTER](RISK_REGISTER.md)): such code could edit `rules.json`
and its consent records. Ddalgak does not pretend otherwise. It limits the damage instead: rules
are declarative (no code to execute), launches are restricted to `.exe` files with argument
lists, shell hosts are refused, and the engine never turns itself on. The local IPC used by the
CLI follows the identity checks of [ADR 0005](adr/0005-local-ipc.md)/[ADR 0019](adr/0019-secure-host-supervisor-and-ipc-v1.md)
(Step 66 decides the details).

## 2. OS-reserved and Kim-reserved shortcuts

- **Triggers never use reserved chords** (`DDALGAK-SEC-009`, RULE_MODEL §3.1): the Secure
  Attention Sequence (`Ctrl+Alt+Delete`, also with `NumpadDecimal`), every `Win` chord,
  `Ctrl+Shift+Escape` (Task Manager, the user's way out), `Ctrl+Escape`, `Alt+Tab` and
  `Alt+Escape` variants, `Alt+Shift+PrintScreen` (High Contrast), `Alt+F4`, the IME mode keys,
  global `F12` and the emergency chord. The schema rejects them (`schema-check` proves it with
  invalid examples).
- **Widened remaps never capture them**: with `Tab` remapped, `Alt+Tab` still switches windows
  (RULE_MODEL §3.2).
- **Outputs never produce** the Secure Attention Sequence (Windows does not let programs
  synthesize it; *documented* for AutoHotkey's `Send`) or the emergency chord. Other chords,
  including `Win` chords such as `Win+D`, may be sent: sending them is what the user's own keys
  could do.
- **Accessibility gestures** (Sticky Keys by pressing Shift five times, Filter Keys by holding
  right Shift, Narrator, Magnifier) are never intercepted or swallowed (HOTKEY_CONTRACT §12.3).
  A rule on a bare `RShift` with `on: release` fires on a tap but lets the hold through.
- **Other programs' global hotkeys** are passed on unless a rule explicitly uses that chord
  (`KIM-SEC-020`); `kim hotkeys check --all-tools` reports such overlaps with Kim tools as
  `hook_overrides_global`.
- **Win chords as triggers** (`Win+N` to open a program, a popular AutoHotkey use) are not in
  v1. Allowing selected ones needs a hotkey contract revision and an ADR; `Win+L` and other
  shell-owned chords would stay reserved in any case.

## 3. Administrator and elevated windows (UIPI)

- Windows lets a program inject input only into windows at an equal or lower integrity level;
  `SendInput` blocked this way fails **without** an error code that says so (*documented*, M2).
  Keys typed into an elevated window are not delivered to a non-elevated low-level hook either
  (UIPI, *documented*, HOTKEY_CONTRACT §12.1).
- Ddalgak therefore checks the foreground window's process integrity before running an
  injecting rule. If it is higher than Ddalgak's, the rule is skipped with
  `skipped_elevated_target` and the key passes unchanged; the activity list and, once per
  session, a quiet notice explain it ("Ddalgak can't type into administrator windows").
  Non-injecting rules (open a program, switch mode, pause) still run.
- **No workaround**: no elevation, no `UIAccess` (which AutoHotkey offers through signed copies
  in Program Files), no service (`DDALGAK-SEC-007`, `KIM-SEC-004`). Running Ddalgak itself
  elevated is not supported: the app refuses to turn the engine on when started elevated and
  says why, because an elevated hook and injector would reach every window.

## 4. Engine lifecycle

- **Off by default.** Each start leaves the engine `off` unless the user chose
  `engine_on_start: on` (`DDALGAK-FR-003`).
- **Crash and emergency.** The app writes a small run marker while the engine is on and removes
  it on a clean stop. If the marker is present at start (crash, power loss, killed process), or
  the last stop was an emergency off, the engine stays off and the app says why. A broken rule
  that crashed the engine can therefore never crash it again on the next start.
- **Session events.** On session lock, a switch to the secure desktop, sign-out and shutdown,
  held keys and buttons are released and running rules are cancelled; the hook stays installed
  while locked but no rule fires until the user's desktop is back. Sleep and resume are treated
  like lock and unlock.
- **Invalid configuration** never turns the engine on, and never crashes it (`KIM-SEC-023`).
- **Never silently re-enabled.** Not after a crash, a reboot, an update, an import or a
  configuration change.

## 5. Keyboard layouts and IME (Korean first)

- **Key identity** follows HOTKEY_CONTRACT §6: logical letters follow the layout's virtual keys
  (on Korean 2-set, `Ctrl+Z` is the key with ㅋ), `code:` keys follow the physical position,
  and stored rules never contain virtual-key numbers (`DDALGAK-NFR-005`).
- **Composition guard.** While an IME composition is active, no rule fires; the keys belong to
  the IME. Ddalgak never commits or cancels a composition (`skipped_ime_composition`).
- **IME mode keys** (`HangulMode` 한/영, `Hanja` 한자, `code:Lang1`–`code:Lang5`, …) are never
  triggers, but may be remap **outputs**: Right Alt acting as 한/영 on a US-layout keyboard is
  one of the most common Korean remaps (example `01-remap.json`). Whether the Korean IME accepts
  an injected `VK_HANGUL` must be verified on real Windows (Step 68).
- **Typed text** uses Unicode key events (`KEYEVENTF_UNICODE`), so Hangul syllables, emoji and
  characters outside the Basic Multilingual Plane arrive independent of the active layout and
  IME mode. Applications that ignore Unicode key events get the `paste` method.
- **AltGr layouts.** `Ctrl+Alt+<key>` triggers get `altgr_collision` warnings; on such layouts
  Windows reports AltGr as `LCtrl+RAlt` (*documented*).
- **Dead keys.** Ddalgak never translates keys to characters inside the hook (no `char:`
  triggers), because doing so disturbs the dead-key state of the focused application.
- **Layout switches** change nothing in `rules.json`; logical rules keep their meaning, physical
  rules keep their position.

## 6. Anti-cheat and games

- Many games read input through paths that ignore synthesized input, and anti-cheat systems may
  detect or forbid low-level hooks and injected input; the AutoHotkey FAQ notes that games with
  anti-cheat are likely to block it (*documented*, F). Using such tools can break a game's terms
  of service.
- Ddalgak **never hides** (no process renaming, no hook concealment), **never evades or bypasses**
  anti-cheat or input protection, and installs no kernel component or virtual input device
  (`DDALGAK-SEC-012`). It makes no promise that rules work in any game.
- Users can keep a near-empty "Gaming" profile, pause rules with one key, or turn the engine off
  so that no hook exists. Step 82 documents tested behavior with a few common non-protected games
  and states plainly that protected games are out of scope.

## 7. Global hook security and emergency disable

- **One hook, short callback.** The hook callback copies the event into a lock-free queue,
  decides synchronously only whether the event is consumed (using precomputed trigger tables),
  and returns in well under 1 ms (`DDALGAK-NFR-002`). Running steps, reading window
  information and launching programs happen on worker threads. Windows removes a hook that
  exceeds `LowLevelHooksTimeout` (at most 1000 ms on Windows 10 1709 and later), silently on
  Windows 7 and later (*documented*, M1); Ddalgak treats getting anywhere near that as a bug and
  never relies on it as a safety net.
- **Hook health.** Windows gives no notice when it removes a hook (*documented*). Step 66
  designs an on-demand health check, run when the editor opens or `ddalgak status` runs and
  never on a timer, that reports a removed hook and offers to reinstall it. The engine never
  reinstalls a hook on its own.
- **Emergency chord** `Ctrl+Alt+Shift+Escape` (`DDALGAK-SEC-003`): evaluated first in the
  callback on the *physical* key state (events without `LLKHF_INJECTED`), so no remap, block,
  mode or invalid configuration can hide it. On detection, within the same event: uninstall the
  hook, send key-up for every key and button Ddalgak holds, cancel all runs, set the engine
  `off`, write the emergency marker (§4), and show a non-modal notice. The chord is reserved for
  every Kim tool (HOTKEY_CONTRACT §7.4). Its choice is provisional until Step 73 verifies it on
  real Windows (including keyboards where the chord is awkward and remote-desktop sessions); a
  change goes through an ADR 0017 note and a contract revision.
- **Other ways out** (`KIM-SEC-023`): the tray menu, the `engine.toggle` shortcut (registered
  with `RegisterHotKey`, independent of the hook), `ddalgak engine off`, `ddalgak-gui.exe
  --no-hotkeys` or `KIM_NO_HOTKEYS` (starts with every shortcut off and the engine off),
  `Ctrl+Shift+Escape` to end the process; Windows removes the hook when the process ends.
- **Visible hook** (`DDALGAK-SEC-010`): the hook exists only inside the user-started app with
  its tray icon; no CLI command starts the app, creates a tray icon or installs a hook.

## 8. Password fields, secure desktop, privacy

- **Secure desktop** (Ctrl+Alt+Del screen, UAC prompts, sign-in, lock screen) is a separate
  desktop whose input never reaches the user desktop's hooks (*documented*). Ddalgak never tries
  to change that (`DDALGAK-SEC-002`, `KIM-SEC-021`).
- **Password fields.** Before acting, the engine asks whether the focused element is a password
  field (`ES_PASSWORD` edit style, or UI Automation `IsPassword`; best effort, *documented*). If
  it is, every rule is skipped and keys pass unchanged, except the emergency chord. Text is never
  typed into a password field. The recorder (Step 78) and hotstrings (Step 69) never record or
  buffer keys there. Detection is best effort: applications that draw their own password fields
  without exposing them may not be recognized, and the documentation says so; rules in such
  applications can be excluded with `except_apps`.
- **No keylogging** (`DDALGAK-SEC-001`): the hook keeps only the state needed for matching (which
  keys are down, a pending sequence step, the hotstring buffer from Step 69 limited to the
  longest abbreviation and cleared on every reset). Nothing else is retained.
- **Activity and logs** contain rule IDs, profile and mode IDs, outcome codes, step indexes and
  durations only; never key names the user typed, text, window titles, paths, arguments, URLs or
  clipboard contents (`DDALGAK-SEC-011`). Persisting them is opt-in.
- **Rule content** is personal data in the user's profile (snippets, paths, URLs). The GUI says
  that `rules.json` is not a password store; an export dialog reminds users that the file
  contains their snippets and targets.

## 9. Linux (X11 and Wayland), milestone D7

Nothing is claimed for Linux before Step 83 (`DDALGAK-NFR-009`). What is already clear:

| Need | X11 | Wayland |
| --- | --- | --- |
| Global triggers | `XGrabKey` for chords; full key observation needs extensions (XInput2 raw events) that any X client may use | No global key observation by design; the desktop portal's GlobalShortcuts interface lets an app *request* shortcuts that the compositor and the user bind (the app gets a trigger description, not raw keys) |
| Remapping a key while held | XTest injection plus grabs; fragile but possible | Not available to normal clients; compositor-specific or `uinput` (needs device permissions, usually root or a group) |
| Injecting keys and pointer | XTest | The RemoteDesktop portal (or libei) after the user approves a session in a dialog; reading keys is not part of it |
| Password fields, secure contexts | Toolkit-specific | Compositor-protected |

Ddalgak on Linux will use only supported, user-consented interfaces, never a root daemon or
a world-writable `uinput` setup, and will mark every unavailable trigger or action per desktop
in the matrix and the GUI. The `rules.json` format already stores portable key names, so a
Linux build can read Windows rules and report what it cannot do.

## 10. Input recursion and stuck keys

- **Recursion** is prevented structurally (RULE_MODEL §7.4, `DDALGAK-SEC-004`): Ddalgak's own
  injected events carry its marker and `LLKHF_INJECTED` and are never matched; rules that accept
  other programs' injected input cannot inject; self-triggering outputs are refused; a per-rule
  rate limit stops loops through other automation tools (two remapping programs feeding each
  other) with `rate_limited`.
- **Stuck keys** (`DDALGAK-SEC-005`): the injector keeps a ledger of every key and button it
  pressed. Each entry is released on trigger release, pause, turn-off, emergency, profile or
  mode switch, session lock or secure-desktop switch, a failing run, and in the process exit
  handler. Before typing or sending, held trigger modifiers are logically released and restored
  only if still physically down (`SendInput` does not reset keyboard state, *documented*, M2).
  A consumed key-down always has its key-up consumed.
- **Event storms**: auto-repeat fires a rule again only with `repeat: true`, and the rate limit
  applies to it too.

## 11. Risk register and verification plan

Likelihood (L) and impact (I): **H**igh / **M**edium / **L**ow.

| ID | Risk | L | I | Mitigation | Owner step |
| --- | --- | --- | --- | --- | --- |
| DR-01 | The hook is slow or blocked and Windows removes it silently, so rules stop without notice | M | M | Short callback, worker threads, hook health check on demand (§7) | 66, 82 |
| DR-02 | A bug leaves a modifier or mouse button logically pressed | M | H | Injector ledger, release on every exit path, tests per path (§10) | 68, 71, 73 |
| DR-03 | The emergency chord does not work on some keyboards, laptops or remote sessions | M | H | Physical-state detection; Step 73 tests laptops, external keyboards, RDP; an ADR 0017 note if the chord must change | 73 |
| DR-04 | Password-field detection misses custom controls | M | H | UI Automation plus edit styles; `except_apps`; documented as best effort (§8) | 66, 69, 82 |
| DR-05 | Injected 한/영 (`HangulMode`) or Unicode text behaves differently across IME versions | M | M | Real-Windows tests with the Korean IME; `paste` fallback | 68, 69 |
| DR-06 | Users expect AutoHotkey scripts to work | H | M | Matrix with explicit statuses; importer previews only a declarative subset (§5 of the philosophy) | 65, 80 |
| DR-07 | Games or anti-cheat flag Ddalgak | M | M | No concealment or evasion; Gaming profile and pause; honest documentation (§6) | 82 |
| DR-08 | A malicious same-user process edits `rules.json` to add a launch rule with forged consent | L | H | Same-user trust boundary (§1); `.exe`-only launches, no shells; launch consent fingerprints; history shows origin of every change | 67, 82 |
| DR-09 | Two automation tools (Ddalgak and another) feed each other's synthetic input | M | M | `injected: ignore` default, accept-rules cannot inject, rate limit (§10) | 66 |
| DR-10 | Widened remaps capture an OS chord | L | H | Reserved chords pass unchanged under widening (§2) | 68 |
| DR-11 | UIPI failures are silent, so a rule seems to "do nothing" in admin windows | H | L | Integrity check before injecting, `skipped_elevated_target` with an explanation (§3) | 66, 70 |
| DR-12 | Wayland makes key remapping impossible for normal clients | H | M | Linux scope per desktop, no root service, matrix marks gaps (§9) | 83 |

**Manual checks on real Windows** (each owner Step reports evidence or writes **not tested**
with the reason):

| Check | Owner |
| --- | --- |
| Hook callback latency and idle cost with 0, 10, 100 and 1000 rules; no timers while idle | 66, 82 |
| Elevated window focus: injecting rules skipped, others run, notice shown | 66, 70 |
| Secure desktop and lock screen: nothing fires; keys released on lock | 73 |
| Password fields in a Win32 edit, a browser and a UWP app: rules skipped, nothing typed | 66, 69 |
| Emergency chord with remaps of Ctrl, Alt, Shift and Escape active, with an invalid file, over RDP | 73 |
| Korean 2-set IME: composition guard, `HangulMode` output, Hangul text typing | 68, 69 |
| US and one AltGr layout: logical versus physical triggers | 68 |
| Stuck keys: kill the app while a remap and a mouse hold are active | 68, 71, 73 |
| Multi-monitor with negative coordinates and mixed DPI for mouse actions | 71 |
| A few non-protected games; document behavior honestly | 82 |

**Step 65 ran none of these**: it is a specification. Its machine checks cover the schemas, the
reserved chords, consent, recursion guards, cross-references and the playground's safety
properties (`cargo xtask schema-check`).
