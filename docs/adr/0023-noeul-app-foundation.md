# ADR 0023 — Noeul app foundation: plain Win32, GDI capture on demand, one instance per user session

Status: **Accepted** (Step 49; the capture overlay is re-measured in Step 50, capture APIs in
Step 58) · Date: 2026-11-04 · Builds on [ADR 0002](0002-independent-tool-binaries.md),
[ADR 0005](0005-local-ipc.md), [ADR 0011](0011-portability-boundary.md),
[ADR 0014](0014-gui-technology.md), [ADR 0017](0017-universal-customizable-hotkeys.md),
[ADR 0019](0019-secure-host-supervisor-and-ipc-v1.md) and
[ADR 0021](0021-noeul-product-definition.md) (§4, §5, §7)

## Context

ADR 0021 §7 left three choices to Step 49, to be made by measurement against the Noeul budgets
([NOEUL_REQUIREMENTS §4](../NOEUL_REQUIREMENTS.md#4-non-functional-requirements)): the GUI and
overlay technology, the screen-capture API, and how the resident process is organized. The
budgets that decide them: idle CPU below 0.1 % with no wake-ups (NOEUL-NFR-001), idle private
memory at most 20 MB with no capture buffer or GPU context while idle (NOEUL-NFR-002), launch to
ready at most 300 ms and a binary of at most 12 MB (NOEUL-NFR-006). The constraints: no
elevation, service, hook or `UIAccess` (NOEUL-SEC-003, NOEUL-SEC-012); no consent prompts or
capture borders the user did not ask for; content excluded by display affinity stays excluded
(NOEUL-SEC-004); headless CLI paths never open a window (NOEUL-FR-111).

Evidence that already existed: Step 04 measured idle windows on this machine
([research/gui-eval](../../research/gui-eval/README.md)): winit + softbuffer 13.1–13.7 MiB working
set and 4.6–4.7 MiB private; **egui + glow 69.3–69.4 MiB working set and 97.6–97.8 MiB private**
(the OpenGL driver stack); egui + wgpu about 300 MiB.

## Decision

### 1. One binary, one core, four separate request sources

`noeul.exe` is both the CLI and the resident instance (`noeul run`, started detached by
`noeul start`). The instance core ([`app::App`](../../crates/noeul/src/app.rs)) owns the capture
sessions, the pins and the shortcuts and runs on the UI thread. Requests arrive from four
sources that cannot reach each other's state except through the core: IPC from the CLI, global
hotkeys, the tray icon, and the pin windows' own events. Every OS dependency sits behind a narrow
interface — `CaptureBackend`, `PinWindows`/`PinWindow`, `Shell` (clipboard, pointer, notices) — so
the whole lifecycle runs headless in tests with fakes. Modules: `frame` (buffers), `capture`
(backend boundary), `selection` (capture sessions), `pins`, `annotation` (**placeholder** until
Step 56), `settings` and `paths`, `hotkeys`, `ipc`, `app`, `tray`, and `win` (all Win32).

### 2. GUI technology: plain Win32, no framework, nothing drawn while idle

Pins are layered, borderless, topmost tool windows whose picture is uploaded once with
`UpdateLayeredWindow` from a premultiplied DIB section: no `WM_PAINT`, no timer, no GPU context.
The instance has one hidden control window (message target, tray owner); the tray icon is
`Shell_NotifyIconW`. **egui + glow is rejected for the resident process**: its measured idle
footprint (98 MiB private) is five times the 20 MB budget, and the budget forbids a GPU context
while idle. `cargo xtask deps-check` now lists `noeul` among the packages that must not reach GUI
framework crates. The selection overlay (Step 50) exists only while a capture is open and is
measured there; if it ever needs a GPU context, that context must be created per capture and
released with the overlay.

### 3. Capture API: GDI on demand (DXGI and Windows.Graphics.Capture compared)

| | GDI `BitBlt` (`CAPTUREBLT`) | DXGI Desktop Duplication | Windows.Graphics.Capture |
| --- | --- | --- | --- |
| Rights | user process, no prompt (*documented*) | user process, no prompt (*documented*) | unpackaged desktop apps may capture without a prompt (*documented*); packaged apps declare a capability |
| Visible side effects | none | none | a yellow capture border on Windows 10; removable on Windows 11 only after an access request (*documented*) |
| Idle cost | nothing held between captures | a D3D device and a duplication per output to stay fast | a D3D device, frame pool, WinRT activation |
| Display affinity / DRM | excluded windows come out black (*documented*) | same | excluded windows are omitted (*documented*) |
| Locked / secure desktop | fails (input desktop is not the user's) | `ACCESS_LOST`/`ACCESS_DENIED` (*documented*) | no session (*documented*) |
| HDR | clipped SDR | gets HDR surfaces (FP16) | gets HDR surfaces |
| Code size | ~150 lines of `windows-sys` | D3D11 + DXGI interop | WinRT projection or raw COM |

**GDI is chosen for Step 49**: it holds no device or buffer between captures (the idle budget),
has no border or prompt surprise, works on Windows 10 1809, and leaves protected content
protected. It runs only on an explicit request (NOEUL-SEC-001), after the desktop check below.
Step 58 (HDR, mixed DPI, NOEUL-FR-092) may add DXGI behind the same `CaptureBackend` trait when its
measurements require it; Step 50 measures the full-desktop freeze latency (NOEUL-NFR-003) with the
overlay. **Windows 11 privacy controls were not checked on Windows 11** (this machine runs
Windows 10 19045); the table cites Microsoft's documentation only.

**Desktop check before every capture and every hotkey action** (`win::gdi::desktop`): an
interactive window station (`WSF_VISIBLE`), a connected session (`WTSConnectState`), the input
desktop is the user's `Default` desktop (`OpenInputDesktop` fails or names `Winlogon` while the
session is locked or a secure desktop is shown), and at least one monitor. Otherwise the request
fails with `desktop_unavailable` (or `unsupported_platform` on other systems) and no picture is
invented.

### 4. Buffer ownership

A [`Frame`](../../crates/noeul/src/frame.rs) (BGRA, top-down, device pixels) is the only owner of a
picture and is not `Clone`: it moves from the backend into a capture session, from the session
into a pin (cropped to the selection, the rest freed), and from a closed pin into the reopen
memory (one pin by default, NOEUL-FR-065). A failed pin hands its picture back (NOEUL-FR-025).
Every frame is counted in a per-instance ledger that `noeul status` reports (`memory.frames`,
`bytes`, `peak_bytes`), so tests prove that cancel, pin, close and quit leave nothing. A pin costs
its pixels twice (the frame and the layered window's DIB), inside NOEUL-NFR-002's "twice its pixel
bytes plus 2 MB". Nothing is written to disk (NOEUL-SEC-005); `Debug` never prints pixels.

### 5. Single instance and Noeul IPC v1

- **Endpoint** `\\.\pipe\mouv-noeul-<user SID>-s<session>`: per user and logon session, local only,
  owner-only DACL, `FILE_FLAG_FIRST_PIPE_INSTANCE`. The pipe is the single-instance guard and is
  bound **before** anything else: a second `noeul run` finds it taken, hands its request to the
  running instance (`activate`) and exits 0, so two instances never register the same hotkeys
  (NOEUL-FR-003). A crash removes the pipe with the process; nothing stale is left.
- **Identity checks** (as ADR 0005/0019): the CLI checks that the server process runs as this user
  and is a `noeul.exe` before sending a byte (`noeul_identity_mismatch` otherwise); the instance
  checks the SID of each connection's own token after the first frame.
- **Framing and messages**: a 4-byte little-endian length and one JSON object (at most 64 KiB);
  `hello` within 5 s → `welcome` → `request`/`response` pairs. Methods: `status`, `activate`,
  `capture`, `pin.list`, `pin.close`, `pin.reopen`, `hotkeys.reload`, `quit`. Errors carry
  registered CLI codes. At most 16 connections; an idle connection closes after 60 s; a malformed
  frame ends that connection only. Pictures never cross the pipe. Schema:
  [noeul-ipc-v1](../contracts/schemas/noeul-ipc-v1.schema.json) with valid and invalid examples.
  It is Noeul's own protocol, not Subak's IPC v1.
- **Threads**: the UI thread runs the message loop and the core; the IPC acceptor, one thread per
  connection, the `kim-hotkeys` registration thread and a `hotkeys.json` change watcher only
  queue work and post one message to the control window. Idle, all of them block in the kernel.
  Window procedures never re-enter the core: they queue events, and the core's pump drains every
  queue after each piece of work (also after modal loops such as the tray menu).

### 6. Lifecycle

`noeul start` creates `noeul run` with `CreateProcessW`, `DETACHED_PROCESS |
CREATE_NEW_PROCESS_GROUP`, **no inherited handles** (a redirected launcher's pipe would otherwise
be held open by the instance; found by this Step's tests), breakaway from the caller's job when
allowed, the user profile as working folder, and waits until the instance answers. Quitting
(`noeul quit`, tray Quit, `WM_CLOSE` from `taskkill`, end of session) cancels the capture, closes
every pin, releases the global hotkeys, removes the tray icon and stops the listener; with open
pins the CLI needs `--yes` and the tray asks once (NOEUL-FR-002). Explorer restarts re-add the
icon (`TaskbarCreated`).

### 7. Shortcuts: the build ships what it implements

The compiled-in manifest is the final manifest ([`noeul.json`](../contracts/examples/kim-hotkey-manifest-v1/noeul.json))
filtered to this build's implemented actions (a test keeps them equal, NOEUL-FR-114):
`paste.float` (`F3`, global) and the pin keys `pin.close` (`Escape`, `Ctrl+W`) and `pin.move-*`
(arrows). `paste.float` pins a clipboard image (`CF_DIBV5`/`CF_DIB`) at the pointer; the rich
formats are Step 55. **`capture.region` (`F1`) is not registered in this build**: its selection
overlay is Step 50, and registering a key that does nothing would steal `F1` from every program
for no use. The same code registers it once Step 50 adds it to the implemented list. Global
keys go through `kim-hotkeys` (`RegisterHotKey`); a key another program owns is reported `taken`
with offers and never taken over or retried in the background; a press while the desktop is not
available does nothing. `kim-hotkeys` gained `Registrar::start_with_waker`, so the instance's
message loop learns about a press without polling. `noeul hotkeys …` is the `kim hotkeys` command
set on the same library; `list`/`show` ask a running instance which keys are `registered` or
`taken`, and a change is applied at once (IPC) or on the file-change notification.

### 8. Tray icon

Optional, on by default (`[general] tray_icon` in `config.toml`, `--no-tray`). Items appear only
once implemented: Pin clipboard, Reopen closed pin, Keyboard shortcuts on/off (the full switch of
HOTKEY_CONTRACT §12.4, written to `hotkeys.json`), Quit. Middle-click pins the clipboard; left and
right click open the menu until the capture overlay exists. The icon is drawn in code (an
original evening-glow disc), so no image asset ships.

### 9. Other platforms

Everything outside `win` compiles everywhere; the capture backend reports `unsupported_platform`
and instance commands fail with it (ADR 0011). Linux is milestone N7.

## Measurements

Windows 10 Pro 22H2 build 19045, Intel Core i5-13600KF, release build (thin LTO, stripped),
2026-11-04. Instance started with `noeul start --no-tray` in an isolated profile, `paste.float`
on `Ctrl+Alt+Shift+F22` (the registration thread runs as in a normal start), no pin. Reproduce with
[`scripts/noeul-measure-idle.ps1`](../../scripts/noeul-measure-idle.ps1) (samples the process
with `Get-Process` after 5 s and after 60 more seconds idle).

| Measure | Result | Budget |
| --- | --- | --- |
| Binary size | 2,258,432 bytes (2.2 MiB) | ≤ 12 MB (NOEUL-NFR-006) |
| CLI `start` until the new instance answers IPC | 92 ms | launch to ready ≤ 300 ms (NOEUL-NFR-006) |
| CPU time over 60 s idle | 0 ms (below the 15.6 ms timer resolution) | < 0.1 % of a core (60 ms) (NOEUL-NFR-001) |
| Private bytes, idle | 1.47 MiB | ≤ 20 MB (NOEUL-NFR-002) |
| Working set, idle | 7.32 MiB | — |
| Threads / handles, idle | 6 / 120 | — |
| Image memory held, idle | 0 bytes | no capture buffer while idle |
| Pipe round trip (`noeul health`, debug build, manual smoke run) | 1 ms | — |

Not measured here: capture latency of a full multi-monitor freeze (NOEUL-NFR-003; Step 50, which
owns the overlay — capturing the user's whole screen for a benchmark is not done on this shared
machine), pin latency (NOEUL-NFR-004; Steps 51/53), a visible tray icon, and context-switch counts.

## Consequences

- Noeul's windows are hand-written Win32. That is more code per widget than egui, and the settings
  window (Step 61) must choose again: it is opened on demand, so a framework there would not count
  against the idle budget if it is created and destroyed with the window.
- `F1` is unbound until Step 50; `noeul --help`, the docs and `status.hotkeys.planned` say so.
- The CLI and the instance are one executable. A Start-menu shortcut to `noeul start` would flash a
  console; release packaging (Step 63) adds a GUI-subsystem launch path (e.g. a small `noeulw.exe`
  or a launcher argument) and records it.
- The IPC v1 schema is closed: new fields are additive changes to the schema in the same change.

## Not tested (and why)

- The real tray icon, its menu and balloons: creating one in an automated run would show it on the
  maintainer's taskbar. The menu model and the core's reactions are tested headlessly. Manual
  check: `noeul start`, right-click the icon (menu as in §8), middle-click with an image on the
  clipboard (a pin appears at the pointer), Quit.
- A real `F3` press: tests never register `F3` and never synthesize input. Registration, `taken`
  reporting, reload and release are tested with `Ctrl+Alt+Shift+F22`/`F21`; the press path is
  tested from the registration id onward. Manual check: `noeul start`, copy an image, press `F3`,
  then `Esc` on the new pin.
- The locked, secure, disconnected and display-less desktop states: the tests detect and report
  them honestly when they occur, but this machine was unlocked during the run. Manual check: run
  `noeul status --json` over SSH (`non_interactive`), or lock with `Win+L` and run a scheduled
  `noeul capture --region 0,0,1,1 --pin --json` (`desktop_unavailable`).
- Windows 11, ARM64 and other users' clients (the cross-user rejection follows ADR 0019's manual
  procedure).

## Alternatives considered

- **egui + glow for pins and overlay** (ADR 0014's stack): measured 98 MiB private idle — rejected
  for the resident process.
- **A separate GUI process and a headless CLI binary**: two binaries to version and install for
  no gain; the CLI paths of the single binary stay headless by construction.
- **A named mutex as the single-instance guard**: the pipe already is one, atomically, and a mutex
  alone would not hand the request over.
- **Registering `F1` now with a "coming soon" action**: rejected (NOEUL-FR-114).
- **DXGI Desktop Duplication now**: faster for repeated frames, but Noeul freezes one frame per
  capture and must hold nothing while idle; kept as a Step 58 option.

## Requirements

`NOEUL-FR-001`…`NOEUL-FR-004`, `NOEUL-FR-010`, `NOEUL-FR-013`, `NOEUL-FR-025`, `NOEUL-FR-026`,
`NOEUL-FR-060`, `NOEUL-FR-065`, `NOEUL-FR-070`, `NOEUL-FR-074`, `NOEUL-FR-100`, `NOEUL-FR-102`,
`NOEUL-FR-110`…`NOEUL-FR-112`, `NOEUL-FR-114`, `NOEUL-NFR-001`, `NOEUL-NFR-002`, `NOEUL-NFR-006`,
`NOEUL-NFR-008`, `NOEUL-NFR-011`, `NOEUL-NFR-014`, `NOEUL-SEC-001`…`NOEUL-SEC-005`,
`NOEUL-SEC-008`, `NOEUL-SEC-011`, `NOEUL-SEC-012`, `KIM-FR-012`, `KIM-SEC-020`, `KIM-SEC-022`.

## Notes

- 2026-11-04 (Step 50): `capture.region` (`F1`) is registered since its selection overlay exists
  ([ADR 0026](0026-noeul-capture-overlay.md)); §7's "not registered in this build" and the
  Consequences line "`F1` is unbound until Step 50" describe Step 49 only. The full-desktop freeze
  latency deferred here was measured there (GDI read-back of a 5120×1440 desktop: about 87 ms;
  first frame p95 114.8 ms off-screen), and the GDI choice stands for one monitor; multi-monitor
  latency is left to Step 58. `Monitor` gained its effective DPI (`status.desktop.monitors[].dpi`).
- 2026-11-04 (Step 53): the pin engine is [ADR 0029](0029-noeul-pin-engine.md). Pin windows no
  longer keep their DIB section after `UpdateLayeredWindow` (the system retains the surface), so
  §2's "picture uploaded once" stands but §4's "a pin costs its pixels twice" now reads "once in
  Noeul's process" (measured there). §7's shipped pin keys gained `pin.destroy`, `pin.zoom-in`,
  `pin.zoom-out`, `pin.zoom-reset` and `pin.hide`; §8's tray gained Hide all pins / Show all pins
  and Bring pins on screen; §5's IPC gained `pin.create`, `pin.hide` and `pin.show`.
- 2026-11-04 (Step 58): [ADR 0050](0050-noeul-displays-and-capture-backends.md) revisits §3 as
  planned: GDI stays the only backend, with no silent fallback to DXGI, Windows.Graphics.Capture
  or `PrintWindow` (measured: `PrintWindow` without `PW_RENDERFULLCONTENT` and `BitBlt` from a
  window's own DC return the content of a window that excludes itself from capture). §3's table
  row "HDR: clipped SDR" is refined there (an 8-bit conversion by Windows, not verified on an HDR
  display). `Monitor` gained `rotation` and `hdr` from the display configuration; captures make
  the gaps of the virtual desktop opaque black; IPC `capture` also takes `monitor`, `window`
  and `pointer`.
