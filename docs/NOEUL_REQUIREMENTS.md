# Noeul Requirements

Status: **current** (product definition; the app foundation is implemented since Step 49, [ADR 0023](adr/0023-noeul-app-foundation.md); the `F1` selection overlay since Step 50, [ADR 0026](adr/0026-noeul-capture-overlay.md), with its NOEUL-NFR-003 measurements; the pin engine since Step 53, [ADR 0029](adr/0029-noeul-pin-engine.md), with its NOEUL-NFR-001/002/004/007 measurements; the double-click-to-pin signature flow, the toolbar, copy, save, quick save and the gesture settings since Step 51, [ADR 0032](adr/0032-noeul-double-click-pin-and-outputs.md); window and UI element detection, the magnifier, the precision keys, snapping and the in-memory rectangle history since Step 52, [ADR 0035](adr/0035-noeul-smart-selection-and-precision.md), with the detection cost measured there; annotation — rectangle, ellipse, line, arrow, pen, marker and text with the input method and font fallback, undo and redo, editable objects, crop and the size readout, in the overlay and on pins — since Step 56, [ADR 0038](adr/0038-noeul-annotation.md), without polyline and colour opacity; the privacy tools — mosaic, blur, the eraser and an irreversible solid redaction — since Step 57, [ADR 0041](adr/0041-noeul-privacy-tools.md), with their CPU cost measured there; the pin transforms — rotation, flips, opacity, thumbnails, border resizing, precise placement, the outline and shadow — click-through with its ways back, groups and an honest first-frame notice for animated GIFs since Step 54, [ADR 0044](adr/0044-noeul-pin-transforms-and-control.md), with the hidden-pin cost and the transform latency measured there; GIF playback (NOEUL-FR-068) is not implemented) · Owner: mouv ·
Step 48 · Derived from [NOEUL_PHILOSOPHY](NOEUL_PHILOSOPHY.md) and [KIM_PHILOSOPHY](KIM_PHILOSOPHY.md) ·
Parity: [NOEUL_SNIPASTE_MATRIX](NOEUL_SNIPASTE_MATRIX.md) · UX: [NOEUL_UX_SPEC](NOEUL_UX_SPEC.md) ·
Decision: [ADR 0021](adr/0021-noeul-product-definition.md)

## 0. How to read this document

- **IDs** follow [PRODUCT_REQUIREMENTS §0](PRODUCT_REQUIREMENTS.md): `NOEUL-FR` functional,
  `NOEUL-NFR` non-functional, `NOEUL-SEC` security, privacy and safety. IDs are permanent, never
  renumbered or reused; a dropped requirement is marked *withdrawn*. Gaps in the numbering are
  room for later requirements of the same area. `cargo xtask docs-check` validates every row and
  every reference.
- **Priority:** `P0` blocks its milestone · `P1` should ship in its milestone · `P2` tracked
  aspiration, not promised.
- **Milestone:** Noeul's own milestones `N1`…`N7` (§1). They are independent of Kim's `M1`…`M4`,
  except that `N7` (Linux) never comes before Kim's M4 rules allow a Linux claim (`KIM-NFR-011`).
- **Verify:** `T` automated test · `M` manual check on real Windows · `I` inspection/review ·
  `D` documentation exists and is accurate.
- **Inherited requirements.** Noeul is a Kim tool. Every `KIM-*` requirement that applies to
  tools applies to Noeul unchanged (§8); this document only adds and refines.
- **Windows behavior** cited from Microsoft documentation is marked *documented*. Step 48 ran
  nothing against Windows; each Step that implements a requirement verifies it.

## 1. Roadmap and milestones

Noeul is built in Steps 49–64 (GitHub issues #54–#69), in Step order. The first public release
is N6; nothing is offered for download before it (`KIM-FR-051`).

| Milestone | Steps | Goal | Done when |
| --- | --- | --- | --- |
| **N1 – Capture and pin core** | 49, 50, 51, 52, 53 | App skeleton, single instance, optional tray, CLI skeleton; `F1` overlay with region selection; **double-click to pin**; copy and save; window detection, magnifier and pixel precision; the pin window engine | All `P0` N1 rows pass, including the real-Windows signature-flow script (UX_SPEC §4.4) and the N1 budgets |
| **N2 – Pin control and paste** | 54, 55 | Pin transforms, click-through with recovery, thumbnails, groups, hide/show; `F3` paste of images, text, HTML, colors and files | All `P0` N2 rows pass |
| **N3 – Annotation and color** | 56, 57, 59 | Shapes, text with IME, undo/redo, editable objects, crop; mosaic, blur, eraser, true redaction; color picker and color cards | All `P0` N3 rows pass |
| **N4 – Displays, history and recovery** | 58, 60 | Mixed-DPI and changing monitor layouts, window/monitor capture, pointer capture, HDR limits; capture history; opt-in pin backup and restore | All `P0` N4 rows pass on the recorded display matrix |
| **N5 – Preferences and automation** | 61, 62 | Settings window, shortcut list and keyboard-only binding editor, gesture settings, accessibility; full CLI, optional MCP adapter, Kim integration | All `P0` N5 rows pass; Noeul conforms to the hotkey contract (KIM-FR-041…046) |
| **N6 – Windows release** | 63 | Performance and security hardening, threat model, parity evidence, signed release in the Kim catalog | All `P0` N1–N6 rows pass with evidence; the parity matrix is re-audited |
| **N7 – Linux** | 64 | X11 and Wayland (portal) backends, packaging | Linux runtime tests pass on real Linux; nothing is claimed before |

Rule for every Step: **only implemented actions exist.** A build registers hotkeys and lists
actions only for what it implements, and labels anything else as planned [NOEUL-FR-114].

## 2. Platforms and permissions

| Platform | Status |
| --- | --- |
| Windows 11, x86_64 | **Primary** (N1–N6) |
| Windows 10 version 1809 or later, x86_64 | **Primary** (N1–N6) |
| Windows on ARM64 | Not tested, not claimed |
| Linux x86_64 (X11, Wayland via desktop portals) | N7 only; before that, compile checks with `unsupported_platform` stubs |
| macOS | Out of scope |

What Noeul needs from Windows, and what it never asks for:

| Capability | Mechanism (Windows) | Consent or limit |
| --- | --- | --- |
| Two global shortcuts | `RegisterHotKey`, only while Noeul runs and they are enabled | First come, first served; a taken key is reported, never stolen (`KIM-SEC-020`) |
| Read the screen | A capture API of the user's own desktop, chosen in Step 49 and refined in Step 58 (GDI, DXGI Desktop Duplication or Windows.Graphics.Capture), at the moment of an explicit capture | A desktop app needs no permission to read the user's own desktop through GDI or DXGI on Windows 10 (*documented*); Step 49 checks which privacy controls current Windows 11 builds apply to each capture API. Noeul's own rule stands in for the missing prompt: capture only on an explicit action (NOEUL-SEC-001). Content excluded by display affinity or DRM stays excluded (NOEUL-SEC-004). |
| Window and element geometry | `WindowFromPoint`/window rectangles; UI Automation `BoundingRectangle` | Geometry only (NOEUL-SEC-007); elevated windows refuse UI Automation from a normal process (UIPI, *documented*), so detection falls back to the window rectangle |
| Clipboard | Open, read or write, close, only on paste or copy | No clipboard listener (NOEUL-SEC-002) |
| Pointer position in the overlay | `SetCursorPos` while Noeul's overlay is the foreground window | Never `SendInput`, never input to other programs (NOEUL-SEC-012) |
| Files | Per-user folders (`%APPDATA%`, `%LOCALAPPDATA%`, the user's chosen save folder) | No writes elsewhere, no admin rights |
| Start at sign-in | Per-user `Run` entry, only after the user turns it on | Catalog permission `autostart` (opt-in) |
| Never | Administrator rights, a service, a driver, `UIAccess`, a keyboard or mouse hook, code injection into other processes, network access | (NOEUL-SEC-003, NOEUL-SEC-012, NOEUL-NFR-008) |

## 3. Functional requirements

### 3.1 Application, lifecycle and Kim integration

| ID | Requirement | Pri | MS | Verify |
| --- | --- | --- | --- | --- |
| NOEUL-FR-001 | Noeul is its own package and binary (`noeul.exe`), separately versioned and installable through Kim or by hand. It needs neither Kim, Kim Manager nor Subak installed or running, and shares no process with them. | P0 | N1 | T, I |
| NOEUL-FR-002 | Noeul runs as one per-user process only while the user keeps it running: started by the user (Start menu, `noeul`, Kim Manager), and at sign-in only after the user turned that on. Quitting (tray menu, `noeul quit`) ends it completely: its global hotkeys are released and its pins close (after an opt-in backup, NOEUL-FR-066). With open pins and backup off, the GUI asks once and `noeul quit` needs `--yes`. No service, helper process or scheduled task exists. | P0 | N1 | T, M |
| NOEUL-FR-003 | Single instance per user session. A second start hands its request to the running instance over a same-user local IPC endpoint and exits; a plain second start opens the settings window. Two instances never register the same hotkeys. | P0 | N1 | T |
| NOEUL-FR-004 | The notification-area icon is optional (on by default). Left-click starts a capture, middle-click pins the clipboard, right-click opens the menu: Capture, Pin clipboard, Show or hide all pins, Make all pins clickable, Bring pins on screen, Reopen closed pin, Settings, Keyboard shortcuts on or off, Quit. Items appear only once implemented. With the icon off, hotkeys, a second start (settings) and the CLI still reach everything. | P1 | N1 | M |
| NOEUL-FR-005 | Kim integration: the package ships `capabilities.json` and `hotkeys.manifest.json` (CLI_CONTRACT §9, HOTKEY_CONTRACT §3.4); `noeul capabilities --json` describes every command; the catalog's shortcut summary is updated in the same change as the manifest's global and app actions; the catalog's documentation links point at these documents from the next catalog version. | P1 | N5 | T, D |

### 3.2 Capture and selection

| ID | Requirement | Pri | MS | Verify |
| --- | --- | --- | --- | --- |
| NOEUL-FR-010 | The action `capture.region` (global default `F1`) freezes the current picture of every monitor and shows a selection overlay over the whole virtual desktop, above all windows, on the interactive desktop only. The frozen picture, not the live screen, is what gets pinned, copied or saved. | P0 | N1 | T, M |
| NOEUL-FR-011 | Rectangle selection by press, drag and release, with the outside dimmed, a one-device-pixel border, and a live readout of position and size in device pixels. A drag starts only after the pointer moved beyond the system drag threshold. Holding Shift while dragging makes a square. | P0 | N1 | T, M |
| NOEUL-FR-012 | A confirmed selection is moved by dragging inside it and resized with eight handles. `Esc` cancels the whole capture without copying, saving or pinning. Right-click outside a confirmed selection clears it; right-click with no selection cancels. | P0 | N1 | T, M |
| NOEUL-FR-013 | One overlay at a time: the capture action while an overlay is open does nothing, and rapid repeated presses never open a second overlay. The overlay closes when its capture is pinned, copied, saved or cancelled, and when another program's window becomes active (setting, default on). | P0 | N1 | T |
| NOEUL-FR-014 | In the overlay, `selection.full` (`Ctrl+A`) selects the monitor under the pointer and, pressed again, all monitors; `selection.last` (`R`) restores the rectangle of the last successful capture. | P1 | N1 | T |
| NOEUL-FR-015 | Delayed capture and a custom capture (position, size and delay entered by the user) from the tray, the settings window and the CLI. | P2 | N5 | T |
| NOEUL-FR-016 | Optional capture of the mouse pointer into the frozen picture, off by default, toggled in the overlay. | P2 | N4 | T, M |
| NOEUL-FR-017 | Capture of the active window or of one monitor without a selection overlay, from the tray menu and the CLI (`noeul capture --window active`, `--monitor <n>`). | P1 | N4 | T, M |

### 3.3 The signature flow: double-click to pin

This is Noeul's deliberate deviation from Snipaste (NOEUL_PHILOSOPHY §2, ADR 0021). The precise
rules, edge cases and tests are in [NOEUL_UX_SPEC §4](NOEUL_UX_SPEC.md#4-the-signature-flow-precise-rules).

| ID | Requirement | Pri | MS | Verify |
| --- | --- | --- | --- | --- |
| NOEUL-FR-020 | **Signature flow.** After `capture.region`, a double-click inside the confirmed selection immediately creates a floating pin of exactly that rectangle, at the same screen position and pixel size, and closes the overlay. No dialog, no extra key, and no clipboard or file write. Snipaste's default double-click copies to the clipboard; Noeul's default pins. | P0 | N1 | T, M |
| NOEUL-FR-021 | A double-click counts only when both presses are left-button presses in the selection's interior (not on a resize handle, the border band or the toolbar), within the system double-click time and double-click rectangle, with no drag between them, with no modifier key held, and while no annotation tool is active. A press that started a drag, move or resize never counts as the first click. Anything else pins nothing. | P0 | N1 | T |
| NOEUL-FR-022 | The same pin is reachable without a double-click: `capture.pin` (`Enter`) while the selection has focus, the toolbar's first and primary button **Pin**, the selection's context menu, and a middle-click inside the selection. | P0 | N1 | T, M |
| NOEUL-FR-023 | Copy and save are separate, discoverable actions that never pin: `capture.copy` (`Ctrl+C`) and the Copy button copy and close; `capture.save` (`Ctrl+S`) opens a save dialog; `capture.quick-save` (`Ctrl+Shift+S`) saves to the quick-save folder. Tooltips, the context menu and the shortcut list show every key and gesture. | P0 | N1 | T, M |
| NOEUL-FR-024 | The gesture is a setting: the double-click inside a selection does `pin` (default), `copy`, `save` or `none`. An opt-in "Snipaste-style gestures" preset (double-click and `Enter` copy, `Ctrl+T` and middle-click pin) is shown by name in settings and in the shortcut list, and is undone by reset. Defaults never change silently. | P1 | N5 | T |
| NOEUL-FR-025 | Pinning never depends on the clipboard. If a pin cannot be created (memory, window creation), the overlay and the selection stay open with an error so the user can copy or save instead; nothing is lost silently. | P0 | N1 | T |
| NOEUL-FR-026 | A double-click on a pin has no default action; in particular it never closes the pin, so the remaining clicks of the gesture that created a pin cannot destroy it. | P0 | N1 | T |

### 3.4 Output and clipboard writes

| ID | Requirement | Pri | MS | Verify |
| --- | --- | --- | --- | --- |
| NOEUL-FR-030 | Copy places the image on the clipboard as PNG (the registered `PNG` format, alpha kept) and as a device-independent bitmap (`CF_DIBV5`), so browsers, office suites, chat programs and image editors accept it. The clipboard is opened only for the write and closed at once; a clipboard held by another program is retried briefly and then reported. | P0 | N1 | T, M |
| NOEUL-FR-031 | Save writes PNG by default (JPEG and BMP selectable, JPEG quality setting). The suggested name is `noeul-YYYYMMDD-HHMMSS.png` and never contains window titles or program names. Fully opaque images are saved without an alpha channel. | P0 | N1 | T |
| NOEUL-FR-032 | Quick save writes to a user-chosen folder (default `Pictures\Noeul`) without a dialog and never overwrites an existing file. | P1 | N1 | T |
| NOEUL-FR-033 | Copy as file (opt-in): the copied image can also be pasted into Explorer as a file. The file lives in Noeul's user-only data folder and is deleted when Noeul quits. | P2 | N5 | T |
| NOEUL-FR-034 | Noeul's clipboard writes carry the Windows flag that keeps them out of cloud clipboard sync by default (`CanUploadToCloudClipboard`, *documented*), with a setting to allow it. Whether they appear in the local clipboard history stays the user's Windows setting. | P1 | N2 | T, M |

### 3.5 Detection, precision and color

| ID | Requirement | Pri | MS | Verify |
| --- | --- | --- | --- | --- |
| NOEUL-FR-040 | Auto-detection: while hovering, the window under the pointer (or, after `detect.cycle` (`E`), the UI element; again, off) is highlighted, and a click without drag selects it. Detection asks only for rectangles and falls back to plain region selection when a window or element cannot be queried (elevated, protected, unresponsive or browser content without accessibility support). | P1 | N1 | T, M |
| NOEUL-FR-041 | A magnifier near the pointer shows enlarged pixels with a center crosshair, the device-pixel position and the pixel's color. It appears while selecting and is toggled with `magnifier.toggle` (`M`). | P1 | N1 | T, M |
| NOEUL-FR-042 | Pixel-precise keyboard control: arrows move the selection by one device pixel, `Ctrl`+arrows grow and `Shift`+arrows shrink the matching edge, and the physical keys `W A S D` move the pointer by one pixel inside the overlay. Holding a key repeats, with acceleration. | P1 | N1 | T |
| NOEUL-FR-043 | Keyboard-only selection: `selection.mark` (`Space`) sets the first corner at the pointer and then the opposite corner, or accepts the highlighted detection, so a capture can be made, pinned, copied or saved without a mouse. | P0 | N1 | T, M |
| NOEUL-FR-044 | Color picker: `color.copy` (`C`) copies the color under the crosshair as text in the current format, and `color.format-next` (`Shift+C`) switches between HEX, RGB and HSL (uppercase or lowercase HEX is a setting). Values are those of the captured sRGB picture. | P1 | N3 | T, M |
| NOEUL-FR-046 | Selection refinements: fixed aspect ratio, a size typed during selection, device or logical pixel units, the mouse wheel to step to a parent or child element, a list of programs excluded from detection, and moving the selection while dragging. | P2 | N4 | T |

### 3.6 History

| ID | Requirement | Pri | MS | Verify |
| --- | --- | --- | --- | --- |
| NOEUL-FR-050 | Capture history in memory: successful captures (pinned, copied or saved) are kept, bounded by count (default 20) and total size. In the overlay `history.previous` and `history.next` (`Comma`, `Period`) step through earlier rectangles over the current picture; earlier images can be pinned again from the tray and the CLI. | P1 | N4 | T |
| NOEUL-FR-051 | History on disk, and automatic saving of every capture, are off by default. When the user turns them on, images go to Noeul's user-only data folder (or the chosen folder for auto-save) with limits by count and age, a Clear command, and an exclusion for captures the user marks private. Cancelled captures are never kept unless the user opts in. | P1 | N4 | T |

### 3.7 Pins

| ID | Requirement | Pri | MS | Verify |
| --- | --- | --- | --- | --- |
| NOEUL-FR-060 | Pins are independent, borderless, always-on-top windows with stable IDs, created from a capture, the clipboard or a file; many can exist at once. A pin takes keyboard focus only when the user creates or clicks it, never sends input to another program, and does not appear in Alt+Tab or on the taskbar. Closing the overlay never closes pins. | P0 | N1 | T, M |
| NOEUL-FR-061 | Pin basics: drag to move; arrow keys move by one pixel; the mouse wheel and `Plus`/`Minus` zoom around the pointer; dragging the border resizes; `pin.zoom-reset` (`0`) and middle-click return to 100 % and full opacity; copy (`Ctrl+C`), save (`Ctrl+S`), quick save, close (`Esc`, `Ctrl+W`); a context menu lists every pin action. 100 % means one image pixel per device pixel on any monitor. | P0 | N1 | T, M |
| NOEUL-FR-062 | Pin transforms, each reversible and per pin: rotate by 90° (`1`, `2`), flip (`3`, `4`), opacity (`Ctrl`+wheel, `Ctrl+Plus`/`Ctrl+Minus`, never below 10 %), thumbnail mode (`T`, Shift+double-click), hide (`H`). | P1 | N2 | T, M |
| NOEUL-FR-063 | Click-through: `pin.click-through` (`Ctrl+Shift+T`) makes a pin ignore the mouse and shows a small marker on it. Noeul enables it only while a recovery path exists: the tray item "Make all pins clickable", the pin list in the settings window (reached by starting `noeul` again), `noeul pin set --all --click-through off`, and an optional global action. | P1 | N2 | T, M |
| NOEUL-FR-064 | Groups: every pin belongs to a named group; a group can be switched to, shown, hidden or closed at once; all pins can be hidden and shown together. Closing several pins at once asks for confirmation. | P1 | N2 | T |
| NOEUL-FR-065 | Close and destroy: a closed pin can be reopened from memory (default: the last one; configurable) through "Reopen closed pin" in the tray and the CLI; `pin.destroy` (`Shift+Esc`) removes a pin for good. The paste action never reopens a closed pin. | P1 | N1 | T |
| NOEUL-FR-066 | Pin backup and restore, opt-in: pins (image, position, zoom, rotation, opacity, group, annotations) are written atomically to the user-only data folder and offered for restore at the next start. Restore never duplicates pins that are open, never captures anything, and skips and reports damaged entries. | P1 | N4 | T, M |
| NOEUL-FR-067 | Pins and display changes: pins keep their place when monitors change, even off-screen, and "Bring pins on screen" (tray, CLI) moves every off-screen pin onto a visible monitor. | P1 | N4 | T, M |
| NOEUL-FR-068 | Animated GIF pins play their frames, can be paused and stepped frame by frame, and have a speed setting. Where animation is not supported, the pin says it shows only the first frame. | P2 | N2 | T |
| NOEUL-FR-069 | Pin extras: snapping to screen edges and other pins while dragging with Shift, grayscale and color inversion, solo mode, multi-selection, turning off always-on-top per pin, background modes for transparent images, free-size thumbnails, smooth or pixelated scaling, replacing a pin's content from the clipboard, and pins on all virtual desktops. | P2 | N4 | T |

### 3.8 Paste (`F3`)

| ID | Requirement | Pri | MS | Verify |
| --- | --- | --- | --- | --- |
| NOEUL-FR-070 | The action `paste.float` (global default `F3`) reads the clipboard once, at that moment, and pins an image it finds (PNG, `CF_DIBV5`, `CF_DIB`, or an image file copied in Explorer) at the pointer, at 100 %. | P0 | N2 | T, M |
| NOEUL-FR-071 | Rich sources become cards that behave like image pins: plain text and HTML (rendered by Noeul's own sanitizer, NOEUL-SEC-009) as text cards with `pin.copy-text` (`Ctrl+Shift+C`) to copy the text back; color values (`#RGB`, `#RRGGBB`, `rgb(…)`, three integers 0–255 or three numbers 0–1) as color swatches; copied non-image files as a card listing their names (setting: off). With several formats, the order is image, image file, color, HTML, text, and a "Pin as…" menu lets the user choose another. | P1 | N2 | T |
| NOEUL-FR-072 | Image decoding for pins: PNG, JPEG, BMP, GIF, ICO, TIFF and TGA (WebP optional), alpha preserved, with limits on bytes and pixels; malformed or oversized data is rejected with a message and never crashes Noeul or leaves a broken pin. | P1 | N2 | T |
| NOEUL-FR-073 | Dropping image files on a pin or on the settings window pins them; `noeul pin <file>` does the same from the CLI. | P1 | N2 | T |
| NOEUL-FR-074 | An empty clipboard or one without supported content shows a short non-modal notice ("Nothing to pin") and changes nothing; a clipboard another program keeps open is retried for at most 500 ms, then reported. | P0 | N2 | T |

### 3.9 Annotation

| ID | Requirement | Pri | MS | Verify |
| --- | --- | --- | --- | --- |
| NOEUL-FR-080 | Annotation in the overlay before any output, and on any pin (`pin.annotate`, `Space`): rectangle, ellipse, line and polyline, arrow, pen, marker, and text with IME input (Korean first) and font fallback; color with opacity, width (`[`, `]`, wheel), Shift to constrain shapes. Tool actions are bindable and unbound by default. | P1 | N3 | T, M |
| NOEUL-FR-081 | Every edit can be undone and redone (`Ctrl+Z`; `Ctrl+Y` or `Ctrl+Shift+Z`). Annotations stay editable objects (select, move, resize, reorder, delete) until the image is copied, saved or flattened. Right-click finishes the current shape, `Esc` leaves the current tool, and cancelling the capture leaves nothing behind. "Clear all" is an ordinary undoable edit. | P1 | N3 | T |
| NOEUL-FR-082 | Privacy tools: mosaic, blur, eraser and a solid redaction. Exported pixels under a solid redaction contain nothing of the original, no hidden layer or metadata keeps it, and the UI says that blur and mosaic are weak protection. | P1 | N3 | T |
| NOEUL-FR-083 | Crop on pins and a size readout for measuring. | P1 | N3 | T, M |
| NOEUL-FR-084 | Annotation extras: numbered step markers, rounded rectangles, dashed lines, double-headed and styled arrows, filled or outlined text, a magnifier shape, rotation of shapes, several pen sizes, per-tool colors and a shareable palette, and marking the detected element under the pointer with a right-click. | P2 | N4 | T |

### 3.10 Displays and color

| ID | Requirement | Pri | MS | Verify |
| --- | --- | --- | --- | --- |
| NOEUL-FR-090 | Noeul is per-monitor DPI aware (v2). The overlay covers the whole virtual desktop, including monitors at negative coordinates. On one monitor at any scale, and on several monitors with the same scale, selections, readouts, pins and saved images are exact in device pixels. | P0 | N1 | T, M |
| NOEUL-FR-091 | Mixed and changing layouts: monitors with different scales, rotations and resolutions, and monitors attached, removed or rearranged while Noeul runs. Each monitor is captured at its native resolution, the toolbar keeps its visual size on every monitor, and pins keep their pixel size when moved between monitors. | P1 | N4 | T, M |
| NOEUL-FR-092 | HDR and color management: on HDR or color-managed displays the captured picture is converted to SDR sRGB, the color picker reports that picture's values, and Noeul states that they can differ from what the display shows. | P2 | N4 | T, M, D |

### 3.11 Settings, shortcuts and accessibility

| ID | Requirement | Pri | MS | Verify |
| --- | --- | --- | --- | --- |
| NOEUL-FR-100 | Noeul conforms to hotkey contract v1 (backend `register`): the manifest of [UX_SPEC §6](NOEUL_UX_SPEC.md#6-default-shortcuts-and-gestures) is compiled in and shipped as `hotkeys.manifest.json`; `F1` and `F3` are its only global defaults; `hotkeys.json` is read with safe mode and reload; `--no-hotkeys` and `KIM_NO_HOTKEYS` start it with every shortcut off; a taken key is reported with remap offers and never retried in the background. | P0 | N1 | T |
| NOEUL-FR-101 | Settings window, compact and keyboard-operable: General (start at sign-in, tray icon, language), Capture (detection, magnifier, pointer capture, close on focus loss, quick-save folder, file format), Gestures (double-click and middle-click actions, the Snipaste-style preset), Pins (reopen memory, backup, default opacity), History (count, disk, clear), Privacy (CLI and agent captures, cloud clipboard), Shortcuts (HOTKEY_CONTRACT §11 list and keyboard-only binding editor), About (version, licenses). Each section can be reset. | P1 | N5 | T, M |
| NOEUL-FR-102 | Settings live in `%APPDATA%\mouv\noeul\config.toml` (versioned; unknown keys preserved; an invalid value falls back to its default with a message naming the key, never a crash) and shortcuts in `hotkeys.json` next to it; both can be exported and imported. | P1 | N1 | T |
| NOEUL-FR-103 | Themes: the Kim dark tokens by default, a light theme, and Windows contrast themes with system colors. | P1 | N5 | M |
| NOEUL-FR-104 | User interface text in English and Korean. | P2 | N5 | M |

### 3.12 CLI, headless and agents

| ID | Requirement | Pri | MS | Verify |
| --- | --- | --- | --- | --- |
| NOEUL-FR-110 | The CLI follows CLI contract v1: `noeul --help`, `--version`, `capabilities --json`, `status --json` (running or not, pins, hotkey registrations), human output and `--json`, documented exit codes and registered error codes (§7), `--dry-run` and `--yes` for changes. | P0 | N1 | T |
| NOEUL-FR-111 | Headless commands need no desktop and never open a window, tray icon or prompt, never register a hotkey and never start the GUI: `status`, `capabilities`, `hotkeys …`, `config …`, `history list`, `history clear`. | P0 | N1 | T |
| NOEUL-FR-112 | Desktop commands: `noeul capture --region <x,y,w,h>`, `--monitor <n>`, `--window active` with `--output <file>`, `--clipboard` or `--pin`; `noeul pin <file>`; `noeul pin list`, `hide`, `show`, `close`, `set`. A capture to a file or the clipboard works without a running instance and opens no window; pin commands need the running instance and fail with `noeul_not_running` unless `--start` is given. They run only on the caller's own unlocked interactive desktop and fail with `desktop_unavailable` otherwise. | P1 | N5 | T, M |
| NOEUL-FR-113 | Optional local MCP adapter (`noeul mcp`, stdio, started only by the user's MCP client) with an allowlist: `status`, `pin.list`, `pin.create` from a file, and capture to a file. Capture and clipboard tools are disabled until the user enables them in Noeul's settings. | P2 | N5 | T, D |
| NOEUL-FR-114 | Only implemented actions exist: each build lists and registers only the actions and commands it implements; anything planned is labeled as planned in `--help` and the docs. | P0 | N1 | T, I |

## 4. Non-functional requirements

Budgets are initial targets for a release build on a typical machine (the measuring machine and
OS build are recorded with every result). A budget is changed only by an ADR with measurements.

| ID | Requirement | Pri | MS | Verify |
| --- | --- | --- | --- | --- |
| NOEUL-NFR-001 | **Idle CPU.** No timers and no polling: while idle (tray icon, any number of static pins) Noeul blocks in its message loop; average CPU below 0.1 % of one core over 60 s, and no wake-ups of its own. | P0 | N1 | T, M |
| NOEUL-NFR-002 | **Idle memory.** Private working set at most 20 MB with no pin and no open window. After the overlay closes, memory returns to within 5 MB of that baseline within 10 s. A pin costs at most twice its pixel bytes plus 2 MB. | P1 | N1 | M |
| NOEUL-NFR-003 | **Capture latency.** From the `F1` press (`WM_HOTKEY` received) to the first overlay frame showing the frozen picture: p95 at most 150 ms on one 3840×2160 monitor and at most 250 ms on three monitors (target 100 ms). | P1 | N1 | M |
| NOEUL-NFR-004 | **Pin latency.** From the second press of the double-click (or `Enter`) to the visible pin: p95 at most 50 ms for a 1920×1080 selection and at most 100 ms for 3840×2160. | P1 | N1 | M |
| NOEUL-NFR-005 | **Paste latency.** From `F3` to the visible pin: p95 at most 150 ms for a 3840×2160 PNG and for a 10 KB text card. | P1 | N2 | M |
| NOEUL-NFR-006 | **Start, size and disk.** Launch to ready (hotkeys registered, tray shown) at most 300 ms; binary at most 12 MB; no disk writes while idle; logs capped at 10 MB. | P1 | N1 | M |
| NOEUL-NFR-007 | **Many pins.** Twenty 1920×1080 pins stay responsive (dragging at the display's refresh rate) within the memory rule of NOEUL-NFR-002; hidden pins and thumbnails render nothing while idle. | P1 | N2 | M |
| NOEUL-NFR-008 | **No network.** Noeul opens no network connection at all: no update check (updates are Kim's explicit job, `KIM-SEC-002`), no telemetry, no remote images, no online text recognition; HTML cards never fetch. | P0 | N1 | T, I |
| NOEUL-NFR-010 | **Platforms** as in §2: Windows 10 1809 or later and Windows 11 on x86_64. Windows on ARM64 and Linux are not claimed before they are tested. | P0 | N1 | M |
| NOEUL-NFR-011 | **Portability.** Screen capture, overlay and pin windows, clipboard, hotkey registration, session state and paths sit behind narrow interfaces (ADR 0011); non-Windows builds compile and report `unsupported_platform`. | P0 | N1 | I, T |
| NOEUL-NFR-012 | **Keyboard and motor accessibility.** Every action is reachable by keyboard and every pointer gesture has a keyboard alternative. Double-click time and area and the drag threshold come from the user's Windows settings, never from constants of Noeul's. | P0 | N1 | T, M |
| NOEUL-NFR-013 | **Assistive technology.** UI Automation names and roles for the overlay, toolbar, magnifier readout, pins and settings; visible focus; text contrast at least 4.5:1; reduced motion honored; "Pinned", "Copied" and "Saved" are announced. | P1 | N5 | T, M |
| NOEUL-NFR-014 | **IME.** The overlay and pins have no input-method context, so letter shortcuts work in Korean Hangul mode; the text tool enables the IME for typing and no shortcut fires during a composition. | P1 | N3 | T, M |
| NOEUL-NFR-015 | **Linux.** Capture through the desktop portal on Wayland (with its consent dialog) or X11, pins with whatever stacking the compositor allows, global shortcuts only where the portal or X11 offers them; every limitation is documented. | P2 | N7 | T, M |

How the budgets are measured (Steps 50, 51, 53, 55 and 63 record the results; Step 50's
first-frame and memory results are in [ADR 0026](adr/0026-noeul-capture-overlay.md#measurements),
Step 53's pin latency, memory per pin, twenty pins and idle cost with pins in
[ADR 0029](adr/0029-noeul-pin-engine.md#measurements), Step 51's pin latency from the second
press of the double-click in [ADR 0032](adr/0032-noeul-double-click-pin-and-outputs.md#measurements),
Step 52's window-list and element-lookup cost in
[ADR 0035](adr/0035-noeul-smart-selection-and-precision.md#measurements), Step 54's hidden-pin
idle cost (NOEUL-NFR-007: twenty hidden pins, 0.0 ms CPU and no message in 10 s), transform
latency and the pin latency with the outline and shadow in
[ADR 0044](adr/0044-noeul-pin-transforms-and-control.md#measurements)):

| Budget | Measured as | Method |
| --- | --- | --- |
| Idle CPU, wake-ups | CPU time delta over 60 s; context switches of Noeul's threads | `GetProcessTimes`; ETW or a thread-switch counter; tray only and with 20 pins |
| Idle memory | Private working set | `GetProcessMemoryInfo` after 60 s idle, and 10 s after an overlay closed |
| Capture latency | `WM_HOTKEY` timestamp → first `Present`/paint of the overlay | Monotonic clock inside Noeul (a debug metric, no content); p50/p95 over 50 captures |
| Pin and paste latency | Second button-down (or key) → pin window shown and painted | Same clock; 50 runs per size |
| Start | Process start → hotkeys registered | Same clock |

## 5. Security, privacy and safety

| ID | Requirement | Pri | MS | Verify |
| --- | --- | --- | --- | --- |
| NOEUL-SEC-001 | **NON-NEGOTIABLE.** Noeul captures the screen only after an explicit action for that capture: a hotkey, a tray click, a button, a CLI command, or an agent request the user allowed (NOEUL-SEC-010). There is no timed, background, scheduled or start-up capture, and restoring pins never captures. | P0 | N1 | T, I |
| NOEUL-SEC-002 | **NON-NEGOTIABLE.** Noeul reads the clipboard only when the user pastes into Noeul (`F3`, Pin clipboard, `noeul pin --clipboard`). It never monitors the clipboard (no listener, no polling) and never logs or stores clipboard content beyond the pin the user asked for. | P0 | N1 | T, I |
| NOEUL-SEC-003 | **NON-NEGOTIABLE.** The secure desktop, the lock screen, UAC prompts and sign-in are never captured or worked around: no service, SYSTEM component, `UIAccess` manifest, driver or elevation. While the session is locked or the input desktop is not the user's, Noeul's hotkeys do nothing, no overlay opens and CLI captures fail with `desktop_unavailable`. | P0 | N1 | T, I, M |
| NOEUL-SEC-004 | Protected content stays protected: windows that exclude themselves from capture (display affinity, *documented*) and DRM-protected video are captured exactly as Windows delivers them (black or absent). Noeul never changes another window's display affinity, injects code into another process or uses another capture path to get around it. | P0 | N1 | I, M |
| NOEUL-SEC-005 | Nothing on disk unless the user asked: captures, history and pins live in memory; Noeul writes images only when the user saves or quick-saves, or has turned on disk history, auto-save, pin backup or copy as file. There are no temporary image files otherwise. Noeul's data folders are user-only (ADR 0006). | P0 | N1 | T, I |
| NOEUL-SEC-006 | Saved and copied images carry no metadata that identifies the user, the machine or the captured program: no window titles, process names, paths, user names or software tags. | P1 | N1 | T |
| NOEUL-SEC-007 | Detection reads only geometry and control types (window rectangles, UI Automation bounding rectangles). It never reads element names, values or text, never touches password fields beyond their rectangle, and stores or logs nothing it saw. | P0 | N1 | I, T |
| NOEUL-SEC-008 | Logs, diagnostics and `--json` never contain pixels, clipboard text, window titles, image file paths or typed text; they name action IDs, pin IDs and error codes (`KIM-SEC-012`, `KIM-SEC-013`). | P0 | N1 | T, I |
| NOEUL-SEC-009 | Untrusted input is contained: clipboard HTML is rendered by Noeul's own sanitizer (text and basic formatting only; no scripts, no remote resources, no embedded browser); image decoders enforce size limits; malformed input fails without a crash and without a partial pin. | P0 | N2 | T |
| NOEUL-SEC-010 | Captures and clipboard reads requested through the CLI or IPC are never silent: by default each one shows a visible notice from the running Noeul (setting: notify, allow silently, or deny). Through MCP they are disabled until the user enables them. No prompt ever blocks a headless caller; a refused request fails with `capture_not_allowed`. | P0 | N5 | T, M |
| NOEUL-SEC-011 | Only the same user's processes can reach the running Noeul: a per-user named pipe with an owner-only security descriptor and a verified peer, as for Subak (ADR 0005, ADR 0019); no network listener. | P0 | N1 | T, I |
| NOEUL-SEC-012 | Input stays minimal: global shortcuts only through `RegisterHotKey` while Noeul runs (`KIM-SEC-020`…`KIM-SEC-022`); never a keyboard or mouse hook; never synthetic input to other programs; keyboard pointer movement uses `SetCursorPos` only while Noeul's overlay is the foreground window. | P0 | N1 | I, T |
| NOEUL-SEC-013 | No trapped UI: `Esc` closes the focused pin, click-through and off-screen pins always have a recovery path (NOEUL-FR-063, NOEUL-FR-067), and no pin can cover the screen without a way to close it. | P0 | N2 | T, M |

## 6. Deferred and not intended

Every Snipaste capability Noeul does not plan has a row in the parity matrix with one of these
reasons. Moving an item into scope needs a requirement row and, when it trades a principle, an
ADR.

| Ref | Capability | Decision | Reason |
| --- | --- | --- | --- |
| D-01 | Screen or GIF recording, scrolling capture | Not intended | Another product; would need continuous capture |
| D-02 | Text extraction (OCR) | Deferred (no milestone) | Online engines conflict with NOEUL-NFR-008; only a local engine could be considered later, opt-in |
| D-03 | QR and barcode decoding | Deferred (no milestone) | Not core; would be local only |
| D-04 | Upload, share sheet, "send to another program", file-dialog integrations | Not intended | Local-first; tools compose through files and the CLI (KIM_PHILOSOPHY §1) |
| D-05 | Images dragged or pasted from web pages that must be downloaded | Not intended | No network (NOEUL-NFR-008); images already on the clipboard are pinned normally |
| D-06 | Capture without a hotkey by holding `Win` and dragging | Not intended | `Win` chords are reserved for the OS and would need a hook (HOTKEY_CONTRACT §7.1, NOEUL-SEC-012) |
| D-07 | Hot corners | Not intended | Needs pointer polling or a hook (NOEUL-NFR-001, NOEUL-SEC-012) |
| D-08 | Ignoring global hotkeys while certain programs are in front | Not intended | `RegisterHotKey` already consumed the key; giving it to the program would need synthetic input. The user switches Noeul's global shortcuts off instead (HOTKEY_CONTRACT §12.4) |
| D-09 | Simple keys (letters, `Esc`) as global hotkeys | Not intended | Reserved typing and navigation keys (HOTKEY_CONTRACT §7.2) |
| D-10 | Running a command after a capture, or binding arbitrary commands to keys | Not intended | Shortcuts serve Noeul's own actions; scripts call the CLI (Ddalgak automates) |
| D-11 | "Paste as image" or "paste as plain text" into other programs | Not intended | Needs synthetic `Ctrl+V` into another program |
| D-12 | Whiteboard mode, screenshots with rounded corners, shadows or borders | Deferred (no milestone) | Not core |
| D-13 | Printing | Deferred (no milestone) | Users print saved files; may come later |
| D-14 | Accounts, licenses, activation, paid tiers | Not intended | `KIM-SEC-010`; Noeul is MIT OR Apache-2.0 |
| D-15 | Update checks inside Noeul | Not intended | Kim updates tools only on explicit request (`KIM-SEC-001`, `KIM-SEC-002`) |
| D-16 | Run as administrator, process priority settings | Not intended | `KIM-SEC-004`; a frugal tool needs no priority boost |
| D-17 | Reading or importing Snipaste's configuration | Not intended | IP boundary (NOEUL_PHILOSOPHY §10) |
| D-18 | 32-bit Windows, Windows XP–8.1, macOS | Not intended | Platform policy (§2) |

## 7. Planned CLI error codes

Proposed for the Steps that implement them (Steps 49 and 62); each is added to
[CLI_CONTRACT §6](CLI_CONTRACT.md) together with the code that returns it. **Registered in Step 49:**
`desktop_unavailable`, `noeul_not_running`, `pin_not_found`, `clipboard_unavailable`,
`nothing_to_pin` and `image_invalid`, plus the IPC codes `noeul_identity_mismatch`,
`noeul_unresponsive`, `noeul_incompatible` and `noeul_start_failed` (CLI_CONTRACT §6, Noeul table).
`capture_not_allowed` waits for the privacy setting of Step 62. **Registered in Step 51** with
`noeul config set|reset`: `config_invalid` (4: `config.toml` is invalid or cannot be edited in
place without changing other keys) and `config_from_newer_version` (5: written by a newer Noeul,
never rewritten) ([ADR 0032](adr/0032-noeul-double-click-pin-and-outputs.md) §4).
Noeul also uses the shared codes (`usage`, `io`, `permission_denied`, `internal`,
`confirmation_required`, `cancelled`, `unsupported_platform`) and the `hotkeys` codes.

| Code | Exit | Meaning |
| --- | --- | --- |
| `desktop_unavailable` | 6 | No unlocked interactive desktop for this user: locked, secure desktop, disconnected remote session, or no display. Retry after unlocking. |
| `noeul_not_running` | 3 | The command needs the running Noeul (pins), and none runs; start it or pass `--start`. |
| `pin_not_found` | 3 | No pin with this ID. |
| `capture_not_allowed` | 7 | The user's privacy setting refuses captures or clipboard reads from the CLI or an agent. |
| `clipboard_unavailable` | 6 | Another program kept the clipboard open. |
| `nothing_to_pin` | 3 | The clipboard or file holds nothing Noeul can pin. |
| `image_invalid` | 4 | The image is malformed or exceeds the size limits. |

## 8. Inherited Kim requirements

Noeul satisfies these platform requirements as a Kim tool; the rows above refine them.

| Area | Requirements |
| --- | --- |
| Independent tool, consent | `KIM-FR-001`, `KIM-FR-002`, `KIM-SEC-001`, `KIM-SEC-002`, `KIM-SEC-005`, `KIM-SEC-006` |
| CLI and headless | `KIM-FR-010`…`KIM-FR-013`, `KIM-FR-020`, `KIM-FR-021` |
| Frugality and platforms | `KIM-NFR-001`…`KIM-NFR-005`, `KIM-NFR-010`…`KIM-NFR-014` |
| Keyboard shortcuts | `KIM-FR-040`…`KIM-FR-046`, `KIM-NFR-020`…`KIM-NFR-022`, `KIM-SEC-020`…`KIM-SEC-023` |
| Privacy | `KIM-SEC-010`…`KIM-SEC-013` |
