# Kim design assets

| Path | What |
| --- | --- |
| `tokens.json` | Design tokens v1 (colors, type, space, radius, sizes, motion) — the source of truth for both GUIs. Contrast requirements are declared in the file and enforced by `cargo xtask design-check` (also run by `cargo test`). |
| `mockups/*.png` | Static mock-ups at 1× and 2×, rendered **from the tokens** by `mockups/render.py`. They illustrate the design system; they are not screenshots of implemented software. |
| `../assets/icon/` | Kim icon set (see its README). |

Regenerate the mock-ups (developer tool; needs Python with Pillow and the Windows fonts Segoe
UI, Cascadia Mono and Malgun Gothic):

```sh
python -I design/mockups/render.py
```

The specification is [docs/DESIGN_SYSTEM.md](../docs/DESIGN_SYSTEM.md); the GUI toolkit decision
and its measurements are in [docs/adr/0014-gui-technology.md](../docs/adr/0014-gui-technology.md).
