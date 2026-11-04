# Terminal emulator evaluation (Step 15)

Evidence for [ADR 0036](../../docs/adr/0036-terminal-emulation-and-rendering-path.md) and
[TERMINAL_EMULATOR §7](../../docs/TERMINAL_EMULATOR.md). Unlike `research/gui-eval`, the
harness is part of the workspace: `crates/subak-term/examples/term_bench.rs` (not shipped).

## Reproduce

```powershell
cargo build --release -p subak-term --example term_bench
cargo build --release -p kim-manager --example preview      # the egui + glow comparison
target\release\examples\term_bench.exe --json --idle-secs 30 `
    --compare -- target\release\examples\preview.exe --idle 60 > results.json
```

The idle section starts the harness itself again (`--idle-child`): a 120×30 view with 2 000
history lines and the demo screen, its renderer with the system fonts, and a popup tool window at
(-20000, -20000), shown without activation, that presents the frame with `SetDIBitsToDevice` on
`WM_PAINT`. `--compare` starts another program the same way (Kim Manager's preview places its
egui + glow window off-screen and unactivated by itself). Both are sampled after a warm-up and
after 30 s idle, then ended. Nothing appears on the desktop.

## Results (2026-11-04)

Machine: Intel Core i5-13600KF, 32 GB RAM, NVIDIA GeForce RTX 4070 Ti, Windows 10 Pro 22H2 build
19045, rustc 1.99.1 `x86_64-pc-windows-gnu`, release profile (thin LTO). Raw data:
[results.json](results.json); the summary is in TERMINAL_EMULATOR §7.

| Idle, 30 s | CPU | Private | Working set | Threads | Handles |
| --- | --- | --- | --- | --- | --- |
| CPU renderer + GDI window (subak-term) | 0 ms | 3.40 MiB | 8.10 MiB | 1 | 95 |
| egui 0.37 + glow (Kim Manager preview) | 0 ms | 107.0 MiB | 73.0 MiB | 12 | 366 |

Binary sizes: `term_bench.exe` (renderer, view, model, harness, `serde_json`) 0.66 MiB; the Kim
Manager preview (egui + glow GUI) 8.2 MB.
