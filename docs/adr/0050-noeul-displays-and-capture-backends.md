# ADR 0050 — Noeul displays and capture backends: GDI kept, monitor and window targets, debounced display changes, honest limits

Status: **Accepted** (Step 58; the physical multi-monitor, mixed-DPI, rotation and HDR checks are
pending the maintainer, [NOEUL_UX_SPEC §4.13](../NOEUL_UX_SPEC.md#413-step-58-displays-capture-targets-and-the-physical-test-matrix))
· Date: 2026-11-04 · Builds on [ADR 0023](0023-noeul-app-foundation.md) §3 (GDI on demand),
[ADR 0026](0026-noeul-capture-overlay.md) (the overlay over the virtual desktop),
[ADR 0029](0029-noeul-pin-engine.md) (pins keep their place after a display change),
[ADR 0035](0035-noeul-smart-selection-and-precision.md) (window facts, logical and device pixels),
[ADR 0044](0044-noeul-pin-transforms-and-control.md) (a pin's outline per monitor scale) ·
Requirements: NOEUL-FR-016, NOEUL-FR-017, NOEUL-FR-067, NOEUL-FR-090, NOEUL-FR-091,
NOEUL-FR-092, NOEUL-FR-093, NOEUL-FR-112, NOEUL-SEC-004, NOEUL-NFR-001

## Context

ADR 0023 chose GDI `BitBlt` for the first capture and left the capture API to be revisited in
Step 58 for HDR and mixed DPI. Steps 50–57 already made Noeul per-monitor DPI aware (v2), kept
every rectangle in device pixels of the virtual desktop (negative coordinates included), drew
Noeul's own UI in logical pixels of the monitor it is on, kept a pin's pixel size on every monitor
and kept pins in place, even off-screen, after `WM_DISPLAYCHANGE`, with "Bring pins on screen" as
the way back. What was missing: capturing a whole monitor or the active window without the
overlay, the mouse pointer in a capture, rotated monitors and gaps of the virtual desktop, what an
open overlay does when the monitors change, notification storms during a rearrangement, a pin's
outline after a scale change it is not dragged through, HDR and colour, and a decision on the
capture APIs with their limits stated honestly.

This Step ran on Windows 10 Pro 19045 with **one** 5120×1440 monitor at 100 %, advanced colour
off (read with `QueryDisplayConfig`). The maintainer uses the machine, so nothing here changed a
display setting, opened a visible window or read the visible screen: layouts are synthetic (a
debug-only layout file the running instance reads again on every display change), windows are
this Step's own and far off-screen, and the GDI path was measured with a memory DC as the source.

## Research: Windows capture APIs and their limits

**D** = *documented* by Microsoft (the pages below; read in the earlier Noeul research and cited
from it — this Step ran without network, so they were not re-read now). **M** = *measured* on this
machine in this Step (`cargo run --release -p noeul --example display_measure`, the tests named
below). Nothing is claimed beyond these.

| | GDI `BitBlt` from the screen DC (`CAPTUREBLT`) | DXGI Desktop Duplication | Windows.Graphics.Capture | `PrintWindow` / a window's own DC |
| --- | --- | --- | --- | --- |
| What it gives | Any rectangle of the virtual desktop as the user sees it, layered windows included (D) | One output (monitor) per duplication, as GPU textures, with dirty and move rectangles (D) | A monitor or one window, as a frame pool of GPU textures (D) | One window's own rendering, also when covered (D); the target window paints itself through `WM_PRINT` unless the full-content flag is used |
| Rights, prompts, side effects | None for a desktop app (D) | None for a desktop app (D) | A yellow border on Windows 10; borderless on Windows 11 only after an access request (D); cursor capture switchable from Windows 10 2004 (D) | None; a synchronous call into the target window's thread that can block on a hung program (D) |
| Idle cost | Nothing held between captures (M: Noeul keeps no DC or bitmap, ADR 0023) | A D3D device and one duplication per output kept alive to stay fast (D) | A D3D device, a frame pool and a WinRT session (D) | Nothing |
| Rotated monitors | GDI works in desktop coordinates, which are already turned, so the picture should come upright (expected; **not verified** on a turned monitor) | Frames come in the output's native orientation with the rotation reported; the app must turn them (D) | Not checked | n/a |
| Mixed DPI, several monitors | One call for any rectangle across monitors, in device pixels for a per-monitor-aware process (D); synthetic layouts tested (M) | One duplication per output; a rectangle across monitors needs several and a stitch; on hybrid-GPU laptops each must be made on the adapter that drives the output (D) | One item per monitor or window (D) | n/a |
| Mouse pointer | Not included; drawn by the caller (`GetCursorInfo` + `DrawIconEx`, D) | Delivered separately (shape and position, D) | Optional (D) | Not included |
| `WDA_MONITOR` window | Black (D) | Black, with a flag saying protected content was masked (D) | Black (D) | **Content** with `PrintWindow(…, 0)` and with `BitBlt` from the window's own DC; black with `PW_RENDERFULLCONTENT` (M, this Step's own off-screen window) |
| `WDA_EXCLUDEFROMCAPTURE` window (Windows 10 2004+) | Left out: what is behind it shows (D) | Left out (D) | Left out (D) | **Content** with `PrintWindow(…, 0)` and the window DC; black with `PW_RENDERFULLCONTENT` (M) |
| DRM video | Black (D) | Black (D) | Black (D) | Black or the app's choice |
| Locked or secure desktop | Fails: the input desktop is not the user's (D; Noeul checks first, ADR 0023) | `ACCESS_LOST`, then `ACCESS_DENIED` (D) | No session (D) | n/a |
| HDR / advanced colour | 8-bit SDR picture of what DWM composes in FP16 scRGB: SDR content keeps its values, brighter-than-SDR content is clipped (expected from the documented composition model; **not verified**: this machine's only monitor has advanced colour off, M) | 8-bit by default; FP16 scRGB frames with `DuplicateOutput1` (D) — would need Noeul's own tone mapping | FP16 frames possible (D) — same | n/a |
| ICC / colour management | The framebuffer's values, i.e. what applications drew: a colour-managed application has already converted its content to the monitor's profile, the others draw sRGB values; the calibration curve (VCGT) is applied by the GPU after the framebuffer and is never in a picture (D) | Same | Same | Same |
| Windows 10 1809 | Yes | Yes (Windows 8+) | Window and monitor items from interop since 1903 (D) | Yes; `PW_RENDERFULLCONTENT` (value 2) is in the SDK header from Windows 8.1, not on the function's page (D, header only) |

Sources (Microsoft Learn): `BitBlt` <https://learn.microsoft.com/windows/win32/api/wingdi/nf-wingdi-bitblt>;
`SetWindowDisplayAffinity` <https://learn.microsoft.com/windows/win32/api/winuser/nf-winuser-setwindowdisplayaffinity>
(it is "not a security feature": a camera still sees the screen);
Desktop Duplication <https://learn.microsoft.com/windows/win32/direct3ddxgi/desktop-dup-api>,
`DXGI_OUTDUPL_FRAME_INFO` (`ProtectedContentMaskedOut`)
<https://learn.microsoft.com/windows/win32/api/dxgi1_2/ns-dxgi1_2-dxgi_outdupl_frame_info>,
`IDXGIOutput5::DuplicateOutput1` <https://learn.microsoft.com/windows/win32/api/dxgi1_5/nf-dxgi1_5-idxgioutput5-duplicateoutput1>;
Windows.Graphics.Capture <https://learn.microsoft.com/windows/uwp/audio-video-camera/screen-capture>,
`GraphicsCaptureSession.IsBorderRequired` <https://learn.microsoft.com/uwp/api/windows.graphics.capture.graphicscapturesession.isborderrequired>;
`PrintWindow` <https://learn.microsoft.com/windows/win32/api/winuser/nf-winuser-printwindow>;
`GetCursorInfo` <https://learn.microsoft.com/windows/win32/api/winuser/nf-winuser-getcursorinfo>,
`DrawIconEx` <https://learn.microsoft.com/windows/win32/api/winuser/nf-winuser-drawiconex>;
High DPI <https://learn.microsoft.com/windows/win32/hidpi/high-dpi-desktop-application-development-on-windows>,
`WM_DPICHANGED` <https://learn.microsoft.com/windows/win32/hidpi/wm-dpichanged>,
`WM_DISPLAYCHANGE` <https://learn.microsoft.com/windows/win32/gdi/wm-displaychange>,
`SetTimer` (an existing timer with the same ID is replaced) <https://learn.microsoft.com/windows/win32/api/winuser/nf-winuser-settimer>;
`QueryDisplayConfig` <https://learn.microsoft.com/windows/win32/api/winuser/nf-winuser-querydisplayconfig>,
`DISPLAYCONFIG_GET_ADVANCED_COLOR_INFO` <https://learn.microsoft.com/windows/win32/api/wingdi/ns-wingdi-displayconfig_get_advanced_color_info>,
advanced colour and HDR <https://learn.microsoft.com/windows/win32/direct3darticles/high-dynamic-range>;
`DWMWA_EXTENDED_FRAME_BOUNDS` <https://learn.microsoft.com/windows/win32/api/dwmapi/ne-dwmapi-dwmwindowattribute>;
Windows Color System <https://learn.microsoft.com/windows/win32/wcs/windows-color-system>.

## Decision

### 1. GDI stays the only capture backend; no silent fallback to another API

Every capture — the `F1` freeze, a region, a monitor, the active window — is one `BitBlt` with
`CAPTUREBLT` from the screen DC into a DIB section, after the desktop check of ADR 0023. GDI is
kept because it holds nothing while idle (NOEUL-NFR-001/002), takes any rectangle across monitors
of different scales in one call in device pixels, delivers rotated monitors upright, shows no
border or prompt, and respects both display affinities exactly as Windows applies them
(NOEUL-SEC-004). There is **no fallback to another API**: the measured alternatives either get
around a window's protection (`PrintWindow` without the full-content flag, `BitBlt` from a
window's own DC), or add a border, a prompt or a device kept alive (Windows.Graphics.Capture,
DXGI). When GDI fails, the capture fails with its reason: `desktop_unavailable` for a locked,
secure, disconnected or display-less session (re-checked after a failed copy), `internal` with the
call's name otherwise. DXGI Desktop Duplication stays the candidate for an HDR backend behind the
same `CaptureBackend` trait; it would need FP16 frames, Noeul's own tone mapping and a stitch
across outputs, and would still not reproduce what an HDR display shows. Not done in this Step.

### 2. Capture targets: a region, a whole monitor, the active window

- **Monitor**: its rectangle, at its native resolution. `--monitor <n>` counts in the order
  `noeul status` lists the monitors — on Windows left to right, then top to bottom (the backend
  sorts them; `EnumDisplayMonitors` gives no order) — or `primary`, or `pointer` (the monitor under
  the pointer, else the nearest one). A monitor that is not attached is `monitor_not_found` (3).
- **Active window**: the foreground window's visible frame (`DWMWA_EXTENDED_FRAME_BOUNDS`, without
  the invisible resize border; else `GetWindowRect`), **cut from the screen as the user sees it**,
  including what lies over it, and clipped to the virtual desktop. Never `PrintWindow`: it sends a
  message into the other program, can block on a hung one and, measured, returns the content of a
  window that excludes itself from capture. Nothing but geometry and flags is read (no title,
  class or process name, NOEUL-SEC-007; the desktop's own shell window counts as "no window"). No
  window with focus, a minimized, hidden or cloaked one, or one off every monitor is
  `window_not_found` (3); a window that protects itself is captured as Windows delivers it and the
  result says so.
- **Every result** names the monitor that holds the rectangle's centre (number, rectangle, work
  area, DPI and scale, rotation, HDR), the rectangle in that monitor's **logical pixels** relative
  to its top-left corner (each edge rounded on its own, the `dpi` module's conversions), how many
  monitors it touches and whether their scales differ, and **honest warnings**: mixed scales (the
  picture keeps every monitor's device pixels; nothing is rescaled), HDR (converted to SDR by
  Windows), a protected window (black or left out), a window partly off the monitors (only the
  part on them), a pointer asked for and not drawn. The CLI shows them as warnings.
- **Pins**: a capture is pinned in place at 100 % (`--pin`, required in this build; `--output`
  and `--clipboard` stay Step 62's). From the CLI the pin takes no focus and a notice says a
  capture was made (NOEUL-SEC-010's default); from a key it takes focus, like the overlay's pin.
- **Keys**: `capture.monitor` (the monitor under the pointer) and `capture.window` (the active
  window), scopes `global` and `app`, **no default keys** (HOTKEY_CONTRACT §7.5: `F1` and `F3` stay
  the only global defaults); the catalog's shortcut summary lists them. A failure is a notice.
- **No tray items** for these: opening the tray menu makes the taskbar the active window, and the
  menu's fade-out would be in a monitor capture taken from it. The keys and the CLI are the ways.

### 3. The mouse pointer

On request (`--pointer`, IPC `pointer: true`) or with `[capture] pointer = "on"` (the `F1`
freeze and the two keys; off by default), the pointer is drawn into the capture's own bitmap right
after the copy: `GetCursorInfo` (image, position in device pixels, shown or not), `GetIconInfo`
(hotspot, size; its bitmaps freed at once), `DrawIconEx` with `DI_NORMAL` at the image's own size,
so a monochrome pointer inverts what is under it exactly as on screen and an animated one shows
its current frame (D). A hidden or suppressed pointer is not drawn and the result says so. Not
verified: whether Windows shows a pointer larger than its image on a scaled monitor or with the
accessibility pointer size; the physical matrix checks it. The overlay toggle of NOEUL-FR-016 is
not in this build.

### 4. Displays and their changes

- **Monitor facts**: rotation and advanced-colour state come from one
  `QueryDisplayConfig(QDC_ONLY_ACTIVE_PATHS)` and `DisplayConfigGetDeviceInfo` per path, matched to
  `MONITORINFOEXW.szDevice`; read-only, the device names never reported. Status and every capture
  result carry `rotation` and `hdr` (`null` when Windows did not say).
- **Gaps**: pixels of a picture that no monitor shows (an L-shaped or staggered virtual desktop)
  are made opaque black after every capture, so pins, copies and saves never carry what a driver
  left there.
- **Debounce without polling** (NOEUL-FR-093): `WM_DISPLAYCHANGE`, a work-area
  `WM_SETTINGCHANGE` and `WM_DPICHANGED` of the control window, a pin or the overlay are only
  noted; the pump (re)arms a **one-shot timer** of the control window (250 ms after the last
  notification, at most 1 s after the first); when it fires it is killed and the core looks at the
  displays once. An idle Noeul has no timer. `status.displays` counts notifications and handlings.
- **The core's reaction**: the backend re-reads what it keeps (nothing for GDI; the layout file
  for the synthetic desktop); pins follow where Windows put their windows, keep their picture's
  pixel size and place, and redraw their outline, shadow and resize band for their monitor's
  scale; pins no monitor shows stay put and a notice names the way back (unchanged, ADR 0029). An
  open **capture overlay** compares its monitors with the new ones: if one was attached, removed,
  moved, resized or turned, its frozen picture no longer matches and the capture is **cancelled
  with a notice** (nothing pinned, copied or kept); if only a scale, a work area, HDR or the
  primary changed, it keeps its selection and is drawn again for the new scales (handles, band,
  toolbar, magnifier, labels). A **pin editor** keeps going and follows the new scales. The
  overlay window no longer cancels itself on `WM_DISPLAYCHANGE` (Step 50); the core decides.

### 5. Colour

Noeul treats captured pixels as sRGB values and writes PNGs without an ICC, `sRGB`, `gAMA` or
`cHRM` chunk (viewers assume sRGB; NOEUL-SEC-006's chunk list is unchanged). It converts nothing
and labels nothing: the captured values are what applications drew (the content of a
colour-managed application is already in the monitor's colour space, which Noeul cannot tell
apart), and the calibration curve acts after the framebuffer. So on a wide-gamut monitor with a
custom profile, colours picked from a capture are the drawn values, not measured colours; the
docs say so. On an HDR (advanced-colour) monitor Windows converts
the picture to 8-bit SDR for GDI; Noeul says so in the result and promises nothing about how
closely it matches the display (NOEUL-FR-092). Pasted images with an embedded profile are still
converted to sRGB on decode (ADR 0047).

## Measurements (Windows 10 Pro 19045, one 5120×1440 monitor at 100 %, release build)

| What | p50 | p95 | How |
| --- | --- | --- | --- |
| `gdi::desktop()` (session checks, monitors, DPI, display configuration) | 0.223 ms | 0.316 ms | `display_measure`, 200 runs |
| of which `QueryDisplayConfig` details | 0.004 ms | 0.004 ms | same |
| `BitBlt` 1920×1080 from a **memory DC** + `DrawIconEx` + copy out + alpha | 4.12 ms | 4.67 ms | `display_measure`, 50 runs (the screen DC is never read) |
| same, 3840×2160 | 19.38 ms | 20.39 ms | same |
| `fill_gaps`, half the picture a gap, 1920×1080 / 3840×2160 | 2.15 / 10.82 ms | 2.36 / 13.75 ms | same |
| synthetic capture + pointer, 1920×1080 / 3840×2160 | 2.54 / 12.11 ms | 2.71 / 12.64 ms | same |
| logical mapping of a rectangle on three monitors | 0.0001 ms | 0.0001 ms | same |
| display change with twenty 1920×1080 pins: follow the windows | 0.001 ms | 0.001 ms | same, real off-screen windows |
| same: redraw every outline for a new scale (96 ↔ 144) | 166.6 ms | 167.7 ms | same; once per debounced change |
| ten `WM_DISPLAYCHANGE` posted at once → one handling | 328 ms (one run) | — | `tests/displays.rs`, debug build; 250 ms of it is the settle time |
| `noeul capture --monitor 2 --pin` (480×300, synthetic), CLI round trip | 40–46 ms | — | `tests/displays.rs`, debug build |

Protected windows, this Step's own 64×48 window far off-screen (M):

| Affinity | `PrintWindow(…, 0)` | `PrintWindow(…, PW_RENDERFULLCONTENT)` | `BitBlt` from the window's DC |
| --- | --- | --- | --- |
| `WDA_NONE` | content | content | content |
| `WDA_MONITOR` | **content** | black | **content** |
| `WDA_EXCLUDEFROMCAPTURE` | **content** | black | **content** |

So two common "window capture" techniques get around display affinity on this Windows build;
Noeul uses neither (NOEUL-SEC-004). The screen-DC behaviour with a protected window on a real
monitor is *documented* only here and is a step of the manual matrix.

## Consequences

- `noeul capture --monitor <n|primary|pointer>`, `--window active` and `--pointer`; IPC `capture`
  takes `monitor`, `window` and `pointer` and answers with `target`, `monitor`, `mapping`,
  `window`, `pointer` and `warnings` (noeul-ipc-v1, examples); `status` adds `displays`, and each
  monitor `rotation` and `hdr`; `[capture] pointer`; error codes `monitor_not_found` and
  `window_not_found` (CLI_CONTRACT §6).
- The manifest gains `capture.monitor` and `capture.window` (no keys); the signed catalog's
  shortcut summary lists them and must be re-signed (the master does this at merge).
- Tests run every layout synthetically — mixed DPI, negative origins, a turned and an HDR monitor,
  gaps, attach and detach, notification bursts, `WM_DPICHANGED` of pins and the overlay — and
  compare captured pixels exactly (`displays::tests`, `app::display_tests`, `win::displays`,
  `tests/displays.rs`, `e2e/tests/step58_noeul_displays.rs`).
- **Not verified on real hardware** (one monitor at 100 %, advanced colour off, here): mixed DPI,
  negative coordinates, rotation, hot-plugging, a real scale change, an HDR display, the real
  pointer's size on a scaled monitor, and real protected windows on screen. The reproducible
  physical matrix is [NOEUL_UX_SPEC §4.13](../NOEUL_UX_SPEC.md#413-step-58-displays-capture-targets-and-the-physical-test-matrix).
- Not in this build: the overlay's pointer toggle, logical units in the size chip (NOEUL-FR-046),
  an HDR-preserving capture, `--output` and `--clipboard` for captures (Step 62), tray items for
  monitor and window capture (see §2).
