# ADR 0024 — Ddalgak engine: Windows input APIs, threads, IPC and permissions

Status: **Accepted** · Date: 2026-11-04 · Step 66 · Refines [ADR 0022](0022-ddalgak-rule-model-and-safety.md)
§3 (process model) and fixes what ADR 0022 left to Step 66 (the IPC details, the hook health
check, the performance budget) · Builds on [ADR 0017](0017-universal-customizable-hotkeys.md),
[ADR 0005](0005-local-ipc.md), [ADR 0019](0019-secure-host-supervisor-and-ipc-v1.md),
[ADR 0011](0011-portability-boundary.md)

## Context

Ddalgak's engine must see keys before applications do, decide within a fraction of a
millisecond whether a rule takes a key, run rules without blocking input, never react to its own
synthetic input, release everything it holds on every exit path, and stay out of the way on the
secure desktop, in password fields, during IME composition and over elevated windows
([DDALGAK_SAFETY](../DDALGAK_SAFETY.md)). It must also be cheap when idle, single per user
session, controllable from a headless CLI that never starts it, and optionally started at
sign-in without a service. ADR 0022 decided the shape (a tray app owns the engine; the CLI
talks to it) but not the Windows mechanisms. Windows offers several, each with different
reach, privileges and failure modes.

## Decision

### 1. Which input API for what

| Need | API | Why this one |
| --- | --- | --- |
| Ddalgak's own GUI shortcuts (`engine.toggle`, `engine.pause`) | `RegisterHotKey` on the app's hidden window, `MOD_NOREPEAT` | Works while the hook is off (the only way to turn the engine on by key, `DDALGAK-FR-004`); the OS arbitrates ownership (`taken`, never stolen, `KIM-SEC-020`); no hook. Chord mapping and the refusals of what it cannot express come from `kim-hotkeys` (`win32_chord`). |
| Rule triggers (remaps from Step 68, left/right modifiers, physical keys, lock keys, bare modifiers, release, hold, sequences, injected-input detection) | One `WH_KEYBOARD_LL` hook | `RegisterHotKey` cannot tell sides, see key-ups, holds or sequences, or consume a plain key (ADR 0022 alternatives). The low-level hook runs in Ddalgak's own process (no DLL injection), sees `LLKHF_INJECTED` and `dwExtraInfo`, and can consume an event. It exists only while the engine is `running` or `paused`. |
| Mouse | **No `WH_MOUSE_LL` in Step 66.** | Nothing in D2 needs mouse input. Mouse-button triggers (Step 68) need a `mouse-hook` permission disclosed in the catalog first (ADR 0022); mouse *actions* (Step 71) use `SendInput` and need no hook. |
| Injection | `SendInput` with `dwExtraInfo = OWN_MARKER` | The documented way; Windows adds `LLKHF_INJECTED`, so the hook can tell Ddalgak's own events from physical ones and from other programs'. |
| Context (stepping aside) | Out-of-context WinEvent hooks (`EVENT_SYSTEM_FOREGROUND`, `EVENT_OBJECT_FOCUS`, `EVENT_OBJECT_IME_SHOW/HIDE`, `EVENT_SYSTEM_DESKTOPSWITCH`), `WTSRegisterSessionNotification` | Event-driven, no DLL is loaded into other processes, no polling. They feed atomic flags the hook callback reads: elevated foreground (integrity level of the foreground process), Win32 password edit (`ES_PASSWORD`), IME window shown, session locked. A desktop switch (lock screen, UAC, Ctrl+Alt+Del) or an unlock makes the hook forget its key state, because the key-ups made on the other desktop never reach it; polling `GetAsyncKeyState` in the callback was rejected (its answer depends on the foreground thread and desktop rights, and it could clear real modifiers). |
| Source device | none (`unknown`) | `WH_KEYBOARD_LL` does not identify the keyboard. Raw Input (`RIDEV_INPUTSINK`) would, but it means a second stream of every key in the session and correlating the two; not worth it until a rule needs per-device triggers. The event model has the field, honestly set to `unknown`/`synthetic`. |

### 2. The event model

The hook copies `KBDLLHOOKSTRUCT` into a platform-neutral `RawKey`, and a normalizer keeps only
what matching needs (which keys and modifiers are down): virtual key and scan code both kept
(logical keys follow the layout's virtual keys, `code:` keys the position), generic modifier
codes resolved to sides, the fake left Ctrl that Windows sends before Right Alt on AltGr layouts
(scan `0x21D`) recognized so `AltGr+Q` ≠ `Ctrl+Alt+Q`, auto-repeat derived from key state, and
the source classified as physical, own (`LLKHF_INJECTED` + marker) or other program. The
emergency chord uses the *physical* modifier state only, with AltGr's fake Ctrl counting as Ctrl
so the chord also works on AltGr layouts. Nothing is recorded beyond the current key state.

### 3. Threads and the callback budget

- **Hook thread**: installs the hook and sleeps in `GetMessageW`; Windows calls the hook
  procedure there. The callback runs an allocation-free decision over a precompiled table
  (emergency chord first, own input passed, consumed-down/consumed-up pairing, candidates in
  precedence order, stepping aside, per-rule rate limit), pushes at most one small `Copy` signal
  into a bounded queue with `try_send` (a full queue drops and counts, never blocks), and returns.
  A panic is caught and the event passes on. Unconsumed events always go to `CallNextHookEx`.
  Table swaps arrive as posted messages and happen between callbacks.
- **Worker thread**: runs fired rules, times `hold` triggers (a timeout exists only while a hold
  is pending), records activity (rule IDs and outcome codes only). It never changes the engine
  state; engine-control steps go to the controller.
- **Main thread** of `ddalgak-gui.exe`: a hidden, never-shown tool window (no taskbar entry, no
  focus) for the tray icon, `WM_HOTKEY`, session notifications and the WinEvent callbacks; it
  owns the lifecycle and applies its side effects.
- **IPC thread**: serves the pipe one request at a time.

Budget (`DDALGAK-NFR-002`): the callback must return in < 1 ms at p99 and must never come near
`LowLevelHooksTimeout`. Windows removes a hook that times out without telling the program, so
the engine records every callback's duration and every event's queueing delay (event time to
callback) and reports them on demand.

### 4. Lifecycle, single instance and visibility

- `off` → `running` installs the hook (and the context watchers) and writes the run marker;
  `running` ↔ `paused` keeps the hook and releases held keys; any → `off` releases held keys,
  removes the hook and the run marker. The emergency chord trips inside the callback (the
  engine state flips to off atomically, the hook thread unhooks right after the callback), and
  the controller releases keys and writes the emergency marker.
- **Start policy**: off unless `engine_on_start: on`, and always off after an unclean exit (run
  marker present), an emergency off, `--no-hotkeys`, or when the app runs elevated
  (`engine_elevated`). No rule can turn the engine on.
- **Single instance**: a named mutex in the session's `Local\` namespace including the user SID;
  a second start exits with `app_already_running` (7) without touching the first. The pipe's
  `FILE_FLAG_FIRST_PIPE_INSTANCE` is a second guard.
- **Visible**: the shipped app always shows its tray icon (state by shape and text). Debug
  builds have a **test host** (`--test-host`): no tray icon, a hook that never consumes a key,
  and a bounded lifetime. It is compiled out of release builds (`cfg(debug_assertions)`), so a
  shipped Ddalgak cannot run a hidden hook (`DDALGAK-SEC-010`).
- **Held keys**: every injected key-down is recorded in a ledger and released on pause, stop,
  emergency, session lock, shutdown, `Drop` and in the panic hook. A killed process cannot run
  any of these; that path is verified in Step 73.

### 5. Local IPC

Pattern of ADR 0005/0019, adapted without depending on Subak (`KIM-FR-001`):
`\\.\pipe\mouv-ddalgak-<user SID>-s<session>`, local only, owner-only DACL denying NETWORK,
first instance owned by the app. The CLI checks **before sending anything** that the server
process is `ddalgak-gui.exe` of the same user (`app_identity_mismatch` otherwise); the app
reads the client's SID from the connection's token and answers nobody else. One JSON line in,
one out, bounded sizes and timeouts; methods `status`, `health`, `engine.start|pause|resume|stop`,
`app.quit`. There is no method to trip the emergency off: only physical keys can. No IPC
message carries keys or rule content.

### 6. Start at sign-in (opt-in)

`ddalgak startup enable` writes one `REG_SZ` value `mouv.ddalgak` =
`"<install folder>\ddalgak-gui.exe" --startup` under
`HKCU\Software\Microsoft\Windows\CurrentVersion\Run`, and `disable` removes it. Per user, no
administrator rights, visible in Windows' Startup apps settings. No service, scheduled task,
SYSTEM process or Kim daemon. The store is behind a trait; tests use a private key under
`HKCU\Software\mouv-test\` that they delete (debug builds read it from
`DDALGAK_DEV_STARTUP_KEY`), so no test ever writes the real Run key.

### 7. Permissions

| Permission | Used by Step 66 | Notes |
| --- | --- | --- |
| `keyboard-hook` (catalog) | yes, while the engine is on | in-process `WH_KEYBOARD_LL`, no DLL injection |
| `input-injection` (catalog) | mechanism only (marked `SendInput`, ledger); no rule injects before Step 68 | rules need consent (ADR 0022 §7) |
| `mouse-hook` | **no** | catalog disclosure first (Step 68) |
| Administrator, `UIAccess`, service, driver, SYSTEM | **never** | an elevated app refuses to turn the engine on |
| Network | **never** | the pipe rejects remote clients |
| Registry | the user's own Run value, only on explicit `startup enable` | |

### 8. Hook health (on demand, never on a timer)

`ddalgak health` asks the app and reports: hook installed and hook thread alive; callback count,
maximum and p99 bucket; events that reached the callback more than 200 ms late (a sign Windows
may have removed the hook); dropped signals; caught panics; wake-ups of each thread; CPU time.
Problems come with a hint to turn the engine off and on; Ddalgak never reinstalls a hook by
itself. An active probe (inject a marked no-op key and check that the hook saw it) was
considered and left out: it injects input, and the passive signals already cover the documented
removal cause (a slow callback).

## Consequences

- Global shortcuts work without the hook, the hook works without a window, and the CLI works
  without the app (status from the files). Closing the app removes everything; there is no
  resident component.
- The callback is cheap and bounded: compiled tables, atomics, a bounded queue. Measurements
  and the idle benchmark are in [DDALGAK_ENGINE §7](../DDALGAK_ENGINE.md).
- Password-field detection covers Win32 edits only (UI Automation in Step 69), IME detection
  depends on the IME showing an IME window (verified with the Korean IME in Steps 68–69); both
  are documented as best effort.
- Test hosts and private registry keys make real-binary tests possible on the maintainer's
  machine without a tray icon, a consumed key or a touched Run key. Real-input tests skip
  honestly when the workstation is locked.
- The tray app is still the engine owner of ADR 0022; no "headless engine mode" ships.

## Alternatives considered

- **`WH_KEYBOARD` (non-low-level) hook**: needs a DLL injected into every GUI process. Rejected
  (`DDALGAK-SEC-012`, no injection into other processes).
- **Raw Input only**: sees keys and devices but cannot consume or remap them. Rejected for
  triggers; kept in mind for per-device triggers.
- **Polling `GetAsyncKeyState`**: timers, missed short presses, keylogger-like. Rejected.
- **A hidden headless engine process** started by the CLI: violates "the CLI never starts the
  app" and "a visible hook" (ADR 0022). Rejected; the debug-only test host is the narrow
  exception for automated tests.
- **A named mutex alone, or the pipe alone, for single instance**: either can be squatted; both
  together plus the identity checks are cheap.
- **Scheduled task or service for sign-in start**: needs more privileges or a resident
  component. Rejected for the user's Run key.
- **An active hook probe in `health`**: rejected for now (§8).

## Requirements

`DDALGAK-FR-001…004`, `DDALGAK-FR-008`, `DDALGAK-FR-017`, `DDALGAK-FR-060`, `DDALGAK-FR-061`,
`DDALGAK-NFR-001`, `DDALGAK-NFR-002`, `DDALGAK-NFR-005`, `DDALGAK-NFR-006`, `DDALGAK-NFR-009`,
`DDALGAK-SEC-001…005`, `DDALGAK-SEC-007`, `DDALGAK-SEC-009…011`, `DDALGAK-SEC-014`,
`KIM-FR-001`, `KIM-FR-012`, `KIM-SEC-004`, `KIM-SEC-020…023`.
