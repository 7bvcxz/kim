# ADR 0053 — Noeul colour picker: one pixel of the frozen picture, copied as CSS text on an explicit key, `C` yields to typing, a selection is never lost

Status: **Accepted** (Step 59; the on-screen check with a real mouse, keyboard, clipboard and
applications is pending the maintainer, [NOEUL_UX_SPEC §4.14](../NOEUL_UX_SPEC.md#414-step-59-the-colour-picker-and-the-colour-clipboard-workflow))
· Date: 2026-11-04 · Builds on [ADR 0035](0035-noeul-smart-selection-and-precision.md) §3 and §6
(the magnifier, its colour "drawn, never reported"), [ADR 0038](0038-noeul-annotation.md) (the
`annotate` context inside the overlay's, labels that suspend shortcuts),
[ADR 0047](0047-noeul-clipboard-paste-and-rich-sources.md) (`F3` colour cards, the test hook's
stand-in clipboard) and [ADR 0050](0050-noeul-displays-and-capture-backends.md) §5 (colour and
HDR) · Requirements: NOEUL-FR-041, NOEUL-FR-044, NOEUL-FR-071, NOEUL-FR-092, NOEUL-NFR-013,
NOEUL-NFR-014, NOEUL-SEC-002, NOEUL-SEC-008

## Context

Step 59 (issue #64) adds Snipaste's colour picking to the `F1` overlay: `C` copies the colour of
the hovered pixel, the format can be switched quickly, alpha can be included, and `F3` turns the
copied text into a small floating swatch. What existed: the magnifier (Step 52) showed 15 × 15
enlarged pixels with the pixel's colour as HEX and as `58, 110, 165`; `F3` (Step 55) already
parsed HEX, `rgb()`/`rgba()` and plain numbers into a swatch card; the final hotkey manifest
already named `color.copy` (`C`) and `color.format-next` (`Shift+C`) in the `capture-overlay`
context; and ADR 0035 §3 ruled that the magnifier's colour is drawn, never reported. Open were:
which pixel exactly, when `C` may copy, what happens to the capture after it, the exact strings,
how `C` behaves while a label is typed or a tool draws, what a screen reader gets, what HDR and
colour management mean for the values, and whether a command line can pick.

## Decision

### 1. The pixel: under the magnifier's crosshair, of the frozen picture, only while it is shown

`color.copy` reads **one pixel of the frozen picture** — the picture the overlay shows and the
magnifier enlarges — at the overlay's pointer, the crosshair's centre cell. It never reads the
live screen (that would read the overlay itself, or something that changed since `F1`), and it
copies **only while the magnifier is on screen**: hovering, dragging, marking, resizing, and next
to a confirmed selection or on its handles; not inside the selection, over the toolbar or the size
chip, while moving the selection, while a tool draws, after `M` hid the magnifier, nor in the pin
editor (which has no magnifier). So `C` copies exactly a pixel the user sees enlarged, and does
nothing otherwise. The pointer keys (`W A S D`, Step 52) nudge the crosshair one device pixel at
a time, with acceleration; device pixels on every monitor, whatever its scale.

### 2. The text: CSS in three formats, HEX case and alpha as settings

| Format | Opaque | `[color] alpha = "on"` |
| --- | --- | --- |
| HEX | `#3A6EA5` (`[color] hex_case = "lower"`: `#3a6ea5`) | `#3A6EA5FF` |
| RGB | `rgb(58, 110, 165)` | `rgba(58, 110, 165, 1)` |
| HSL | `hsl(211, 48%, 44%)` | `hsla(211, 48%, 44%, 1)` |

Every string is CSS, so it pastes into style sheets and design tools as it is, and the swatch card
shows the HEX and `rgb()` lines the picker writes. The RGB format is `rgb(…)`, not Snipaste's bare
`58, 110, 165` (◆, `COLOR-02`); `F3` reads both. The alpha is a number 0–1 with at most three
decimals (`0.502`), which gives every 8-bit alpha back exactly. HSL is whole degrees and percent,
so a colour taken back from its HSL text can differ by up to 3 units per channel (tested over the
cube); HEX and RGB are exact. A screen picture is opaque, so its alpha is always `FF` / `1`: the
option is for tools that expect eight-digit HEX or `rgba()`, not a promise of transparency. The
magnifier's readout writes **exactly** what `C` would copy: HEX and RGB always, and HSL when it
is the chosen format; the chosen line is drawn in the primary text colour with an accent bar
beside it (a shape, not colour alone). `color.format-next` (`Shift+C`) cycles HEX → RGB → HSL.
`[color] format` is the format a capture starts with; the format chosen with `Shift+C` is kept in
memory for the next captures until Noeul quits or the settings change, and never written.

### 3. After the copy: the capture ends only when there is nothing to lose

With **no selection** (and so no annotation), `C` copies and the capture ends at once, as in
Snipaste: the frozen picture is freed, nothing is pinned, copied as an image or added to the
rectangle history, and `noeul status` counts it in `captures.colors`. The way to a swatch is
`F1` → hover → `C` → `F3` (`F3` does nothing while an overlay is open, ADR 0047). With a
**selection or annotations**, `C` copies but the capture **stays** with its selection and drawing,
and a banner says what was copied and that the paste key pins it after the capture — losing a
selection and its annotations to a letter key would be data loss. A clipboard held by another
program keeps the overlay with a banner and copies nothing (as Copy does, NOEUL-FR-025).

### 4. Keys and the `C` conflicts

The shipped manifest gains `color.copy` (`C`) and `color.format-next` (`Shift+C`) under the final
manifest's IDs, titles and defaults; both are rebindable and can be turned off like every other
key (`noeul hotkeys set color.copy X`), and the overlay's keyboard help names the effective keys
("C copies the colour · Shift+C switches HEX, RGB, HSL"). The conflicts:

- **Typing a label** (the text tool): every overlay shortcut is suspended (ADR 0038), so `C` and
  `Shift+C` type `c` and `C` into the label and copy nothing.
- **Input method:** the overlay has no input context, so `C` works in Korean Hangul mode; while a
  label composes, no shortcut fires (NOEUL-NFR-014).
- **Annotation:** the `annotate` context's keys are matched first while a selection is
  confirmed; none uses `C` by default. A user who binds an `annotate` action to `C` gets that action
  while a selection is confirmed (the inner context wins, HOTKEY_CONTRACT §13.3); `hotkeys check`
  does not report it, because the two are different contexts — the docs say so. While a tool
  draws inside the selection the magnifier is hidden, so `C` copies nothing there.
- **`Shift`:** Snipaste switches the format with `Shift` alone; the hotkey contract has no
  bare-modifier keys, so it is `Shift+C` (◆, as decided in the UX spec §6).

### 5. The clipboard: one write on the key, no read, no monitoring

`C` writes the text once with the same clipboard write as `pin.copy-text` (opened only for the
write, kept out of cloud sync, retried briefly when held) and reads nothing; nothing listens to or
polls the clipboard (NOEUL-SEC-002). Under the debug-build test hook a copy goes to the in-memory
stand-in and **now replaces the stand-in folder's formats** (`CF_UNICODETEXT` for text, `PNG` and
`CF_DIBV5` for a picture), as a real clipboard write replaces the clipboard, so tests read the
exact copied text and paste it back as a swatch without touching the user's clipboard.

### 6. What is reported, and the accessible name

`noeul status` and IPC report the picker's format, HEX case and alpha (`captures.overlay.color`),
the `[color]` settings and the `captures.colors` count — never the colour under the pointer
(NOEUL-SEC-008; an invalid schema example has a `value`). The overlay window's **accessible
name** — its window text, which UI Automation reads as the window's Name through the system's
window proxy, `DefWindowProcW` announcing each change — is what the overlay shows: "Capture
overlay, selection 640 by 360 at 412, 188. Magnifier at 412, 188 (275, 125 at 150 %): #3A6EA5,
rgb(58, 110, 165); copy format HEX" (and "(HDR shown as SDR)" on an HDR monitor). It does carry
the colour while the magnifier shows it: a screen reader needs the readout, and the name exposes
nothing the screen does not already show to every program on that desktop; it never contains a
window title, an element name or anything outside the overlay. It is set only when it changes. A
full UI Automation provider (the toolbar's buttons, a live region) stays NOEUL-NFR-013's N5 work.

### 7. Colour management and HDR

The values are the captured picture's (ADR 0050 §5): what GDI's capture gives for the desktop,
8-bit values that Noeul treats as sRGB, before the monitor's calibration curve and profile; a
colour-managed application's content is already in the monitor's space, which Noeul cannot tell
apart. So on a wide-gamut or calibrated monitor the picker gives the drawn values, not measured
colours. On a monitor with HDR on, Windows converts the picture to SDR for GDI; the magnifier then
adds the line "HDR shown as SDR" (also in the accessible name), and the value is that SDR
picture's (NOEUL-FR-092). Noeul corrects nothing. Windows 11's Auto Color Management and a real
HDR display are **not tested** here (one SDR monitor, Windows 10); the manual check records them.

### 8. Headless: no command reads a pixel

The CLI and IPC reach everything around the picker — `noeul config set color.format|hex_case|alpha`,
`noeul hotkeys …` for its keys, `noeul status` for its state, `noeul capture --interactive` to open
the overlay, and `noeul pin --clipboard` (`--as color`) to turn a copied value into a swatch —
but **no command picks a pixel**. A pixel read from a command line would be a screen capture
without the overlay, and its result, a colour, would put a pixel into `--json`, which
NOEUL-SEC-008 rules out. `CLI-08` ("pick a color" commands) stays planned for that reason.

## Consequences

- `C` and `Shift+C` are bound in the overlay; the final manifest is unchanged.
- The magnifier's RGB line changed from `58, 110, 165` to `rgb(58, 110, 165)`; the Step 52 manual
  check now expects `#FFFFFF  rgb(255, 255, 255)` over white.
- `F3` reads `hsl()`/`hsla()` too, so every text the picker copies pins back as a swatch.
- Colour picking on pins (`PIN-24`, `color.copy` on image windows) is not in this build: it needs a
  magnifier on pins and a `pin` context binding, a manifest change for a later Step. HSV and other
  models (`COLOR-04`) and editing a card's value (`COLOR-05`) stay planned.
- Idle cost is unchanged: no timer, thread or listener; a few strings are formatted per redraw.

## Alternatives considered

- **Always end the capture after `C`** (Snipaste): a selection and its annotations would be lost to
  one letter; rejected for the case with a selection.
- **Never end it:** the common flow would need `Esc` before `F3`; rejected for the case without a
  selection.
- **Read the live screen at the key press:** reads the overlay or a changed screen, not what the
  magnifier shows; rejected.
- **Bare `58, 110, 165` for RGB:** familiar from Snipaste, but not CSS and not what the swatch card
  shows; `F3` still accepts it.
- **A UI Automation provider now:** more COM for the whole overlay; the window's name gives screen
  readers the readout today, the provider stays N5.
- **`noeul color pick --at x,y`:** a capture without the overlay with a pixel in the JSON; rejected
  (§8).
