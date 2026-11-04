# Ddalgak Philosophy

Status: **current** (Step 65, specification; nothing of Ddalgak is implemented yet) · Owner:
mouv · Decision: [ADR 0022](adr/0022-ddalgak-rule-model-and-safety.md) · Requirements:
[DDALGAK_REQUIREMENTS](DDALGAK_REQUIREMENTS.md) · Builds on [KIM_PHILOSOPHY](KIM_PHILOSOPHY.md)

**Ddalgak** (딸깍, the sound of a click) is a small Kim tool that turns **one action into
useful automation**. You press a key, a chord or a short sequence, and Ddalgak does what you
asked for: a key acts as another key, a phrase is typed, a program or a website opens, the
pointer moves, a mode changes. It is the kind of tool people reach AutoHotkey for, rebuilt as a
native Rust program with a visual editor that a beginner can use and a headless interface that
scripts and agents can drive.

The documents that make this concrete:

| Document | What it fixes |
| --- | --- |
| [DDALGAK_REQUIREMENTS](DDALGAK_REQUIREMENTS.md) | Numbered requirements (`DDALGAK-FR/NFR/SEC-ddd`) and milestones D1–D7 |
| [DDALGAK_RULE_MODEL](DDALGAK_RULE_MODEL.md) | Triggers, the seven core action types, rules, modes, profiles, consent, precedence, validation, playground, errors and activity, versioned schemas |
| [DDALGAK_GUI_JOURNEYS](DDALGAK_GUI_JOURNEYS.md) | The end-to-end GUI journeys and their wireframes |
| [DDALGAK_SAFETY](DDALGAK_SAFETY.md) | Risks and scope: reserved shortcuts, elevated windows, layouts and IME, password fields, anti-cheat, recursion, hook security, Linux |
| [DDALGAK_AHK_MATRIX](DDALGAK_AHK_MATRIX.md) | A versioned comparison with AutoHotkey v2, row by row |

## 1. One action, useful automation

- **The unit is a rule**: *when* this trigger happens (in this mode, in this app), *do* these
  steps. A rule is data the user can read, not a program.
- **Seven first-class effects** cover what most people automate: remap a key to another key or
  combination, type Unicode text, open a program, open a website, move, scroll and click the
  mouse, switch or toggle a user-defined mode, and pause or turn off Ddalgak itself. Each has
  precise inputs, outputs, errors and safety rules ([RULE_MODEL §5](DDALGAK_RULE_MODEL.md)).
- **Growth is additive.** Ordered steps with delays exist from the start. App-specific rules,
  window and clipboard actions, macro recording, richer conditions and profiles grow the same
  model in later Steps. Nothing grows into a general programming language.
- **Modes and profiles** let the same key mean different things: a navigation layer while
  CapsLock is held, a "Gaming" profile with almost nothing in it, a "Work" profile with app
  rules.

## 2. Beautiful and novice-friendly; programmable and headless

- **A rule takes a few clicks.** Press the keys you want, pick one of the effect tiles, fill in
  one small form, try it, save. No syntax to learn, and no rule is saved or shown as active
  before the engine has really loaded it ([GUI_JOURNEYS](DDALGAK_GUI_JOURNEYS.md)).
- **It looks like Kim.** Near-black surfaces, compact type, one lilac accent, keyboard-first,
  Korean and English labels ([DESIGN_SYSTEM](DESIGN_SYSTEM.md)). State is never shown by color
  alone.
- **Try before you trust.** The test playground answers "what would happen if I pressed this,
  here?" without injecting anything system-wide. A live test types only into the playground's
  own field and opens a program only after you confirm that exact launch.
- **Everything the GUI does, the CLI does** (`KIM-FR-013`): `ddalgak rule list --json`,
  `ddalgak test --dry-run --json`, `ddalgak pause`, with the Kim CLI contract's exit codes,
  `--dry-run` and stable error codes. Agents get the same power through the same contract, and
  an opt-in MCP adapter later, with read-only defaults.

## 3. Small, fast, local

- **Native Rust, Windows first.** Two binaries in one package: `ddalgak.exe`, the headless CLI,
  and `ddalgak-gui.exe`, the tray app that owns the engine and opens the editor on demand
  ([ADR 0002](adr/0002-independent-tool-binaries.md), [ADR 0022](adr/0022-ddalgak-rule-model-and-safety.md)).
- **No AutoHotkey runtime, no Electron, no resident Kim daemon, no account, no cloud.** Ddalgak
  is not a script interpreter. It needs no other Kim tool, and Kim never starts it.
- **Frugal when idle.** The engine waits on OS events; no polling, no timers while nothing
  happens. The hook callback is designed to decide in well under a millisecond and hands all
  work to a worker thread, far below the system's hook timeout (`DDALGAK-NFR-002`; measured
  from Step 66 on).
- **Local-first.** Rules, profiles and history stay in the user's profile. Ddalgak opens no
  network connection. "Open a website" hands an `https` link to the user's browser.

## 4. Safe by construction

An automation engine sees keys and can type for you. That is powerful, so the safety rules are
part of the design, not settings ([DDALGAK_SAFETY](DDALGAK_SAFETY.md)):

- **Off until you turn it on.** The keyboard hook exists only while the engine runs, inside the
  app you started, with its tray icon visible. A fresh start, a crash or an emergency stop
  always leaves it off unless you chose otherwise.
- **One chord always gets you out.** `Ctrl+Alt+Shift+Escape` is checked on the physical keys
  before any rule. It removes the hook, releases every key and button Ddalgak holds, cancels
  running rules and turns the engine off. No rule can remap, hide or produce it.
- **No keylogging.** Keys are inspected only to decide whether a rule matches. Nothing typed is
  stored, logged or sent anywhere. The activity list names rule IDs and outcome codes only.
- **It steps aside where it must.** Ddalgak never touches the secure desktop (Ctrl+Alt+Del, UAC,
  sign-in, lock screen), never types into password fields, never elevates, and never works
  around Windows' protection of administrator windows. It says so instead.
- **Consent for powerful effects.** Injecting input, launching programs or websites, and using
  the clipboard each need your explicit consent, recorded per rule. A launch consent covers
  exactly that program and those arguments. Imported profiles never carry consent.
- **No loops.** Ddalgak's own output never triggers its rules, so two remaps that point at each
  other are a swap, not a loop. A rule that reacts to other programs' synthetic input cannot
  inject input itself, and a rule that only re-sends its own trigger is refused.
- **Never broader than written.** A rule this version does not fully understand (a newer step
  type, a condition it cannot evaluate yet) is kept but inactive. Ddalgak never guesses.

## 5. Honest about AutoHotkey

AutoHotkey is the behavioral reference for what people expect from a hotkey tool: remaps,
hotstrings, context-sensitive hotkeys, sending keys, launching things. Ddalgak learns from it
and says, row by row, what it covers ([DDALGAK_AHK_MATRIX](DDALGAK_AHK_MATRIX.md)):

- **Ddalgak does not run AutoHotkey scripts** and never will claim to. AutoHotkey v2 is a
  general-purpose language (functions, classes, `DllCall`, COM, files). Ddalgak's rules are
  declarative data.
- A later importer (Step 80) converts a **well-defined declarative subset** (plain hotkeys,
  remaps, hotstrings, `Run` of a fixed program or URL) into rules, shows a coverage report with
  line numbers, and imports everything as disabled rules for review. It never executes the
  script.
- Where Ddalgak deliberately does less than AutoHotkey (no `UIAccess`, no Win-key triggers in
  v1, no arbitrary code), the matrix says so and why.

## 6. Windows first, Linux later

Windows is the target until milestone D7 (Step 83). The rules file stores portable key names
(`KIM-NFR-022`), never virtual-key numbers, so a Linux backend can read the same file later. On
Linux, X11 and Wayland differ fundamentally in what a user program may capture or inject.
Ddalgak will support what each desktop allows through its supported, user-consented interfaces
and mark the rest as unavailable. It will not ship a root input service ([DDALGAK_SAFETY §9](DDALGAK_SAFETY.md)).

## 7. Who it is for

| Person | What Ddalgak gives them |
| --- | --- |
| Someone who has never written a script | CapsLock as Ctrl, a Korean sign-off on a key, a program on F14, made in the editor and tried in the playground |
| A keyboard enthusiast | Physical-key remaps, dual-role keys, hold layers, left/right modifiers, sequences, modes |
| A former AutoHotkey user | Familiar behaviors, an honest comparison, and an importer for the simple parts |
| A developer or agent | Versioned JSON rules, `--json` everywhere, dry-run simulation with traces, conflict reports |
