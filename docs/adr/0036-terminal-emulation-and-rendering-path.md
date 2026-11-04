# ADR 0036 — Terminal emulation core and rendering path

Status: **Accepted** · Date: 2026-11-04 · Step 15 · Refines [ADR 0014](0014-gui-technology.md)
decision 2 and its fallbacks for the terminal surface, extends
[ADR 0030](0030-host-terminal-model-scrollback-and-replay.md), closes the open item 7 of
[ADR 0004](0004-pty-and-process-ownership.md) · Details, limits and evidence:
[TERMINAL_EMULATOR.md](../TERMINAL_EMULATOR.md)

## Context

Step 15 must give Subak a faithful terminal — escape sequences, cursor operations, the
alternate screen, scroll regions, UTF-8 with Korean, wide characters, emoji and combining
marks, 24-bit colour, OSC 8 hyperlinks, mouse reporting, bracketed paste, resize — and a
rendering path for the Subak GUI (Steps 16–20) that is measurable, maintainable and has an
extremely low idle cost (`KIM-NFR-001`, `KIM-NFR-002`). Two choices were open:

1. **Emulation.** Since Step 13 the Host models every pane with an in-house parser and screen
   (ADR 0030). A client view needs the same semantics: if the Host and the GUI parsed bytes
   differently, a late-attaching GUI would show something else than the Host captures.
2. **Rendering.** ADR 0014 chose egui + glow for chrome and asked Step 15 to pick the
   terminal-grid renderer "with conformance and latency measurements". ADR 0023 measured
   egui + glow at ~98 MiB private memory idle against ~1.5 MiB for plain Win32 and rejected it
   for Noeul.

## Options compared

### Emulation components (crates.io metadata fetched 2026-11-04)

| Option | What it is | License | Dependencies (normal) | Fit |
| --- | --- | --- | --- | --- |
| **In-house model (Step 13), evolved** | Paul Williams parser + xterm-subset screen, compact VT-encoded scrollback, snapshot/capture | ours (MIT OR Apache-2.0) | 0 | Already runs in the Host on every byte; one implementation for Host and view |
| `vte` 0.15.0 (2025-02-02) | Parser only (Alacritty's) | Apache-2.0 OR MIT | 2 (+4 optional) | Would replace ~250 lines of our parser; the screen, scrollback and snapshot remain ours |
| `alacritty_terminal` 0.26.0 (2026-04-06) | Full terminal: grid, scrollback, selection, vi mode, its own PTY event loop | Apache-2.0 | 17, incl. `polling`, `signal-hook`, `parking_lot`, `miow`, `piper`, `rustix`, `regex-automata` | Brings an event loop and PTY layer into the Host (ADR 0030); scrollback as cell grids (~24 bytes per cell); no VT snapshot to replay into another terminal |
| `termwiz` 0.23.3 (2025-03-20) | WezTerm's surface model and parser | MIT | 38, incl. `terminfo`, `pest`, `nix`, `winapi`, `sha2` | Large tree for a headless Host; `winapi` (unmaintained line) next to our `windows-sys` |
| `wezterm-term` | WezTerm's terminal model | MIT | not on crates.io (git only) | Excluded by `deny.toml` (`allow-git = []`) |
| `vt100` 0.18.1 (2026-10-19) | Screen model with formatted output | MIT | 4 | ADR 0030 found it stores scrollback as cells and cannot restore regions, saved cursor or partial sequences in its output |

Measured on our model (release, this machine, `term_bench`): 42–143 MiB/s parsing depending on
content (ConPTY delivers ~0.9 MiB/s, ADR 0025), a 120×30 pane with 10 000 history lines in
1.27 MB, no allocation per byte in the common path.

### Rendering paths

| Option | Idle CPU | Idle memory (measured) | Glyphs | Binary | Windows story | Linux later | Verdict |
| --- | --- | --- | --- | --- | --- | --- | --- |
| **CPU cell renderer + GDI present** (built here) | 0 ms / 30 s | **3.40 MiB private, 8.10 MiB working set, 1 thread, 95 handles** — window, 120×30 view with 2 000 history lines, renderer, glyph cache | system fonts via GDI, cmap fallback, procedural box drawing; colour emoji monochrome | `term_bench.exe` with the harness: 0.66 MiB | GDI everywhere since 1809; no GPU context, RDP/VM-safe | same renderer; present through softbuffer/X11 (rasterizer to add) | **Chosen** |
| egui 0.37 + glow paint callback (ADR 0014 chrome) | 0 ms / 30 s | **107.0 MiB private, 73.0 MiB working set, 12 threads, 366 handles** — Kim Manager preview, same sampler, same run | egui text: not a cell grid, no colour emoji (ADR 0014) | Kim Manager preview: 8.2 MB | needs an OpenGL driver; WARP/wgpu fallback costs more | yes | Rejected for the terminal surface |
| egui + wgpu (DX12) | 0 ms | ~600 MiB private (ADR 0014) | as above | 11 MiB | DX12 | yes | Rejected |
| Direct2D/DirectWrite surface | ~0 | not built; a D2D device context per window | best on Windows (ClearType, colour emoji, shaping) | COM via the `windows` crate or hand-written vtables | native | no — a second renderer | Kept as the **upgrade path for the rasterizer only** (colour emoji), behind the same boundary |
| Pure-Rust rasterizer (`swash` 0.2.10, `cosmic-text` 0.19.0, `fontdue` 0.9.5) | 0 | each process reads the font files it uses (Malgun Gothic alone is 4.4 MB) instead of sharing the system font cache | `swash`: colour outlines; `fontdue`: no hinting, no colour | +0.3–2 MB | portable | **yes** | Candidate for the Linux rasterizer (M4), not for Windows |

Sampler: `cargo run --release -p subak-term --example term_bench -- --only idle --compare --
<kim-manager preview>`: both processes are started with no console, place their window
off-screen and unactivated, are sampled after a warm-up and again after 30 s idle
(`GetProcessTimes`, `GetProcessMemoryInfo`, Toolhelp threads, `GetProcessHandleCount`), and are
ended. Raw data: [research/terminal-eval/results.json](../../research/terminal-eval/results.json).

## Decision

1. **One emulation core, in-house, in a new crate `subak-term`.** The Step 13 model moves out of
   the `subak` binary into `crates/subak-term` (headless, no dependencies) and is used by the
   Host *and* by every client view, so both parse every byte the same way (`SUBAK-FR-004`).
   Step 15 extends it with OSC 8 hyperlinks (per cell, replayed in snapshots and kept in the
   history), explicit mouse tracking and encoding modes (one at a time, as xterm), soft wraps in
   snapshots, a public read API for views and renderers, and **device reports that only a view
   answers**: the Host never answers DA/DSR/CPR/DECRQM (ADR 0030); the view that owns the pane's
   size may (`Screen::set_answer_reports`), so a program gets exactly one answer.
   *Why not a crate:* none of the candidates keeps a compact, replayable history or a VT
   snapshot, and the full models pull PTY and event-loop code into the headless Host; the
   parser alone (`vte`) would save little and split the semantics across two codebases.
2. **Client view state is testable without rendering** (`subak_term::view`, `input`): the
   scrollback viewport (anchored while output arrives), selection and copy (cell, word, line,
   block; wide characters once, combining marks kept, soft wraps joined), hyperlink hit tests,
   xterm key encoding and win32-input-mode, sanitized bracketed paste, mouse reports for modes
   9/1000/1002/1003 in the default, UTF-8, SGR, urxvt and SGR-pixel encodings, the wheel's local
   scrolling and alternate scroll, focus reports.
3. **The terminal surface is drawn on the CPU** (`subak_term::render`, feature `render`, off for
   the Host): a fixed cell grid into a `0x00RRGGBB` frame, only rows whose hash changed are
   drawn again and reported as damage, nothing runs between frames (no timer, thread or GPU
   context). Glyphs come from a `Rasterizer`: **GDI** on Windows with the design system's mono
   chain (Cascadia Mono → Consolas → D2Coding → Malgun Gothic → Segoe UI Emoji → Segoe UI
   Symbol), each face recognised through its `name` table and chosen per character through its
   `cmap`, grayscale antialiasing, fallback faces at the primary em size on its baseline; a
   **deterministic built-in rasterizer** for golden tests and other platforms. Box drawing and
   block elements are drawn procedurally so lines join at any line height. The frame is
   presentation-agnostic; on Windows it is presented with `SetDIBitsToDevice` in `WM_PAINT`.
4. **Consequence for the Subak window (Step 16):** the terminal surface needs no GPU. A Subak
   window whose chrome is egui + glow would cost ~100 MiB private memory idle for chrome alone —
   ADR 0014's fallback B trigger ("memory budget missed or GPU stack undesired"). Step 16 builds
   the window on plain Win32 with this renderer (as Noeul does, ADR 0023), or records in its own
   ADR why it accepts the GPU stack's cost.
5. **Inbox ConPTY stays** (ADR 0004 item 7). Measured on Windows 10 19045: the inbox ConPTY
   passes bracketed paste (2004) and focus events (1004) through, but **not** mouse tracking
   (1000/1006) or OSC 8; conhost renders those itself. Shipping `conpty.dll` + `OpenConsole.exe`
   would add a second binary set with its own update and signing path (`KIM-SEC-001`,
   RELEASE_POLICY) for mouse-driven TUIs and hyperlinks on Windows 10. Subak keeps the inbox
   ConPTY for M2; the emulator and view already handle both when a newer ConPTY (or Linux) sends
   them, so revisiting this is a packaging decision, not an emulator change. Sixel stays blocked.
6. **Security of terminal output** (`SUBAK-SEC-013`): output never makes the client act on its
   own. Hyperlinks are only hit-tested (opening one is an explicit user action that shows the
   target, Step 19); OSC 52 clipboard writes are ignored and never replayed; device reports are
   answered by one view only and bounded; pasted text loses every control character except tab
   and line breaks, so it cannot end a bracketed paste early or inject sequences.

## Consequences

- One parser, two consumers: a Host change to the model is a client change, tested once.
  `crates/subak-term/tests/conformance.rs` checks every fixture also replays through a snapshot
  into a fresh model (what an attaching client does).
- Memory of a view: the model (~1.3 MB with 10 000 history lines at 120×30), the frame
  (`width × height × 4`: 2.0 MiB at 120×30 and 100 %, 7.4 MiB at 200 %, 32 MiB for 240×67 at
  200 %), the glyph cache (18–55 KiB for a typical screen, capped at 8 MiB).
- Rendering cost (release, this machine): a warm full frame of 120×30 in 0.29 ms at 100 % and
  0.84 ms at 200 %; one changed row in 0.03 ms; keyboard latency inside the view 31 µs (p50);
  through Host + ConPTY + cmd 15.5 ms (p50), dominated by ConPTY's output cadence.
- Colour emoji are monochrome until a DirectWrite rasterizer exists (DESIGN_SYSTEM §4.4 allows
  the monochrome fallback). IME composition drawing is Step 16's.
- `subak-term` is a headless package for `cargo xtask deps-check`; the renderer uses GDI through
  `windows-sys`, no GUI framework.

## Not tested (and why)

- **A visible window on the maintainer's desktop.** Every render in tests is headless (memory DC
  and frames compared with goldens); the idle probe's window is placed off-screen and never
  activated. The manual check is TERMINAL_EMULATOR.md §9 (`term_bench --show`).
- Windows 11 and other Windows 10 builds: the ConPTY passthrough above was measured on 19045
  (the CI runner, Windows Server 2025 build 26100, runs the same probe and logs its result).
- High-DPI on a real multi-monitor setup: scales 100–200 % are rendered headlessly; moving a
  window between monitors is Step 16's.

## Alternatives considered

See the two tables. Also: rendering each glyph with GDI directly into the window DC per frame
(no glyph cache, no frame) — fewer bytes, but a repaint after occlusion re-runs text layout,
and tests could not compare frames; keeping the Host model private to `subak` and copying it
into the GUI — two divergent emulators, which this Step's assignment forbids.

## Requirements

`SUBAK-FR-004`, `SUBAK-FR-013`, `SUBAK-FR-031`, `SUBAK-FR-036`, `SUBAK-NFR-001`,
`SUBAK-SEC-013`, `KIM-NFR-001`, `KIM-NFR-002`, `KIM-NFR-011`, `KIM-NFR-012`, `KIM-NFR-013`.
