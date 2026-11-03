# ADR 0014 — GUI technology for Kim Manager and Subak

Status: **Accepted** · Date: 2026-11-03 · Step 04 · Re-validated in Steps 15–16 (terminal
renderer, RDP/VM behavior, input latency)

## Context

Kim Manager (Step 06) and the Subak GUI (Steps 16–20) need a native GUI that is beautiful and
compact, uses near-zero resources while idle, renders Korean/CJK and symbols well, is accessible
(UI Automation), supports Korean IME, runs on Windows now and Linux later, and must never be
linked into headless binaries ([ADR 0002](0002-independent-tool-binaries.md)). The project
forbids Electron and embedded browsers. "Rust is fast" is not evidence: we measured.

## Evidence

Prototypes and raw data: [research/gui-eval](../../research/gui-eval/README.md) (i5-13600KF,
RTX 4070 Ti, Windows 10 19045, release builds, 3 runs each, 30 s idle window).

| Stack | Binary | First frame | Idle CPU (30 s) | Working set | Private | Threads |
| --- | --- | --- | --- | --- | --- | --- |
| winit + softbuffer (bare window, CPU) | 0.6 MiB | 15–23 ms | 0 ms | 13 MiB | 5 MiB | 5 |
| **egui 0.37 + glow (OpenGL)** | 5.7 MiB | 171–280 ms | 0 ms | 69 MiB (125 cold) | 98 MiB | 12 |
| egui 0.37 + wgpu 30 (DX12) | 11.0 MiB | ~1.6 s | 0 ms | 300–338 MiB | ~600 MiB | 66 |

Glyph tests (framebuffer read-back, no visible window): egui renders Korean through a
Malgun Gothic fallback and symbols through Segoe UI Symbol, but **color emoji are not
supported** (monochrome outlines) and its label layout is **not a terminal cell grid** (Hangul
not aligned to two cells).

## Options considered

| Option | Idle cost | Glyphs / CJK | Accessibility | IME | Windows | Linux later | Headless UI tests | Verdict |
| --- | --- | --- | --- | --- | --- | --- | --- | --- |
| **egui/eframe + glow** | 0 CPU, ~70 MiB (GL driver) | good for UI text with a fallback chain; no color emoji; not a cell grid | AccessKit → UI Automation | yes (winit) | yes | yes (same code) | yes (egui_kittest, offscreen) | **Chosen for UI chrome** |
| egui/eframe + wgpu | 0 CPU, ~300 MiB, 1.6 s start | same as above | same | same | yes | yes | yes | Rejected: 4× memory, slow start; kept as fallback renderer if GL is unavailable (risk R-14) |
| Bare winit + softbuffer + own UI | 0 CPU, ~13 MiB | whatever we build | via accesskit_winit, by hand | by hand | yes | yes | by hand | Too much UI work for chrome; **the right foundation if GPU memory must go** (fallback B) |
| iced | not measured | own text stack (cosmic-text) | weaker accessibility story at time of writing | partial | yes | yes | limited | Not chosen; re-evaluate only if egui fails |
| Slint | not measured | good | AccessKit | yes | yes | yes | yes | Rejected: GPL-3.0 / royalty-free-with-attribution / commercial licensing conflicts with the permissive-only dependency policy (`deny.toml`) |
| Native Win32 + Direct2D/DirectWrite | lowest | best on Windows (ClearType, system fallback, color emoji) | UIA by hand or AccessKit | native | yes | **no** — second GUI for Linux | hard | Not for chrome; **fallback A for the terminal surface** on Windows |
| Tauri/WebView2, Electron | high | browser-grade | good | good | yes | yes | yes | Excluded by philosophy (no embedded browser) |

## Decision

1. **UI chrome** of both GUIs (Kim Manager window; Subak tabs, menus, dialogs, session manager,
   floating icon) uses **egui/eframe with the glow renderer**, reactive repainting only, the
   Kim tokens ([DESIGN_SYSTEM](../DESIGN_SYSTEM.md)), AccessKit enabled, and the font fallback
   chain from `design/tokens.json` loaded from system fonts at runtime (no bundled CJK fonts —
   keeps binaries small).
2. **The terminal grid is not drawn with egui text layout.** Step 15 builds a dedicated
   cell-grid renderer (fixed cell advance, East-Asian-width aware, glyph atlas, color-glyph
   capable shaping/fallback — candidates `cosmic-text`/`swash`, or DirectWrite) that paints into
   the egui frame via a paint callback. Step 15's ADR picks it with conformance and latency
   measurements.
3. **Budgets** (`KIM-NFR-002`): Kim Manager idle working set target < 80 MiB is met by glow on
   the reference machine (69 MiB warm). Subak GUI target is set in Step 16 with measurements
   including the terminal renderer.
4. **Fallbacks and their triggers**:
   - *GL unavailable or broken* (RDP without GPU, VMs, old drivers) → try wgpu with its DX12
     WARP software adapter for that session; if memory is unacceptable, fallback B.
   - *Memory budget missed or GPU stack undesired* → **fallback B**: winit + softbuffer with a
     CPU renderer for chrome and terminal (the bare-window floor is 13 MiB).
   - *Glyph quality, color emoji or IME not acceptable on Windows* → **fallback A**: native
     DirectWrite surface for the terminal grid only, keeping egui chrome.
5. **Isolation**: GUI crates (`eframe`, renderer) are dependencies only of `kim-manager` and
   `subak-gui` binaries. A test in each headless crate (Step 06/16) asserts that its dependency
   tree contains no `eframe`/`egui`/`winit`.

## Consequences

- One toolkit for both GUIs and both OSes; headless UI tests can render offscreen (good for
  the maintainer-requested headless workflow).
- The GL driver's memory (~55 MiB) is the price of GPU rendering; it is measured and bounded,
  with a documented CPU fallback.
- Terminal text quality depends on Step 15, not on egui.

## Requirements

`KIM-NFR-001`, `KIM-NFR-002`, `KIM-NFR-012`, `KIM-NFR-013`, `KIM-FR-009`, `KIM-FR-012`,
`SUBAK-FR-031`, `KIM-NFR-011`.
