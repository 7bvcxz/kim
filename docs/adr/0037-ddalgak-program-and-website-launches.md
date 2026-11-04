# ADR 0037 — Ddalgak launches: CreateProcessW without a shell, the shell's `open` for websites and apps, a launch thread per run

Status: **Accepted** · Date: 2026-11-04 · Step 70 · Refines [ADR 0022](0022-ddalgak-rule-model-and-safety.md)
(launches as declarative steps with fingerprinted consent) · Changes
[DDALGAK_RULE_MODEL](../DDALGAK_RULE_MODEL.md) §5.5 (new optional `env`), §5.6, §8, §12 · Builds on
[ADR 0024](0024-ddalgak-windows-input-apis-and-permissions.md), [ADR 0028](0028-ddalgak-rule-store-validation-and-reload.md),
[ADR 0025](0025-pane-programs-environment-and-backpressure.md) (Subak's program resolution and quoting; the approach,
not the crate)

## Context

Step 70 makes `open_program` and `open_url` run. Starting programs is the most consequential
thing Ddalgak does after typing: a rule could run any file the user can run, hand any string to
any registered protocol handler, or ask for administrator rights. The rule model (§5.5, §5.6)
already fixed the shape — absolute `.exe` paths or packaged-app IDs, argument lists, no shell
hosts, `http`/`https` only, consent bound to a fingerprint of the exact targets — but not how the
engine carries a launch out, what happens with slow or repeated triggers, how environment
overrides (issue #75) are expressed, or how automated tests can prove real launches on the
maintainer's machine without opening anything visible.

## Decision

1. **Programs start with `CreateProcessW`, never through a shell.** The step's path is expanded
   (only the six known-folder tokens of §5.5; nothing is searched: no `PATH`, no current folder)
   and checked again just before the start: it exists and is a file (otherwise
   `program_not_found`), its real file behind links is an `.exe` and not a command interpreter or
   script host (`launch_failed`), and it has no `:` after the drive (an alternate data stream; the
   schema's pattern allows it, the runtime does not). The expanded path is the application name,
   so the command line is never split at a space of the path; each argument is quoted by the
   `CommandLineToArgvW` rules (tested against Windows itself); handles are not inherited; the
   working folder defaults to the program's folder; the window state maps to `SW_SHOWNORMAL`,
   `SW_SHOWMAXIMIZED` or `SW_SHOWMINNOACTIVE`. There is **no hidden window state** for rules
   (AutoHotkey's `Hide`): a program the user cannot see is silent execution.
2. **Never elevated.** `CreateProcessW` does not show a UAC prompt; a program whose manifest asks
   for administrator rights fails with `ERROR_ELEVATION_REQUIRED`, which becomes
   `program_requires_elevation`. Ddalgak never retries with the `runas` verb, never passes a
   program path to `ShellExecuteExW` (which would prompt), and never starts anything with another
   token.
3. **Websites and packaged apps go to the shell's `open`.** A checked `http`/`https` URL, or
   `shell:AppsFolder\<AppUserModelID>` for an `app_id`, is handed to `ShellExecuteExW` with the
   verb `open`, `SEE_MASK_FLAG_NO_UI` (no error dialog: the failure is an outcome) and
   `SEE_MASK_NOASYNC`, with COM initialized as an apartment on the calling thread as the shell
   requires. No association becomes `browser_unavailable` (websites) or `program_not_found`
   (apps). The Windows layer refuses anything else (another verb, another target form, a hidden
   window) even if a bug produced it. Ddalgak makes no network connection itself
   (`DDALGAK-SEC-014`). Packaged apps use the AppsFolder activation through the shell rather than
   `IApplicationActivationManager`, which needs COM interface bindings `windows-sys` does not have;
   listing installed apps for the picker needs the same and is left to the editor (Step 74).
4. **URL normalization belongs to input, not to the stored rule.** The editor and the CLI
   normalize what the user typed (`https://` added when no scheme is given, scheme and host in
   lower case, a non-ASCII host label in its Punycode form, `/` added, spaces, quotes, `<`, `>`,
   `\`, `^`, `` ` ``, braces, `|` and non-ASCII characters percent-encoded) before consent is given,
   so the fingerprint covers exactly what will open. Every other scheme is refused — `file:`,
   `javascript:`, `vbscript:`, `data:`, `ms-*:`, `search-ms:`, `shell:` with the stronger message,
   custom protocols (`mailto:`, `zoommtg:`) with the plain one — and so are a user name or
   password. The stored URL is checked more strictly than the schema at compile time (no control
   or whitespace characters, no `"`, `<`, `>`, `\`, a port 1–65535, a well-formed host), so a
   browser's command line cannot be misread. **Custom protocols are not allowed in rules v1**: a
   per-protocol allowance needs its own explicit, separately consented configuration and is left
   to Step 79 with `run_command`.
5. **Environment overrides are an explicit list in the step** (`env: [{ "name", "value" }]`, at
   most 32, names `[A-Za-z_][A-Za-z0-9_]*`, unique case-insensitively, values without control
   characters). They replace variables of the same name in Ddalgak's environment for that program
   only; without `env` the environment is inherited unchanged. Being part of the step, they are
   part of the launch fingerprint: changing a value makes the rule `consent_stale`. Names may be
   shown; values never appear in `rule show` (without `--show-targets`), status, activity or
   messages. This is an additive v1 change: an older Ddalgak keeps such a rule inactive
   (`rule_unsupported`), never runs it without the overrides.
6. **A launch thread per run.** The worker admits a fired launch rule through a gate and hands
   the plan to a short-lived thread named `ddalgak-launch`, which carries out the targets in order
   and records the outcome. A slow shell hand-off (a browser that takes a second) never delays
   remaps or typing, and no thread exists while nothing launches (`DDALGAK-NFR-001`). **One run per
   rule at a time** (`busy`) and **at most one start per rule per second** (`rate_limited`): a
   double press or a held key never opens a program twice. Launch rules run their steps together,
   in order, honoring `on_error`; launches mixed with other step types wait for ordered multi-step
   runs (Step 78). A pause, stop, reload or the emergency chord ends a launch run before its next
   target (`cancelled`); a program already started is never stopped.
7. **Launches inject nothing.** They still run while an elevated window has focus (DDALGAK_SAFETY
   §3); the secure desktop, a password field and an IME composition skip them as every rule.
8. **Foreground: honest limits.** After a start Ddalgak calls `AllowSetForegroundWindow(pid)`
   (before a shell hand-off, `ASFW_ANY`, because the browser is often an already running process);
   this passes on Ddalgak's right to set the foreground window only if Ddalgak has it at that
   moment, which Windows' foreground lock decides (*documented*: the process that received the
   last input event, among other conditions). Whether a low-level hook's process counts is not
   documented; when it does not, the new window opens behind the active one and its taskbar button
   flashes. `if_running: activate` restores a minimized window and calls `SetForegroundWindow`
   under the same rules. Ddalgak does not use the known workarounds (attaching thread input,
   simulated Alt presses, `LockSetForegroundWindow` tricks): they defeat a protection the user
   relies on. The real behavior is a manual check (DDALGAK_ENGINE §13).
9. **Nothing launches except a fired trigger.** Loading, validating, reloading, editing and
   importing rules never reach the launcher: the only call is the launch thread's. Imported rules
   arrive disabled and without consent (§10); a changed target makes consent stale.
10. **Automated tests never open anything visible.** The debug test host (compiled out of release
    builds) starts programs only from the folder given with `--test-launch-root`, always hidden
    (`CREATE_NO_WINDOW`, `SW_HIDE`), never activates another program's window, and **records**
    every shell hand-off (verb, target, window state) as a line in that folder instead of calling
    the shell; without the option every launch is refused. The tests build their own tiny console
    program with the toolchain's `rustc`, start it from a sandbox path with spaces and Korean
    characters, read back what it received and kill it if it still runs. Websites, packaged apps
    and `ERROR_ELEVATION_REQUIRED` are tested through a fake launcher.

## Consequences

- A rule cannot run `cmd /c …`, a `.bat`, a `.lnk` or a document; users who need those wait for
  Step 79's consented command action. AutoHotkey's `Run` with a shell string has no equivalent.
- Programs started by Ddalgak inherit its environment as it was when the app started (at sign-in
  for the opt-in startup entry), plus the rule's overrides; a variable the user changed later is
  not seen until the app restarts. This is documented, not hidden.
- A browser or app association that itself asks for elevation would show the system's UAC prompt;
  that is the user's own association, not a Ddalgak elevation.
- The editor's packaged-app picker cannot list installed apps yet; an app ID is typed or pasted
  and validated by its form, and a missing app ends with `program_not_found`.
- Real launches of installed programs, the real default browser, an elevated target and focus
  behavior are **not tested** by the automated tests on the shared machine; the manual script is
  DDALGAK_ENGINE §13.

## Alternatives considered

- **`ShellExecuteExW` for programs too.** Simpler, and it resolves App Paths, but it prompts for
  elevation when a manifest asks for it and would accept documents and shortcuts. Rejected.
- **Run launches on the worker thread.** No extra thread, but a slow hand-off would stall remaps
  and typing for its duration. Rejected for a short-lived thread per run.
- **Queue a trigger that arrives during a launch.** Two windows a second later are rarely wanted.
  Rejected for `busy` and the one-second gate.
- **A `hidden` window state.** Useful for background tools, but indistinguishable from silent
  execution. Rejected for rules v1; the debug test host alone uses it for its own test program.
- **Custom protocols with a per-rule flag.** Would make `zoommtg:` or `obsidian:` links possible,
  but protocol handlers are a known attack surface (`ms-msdt:`, `search-ms:`) and the preview
  model (scheme and host) does not fit them. Deferred to Step 79's explicit configuration.

## Requirements

`DDALGAK-FR-013`, `DDALGAK-FR-014`, `DDALGAK-NFR-001`, `DDALGAK-SEC-006`, `DDALGAK-SEC-007`,
`DDALGAK-SEC-008`, `DDALGAK-SEC-011`, `DDALGAK-SEC-013`, `DDALGAK-SEC-014`, `KIM-SEC-004`.
