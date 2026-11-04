# ADR 0047 — Noeul clipboard paste and rich sources: one user-triggered read, pure-Rust decoders, cards from Noeul's own sanitizer and layout

Status: **Accepted** (Step 55; the real-clipboard checks with real applications are manual) ·
Date: 2026-11-04 · Builds on [ADR 0029](0029-noeul-pin-engine.md) (the pin engine, placement of a
paste), [ADR 0044](0044-noeul-pin-transforms-and-control.md) (transforms; the honest first-frame
notice of animated GIFs), [ADR 0041](0041-noeul-privacy-tools.md) (outputs hold pixels only),
[ADR 0038](0038-noeul-annotation.md) (GDI text with font fallback) and
[ADR 0021](0021-noeul-product-definition.md) (§3, privacy)

## Context

Step 55 (issue #60) makes `F3` (`paste.float`) pin what the clipboard holds: bitmaps and images
(`CF_DIBV5`, `CF_DIB`, PNG), image files and copied paths (`CF_HDROP`), plain text, HTML and colour
values, as floating pins and cards that pin, transform and close like images; local decoding of
PNG, JPEG, BMP, TGA, ICO, TIFF and GIF with alpha, bounds on hostile input and an honest answer for
multi-image data; a format priority with a way to choose another; drops and `noeul pin <file>`;
an empty or held clipboard; and privacy: the clipboard is never read in the background, content is
never logged, secrets are never shown in previews or logs, nothing is downloaded. The
requirements: NOEUL-FR-070…NOEUL-FR-074, NOEUL-SEC-002, NOEUL-SEC-008, NOEUL-SEC-009,
NOEUL-SEC-010, NOEUL-NFR-008.

## Decision

### 1. One read, only when the user pastes, through a narrow trait

A paste goes through [`paste::take`](../../crates/noeul/src/paste.rs) on a
[`paste::Clipboard`](../../crates/noeul/src/paste.rs): the Win32 clipboard while it is open
(`win::clipboard::WinClipboard`), the debug-build test hook's in-memory stand-in
(`paste::Snapshot::from_dir`, a folder of formats a test synthesizes; the hook never reads the
user's clipboard — before this Step its `F3` did), or a snapshot in unit tests. `take` looks at
which formats are offered, reads only the short text it needs to recognise a colour or a copied
path, then copies out **only the chosen format**, bounded (images at the decoder's limit, an
uncompressed DIB at the pixel limit, HTML 4 MiB, text 2 MiB); the clipboard is closed before
anything is decoded. Callers: `F3`, the tray's middle click, the pin menu's "Pin clipboard as…"
and `pin.create` (`noeul pin --clipboard`). Nothing listens or polls (NOEUL-SEC-002).

**Private content is refused before anything is read.** Programs that copy secrets mark the
clipboard with `ExcludeClipboardContentFromMonitorProcessing`, `Clipboard Viewer Ignore` or
`CanIncludeInClipboardHistory` = 0 (*documented* for clipboard history). A floating,
always-on-top card would show the secret on every screen share, so Noeul pins nothing and says
so ("marked private"); the user pastes it where it is needed instead.

### 2. Priority, and another kind on request

The kinds ([`paste::Kind`](../../crates/noeul/src/paste.rs)) in order: **image** (registered `PNG`
or `image/png`, `CF_DIBV5`, `CF_DIB`, registered `GIF`, `JFIF`/`image/jpeg` — lossless with alpha
first), **image file** (image files of `CF_HDROP`, or the text of one copied local path, setting
`[paste] paths`, on), **colour** (`#RGB`, `#RGBA`, `#RRGGBB`, `#RRGGBBAA`, `rgb()`/`rgba()`, three
integers 0–255, three numbers 0–1), **HTML**, **text**, and **file names** (non-image files as a
card, setting `[paste] file_cards`, off). A browser's "Copy image" offers a bitmap and HTML with an
`<img>`: the image wins. An editor's copy offers text and HTML: the HTML card wins, `--as text` or
the menu's "Pin clipboard as: Plain text" gives the text. The pin keeps only the *names* of the
kinds that were offered; choosing another kind reads the clipboard again, once, on that request.
Keeping the alternatives' content would store clipboard content beyond the pin the user asked for.

### 3. Pure-Rust decoders with explicit limits

[`decode`](../../crates/noeul/src/decode.rs) uses the `image` crate (already in the workspace lock
through eframe, with PNG only) with exactly the formats Noeul pins: `png`, `jpeg` (zune-jpeg),
`gif`, `bmp`, `ico`, `tga` (built into `image`) and `tiff`; no default features (no rayon, AVIF,
EXR or WebP). The format is recognised by its signature, never by a name alone; TGA, which has
none, only for a `.tga` file or the TGA 2.0 footer. Limits are checked before a large allocation:
128 MiB of encoded data, 32767 pixels a side, 40 million pixels (an 8K screenshot fits),
`image::Limits::max_alloc` 320 MB — a small file claiming a huge picture is refused from its header.
A decoder that panics on hostile data is caught (`catch_unwind`; release builds unwind) and
reported as `image_invalid`, so a malformed picture never takes the instance down and nothing
partial is kept. Alpha is kept (straight BGRA). An embedded **ICC profile is converted to sRGB**
with `moxcms` (the colour engine `image` already depends on), because pins are shown and copied as
sRGB; EXIF orientation is applied. No other metadata is kept, and outputs hold IHDR, IDAT and IEND
only (ADR 0041), so no metadata of a pasted file reaches an export. **Multi-image data** is counted
from its structure: GIF frames, TIFF pages, ICO sizes; the pin shows the first frame or page (the
largest icon) and says so (a GIF keeps the `animation` marker of ADR 0044). **Animated playback is
not implemented**: a proper player needs a timer per pin, pause, frame stepping and speed
(NOEUL-FR-068, P2) and does not fit this Step; the honest notice stays.

New crates in the lock, each published more than two weeks before this Step, permissive and
accepted by `cargo deny`:

| Crate | Version (published) | License | Why |
| --- | --- | --- | --- |
| `gif` | 0.14.2 (2026-04-09) | MIT OR Apache-2.0 | GIF decoding (image-rs) |
| `weezl` | 0.1.12 (2025-11-11) | MIT OR Apache-2.0 | LZW for GIF and TIFF |
| `color_quant` | 1.1.0 (2020-10-09) | MIT | Enabled with `image`'s `gif` feature (its encoder; unused by Noeul) |
| `zune-jpeg`, `zune-core` | 0.5.15 (2026-03-26), 0.5.3 (2026-08-07) | MIT OR Apache-2.0 OR Zlib | JPEG decoding, `image`'s JPEG backend |
| `tiff` | 0.11.3 (2026-02-10) | MIT | TIFF decoding (image-rs) |
| `fax` | 0.2.7 (2026-04-27) | MIT | CCITT fax compression inside TIFF (a `tiff` default) |
| `half`, `crunchy` | 2.7.1 (2025-10-14), 0.2.4 (2025-06-25) | MIT OR Apache-2.0, MIT | 16-bit float samples in TIFF |
| `quick-error` | 2.0.1 (2021-05-07) | MIT OR Apache-2.0 | `tiff`'s error type |

`moxcms` (0.8.1, BSD-3-Clause OR Apache-2.0) becomes a direct dependency without a new crate.

### 4. Cards: Noeul's own sanitizer, layout and GDI glyphs

A card is a picture made once at the paste, at the scale of the monitor it lands on, and pinned
like any picture (ADR 0029): it moves, zooms, turns, fades, copies and saves as an image, closes
and reopens. Its [`pins::Content`](../../crates/noeul/src/pins.rs) adds the card's kind, its text
for `pin.copy-text` (`Ctrl+Shift+C`; the full text, also beyond what a long card shows) and the
kinds offered. `pin.list` reports the kind only.

- **HTML** ([`html`](../../crates/noeul/src/html.rs)): the CF_HTML fragment through Noeul's own
  one-pass tokenizer; a whitelist keeps text and basic formatting (blocks, headings, lists,
  quotes, preformatted text, rules, table rows, bold, italic, underline, strikethrough,
  monospace, highlights, links as underlined text without their targets) and, from `style`,
  only `color`, `background-color` (a plain colour), `font-weight`, `font-style`,
  `text-decoration`, a monospace `font-family` and `white-space: pre` — enough for an editor's
  syntax highlighting. Scripts, styles, frames, objects, forms, SVG, comments and processing
  instructions are dropped with their content; **every image is a placeholder** and no URL is
  kept, so nothing is ever requested; bidi overrides are removed; nesting, characters and blocks
  are bounded.
- **Layout and pixels** ([`card`](../../crates/noeul/src/card.rs)): Noeul's own line breaking (at
  most 560 logical pixels of content, words wrapped, overlong words broken, 240 lines and 20 000
  characters, then a note) over a `TextEngine`; on Windows `win::card::GdiText` draws each run's
  coverage with GDI and per-character font fallback (Segoe UI or Consolas, Malgun Gothic for
  Hangul, … as the annotation labels do), and the card blends it in the run's colour. **No colour
  can hide text**: below 3:1 contrast with what is behind it, the card's text colour is used, so
  `color: transparent`, white on white and similar tricks show the text. The card is on the Kim
  surface (`bg.overlay`, `text.primary`, links in `accent`) unless the HTML brings its own
  background (an editor's theme).
- **Colours**: a 112-logical-pixel swatch (on a checkerboard when translucent) above its HEX and
  `rgb()` values. **Text**: whitespace kept, monospace when it looks like code or a table.
  **File names**: names only, never folders.

### 5. Files: local only

`noeul pin <file>` (the CLI makes the path absolute), a copied image file or path, and image files
dropped on a pin (`WM_DROPFILES` on every pin window, pinned at the drop point; at most 16 files of
one drop or paste, cascading 24 logical pixels apart) are read by
`win::files::read_local_image`: only a drive-letter path on a local drive — never a network path or
mapped network drive, never a cloud placeholder whose data is not on this PC (reading it would make
the sync client download it), never a link or junction, never a folder — at most 128 MiB.

### 6. Privacy and the command line

Notices, logs, `--json` and `Debug` output name kinds, pin IDs and error codes, never clipboard
content, file names or paths (NOEUL-SEC-008). A clipboard paste from the command line
(`noeul pin --clipboard`, `pin.create`) runs only in the running instance on the caller's own
unlocked interactive desktop (`desktop_unavailable` otherwise; the pipe already checks the
caller's identity, ADR 0023), never takes focus, and is announced by a visible notice of the
running Noeul (the default "notify" of NOEUL-SEC-010; the setting to allow silently or deny, and
`capture_not_allowed`, arrive with the privacy setting of Step 62). A dry run reads nothing.

## Alternatives considered

- **Windows Imaging Component** (WIC, through raw COM vtables): no crate, OS-maintained codecs and
  ICC, but C++ decoders on untrusted input inside Noeul's process, no TGA, and nothing for the
  Linux port; the pure-Rust decoders are memory-safe and portable. Rejected.
- **The format crates without `image`** (`gif`, `zune-jpeg`, `tiff` plus Noeul's own BMP, ICO and
  TGA): fewer features but more code of Noeul's own for formats `image` decodes well and already
  bounds with `Limits`. Rejected.
- **A browser engine or WebView2 for HTML**: scripts, network, a large runtime and a visible
  child window — against NOEUL-SEC-009 and NOEUL-NFR-008. **RichEdit with the clipboard's RTF**:
  RichEdit can host OLE objects and follow links; its layout is not ours to bound. Rejected.
- **Keeping every offered format for "Pin as…"**: no second read, but clipboard content kept
  beyond the pin the user asked for (NOEUL-SEC-002). Rejected.
- **Pinning private content masked**: of no use, and the secret is still in memory and copyable.
  Rejected.

## Consequences

- `F3` pins every source of the issue; the user can choose another kind from the pin's menu or the
  CLI; nothing reads the clipboard but a paste.
- A card's text is pixels at the paste monitor's scale: zooming a card scales those pixels like any
  picture (no re-layout).
- **Not in this build:** animated GIF playback (the honest notice stays), embedded (`data:`) images
  inside HTML (shown as placeholders like remote ones), tables as aligned grids (rows of cells),
  CSS beyond the whitelist, a notice without the tray icon (with the icon off, a paste that pins
  nothing is silent; PASTE-14), and dropping files on the settings window (N5).
- The test hook's windows no longer read the thread's key state (Step 55's first commit): a
  modifier held elsewhere while tests run cannot change what a test's posted messages mean.
- Verification: unit tests for every module (decoders with generated pictures of every format,
  bombs, truncation and noise; the format priority, colours, paths, private markers and bounds;
  the sanitizer against scripts, remote resources, CSS tricks and 100 000 nested elements; card
  layout and legibility; GDI glyphs in memory), instance-core tests with fake windows and an
  in-memory clipboard, real-binary tests over the stand-in clipboard (with a 127.0.0.1 listener
  that never sees a connection), the drop handler with a synthetic `HDROP`, and the e2e scenario
  `e2e/tests/step55_noeul_clipboard.rs`. The real clipboard with real applications (a browser,
  Excel, Explorer, Paint) is the manual script of [NOEUL_UX_SPEC §4.12](../NOEUL_UX_SPEC.md).
