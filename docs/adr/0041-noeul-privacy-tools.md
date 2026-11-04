# ADR 0041 — Noeul privacy tools: effects as objects, redactions applied first, outputs hold pixels only

Status: **Accepted** (Step 57) · Date: 2026-11-04 · Builds on
[ADR 0038](0038-noeul-annotation.md) (§1–§3, §6, §7), [ADR 0032](0032-noeul-double-click-pin-and-outputs.md)
(the outputs and their PNG), [ADR 0023](0023-noeul-app-foundation.md) (§2, §4: no GPU context,
pictures in memory only) and [ADR 0021](0021-noeul-product-definition.md) (§5, the manifest)

## Context

Step 57 (issue #62) adds mosaic, Gaussian blur, an eraser and a true solid redaction
(NOEUL-FR-082): adjustable strength and size with a preview, exported pixels and metadata that keep
nothing of what a redaction covers, a UI that says blur and mosaic are weak protection, undo, redo,
selectable objects, copy and file export with no intermediate unredacted picture anywhere,
efficient processing of large pictures, and no image work while idle. ADR 0038 left these tools out
and fixed what they must fit into: annotations are vector objects until an output, one rasterizer
serves the screen and every output, a pin keeps its picture unchanged plus its objects. Open were:
how an effect that is made *from* the picture fits a model of objects drawn *on* it; what exactly a
redaction guarantees when blurs, mosaics, erasers and later edits overlap it; what an eraser means
when drawings are objects; how the overlay's partial re-render stays exact; how strength is
adjusted without new shortcut actions; and whether a GPU is needed.

## Decision

### 1. The four tools are objects too

[`Shape::Effect(Effect, Area)`](../../crates/noeul/src/annotation.rs): mosaic, blur, erase or
redact, over a **box** of whole pixels (dragged corner to corner, `Shift` squares it) or a **brush**
stroke as wide as the object's brush. Mosaic, blur and redaction draw boxes, the eraser a brush;
`Ctrl` at the press swaps the form (and never picks an object). An effect is an object like any
other: one undo step per gesture, picked by a press on a box's edge (a press inside draws another
effect over it) or on a brush stroke, moved, resized by its handles (boxes keep whole-pixel
edges), re-levelled, reordered, deleted, cleared and restored losslessly. An eraser stroke shows
nothing of its own, so the mouse never picks it; `N` does.

### 2. Pixels at output time, every redaction first

Nothing is applied to the picture before an output (ADR 0038 §2). [`draw`](../../crates/noeul/src/annotation.rs)
— the one rasterizer behind the overlay's layer, copy, save, quick save and a pin's window — now
works in three steps over the picture part it renders:

1. **Every solid redaction is filled first**, whatever its place in the drawing order, with its
   colour forced opaque: a box exactly, a brush on every pixel its stroke touches at all (no soft
   edge that would keep part of a pixel).
2. The pixels the erasers will bring back are kept as they are *now* — the picture with the
   redactions applied.
3. The objects are drawn in order: shapes as before; a mosaic or blur processes the pixels as they
   are at its place in the order; an eraser puts the kept pixels back; a redaction is filled again
   at its own place, so annotations drawn before it are covered and later ones (a label on the
   black bar) lie on top.

Consequently **no output depends on what lies under a redaction**: no blur or mosaic before or
after it can spread it, no eraser can bring it back, no reordering can uncover it. The tests render
two pictures that differ only under a redaction, under blurs, mosaics, an eraser and a pen stroke
in every order, and require identical outputs. Alternatives: painting redactions into the picture
when drawn (irreversible at once, so undo would need a copy of the original — which is exactly the
hidden layer this Step must avoid in outputs, and it loses editing); or redactions only in drawing
order (a blur drawn earlier would carry blurred redacted text past the bar's edge).

### 3. What mosaic, blur and the eraser do

- **Mosaic** gives every cell of a grid anchored at the object (a box's corner, a stroke's first
  pixel — so the cells move with it and a pin shows the cells the overlay showed) the average of
  the cell's pixels inside the object. **Blur** is a Gaussian approximated by three box passes per
  direction (box widths from √(12σ²/3 + 1)), O(1) per pixel whatever σ. Both read **only the pixels
  inside the object** (edges repeated): no content from outside is pulled in, the result is the
  same in the overlay, an output and a pin, and the overlay's partial refresh can be exact (§6).
  Both average premultiplied colours, so a transparent pixel adds no colour (no fringe at alpha
  edges), and blend a brush stroke's soft edge by its coverage.
- **The eraser** wipes everything drawn before it — shapes, strokes, labels, mosaic, blur — back
  to the picture under it, the picture *with the redactions applied*; what is drawn after it stays.
  This is Snipaste's mark eraser as an undoable object. An object eraser (removing whole objects)
  was rejected: deleting already does that, and partial erasing of a stroke or a blur is the
  feature. An eraser that reveals the original under a redaction was rejected for privacy; taking a
  redaction away is a deliberate delete.

### 4. Strength, size and the preview

The width level that `[`, `]`, the wheel and the width button already change (no new manifest
action) sets a mosaic's cell size ([`MOSAIC_CELLS`](../../crates/noeul/src/annotation.rs): 4, 6, 8,
12, 16, 22, 30 logical pixels), a blur's σ (`BLUR_SIGMAS`: 2, 3, 4, 6, 8, 11, 15) and a brush's
width (`BRUSH_WIDTHS`: 8, 12, 16, 24, 32, 44, 60), all scaled by the DPI of the monitor the object
was drawn on (ADR 0038 §1). A box's size is its drag and its handles. The preview is the object
itself: it is rendered while it is drawn and whenever it or its level changes, and "applying"
happens only when an output flattens it. *Limitation:* a brush's width and its strength share the
level; separate per-tool settings belong to the annotation extras (NOEUL-FR-084).

### 5. Where the original lives, and what leaves Noeul

- Until an output the picture stays **in memory only**, under its objects (the frozen picture of
  the overlay, a pin's picture), so the user — and only the user — restores it: undo, delete,
  clear all, cancel the capture, or delete the redaction in the pin editor and commit. A failed
  output keeps every object (ADR 0032, NOEUL-FR-025). A pin is not an export: it keeps its picture
  and its redactions as objects in memory (also in the reopen memory); its window and every output
  of it are flattened.
- **Outputs hold flattened pixels only**: the PNG of copy, save and quick save has IHDR, IDAT and
  IEND and nothing else (ADR 0032, NOEUL-SEC-006), `CF_DIBV5` is a header and pixels, there is no
  layer, no metadata, no second picture. The only file Noeul writes besides the chosen one is the
  temporary file beside it when a save replaces a file — the same finished PNG, renamed over the
  target (ADR 0032). No intermediate picture is written anywhere, temporary folders included
  (NOEUL-SEC-005); the e2e scenario checks the sandbox's temporary folder stays empty.
- **The UI says what protects.** While the mosaic or blur tool is active, or such an object is
  drawn or selected, the size chip's second line reads "Visual only, weak protection: use Redact to
  hide for good"; for a redaction "Solid fill: nothing under it is copied or saved"; for the eraser
  "Erases drawings, never a redaction". The manifest already titles `annotate.redact` "Solid
  redaction tool (irreversible on export)". A redaction starts **black** with a colour of its own
  (the colour button chooses it while the redaction tool is active; every palette colour is
  opaque), so the redact affordance defaults to the solid, strong option.

### 6. CPU only, on demand, re-rendered whole

The effects run on the CPU of the thread that asks for pixels (ADR 0023 §2: no GPU context per
capture), only when an edit or an output needs them: no thread, no timer, no cache that outlives a
call, nothing while idle ([`effect_passes`](../../crates/noeul/src/annotation.rs) counts the
picture-processing passes for tests; idle redraws and pointer moves add none). A blur allocates a
premultiplied copy of its area (4 bytes per pixel) and a ring of `r + 1` rows for the call; its
column passes run row by row with running sums (cache-friendly). The overlay's
[`refresh`](../../crates/noeul/src/annotation.rs) grows the changed area until it holds every mosaic
and blur it touches, whole (such an effect is made from all of its pixels; re-rendering part of it
would read pixels it had already changed), and returns the area so the window repaints it; a test
compares the refreshed layer with a fresh export after every kind of edit.

Measured on this machine (Windows 10 Pro 19045, release build,
[`examples/redact_measure.rs`](../../crates/noeul/examples/redact_measure.rs), a generated
3840 × 2160 picture, median of 7 runs), time to flatten the whole picture:

| Objects | Time |
| --- | --- |
| none (the crop every output makes) | 7.2 ms |
| solid redaction over the whole picture | 12.2 ms |
| mosaic over the whole picture, 8 px / 30 px cells | 20.0 / 17.5 ms |
| blur over the whole picture, σ 2 / 4 / 15 | 158 / 156 / 157 ms |
| blur over a 1920 × 1080 box, σ 4 | 44.2 ms |
| a 60 px blur brush stroke across the picture | 115 ms |
| the overlay's refresh after a pen stroke into a 1920 × 1080 blur | 38.1 ms |
| effect passes while idle for 500 ms | 0 |

A first version blurred columns one by one (390–723 ms for the whole picture, 184 ms for the
1080p box); the row-by-row column pass made it 2.5–4.6 times faster and independent of σ. A GPU
path is not justified: the common case (boxes over lines of text) costs a few milliseconds, and a
whole-screen blur stays well under the time of the capture itself.

### 7. Toolbar, keys and status

The four tools join the tools group after Text (UX spec §3.1): the toolbar is 744 logical pixels
wide (616 before) and wraps on monitors narrower than that, as ADR 0038 §3 provides — on the
off-screen test layout's 480-pixel monitor at 150 % it now takes three rows. Their icons are
original vector drawings (a checkerboard, a soft disc, an eraser block, a solid bar). The
`annotate.mosaic`, `.blur`, `.eraser` and `.redact` actions, in the final manifest since ADR 0021
and 0038, now ship (bindable, no default key; 64 → 68 implemented actions; the manifest itself is
unchanged). `status` adds the four kinds to the annotation counts and the tool and focus names; it
never reports a pixel, a point or what a redaction covers (NOEUL-SEC-008).

## Consequences

- No new dependency; the e2e crate gains `png` (already in the lockfile) to decode a saved picture.
- The overlay's keyboard drawing (`Space`, ADR 0038 §4) draws each tool's default form; the other
  form needs `Ctrl` with the mouse.
- 8-bit premultiplication rounds the colour of nearly transparent pixels in a blur or mosaic
  (below 16 of 255 alpha), invisible but not exact.
- Tested headlessly: the model, every pixel rule above, the privacy property, screen equals export,
  repeated undo, alpha edges and odd sizes, clipping and cancel, failed exports, pins, idle, the
  real binary on the synthetic desktop and the e2e scenario (ignored, run by `cargo xtask e2e`).
  **Not tested** on the visible desktop with a physical mouse, a real paste into another program and
  a metadata viewer; NOEUL_UX_SPEC §4.10 has the manual script.
