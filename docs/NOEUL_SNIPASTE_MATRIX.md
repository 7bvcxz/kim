# Noeul–Snipaste Feature Matrix

Status: **current** (inventory) · Reference: Snipaste 2.11.3 (changelog entry dated 2026-01-18),
official documentation read on 2026-11-04 · Owner: mouv · Applies to: Noeul · Created in Step 48;
re-audited in Step 63

This matrix lists Snipaste's user-facing capabilities and Noeul's answer to each: its status, the
requirement that covers it ([NOEUL_REQUIREMENTS](NOEUL_REQUIREMENTS.md)), the milestone, and how it
will be verified. It is the tracking document for parity; the Step that implements a row updates
it in the same change.

**Nothing in this matrix is implemented yet.** Every row is `planned` or `not intended`, and every
Evidence cell is `—`. Rows marked **◆** are deliberate deviations from Snipaste's default
behavior; the most important one is the signature flow (`OUT-01`, `OUT-03`): in Noeul a
double-click inside the selection **pins**, while in Snipaste it copies to the clipboard
([NOEUL_PHILOSOPHY §2](NOEUL_PHILOSOPHY.md)).

Snipaste is described here in Noeul's own words, from its public documentation, for comparison
only. Nothing from Snipaste (text, images, code, configuration formats) is copied
([NOEUL_PHILOSOPHY §10](NOEUL_PHILOSOPHY.md)). "Snipaste" refers to the product of its
respective owner; this project is not affiliated with it.

## Sources and method

All sources were read on **2026-11-04**.

| Code | Source | What was used |
| --- | --- | --- |
| **H** | Official site, <https://www.snipaste.com/> | The feature overview: snipping, pasting sources and operations, annotation tools, customization, platforms and editions |
| **G** | Getting Started, <https://docs.snipaste.com/getting-started> | How snipping starts, ends and is aborted; history replay; pixel movement; magnifier and color picking; annotation hints; paste sources and image-window operations |
| **K** | Key Bindings, <https://docs.snipaste.com/key-bindings> | The in-app keys for snipping, image windows and both |
| **P** | PRO, <https://docs.snipaste.com/pro> | The Free versus Pro comparison table and the descriptions of Pro features |
| **C** | Command Line Options, <https://docs.snipaste.com/command-line-options> | The `snip`, `paste` and other commands, output targets and options |
| **A** | Advanced Configs, <https://docs.snipaste.com/advanced-configs>, and Advanced Tips, <https://docs.snipaste.com/advanced-tips> | Settings without UI (history folder, hidden tray icon, detection exclusions, key swaps, metadata) and usage tips |
| **F** | FAQ, <https://docs.snipaste.com/faq> | Background process, click-through, translucency, quick save, close versus destroy, smooth scaling, groups |
| **T** | Troubleshooting, <https://docs.snipaste.com/troubleshooting> | Hotkeys taken by other programs, hotkeys and elevated windows, browser element detection |
| **L** | Changelog, <https://docs.snipaste.com/changelog> | Version and date of the reference (2.11.3, 2026-01-18) and features named in release notes (HDR correction, monitor changes, off-screen images, ARM64, confirmation before closing several images) |

The documentation site renders its pages in the browser; their content is the Markdown of the
Snipaste wiki, which the site serves under `docs.snipaste.com/wiki/`. Features specific to the
macOS and Linux editions are not inventoried. Pro-only features are marked **(Pro)**.

## Legend

**Status** (exactly one per row):

| Status | Meaning |
| --- | --- |
| `supported` | Noeul ships it, with evidence (see [How evidence is recorded](#how-evidence-is-recorded)). *(Not used yet.)* |
| `partial` | Some of it ships, with evidence; the approach cell says what is missing. *(Not used yet.)* |
| `planned` | Noeul intends to provide it in the named milestone. Nothing exists yet. |
| `not intended` | Deliberately not built; the requirement cell names the reason (`D-xx` in [NOEUL_REQUIREMENTS §6](NOEUL_REQUIREMENTS.md#6-deferred-and-not-intended)). |

**◆** = Noeul's default deliberately differs from Snipaste's. **MS** = Noeul milestone
(N1 capture and pin core · N2 pin control and paste · N3 annotation and color · N4 displays,
history and recovery · N5 preferences and automation · N6 Windows release · N7 Linux).
**Verify** uses the codes of the requirements (`T` test, `M` manual on real Windows, `I`
inspection, `D` documentation); `FLOW-nn` refers to the cases in
[NOEUL_UX_SPEC §4.3](NOEUL_UX_SPEC.md#43-test-specification). A priority `P2` requirement is
tracked but not promised.

Each row has a permanent ID so that commits, tests and later audits can cite it.

## 1. Starting a capture and selecting (`SNIP`)

| ID | Snipaste feature | Src | Noeul approach | Status | Requirement | MS | Verify | Evidence |
| --- | --- | --- | --- | --- | --- | --- | --- | --- |
| `SNIP-01` | Start snipping with a global hotkey, `F1` by default | G, H | Global action `capture.region`, default `F1`, through `RegisterHotKey`; a taken key is reported, never stolen | planned | NOEUL-FR-010, NOEUL-FR-100 | N1 | T, M | — |
| `SNIP-02` | Start snipping by clicking the tray icon | G | Tray left-click starts a capture; the tray icon is optional | planned | NOEUL-FR-004 | N1 | M | — |
| `SNIP-03` | Drag a rectangle over a frozen picture of the screen | G, H | Same idea: frozen picture of every monitor, dimmed outside, live size chip in device pixels | planned | NOEUL-FR-010, NOEUL-FR-011 | N1 | T, M | — |
| `SNIP-04` | Move and resize the selection with anchors | G, A | Drag inside to move, eight handles and the border band to resize | planned | NOEUL-FR-012 | N1 | T (FLOW-09, FLOW-11), M | — |
| `SNIP-05` | Abort with `Esc`, the close button, or a right-click outside edit mode | G, K | `Esc` and the toolbar's ✕ cancel; right-click clears the selection, and cancels when there is none; right-click inside opens a menu ◆ | planned | NOEUL-FR-012 | N1 | T, M | — |
| `SNIP-06` | Abort when another program's window becomes active (setting) | G | Same, default on, a setting | planned | NOEUL-FR-013 | N1 | T | — |
| `SNIP-07` | Select the whole screen with `Ctrl+A` (again: all screens) | K, L | `selection.full` (`Ctrl+A`): the monitor under the pointer, again all monitors | planned | NOEUL-FR-014 | N1 | T | — |
| `SNIP-08` | Reuse the last successful rectangle with `R` | K | `selection.last` (`R`) | planned | NOEUL-FR-014 | N1 | T | — |
| `SNIP-09` | Square selection while holding `Shift` | L | Same while dragging | planned | NOEUL-FR-011 | N1 | T | — |
| `SNIP-10` | Move the area while still dragging by holding `Space` | A | Planned as a refinement; `Space` without a held button marks corners for keyboard selection | planned | NOEUL-FR-046 | N4 | T | — |
| `SNIP-11` | Delayed capture and a dialog for position, size and delay | P, C | Delayed and custom capture from tray, settings and CLI | planned | NOEUL-FR-015 | N5 | T | — |
| `SNIP-12` | Type the region size during snipping; fixed-ratio capture (Pro) | P | Typed size and fixed aspect ratio | planned | NOEUL-FR-046 | N4 | T | — |
| `SNIP-13` | Capture the mouse cursor, toggled in the overlay | P, K | Optional pointer capture, off by default | planned | NOEUL-FR-016 | N4 | T, M | — |
| `SNIP-14` | Capture the active window (Pro) or the active screen | P, C | Active window and monitor capture from the tray and CLI, without an overlay | planned | NOEUL-FR-017 | N4 | T, M | — |
| `SNIP-15` | Refresh the frozen screenshot while keeping annotations, `F5` (Pro) | K, P | Not planned for now; cancel and capture again | not intended | D-12 | — | — | — |
| `SNIP-16` | Whiteboard and transparent whiteboard modes | C, P | Deferred, not core | not intended | D-12 | — | — | — |
| `SNIP-17` | "Super-snip": capture by holding `Win` and dragging, no hotkey (Pro) | P | `Win` chords are reserved for the OS and this needs a hook | not intended | D-06 | — | — | — |
| `SNIP-18` | Screenshots with rounded corners, shadow or border (Pro) | P, C | Deferred, not core | not intended | D-12 | — | — | — |
| `SNIP-19` | Text extraction from a screenshot (Pro) | P, C | Deferred: an online engine conflicts with "no network"; only a local one could be considered | not intended | D-02 | — | — | — |
| `SNIP-20` | Decode QR codes and barcodes (Pro) | P, C | Deferred; would be local only | not intended | D-03 | — | — | — |
| `SNIP-21` | Native share, send to other programs, file-dialog integration (Pro) | P | Local-first; tools compose through files and the CLI | not intended | D-04 | — | — | — |
| `SNIP-22` | Print the screenshot | K, C | Deferred; print a saved file | not intended | D-13 | — | — | — |

## 2. Detection, magnifier and pixel precision (`DET`)

| ID | Snipaste feature | Src | Noeul approach | Status | Requirement | MS | Verify | Evidence |
| --- | --- | --- | --- | --- | --- | --- | --- | --- |
| `DET-01` | Detect the window under the pointer and select it with a click | H, P | Same; geometry only, nothing read or stored | planned | NOEUL-FR-040, NOEUL-SEC-007 | N1 | T, M | — |
| `DET-02` | Detect UI elements; `Tab` toggles window or element detection | K, P | Element detection through UI Automation rectangles; `detect.cycle` (`E`) cycles window, element, off ◆ (`Tab` stays keyboard navigation) | planned | NOEUL-FR-040 | N1 | T, M | — |
| `DET-03` | Step to the parent or child element with the mouse wheel (Pro) | P | Planned refinement | planned | NOEUL-FR-046 | N4 | T | — |
| `DET-04` | Exclude programs from detection; fallback algorithm for problem programs | A | Exclusion list; a window that does not answer UI Automation in time falls back to its window rectangle | planned | NOEUL-FR-046, NOEUL-FR-040 | N4 | T | — |
| `DET-05` | Browser content needs accessibility support for element detection | T | Same OS limitation, documented; falls back to the window rectangle | planned | NOEUL-FR-040 | N1 | M, D | — |
| `DET-06` | Magnifier appears when useful; hold `Alt` to show it | G, K | Shown while selecting; `magnifier.toggle` (`M`) ◆ (the hotkey contract has no bare-modifier keys) | planned | NOEUL-FR-041 | N1 | T, M | — |
| `DET-07` | Move the pointer one pixel with `W A S D` | G, K | `pointer.move-*` on the physical keys `W A S D` (same position on every layout), with repeat and acceleration | planned | NOEUL-FR-042 | N1 | T | — |
| `DET-08` | Move the area with arrows, grow with `Ctrl`+arrows, shrink with `Shift`+arrows | G, K | Same keys, one device pixel, repeat | planned | NOEUL-FR-042 | N1 | T | — |
| `DET-09` | Swap the roles of `W A S D` and the arrow keys (setting) | A | Every key is rebindable through the hotkey contract | planned | NOEUL-FR-100, NOEUL-FR-101 | N5 | T | — |
| `DET-10` | Magnifier style, or hide it entirely (Pro) | P | Magnifier toggle and size setting | planned | NOEUL-FR-041 | N1 | T | — |
| `DET-11` | Region size in device or logical pixels (Pro) | P | Device pixels by default; logical units as a refinement | planned | NOEUL-FR-046 | N4 | T | — |
| `DET-12` | Keyboard-only rectangle selection | — (Noeul addition) | `selection.mark` (`Space`) marks two corners, or accepts the detected window | planned | NOEUL-FR-043 | N1 | T (FLOW-18), M | — |

## 3. Color picker (`COLOR`)

| ID | Snipaste feature | Src | Noeul approach | Status | Requirement | MS | Verify | Evidence |
| --- | --- | --- | --- | --- | --- | --- | --- | --- |
| `COLOR-01` | With the magnifier visible, `C` copies the pixel's color | G, K, H | `color.copy` (`C`), in the overlay and on pins | planned | NOEUL-FR-044 | N3 | T, M | — |
| `COLOR-02` | `Shift` switches the color format | G, K | `color.format-next` (`Shift+C`): HEX, RGB, HSL ◆ (no bare-modifier keys) | planned | NOEUL-FR-044 | N3 | T | — |
| `COLOR-03` | `F3` turns the copied color into a color card | G, H | `paste.float` pins color text as a swatch | planned | NOEUL-FR-071 | N2 | T | — |
| `COLOR-04` | More color models in the picker and on color cards (HSV and others; Pro) | P, L | HEX, RGB, HSL; more as a refinement | planned | NOEUL-FR-044 | N3 | T | — |
| `COLOR-05` | Edit the value of a color card (Pro) | P | Not planned yet; tracked with the card extras | planned | NOEUL-FR-069 | N4 | T | — |
| `COLOR-06` | Upper-case HEX (setting) | A | Setting | planned | NOEUL-FR-044 | N3 | T | — |
| `COLOR-07` | Color space of the picker; HDR color correction | L | Values of the captured sRGB picture; HDR converted to SDR with a stated limit | planned | NOEUL-FR-092 | N4 | T, M, D | — |

## 4. History (`HIST`)

| ID | Snipaste feature | Src | Noeul approach | Status | Requirement | MS | Verify | Evidence |
| --- | --- | --- | --- | --- | --- | --- | --- | --- |
| `HIST-01` | Step through earlier captures with `,` and `.` while snipping | G, K, H | `history.previous`/`history.next` (`Comma`, `Period`) step through earlier rectangles over the current picture; earlier images can be pinned again | planned | NOEUL-FR-050 | N4 | T | — |
| `HIST-02` | Only successful snips are recorded; maximum count setting | G | Same; count and total-size limits | planned | NOEUL-FR-050 | N4 | T | — |
| `HIST-03` | Choose the history folder | A | ◆ History stays in memory by default; disk history is opt-in, in a user-only folder, with retention limits | planned | NOEUL-FR-051, NOEUL-SEC-005 | N4 | T, I | — |
| `HIST-04` | Record aborted snips too (Pro) | P | Opt-in only | planned | NOEUL-FR-051 | N4 | T | — |
| `HIST-05` | Clear the snip records (Pro command) | C | Clear in settings, the tray and `noeul history clear --yes` | planned | NOEUL-FR-051, NOEUL-FR-111 | N4 | T | — |
| `HIST-06` | Automatically save every screenshot to a folder | P, L | Opt-in auto-save | planned | NOEUL-FR-051 | N4 | T | — |

## 5. Output and clipboard writes (`OUT`)

| ID | Snipaste feature | Src | Noeul approach | Status | Requirement | MS | Verify | Evidence |
| --- | --- | --- | --- | --- | --- | --- | --- | --- |
| `OUT-01` | Copy the snip: Copy button, `Ctrl+C`, `Enter`, or a **double-click inside the area** | G, K | ◆ Copy is `Ctrl+C`, the Copy button and the menu. `Enter` and the double-click **pin** instead. The opt-in Snipaste-style preset restores copying on both. | planned | NOEUL-FR-023, NOEUL-FR-024 | N1 | T (FLOW-19, FLOW-24), M | — |
| `OUT-02` | Save to a file: Save button, `Ctrl+S` | G, K | Same | planned | NOEUL-FR-023, NOEUL-FR-031 | N1 | T, M | — |
| `OUT-03` | Pin to the screen: Pin button, `Ctrl+T`, middle-click inside | G, K | ◆ **Double-click inside**, `Enter`, the Pin button (first, primary) and middle-click pin. `Ctrl+T` is bound only by the opt-in preset. | planned | NOEUL-FR-020, NOEUL-FR-021, NOEUL-FR-022 | N1 | T (FLOW-01…18), M (UX_SPEC §4.4) | — |
| `OUT-04` | Quick save to a folder: `Ctrl+Shift+S` or `Shift`+Save | G, K, F | Same keys; never overwrites | planned | NOEUL-FR-032 | N1 | T (FLOW-20) | — |
| `OUT-05` | The copied image pastes into other programs | H | PNG and `CF_DIBV5` on the clipboard, alpha kept | planned | NOEUL-FR-030 | N1 | T, M | — |
| `OUT-06` | Copy the image as a file as well (Pro) | P | Opt-in; temporary file in a user-only folder, deleted at quit | planned | NOEUL-FR-033 | N5 | T | — |
| `OUT-07` | Output quality setting | F | JPEG quality setting; PNG is lossless | planned | NOEUL-FR-031 | N1 | T | — |
| `OUT-08` | Remove the alpha channel when the image is fully opaque | A | Same | planned | NOEUL-FR-031 | N1 | T | — |
| `OUT-09` | Write program metadata into saved images (on by default) | A, L | ◆ No identifying metadata at all | planned | NOEUL-SEC-006 | N1 | T | — |
| `OUT-10` | Output targets from the command line: pin, clipboard, file, quick save, file dialog | C | `noeul capture … --pin`, `--clipboard`, `--output <file>` | planned | NOEUL-FR-112 | N5 | T | — |
| `OUT-11` | Run a program after a capture; several outputs at once (Pro) | C | Scripts call the CLI and use its JSON result | not intended | D-10 | — | — | — |
| `OUT-12` | Keep copies out of cloud clipboard sync | — (Noeul addition) | Clipboard writes marked to skip cloud sync by default | planned | NOEUL-FR-034 | N2 | T, M | — |

## 6. Annotation (`ANN`)

| ID | Snipaste feature | Src | Noeul approach | Status | Requirement | MS | Verify | Evidence |
| --- | --- | --- | --- | --- | --- | --- | --- | --- |
| `ANN-01` | Rectangle and ellipse; `Shift` draws a square or circle | H, A | Same | planned | NOEUL-FR-080 | N3 | T, M | — |
| `ANN-02` | Line, line strip and arrow; `Tab` switches line or arrow | G, H | Same tools; tool actions bindable, unbound by default ◆ (`Tab` stays navigation) | planned | NOEUL-FR-080 | N3 | T | — |
| `ANN-03` | Pencil and marker pen | H | Pen and marker | planned | NOEUL-FR-080 | N3 | T | — |
| `ANN-04` | Text that can be scaled and rotated | G | Text with IME (Korean first) and font fallback; scaling now, rotation as an extra | planned | NOEUL-FR-080, NOEUL-NFR-014 | N3 | T, M | — |
| `ANN-05` | Mosaic and Gaussian blur | H, P | Same, labeled as weak protection, plus a solid redaction that is irreversible on export | planned | NOEUL-FR-082 | N3 | T | — |
| `ANN-06` | Eraser | H, P | Same | planned | NOEUL-FR-082 | N3 | T | — |
| `ANN-07` | Undo `Ctrl+Z`, redo `Ctrl+Y`; `Ctrl+Shift+Z` removes all edits without undo | K | `Ctrl+Z`; `Ctrl+Y` and `Ctrl+Shift+Z` redo ◆; "clear all" is an undoable command without a default key | planned | NOEUL-FR-081 | N3 | T | — |
| `ANN-08` | Pen width with the wheel, `1`/`2` or `[`/`]` | G, K | Wheel and `[`/`]` (`char:` keys) | planned | NOEUL-FR-080 | N3 | T | — |
| `ANN-09` | Custom colors and opacity through a color dialog | G, F | Color with opacity | planned | NOEUL-FR-080 | N3 | T | — |
| `ANN-10` | Right-click finishes the current shape | G, K | Same; `Esc` leaves the tool and keeps the drawing | planned | NOEUL-FR-081 | N3 | T (FLOW-15) | — |
| `ANN-11` | Re-edit, clone and delete finished annotations (Pro) | P | Annotations stay editable objects until export | planned | NOEUL-FR-081 | N3 | T | — |
| `ANN-12` | Numbered markers, rounded rectangles, dashes, double-headed and styled arrows, filled or outlined text, magnifier shape (Pro) | P | Annotation extras | planned | NOEUL-FR-084 | N4 | T | — |
| `ANN-13` | Rotatable shapes; several pen sizes; per-tool colors; shareable palette (Pro) | P | Annotation extras | planned | NOEUL-FR-084 | N4 | T | — |
| `ANN-14` | Right-click marks the detected element in rectangle, ellipse or fill modes | A | Annotation extra | planned | NOEUL-FR-084 | N4 | T | — |
| `ANN-15` | Annotate an image window (`Space` shows the paint bar) | K, F | `pin.annotate` (`Space`) | planned | NOEUL-FR-080 | N3 | T, M | — |
| `ANN-16` | Crop an image window (Pro) | P | Crop on pins, and a size readout for measuring | planned | NOEUL-FR-083 | N3 | T, M | — |
| `ANN-17` | Double-click during annotation | — (Noeul rule) | ◆ A double-click while a tool is active never pins; `Enter` pins with the drawing | planned | NOEUL-FR-021 | N3 | T (FLOW-13, FLOW-14) | — |

## 7. Floating pins (`PIN`)

| ID | Snipaste feature | Src | Noeul approach | Status | Requirement | MS | Verify | Evidence |
| --- | --- | --- | --- | --- | --- | --- | --- | --- |
| `PIN-01` | Clipboard content or a snip becomes a topmost floating window | G, H, F | Independent always-on-top pins with stable IDs; no taskbar or Alt+Tab entry | planned | NOEUL-FR-060 | N1 | T, M | — |
| `PIN-02` | Move by dragging; move one pixel with the arrows | K | Same | planned | NOEUL-FR-061 | N1 | T, M | — |
| `PIN-03` | Zoom with the wheel or `+`/`-`; resize by dragging the border | G, H | Same, zoom around the pointer; 100 % is one image pixel per device pixel | planned | NOEUL-FR-061 | N1 | T, M | — |
| `PIN-04` | Middle-click resets to 100 % size and full opacity | G | Middle-click and `pin.zoom-reset` (`0`) | planned | NOEUL-FR-061 | N1 | T | — |
| `PIN-05` | Opacity with `Ctrl`+wheel or `Ctrl`+`+`/`-` | G, F | Same, never below 10 % | planned | NOEUL-FR-062 | N2 | T, M | — |
| `PIN-06` | Rotate with `1`/`2`, flip with `3`/`4` | G, K | Same keys | planned | NOEUL-FR-062 | N2 | T | — |
| `PIN-07` | Thumbnail mode with `Shift`+double-click | G, H | `Shift`+double-click and `pin.thumbnail` (`T`) | planned | NOEUL-FR-062 | N2 | T, M | — |
| `PIN-08` | Free-size thumbnail by right-dragging (Pro) | P | Pin extra | planned | NOEUL-FR-069 | N4 | T | — |
| `PIN-09` | Close with `Esc`, `Ctrl+W` or a **double-click** on the image | G, K | ◆ `Esc`, `Ctrl+W` and the menu close; a double-click on a pin does nothing by default, so the clicks that created a pin cannot close it | planned | NOEUL-FR-026, NOEUL-FR-061 | N1 | T (FLOW-28) | — |
| `PIN-10` | The paste hotkey brings back recently closed images (count setting) | G | ◆ "Reopen closed pin" is its own command (tray, CLI); `F3` always pins the clipboard | planned | NOEUL-FR-065 | N1 | T | — |
| `PIN-11` | Destroy an image (not recoverable): `Shift+Esc` or menu | G, K, F | `pin.destroy` (`Shift+Escape`), menu | planned | NOEUL-FR-065 | N1 | T | — |
| `PIN-12` | Hide and show all images with a global hotkey (`Shift+F3`) | G, C | Tray item and CLI; a global action without a default (no second global default, HOTKEY_CONTRACT §7.5) ◆; per-pin hide with `H` | planned | NOEUL-FR-064, NOEUL-FR-062 | N2 | T, M | — |
| `PIN-13` | Click-through for the image under the pointer, toggled by a user-set global hotkey; again with no image under the pointer makes all clickable | G, F | `pin.click-through` (`Ctrl+Shift+T`) on the focused pin, a marker on the pin, and recovery from the tray, settings and CLI (enabled only while a recovery path exists) | planned | NOEUL-FR-063, NOEUL-SEC-013 | N2 | T, M | — |
| `PIN-14` | Image groups: create, switch, close, clear | C, F, H | Named groups; switch, show, hide, close a group | planned | NOEUL-FR-064 | N2 | T | — |
| `PIN-15` | Group manager window (Pro) | P | Groups are managed in the settings window | planned | NOEUL-FR-064 | N2 | M | — |
| `PIN-16` | Automatic backup and restore of image windows | H | ◆ Opt-in (off by default); atomic, user-only, no duplicates on restore | planned | NOEUL-FR-066 | N4 | T, M | — |
| `PIN-17` | Snap to other images and windows while dragging with `Shift` | A | Pin extra | planned | NOEUL-FR-069 | N4 | T | — |
| `PIN-18` | Off-screen images after a display change stay put; "restore off-screen images" in the tray | L | Same model: pins stay; "Bring pins on screen" in tray and CLI | planned | NOEUL-FR-067 | N4 | T, M | — |
| `PIN-19` | Animated GIFs: playback speed, previous and next frame | H | GIF playback and frame stepping; a pin says when it shows only the first frame | planned | NOEUL-FR-068 | N2 | T | — |
| `PIN-20` | Select several images with `Ctrl`+click, or all with `Ctrl+A` (Pro) | P, K | Pin extra (multi-selection) | planned | NOEUL-FR-069 | N4 | T | — |
| `PIN-21` | Solo mode, topmost toggle, grayscale, inversion, background modes for transparency (Pro) | P | Pin extras | planned | NOEUL-FR-069 | N4 | T | — |
| `PIN-22` | Replace an image with the clipboard content (`Ctrl+V`) | K | Pin extra | planned | NOEUL-FR-069 | N4 | T | — |
| `PIN-23` | Copy the original text of a text image (`Ctrl+Shift+C`) | K | `pin.copy-text` (`Ctrl+Shift+C`) | planned | NOEUL-FR-071 | N2 | T | — |
| `PIN-24` | Magnifier and color picking on image windows | G | `color.copy` on pins | planned | NOEUL-FR-044 | N3 | T | — |
| `PIN-25` | Confirm before closing several images at once | L | Same | planned | NOEUL-FR-064 | N2 | T | — |
| `PIN-26` | Move images beyond the screen edge | L, F | Allowed; recovery through "Bring pins on screen" | planned | NOEUL-FR-061, NOEUL-FR-067 | N4 | M | — |
| `PIN-27` | Disable smooth scaling for sharp text | F | Pixelated scaling as a pin extra | planned | NOEUL-FR-069 | N4 | T | — |
| `PIN-28` | Images on virtual desktops (Pro) | P | Pin extra | planned | NOEUL-FR-069 | N4 | M | — |
| `PIN-29` | Paste or drag images from web pages, downloaded from the internet (Pro) | P | No network; an image already on the clipboard is pinned normally | not intended | D-05 | — | — | — |
| `PIN-30` | Print an image window (`Ctrl+P`) | K | Deferred | not intended | D-13 | — | — | — |

## 8. Paste sources (`PASTE`)

| ID | Snipaste feature | Src | Noeul approach | Status | Requirement | MS | Verify | Evidence |
| --- | --- | --- | --- | --- | --- | --- | --- | --- |
| `PASTE-01` | Paste with a global hotkey, `F3` by default | G, H | Global action `paste.float`, default `F3`; reads the clipboard once, at that moment | planned | NOEUL-FR-070, NOEUL-SEC-002 | N2 | T, M | — |
| `PASTE-02` | Paste by middle-clicking the tray icon | G | Same | planned | NOEUL-FR-004 | N2 | M | — |
| `PASTE-03` | An image on the clipboard | G, H | PNG, `CF_DIBV5`, `CF_DIB` | planned | NOEUL-FR-070 | N2 | T | — |
| `PASTE-04` | Color values: three integers 0–255, three numbers 0–1, or HEX starting with `#` | G | Same, plus `rgb(…)`; becomes a swatch | planned | NOEUL-FR-071 | N2 | T | — |
| `PASTE-05` | Plain text | G, H | Text card | planned | NOEUL-FR-071 | N2 | T | — |
| `PASTE-06` | HTML text, keeping formatting such as syntax highlighting | G, H, T | HTML card from Noeul's own sanitizing renderer; no scripts, no remote resources | planned | NOEUL-FR-071, NOEUL-SEC-009 | N2 | T | — |
| `PASTE-07` | Copied files: an image file is pasted as the image; other paths become an image of the path; option to never paste paths | G | Image files pinned; other files as a name card, off by default ◆ | planned | NOEUL-FR-071 | N2 | T | — |
| `PASTE-08` | Image files: PNG, JPG, BMP, TGA, ICO, TIFF, GIF | H | Same list (WebP optional), with size limits | planned | NOEUL-FR-072 | N2 | T | — |
| `PASTE-09` | Drag and drop | H | Drop image files on a pin or the settings window; `noeul pin <file>` | planned | NOEUL-FR-073 | N2 | T | — |
| `PASTE-10` | Paste from the command line: clipboard, plain text, HTML, files, at a position | C | `noeul pin <file>`, `noeul pin --clipboard`; text and position as CLI options | planned | NOEUL-FR-112 | N5 | T | — |
| `PASTE-11` | Wrap long HTML to the maximum width of text images | A | Card width limit | planned | NOEUL-FR-071 | N2 | T | — |
| `PASTE-12` | Ignore simulated "copy" commands from other programs | L | Noeul never reacts to clipboard changes at all | not intended | NOEUL-SEC-002 | — | I | — |
| `PASTE-13` | Convert clipboard content and send `Ctrl+V` to another program (Pro) | C | Needs synthetic input into another program | not intended | D-11 | — | — | — |
| `PASTE-14` | Empty or unsupported clipboard | — (Noeul rule) | A short notice, nothing else changes | planned | NOEUL-FR-074 | N2 | T | — |

## 9. Displays and color (`DISP`)

| ID | Snipaste feature | Src | Noeul approach | Status | Requirement | MS | Verify | Evidence |
| --- | --- | --- | --- | --- | --- | --- | --- | --- |
| `DISP-01` | Several screens | H, L | Overlay over the whole virtual desktop, negative coordinates included; exact on same-scale layouts in N1 | planned | NOEUL-FR-090 | N1 | T (FLOW-31), M | — |
| `DISP-02` | High-DPI displays | H | Per-monitor DPI awareness v2; device pixels everywhere | planned | NOEUL-FR-090 | N1 | T (FLOW-32), M | — |
| `DISP-03` | Mixed DPI: toolbar keeps its visual size; moving images across monitors | L | Same; pins keep their pixel size | planned | NOEUL-FR-091 | N4 | T (FLOW-33), M | — |
| `DISP-04` | Monitors connected or disconnected while running | L | Handled without restart; pins stay | planned | NOEUL-FR-091, NOEUL-FR-067 | N4 | T, M | — |
| `DISP-05` | HDR color correction | L | Converted to SDR sRGB with a stated limit | planned | NOEUL-FR-092 | N4 | T, M, D | — |
| `DISP-06` | Windows on ARM64 | L, H | Not tested, not claimed | not intended | NOEUL-NFR-010 | — | — | — |

## 10. Settings and customization (`SET`)

| ID | Snipaste feature | Src | Noeul approach | Status | Requirement | MS | Verify | Evidence |
| --- | --- | --- | --- | --- | --- | --- | --- | --- |
| `SET-01` | Preferences dialog with detailed options and explanations | F, H | Compact settings window with sections and reset per section | planned | NOEUL-FR-101 | N5 | T, M | — |
| `SET-02` | Change the global hotkeys | F, T | Hotkey contract: list, change, disable, reset, conflicts reported | planned | NOEUL-FR-100, NOEUL-FR-101 | N5 | T, M | — |
| `SET-03` | Run when the system starts | F | Opt-in start at sign-in (per-user, no service) | planned | NOEUL-FR-002 | N5 | T, M | — |
| `SET-04` | Theme color, interface font, tray icon options | H, P | Kim tokens, light theme, contrast themes | planned | NOEUL-FR-103 | N5 | M | — |
| `SET-05` | Hide the tray icon | A | Setting; everything stays reachable | planned | NOEUL-FR-004 | N1 | M | — |
| `SET-06` | Customize the tray icon's mouse actions (Pro) | P | Fixed defaults; customization as a refinement | planned | NOEUL-FR-004 | N1 | M | — |
| `SET-07` | Global hotkeys for most functions (Pro) | P | Every action is bindable; actions with `global` in their scopes can be bound globally by the user | planned | NOEUL-FR-100 | N5 | T | — |
| `SET-08` | Customize in-app shortcuts (Pro) | L | Every context shortcut is rebindable, free | planned | NOEUL-FR-100, NOEUL-FR-101 | N5 | T | — |
| `SET-09` | Turn global hotkeys off and on (Pro command) | C | `noeul hotkeys disable --all`, tray switch, `--no-hotkeys` | planned | NOEUL-FR-100 | N1 | T | — |
| `SET-10` | Hotkeys already used by other programs (e.g. `PrintScreen`) cannot be set | T | Reported as `taken` with remap offers; never stolen | planned | NOEUL-FR-100 | N1 | T | — |
| `SET-11` | Hotkeys may not work while an elevated window has focus | T | Same OS rule (UIPI), documented; Noeul never elevates | planned | NOEUL-SEC-003, NOEUL-SEC-012 | N1 | D, M | — |
| `SET-12` | Restore defaults | L | Reset per section and for all shortcuts | planned | NOEUL-FR-101 | N5 | T | — |
| `SET-13` | Configuration in `config.ini` next to the program; `--config` path | A, C, T | `%APPDATA%\mouv\noeul\config.toml`, versioned, unknown keys kept; import and export | planned | NOEUL-FR-102 | N1 | T | — |
| `SET-14` | Double-tap interval for keys | A | Noeul uses the Windows double-click settings; no double-tap keys | planned | NOEUL-NFR-012 | N1 | T | — |
| `SET-15` | Ignore global hotkeys while certain programs are active (Pro) | P | The registered key would be swallowed anyway; switch Noeul's global shortcuts off instead | not intended | D-08 | — | — | — |
| `SET-16` | Allow simple keys (letters, `Esc`) as global hotkeys | A | Reserved typing and navigation keys | not intended | D-09 | — | — | — |
| `SET-17` | Hot corners (Pro) | P | Needs pointer polling or a hook | not intended | D-07 | — | — | — |
| `SET-18` | Bind arbitrary commands to hotkeys (Pro) | P, C | Shortcuts serve Noeul's actions; automation belongs to Ddalgak and scripts | not intended | D-10 | — | — | — |
| `SET-19` | Process priority, GPU acceleration switch | A | No priority boost; rendering choice is internal | not intended | D-16 | — | — | — |
| `SET-20` | Update checks | L | Noeul never checks; Kim shows updates on request | not intended | D-15 | — | — | — |
| `SET-21` | Interface translations | H | English and Korean | planned | NOEUL-FR-104 | N5 | M | — |
| `SET-22` | Snipaste's double-click-copies gesture as an option | — (Noeul addition) | Opt-in "Snipaste-style gestures" preset | planned | NOEUL-FR-024 | N5 | T (FLOW-24) | — |

## 11. Command line (`CLI`)

| ID | Snipaste feature | Src | Noeul approach | Status | Requirement | MS | Verify | Evidence |
| --- | --- | --- | --- | --- | --- | --- | --- | --- |
| `CLI-01` | `snip` with a given area, size, full screen, last area, active window or screen | C | `noeul capture --region`, `--monitor`, `--window active`; `--interactive` opens the overlay | planned | NOEUL-FR-112 | N5 | T | — |
| `CLI-02` | Delay before snipping | C | `--delay` | planned | NOEUL-FR-015 | N5 | T | — |
| `CLI-03` | Wait for the snip to finish before the command returns | C | Commands return when done, with a JSON result | planned | NOEUL-FR-110, NOEUL-FR-112 | N5 | T | — |
| `CLI-04` | Show, hide or toggle all images; groups | C | `noeul pin hide`, `show`, `list`, groups | planned | NOEUL-FR-112 | N5 | T | — |
| `CLI-05` | Turn click-through off for all images | C | `noeul pin set --all --click-through off` | planned | NOEUL-FR-063, NOEUL-FR-112 | N2 | T | — |
| `CLI-06` | Commands work only while the program runs | C | Captures to a file or the clipboard work without a running instance and open no window; pin commands need the instance (`noeul_not_running`, or `--start`) ◆ | planned | NOEUL-FR-112 | N5 | T | — |
| `CLI-07` | Open preferences or help; quit | C | `noeul settings` (explicitly opens the window), `noeul quit` | planned | NOEUL-FR-112 | N5 | T | — |
| `CLI-08` | Pick a color, clear history, toggle hotkeys or hot corners (Pro commands) | C | History and hotkeys commands; no hot corners | planned | NOEUL-FR-111 | N5 | T | — |
| `CLI-09` | Run an external program (Pro command) | C | Out of scope for Noeul | not intended | D-10 | — | — | — |

## 12. Application and platform (`APP`)

| ID | Snipaste feature | Src | Noeul approach | Status | Requirement | MS | Verify | Evidence |
| --- | --- | --- | --- | --- | --- | --- | --- | --- |
| `APP-01` | Stays running in the background (needed for pasting) | F | One per-user process while the user keeps it running; quitting releases everything | planned | NOEUL-FR-002 | N1 | T, M | — |
| `APP-02` | Portable program, installer or store package | H | Per-user install through Kim on explicit request, or by hand | planned | NOEUL-FR-001, NOEUL-FR-005 | N6 | T, M | — |
| `APP-03` | Windows, macOS and Linux editions | H | Windows first; Linux at N7 within portal and compositor limits; macOS out of scope | planned | NOEUL-NFR-010, NOEUL-NFR-015 | N7 | T, M | — |
| `APP-04` | Free for personal use; paid Pro edition, activation, accounts | P, H | One free edition, MIT OR Apache-2.0, no accounts | not intended | D-14 | — | — | — |
| `APP-05` | Run as administrator | T | Never elevates | not intended | D-16 | — | — | — |
| `APP-06` | 32-bit and Windows XP builds | H | Not supported | not intended | D-18 | — | — | — |
| `APP-07` | Single running instance | F | Single instance with request hand-over; a second start opens settings | planned | NOEUL-FR-003 | N1 | T | — |

## 13. Noeul additions beyond Snipaste (`ADD`)

These rows have no Snipaste counterpart; they record what Noeul adds so the audit sees the whole
picture.

| ID | Capability | Noeul approach | Status | Requirement | MS | Verify | Evidence |
| --- | --- | --- | --- | --- | --- | --- | --- |
| `ADD-01` | Machine-readable CLI | `--json` envelope, stable error codes, capabilities document | planned | NOEUL-FR-110 | N1 | T | — |
| `ADD-02` | Headless commands that never open UI | Status, config, hotkeys and history work without a desktop | planned | NOEUL-FR-111 | N1 | T | — |
| `ADD-03` | Optional MCP adapter | Allowlisted tools; capture and clipboard off until enabled | planned | NOEUL-FR-113 | N5 | T, D | — |
| `ADD-04` | Captures from scripts and agents are announced | Visible notice; setting to deny | planned | NOEUL-SEC-010 | N5 | T, M | — |
| `ADD-05` | Honest session limits | `desktop_unavailable` in locked, secure and display-less sessions | planned | NOEUL-SEC-003 | N1 | T (FLOW-35), M | — |
| `ADD-06` | Shared hotkey contract across Kim tools | Conflicts with other Kim tools found without a daemon | planned | NOEUL-FR-100 | N1 | T | — |
| `ADD-07` | No network at all | No update check, telemetry or remote content | planned | NOEUL-NFR-008 | N1 | T, I | — |

## How evidence is recorded

- **Evidence cell.** One or more entries using the verification codes: `T:` automated test (crate
  path and test name), `M:` manual check on real Windows (Windows version and build, scale and
  monitor setup, date), `I:` inspection, `D:` documentation, each with the commit that introduced
  it. `—` means no evidence yet.
- **`supported`** needs at least one `T:` entry, plus an `M:` entry when the behavior depends on
  the GUI, input or the OS. **`partial`** says in the approach cell what is missing. Neither
  status is allowed without evidence.
- **`not intended`** rows keep their reason. Moving a row into scope adds a requirement row and,
  when it trades a principle, an ADR.
- **IDs are permanent.** A dropped row is marked *withdrawn*, not deleted; a newly found Snipaste
  feature gets the next free ID in its group.
- **Audits.** Step 63 (and any later audit) re-reads the sources, updates the Reference line,
  adds or withdraws rows, recomputes the counts and appends to the audit history.

## Summary counts

| Status | Rows |
| --- | --- |
| `supported` | 0 |
| `partial` | 0 |
| `planned` | 147 (of which 7 are Noeul additions) |
| `not intended` | 24 |
| **Total** | **171** |

| Group | planned | not intended | Total |
| --- | --- | --- | --- |
| 1. Starting and selecting (`SNIP`) | 14 | 8 | 22 |
| 2. Detection and precision (`DET`) | 12 | 0 | 12 |
| 3. Color picker (`COLOR`) | 7 | 0 | 7 |
| 4. History (`HIST`) | 6 | 0 | 6 |
| 5. Output and clipboard (`OUT`) | 11 | 1 | 12 |
| 6. Annotation (`ANN`) | 17 | 0 | 17 |
| 7. Floating pins (`PIN`) | 28 | 2 | 30 |
| 8. Paste sources (`PASTE`) | 12 | 2 | 14 |
| 9. Displays and color (`DISP`) | 5 | 1 | 6 |
| 10. Settings (`SET`) | 16 | 6 | 22 |
| 11. Command line (`CLI`) | 8 | 1 | 9 |
| 12. Application (`APP`) | 4 | 3 | 7 |
| 13. Noeul additions (`ADD`) | 7 | 0 | 7 |

Deviations from Snipaste's defaults (**◆**) are in `SNIP-05`, `DET-02`, `DET-06`, `COLOR-02`,
`HIST-03`, `OUT-01`, `OUT-03`, `OUT-09`, `ANN-02`, `ANN-07`, `ANN-17`, `PIN-09`, `PIN-10`,
`PIN-12`, `PIN-16`, `PASTE-07` and `CLI-06`.

## Audit history

| Date | Snipaste reference | Step | Change |
| --- | --- | --- | --- |
| 2026-11-04 | 2.11.3 (2026-01-18) | Step 48 | Initial inventory. Nothing implemented. |
