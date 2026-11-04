# ADR 0038 — Noeul annotation: vector objects until export, one editor for captures and pins, GDI text with fallback

Status: **Accepted** (Step 56) · Date: 2026-11-04 · Builds on
[ADR 0021](0021-noeul-product-definition.md) (§5, the hotkey manifest),
[ADR 0026](0026-noeul-capture-overlay.md) (§3–§5), [ADR 0029](0029-noeul-pin-engine.md) (§2, §4),
[ADR 0032](0032-noeul-double-click-pin-and-outputs.md) (§1–§3) and
[ADR 0035](0035-noeul-smart-selection-and-precision.md) (§6)

## Context

Step 56 (issue #61) adds Snipaste-style annotation: rectangle, ellipse, line, arrow, pen, marker
and text with an input method (Korean first) and font fallback, colours and widths, undo and redo,
selecting, moving, resizing and reordering objects, crop and a size readout, both before an output
of the capture overlay and on an existing pin (NOEUL-FR-080, NOEUL-FR-081, NOEUL-FR-083,
NOEUL-NFR-014). The requirements fix the outline — annotations stay editable objects until an
output, `Esc` leaves the tool, a double-click while a tool is active never pins (NOEUL-FR-021),
"clear all" is undoable, cancelling leaves nothing — but leave open how objects are stored and
drawn without slowing the overlay down, how a pin (a retained layered window of its picture, ADR
0029) is edited, which keys a keyboard-only user has for objects, how text gets the input method
without breaking letter shortcuts in Hangul mode (ADR 0026 §5), and what `status` may say about
annotations.

## Decision

### 1. A vector model in picture pixels, one undo step per edit

[`annotation::Object`](../../crates/noeul/src/annotation.rs) is a shape (rectangle and ellipse
as a box, line and arrow as two points, pen and marker as point lists, text as a corner, a string
and its typeset glyphs), a colour of an 8-colour palette, a width **level** (1, 2, 3, 4, 6, 8 or 12
logical pixels; markers and text sizes have their own tables) and the **DPI of the monitor it was
drawn on**. Coordinates are device pixels of the picture it annotates — the frozen picture in the
overlay, the pin's image (`(0, 0)` its top-left) on a pin — never window or screen points of
something that moves. So a stroke drawn on a 150 % monitor is 1.5 times as thick in device pixels,
whatever monitor the output goes to, and a pin can move or zoom without touching its objects.

[`annotation::Doc`](../../crates/noeul/src/annotation.rs) keeps the objects in drawing order and
an undo stack of **whole object lists** (at most 200 steps): every gesture (a whole stroke, a whole
move or resize, a whole text label typed with its compositions) records the state before it as one
step, only when it changed something; redo lives until the next edit. Undo and redo therefore
restore exactly the earlier objects (glyphs included), which makes them lossless by construction
and cheap (annotations are small). A command log was the alternative: less memory for huge
sessions, but every command needs an exact inverse, which is where lossless undo usually breaks.

### 2. Pixels only on export, by one rasterizer the overlay shares

Nothing is painted into the frozen picture. [`annotation::draw`](../../crates/noeul/src/annotation.rs)
rasterizes objects in Rust: per object a coverage mask (anti-aliased round-capped segments, an
ellipse from its approximate distance field, crisp axis-aligned rectangle outlines, a filled arrow
head, the glyph mask of a label), blended once with source-over on straight alpha — so a marker is
one translucent coat without darker overlaps, and a transparent picture stays transparent where
nothing is drawn. [`annotation::flatten`](../../crates/noeul/src/annotation.rs) (the selection's
pixels plus the objects) is the only path to copy, save, quick save and a pin's window.

The overlay shows annotations through a **layer**: the selection's pixels with the objects drawn
on them, made by the same `flatten` and refreshed by `annotation::refresh` only where an edit
changed something ([`Editor::take_dirty`](../../crates/noeul/src/annotation.rs)); `compose` takes
the layer's pixels inside the selection. The layer exists only while there are annotations and is
freed with the overlay. Because screen and export run the same code over the same pixels, the
tests compare the overlay's composed pixels with the exported PNG pixel for pixel.

### 3. The toolbar and the rules of a press

The toolbar gains the seven tools, the colour button (next colour; `Shift`: previous), the width
button (one level thicker, back to the thinnest), Undo and Redo, between Save and Cancel (UX spec
§3.1). Their icons are original vector drawings made with the same rasterizer (no image assets).
On a monitor narrower than the bar the buttons wrap into rows inside the monitor.

While a tool is active (phase `annotating`) the selection's handles are hidden and the selection is
a canvas: a press inside draws with the tool, a press on an object picks it (the pen and the marker
always draw, so a stroke may start on another one; the text tool edits the label it presses on),
the handles of the picked object resize it, a drag moves it; drawing is clipped to the selection;
nothing pins (FLOW-13), a double-click on a label selects a word (FLOW-14); `Esc` ends a label,
else aborts a shape being drawn, else leaves the tool and keeps the drawing (FLOW-15, ANN-10);
a right-click finishes the shape being drawn, else leaves the tool. `Enter` and Pin always pin
with the annotations. Without a tool the selection moves and resizes as before; annotations stay
where they are on the picture and an output keeps the ones that show inside the selection. The
size chip shows the object being drawn or selected (its box, or a line's length and angle) — the
measurement readout of NOEUL-FR-083 — and with a tool it never swallows a press.

### 4. Keys: the `annotate` context inside the overlay's

The manifest's `annotate` context is matched before `capture-overlay` **whenever a selection is
confirmed** (and in the pin editor), so `Ctrl+Z` works after `Esc` left the tool; `annotate.done`
applies only while there is something to end — otherwise its `Escape` falls through and cancels the
capture as before. The tool actions stay bindable without defaults (NOEUL-FR-080). Keyboard-only
editing needs object keys the final manifest did not have; this ADR adds them to
`docs/contracts/examples/kim-hotkey-manifest-v1/noeul.json` (73 → 80 actions, ADR 0021 §5):
`annotate.delete` (`Delete`), `annotate.select-next` / `-previous` (`N` / `Shift+N`) and
`annotate.bring-forward`, `-send-backward`, `-bring-to-front`, `-send-to-back` (`PageUp`,
`PageDown`, `Shift+PageUp`, `Shift+PageDown`). While an object is selected the selection keys act
on it (arrows move it, `Ctrl`+arrows grow it, `Shift`+arrows shrink it); with a tool, `Space`
(`selection.mark`) sets a shape's start and end at the pointer the `W A S D` keys move, so every
tool works without a mouse (NOEUL-NFR-012). `char:[` / `char:]` are matched from `WM_CHAR`; the
character of a letter or digit is never matched (its key press already was), so `N` fires once.
While a label is edited every shortcut is suspended except its editing keys and `Esc`
(UX spec §6, HOTKEY_CONTRACT §6.2).

### 5. Text: inline compositions and GDI with per-character fallback

The overlay window keeps no input context (letter shortcuts work in Hangul mode, NOEUL-NFR-014);
**while a label is edited** the owner associates the window's default input context
(`ImmAssociateContextEx(…, IACE_DEFAULT)`) and puts the candidate window at the caret, and removes
it again when editing ends. The user's input method is never switched. `WM_IME_COMPOSITION` gives
the composition (`GCS_COMPSTR`, shown inline at the caret and underlined, not yet text) and the
result (`GCS_RESULTSTR`, inserted); the IME's own composition window is hidden
(`ISC_SHOWUICOMPOSITIONWINDOW` cleared). `WM_CHAR` (surrogate pairs joined) and `WM_IME_CHAR`
insert characters.

Labels are typeset by [`win::typeset`](../../crates/noeul/src/win/typeset.rs): every character is
drawn with the first face of a list (Segoe UI, Malgun Gothic, Segoe UI Emoji, Segoe UI Symbol, the
CJK, Indic, African and historic UI faces of Windows 10) whose font has a glyph for it
(`GetGlyphIndicesW` with `GGI_MARK_NONEXISTING_GLYPHS`); characters outside the BMP go to Segoe UI
Emoji (emoji blocks) or Segoe UI Historic. Runs of one face are drawn with one `ExtTextOutW`
(white on black, grayscale anti-aliasing) into a DIB whose one channel is the coverage mask; the
system's script processing shapes each run and orders a right-to-left run. The glyphs are cached in
the object, so export, undo and pins never need a typesetter. DirectWrite would add colour emoji
and full bidirectional layout across fonts, at the price of much more COM code; it is left for a
later Step. *Limitations:* emoji are monochrome, a line that mixes directions across different
fonts is not reordered between runs, and carets in a right-to-left run follow logical order.

### 6. Pins: objects kept, edited in a layered window of the same model

A pin keeps its picture **unchanged** plus its objects; its window shows `flatten` of both, and
`pin.copy`, `pin.save` and `pin.quick-save` output that; close and reopen keep the objects.
`pin.annotate` (`Space`, the pin menu) opens the overlay model in
[`Mode::PinEdit`](../../crates/noeul/src/overlay.rs) over a canvas made of the pin's picture at
100 % at its top-left corner, room for the toolbar (the same
[`toolbar_layout`](../../crates/noeul/src/overlay.rs)) and a band for the size chip, in a
**layered** window (`WS_EX_LAYERED`, `UpdateLayeredWindow` with premultiplied pixels): the canvas
around the pin is transparent and lets clicks through. The pin's window hides meanwhile. The
selection is the pin and never goes away; dragging its handles (or `Shift`/`Ctrl`+arrows) crops
inside the picture. Done or `Enter` commits the objects and the crop — the kept part stays where it
was shown, at the pin's zoom, and the crop cannot be undone after the commit (Snipaste's crop is
destructive too); Cancel or `Escape` leaves the pin exactly as it was. Losing focus keeps the
editor open (work is never discarded by a click elsewhere). One overlay exists at a time: a capture
request while a pin is edited is ignored, `pin.annotate` while a capture overlay is open does
nothing, and closing or destroying the edited pin ends the edit without committing.

### 7. What `status` says

`captures.overlay` and the new `pins.editor` carry `mode` (`capture`, `pin_edit`), `pin`, phase
`annotating`, the new toolbar focus names and an `annotation` object: the tool, the number of
objects per kind, the selected object's ID, undo and redo counts, whether a label is edited or
composed, the colour name and the width. `pin.list` gives each pin's annotation count and whether
it is edited. Never the text of a label, a point or a pixel (NOEUL-SEC-008); a contract example
with text in it is invalid.

## Consequences

- Annotation adds no dependency: the rasterizer is Rust, text uses GDI and IMM32 already in
  `windows-sys`'s enabled features.
- Memory while annotating: one extra buffer of the selection's size (the layer); a pin with
  annotations renders a temporary flattened copy for its window, which keeps its own copy as before
  (ADR 0029 §2); un-annotated pins are unchanged.
- The toolbar is 616 logical pixels wide; on the off-screen test layout's 480-pixel monitor at
  150 % it wraps into two rows and, without room below or above, sits inside the selection's bottom
  edge (FLOW-38). Step 51's toolbar tests were updated to the new geometry.
- Tested headlessly: the model per tool (create, undo, redo, reorder, editing, IME, long text,
  RTL storage), ASCII goldens at 100 % and 150 %, transparent pictures, screen-equals-export, the
  monitor seam, keyboard-only use, the pin editor's crop and commit, the real off-screen windows
  (layered editor, `WM_CHAR`/`WM_IME_*`/wheel, the input context associated only while asked, the
  GDI fallback faces and right-to-left order on this machine), and two real-binary flows.
  **Not tested** on the visible desktop: drawing with a physical mouse, the real Korean IME's
  candidate window, and pasting an annotated copy into another program; NOEUL_UX_SPEC §4.9 has the
  manual script.
- Mosaic, blur, eraser and solid redaction (NOEUL-FR-082) are Step 57; the manifest keeps their
  IDs and this build does not ship them.
