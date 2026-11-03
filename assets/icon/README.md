# Kim icon

Kim (김) is named after the Korean roasted laver sheet — so the icon is one.

| File | Use |
| --- | --- |
| `kim.ico` | Windows executables and shortcuts (16, 20, 24, 32, 40, 48, 64, 128, 256 px, PNG-compressed entries, ≈ 50 KB) |
| `kim-{16,32,48,64,128,256}.png` | GUI, docs, catalog (palette PNGs with alpha, ≈ 0.5–33 KB) |

All files are generated from the original photo by `make_icons.py` (developer tool, needs
Pillow and numpy; not part of any shipped binary). It cuts the sheet out of the white studio
background, removes the shadow and white edge fringe, crops square, slightly lifts
contrast/saturation for small sizes, and palette-quantizes the output.

```sh
python -I assets/icon/make_icons.py path/to/kim-icon.png
```
