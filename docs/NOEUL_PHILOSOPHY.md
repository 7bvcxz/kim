# Noeul Philosophy

Status: **current** (product definition; nothing of Noeul is implemented yet) · Owner: mouv ·
Applies to: Noeul (`noeul`) · Step 48 · Requirements: [NOEUL_REQUIREMENTS.md](NOEUL_REQUIREMENTS.md) ·
Parity: [NOEUL_SNIPASTE_MATRIX.md](NOEUL_SNIPASTE_MATRIX.md) · UX: [NOEUL_UX_SPEC.md](NOEUL_UX_SPEC.md) ·
Decision: [ADR 0021](adr/0021-noeul-product-definition.md)

Noeul (노을, the glow of the evening sky) is a small native screenshot tool for Windows. It does
three things and tries to do them better than anything heavier: **capture** a part of the screen,
**mark it up**, and **pin** it to the desktop as a floating image you can keep looking at while
you work. It is a Kim tool, so everything in [KIM_PHILOSOPHY](KIM_PHILOSOPHY.md) applies; this
document says what is specific to Noeul. Testable obligations live in the requirements document
(IDs in brackets).

Noeul is inspired by [Snipaste](https://www.snipaste.com/), a well-known Windows snipping and
pinning tool. Noeul is an independent work: it reproduces useful *functionality and interaction
ideas* with its own code, design and assets (§10), and it deliberately changes the one gesture
that matters most (§2).

## 1. What Noeul is for

- **Look at two things at once.** The reason to pin a screenshot is to keep a reference (a
  spec, a color, an error message, a design) visible on top of the window you are working in.
  Pinning is therefore the primary output of a capture, not an afterthought.
- **One keystroke away, then gone.** Press `F1`, select, act, and the overlay disappears. No
  editor window opens unless you ask for one.
- **Precise when it matters.** Device-pixel coordinates, a magnifier, pixel nudging, window
  detection and an exact 100 % pin make Noeul a measuring tool too.
- **Useful alone.** Noeul needs neither Kim nor Subak installed or running [NOEUL-FR-001]. Kim
  can install it on request, like any other tool, and never by itself.

## 2. The signature flow, and why it differs from Snipaste

**`F1`, drag a rectangle, double-click inside it: the rectangle becomes a floating pinned image,
immediately, at exactly the same place and size.** [NOEUL-FR-020]

| Gesture after selecting a region | Snipaste default (its documentation) | Noeul default |
| --- | --- | --- |
| Double-click inside the selection | Copies the image to the clipboard | **Pins the image** (no clipboard or file write) |
| `Enter` | Copies to the clipboard | **Pins** (the keyboard twin of the double-click) |
| Middle-click inside the selection | Pins | Pins |
| `Ctrl+T` | Pins | Not bound (available in the opt-in Snipaste-style preset) |
| `Ctrl+C` / Copy button | Copies | Copies |
| `Ctrl+S` / Save button | Saves | Saves |
| Double-click on a pinned image | Closes it | **Nothing** (closing is `Esc`, `Ctrl+W` or the menu) |
| The paste hotkey (`F3`) after closing a pin | May bring a closed image back | Always pins the clipboard; "Reopen closed pin" is its own command |

Why Noeul changes Snipaste's defaults:

1. **The user asked for it.** The flow Noeul exists for is capture-to-pin. Making the most
   natural "I'm done" gesture (a double-click, or `Enter`) do the most important thing removes a
   step from every capture.
2. **Copy and save stay one key away and are never ambiguous.** They have their own keys and
   buttons, shown in tooltips and in the shortcut list [NOEUL-FR-023]. Nothing copies by
   accident, so the clipboard keeps what the user put there.
3. **A pin must survive the gesture that made it.** The new pin appears under the pointer. If a
   double-click on a pin closed it (as in Snipaste), one extra double-click would destroy the
   pin that was just created. Noeul gives a double-click on a pin no default meaning
   [NOEUL-FR-026].
4. **No silent surprises for Snipaste users.** The difference is stated in the first-run hint,
   in tooltips ("Pin — double-click or Enter"), in the docs and in the matrix. Users who prefer
   Snipaste's gestures can switch on a clearly named preset; it is opt-in, visible in settings,
   and undone by reset [NOEUL-FR-024].

Every key is configurable through the [hotkey contract](HOTKEY_CONTRACT.md); the double-click
action itself is a Noeul setting (`pin`, `copy`, `save` or `none`), because the contract leaves
mouse gestures to each tool (HOTKEY_CONTRACT §1, §11.1). The exact rules and their tests are in
[NOEUL_UX_SPEC §4](NOEUL_UX_SPEC.md#4-the-signature-flow-precise-rules).

## 3. Pins are real windows, and you can always get rid of them

- A pin is an independent, borderless, always-on-top window with its own identity, position,
  zoom, rotation and opacity [NOEUL-FR-060]. Many pins can exist; closing one never affects
  another.
- **100 % means one image pixel per device pixel.** A pin is pixel-exact on any monitor until the
  user zooms it [NOEUL-FR-061]. That makes pins usable as a ruler and a color reference.
- **Pins never steal focus.** They do not appear in Alt+Tab or on the taskbar. A pin takes
  keyboard focus only when the user creates it or clicks it, so `Esc` or the arrow keys work on a
  new pin at once, and a click elsewhere returns to work.
- **No trapped UI.** `Esc` closes the focused pin. A pin that ignores the mouse (click-through)
  can always be made clickable again from the tray, the settings window or the CLI, and Noeul
  turns click-through on only when such a path exists [NOEUL-FR-063, NOEUL-SEC-013]. Pins left
  off-screen after a display change can be brought back [NOEUL-FR-067].

## 4. Small, fast and quiet

- **One process, only while you want it.** Noeul is a per-user program that runs while the user
  keeps it running, usually with a tray icon (optional). No service, no helper, no scheduled task,
  no start at sign-in unless the user turns it on [NOEUL-FR-002]. Its global shortcuts exist only
  while it runs (`KIM-NFR-020`); that is the price of having no daemon (ADR 0017).
- **Nothing happens while nothing happens.** No timers and no polling: idle Noeul sleeps in its
  message loop, whatever number of pins is on screen [NOEUL-NFR-001]. Idle memory is small
  because idle Noeul holds no capture buffers and no graphics context it does not need
  [NOEUL-NFR-002].
- **Speed is measured.** Budgets for capture latency, pin latency, start-up and memory are stated
  and measured on real Windows, not assumed [NOEUL-NFR-003…007].
- **No network, at all.** Noeul has no update check, no telemetry, no remote images, no cloud
  text recognition; updates are Kim's explicit job [NOEUL-NFR-008].

## 5. Private by construction

A screenshot tool sees everything on the screen, so its defaults are strict:

- **Capture only on an explicit action** for that capture: a hotkey, a tray click, a button, or
  a command. Never on a timer, at start-up or on restore [NOEUL-SEC-001]. Captures requested
  through the command line or an agent are never silent [NOEUL-SEC-010].
- **The clipboard is read only when you paste** into Noeul, and never watched [NOEUL-SEC-002].
- **Memory first.** Captures, history and pins live in memory. Noeul writes an image to disk only
  when the user saves it or has switched on disk history, pin backup or copy-as-file
  [NOEUL-SEC-005]. Files carry no metadata about the user, the machine or the captured program
  [NOEUL-SEC-006].
- **The OS's protections stand.** The secure desktop, the lock screen, UAC prompts and content
  that windows or DRM exclude from capture are never captured or worked around
  [NOEUL-SEC-003, NOEUL-SEC-004].
- **Window detection is geometry only.** Noeul asks where windows and controls are, never what
  they contain [NOEUL-SEC-007].
- **Logs carry no content.** No pixels, clipboard text, window titles or image paths in logs or
  JSON output [NOEUL-SEC-008].

## 6. Keyboard and accessibility

- Noeul follows the [hotkey contract](HOTKEY_CONTRACT.md): every shortcut can be seen, changed,
  disabled and reset, headlessly too; `F1` and `F3` are its only global defaults
  (HOTKEY_CONTRACT §7.5, §13.3) [NOEUL-FR-100].
- **Every pointer gesture has a keyboard path** (pin = `Enter`, keyboard-only selection with
  `Space`, pixel movement with the arrow keys and `W A S D`) [NOEUL-NFR-012].
- Noeul uses the user's own Windows double-click speed, double-click area and drag threshold,
  never constants of its own, so it works for people who set these slower or larger.
- The capture overlay and pins switch the input method off, so shortcut letters work while the
  Korean IME is in Hangul mode; text annotation switches it back on for typing.
- Screen-reader names, visible focus, contrast and reduced motion follow the
  [design system](DESIGN_SYSTEM.md) [NOEUL-NFR-013].

## 7. CLI, headless and agents

- Noeul has a CLI that follows the [CLI contract](CLI_CONTRACT.md): human output and `--json`,
  stable error codes, `--dry-run` for changes, no prompts [NOEUL-FR-110].
- **Headless means headless.** Status, configuration, shortcut and history commands never open a
  window or tray icon and never register a hotkey [NOEUL-FR-111].
- **Desktop commands are honest.** A capture needs an unlocked, interactive desktop. In a locked,
  secure, disconnected or display-less session the command fails with a stable error; it never
  pretends, waits forever or reaches into another session [NOEUL-FR-112].
- An optional local MCP adapter may offer an allowlist of operations; screen capture and
  clipboard access through it are off until the user enables them [NOEUL-FR-113].

## 8. Windows first, Linux later

Windows 10 (1809 or later) and Windows 11 on x86_64 are the targets [NOEUL-NFR-010]. Capture,
windows, clipboard, hotkeys and session state sit behind narrow interfaces so a Linux backend can
follow (Step 64); on Linux, Wayland's consent dialogs and compositor rules will decide what is
possible, and Noeul will not claim more [NOEUL-NFR-011].

## 9. What Noeul is not

Not a screen recorder, not a cloud sharing service, not an image editor, not an automation tool
(that is Ddalgak), and not a keylogger: it uses `RegisterHotKey` for its two global shortcuts and
never a keyboard or mouse hook [NOEUL-SEC-012]. Features that need a hook, polling, the network,
an account or input injection are not intended; the requirements list every deferral with its
reason ([NOEUL_REQUIREMENTS §6](NOEUL_REQUIREMENTS.md#6-deferred-and-not-intended)).

## 10. License and IP boundary

Noeul is licensed like the rest of Kim, **MIT OR Apache-2.0**. Its relationship to Snipaste is
one of inspiration, not derivation.

**What Noeul may reproduce** (ideas and functionality, which are not protected by copyright):
the workflow of capturing, annotating and pinning; common interaction conventions (a global
capture key, a dimmed overlay with a selection rectangle, arrow-key nudging, a magnifier, digit
keys that rotate a pin); and compatibility with documented, generic formats such as PNG and the
Windows clipboard formats.

**What Noeul never copies:**

- Snipaste's code, binaries, decompiled or disassembled logic, or private file formats (Noeul
  does not read or import Snipaste's `config.ini`);
- its icons, images, cursors, sounds, screenshots or other artwork, its UI layout taken
  pixel-for-pixel, its color palettes or themes as assets;
- its documentation, help text, tooltips or translations. Noeul's documents describe Snipaste
  features in Noeul's own words and link to the official pages; they quote nothing beyond key
  names and option names that identify a feature;
- its name or branding as Noeul's. "Snipaste" is used only to refer to Snipaste (for example
  "Snipaste-style gestures"); Noeul never suggests endorsement.

**Clean-room practice.** Contributors work from Snipaste's public, user-facing documentation and
from observing behavior, never from its code. Every Noeul asset (icon, cursor, sound, illustration,
wireframe) is made for Noeul and committed with its source or a statement of origin. A pull
request that adds third-party material says where it came from and under which license.

### 10.1 Third-party licenses

| Kind | Rule for Noeul |
| --- | --- |
| Rust crates | Only licenses allowed by the workspace dependency policy (`deny.toml`: MIT, Apache-2.0, BSD-2/3-Clause, ISC, Zlib, Unicode-3.0, BSL-1.0, CC0-1.0). Anything else (MPL, LGPL, GPL, AGPL, "commercial or royalty-free with attribution" UI toolkits) needs a reviewed, crate-scoped exception with a written reason, as for Kim Manager (ADR 0014). A new crate needs a release at least two weeks old. |
| Image decoding | Pure-Rust decoders under the same policy (e.g. the `image`/`png` family, MIT OR Apache-2.0) or the OS's Windows Imaging Component. No bundled C codec libraries with other licenses. |
| HTML rendering for pasted text | Noeul's own small sanitizing renderer (text and basic formatting). No embedded browser engine (KIM_PHILOSOPHY §5). |
| Fonts | System fonts at run time (Segoe UI, Malgun Gothic and the Kim fallback chain, DESIGN_SYSTEM §2.3). Noeul adds no font files of its own. If its UI uses egui, egui's built-in fallback fonts come with it under the existing crate-scoped exception (`epaint_default_fonts`: OFL-1.1, Ubuntu-font-1.0) and are listed in the attribution file. |
| Icons and artwork | Original Noeul artwork only, under Noeul's own license. No icon packs from other products. |
| Color math, color profiles | Implemented from public specifications (sRGB, ICC); no copied profile files except those Windows provides at run time. |
| Attribution | A release ships a third-party license file generated from the dependency tree and shows it under Settings → About (Step 63). |

Decisions in this section are recorded in [ADR 0021](adr/0021-noeul-product-definition.md).
