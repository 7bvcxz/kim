"""Regenerate the Kim icon set from the original photo.

Developer-only asset tool (not part of any shipped binary). Requires Pillow and numpy.

    python -I assets/icon/make_icons.py <path-to-original-kim-icon.png>

Steps: cut the laver (gim) sheet out of its white studio background with a soft alpha so the
irregular, translucent edge survives; drop the shadow; remove the white fringe from edge pixels;
crop square with a small margin; lift contrast and saturation slightly so the sheet still reads
at 16 px; then write palette-quantized PNGs and a PNG-compressed multi-size Windows .ico
(Windows Vista+ format; Kim targets Windows 10 1809+).
"""

import io
import struct
import sys
from pathlib import Path

import numpy as np
from PIL import Image, ImageDraw, ImageEnhance, ImageFilter

OUT = Path(__file__).resolve().parent
PNG_SIZES = [16, 32, 48, 64, 128, 256]
ICO_SIZES = [16, 20, 24, 32, 40, 48, 64, 128, 256]


def colors_for(size: int) -> int:
    return 48 if size <= 48 else 96


def cut_out(src: Image.Image) -> Image.Image:
    rgb = np.asarray(src.convert("RGB")).astype(np.float32)
    # The studio background and its soft grey shadow are bright and unsaturated
    # (measured: shadow saturation <= 30, darkness <= 66). The sheet is either dark
    # (darkness >= 150) or, at its translucent edge, clearly yellow-green (saturation >= 50).
    sat = rgb.max(axis=2) - rgb.min(axis=2)
    dark = 255.0 - rgb.max(axis=2)
    soft = np.maximum(np.clip((sat - 30.0) / 30.0, 0.0, 1.0), np.clip((dark - 90.0) / 60.0, 0.0, 1.0))

    # Background = background-like pixels connected to the image border. Interior glints of
    # the glossy sheet are not connected to the border, so they stay opaque.
    bg_mask = (dark < 100.0) & (sat < 45.0)
    bg_like = Image.fromarray((bg_mask * 255).astype(np.uint8), "L").copy()  # writable
    h, w = bg_mask.shape
    for seed in [(0, 0), (w - 1, 0), (0, h - 1), (w - 1, h - 1)]:
        if bg_like.getpixel(seed) == 255:
            ImageDraw.floodfill(bg_like, seed, 128)
    outside = np.asarray(bg_like) == 128

    alpha = np.where(outside, soft, 1.0)
    alpha = np.asarray(
        Image.fromarray((alpha * 255).astype(np.uint8), "L").filter(ImageFilter.GaussianBlur(0.8))
    ).astype(np.float32) / 255.0

    # Decontaminate: edge pixels were photographed over white, C = a*F + (1-a)*255.
    a = np.clip(alpha, 1e-3, 1.0)[..., None]
    fg = np.where(alpha[..., None] < 0.999, (rgb - (1.0 - a) * 255.0) / a, rgb)
    fg = np.clip(fg, 0.0, 255.0)

    rgba = np.dstack([fg, alpha * 255.0]).round().astype(np.uint8)
    return Image.fromarray(rgba, "RGBA")


def square_crop(img: Image.Image, margin: float = 0.04) -> Image.Image:
    a = np.asarray(img.getchannel("A"))
    ys, xs = np.where(a > 24)
    x0, x1, y0, y1 = xs.min(), xs.max(), ys.min(), ys.max()
    side = int(max(x1 - x0, y1 - y0) * (1 + 2 * margin))
    cx, cy = (x0 + x1) // 2, (y0 + y1) // 2
    left, top = cx - side // 2, cy - side // 2
    canvas = Image.new("RGBA", (side, side), (0, 0, 0, 0))
    canvas.alpha_composite(img.crop((left, top, left + side, top + side)))
    return canvas


def polish(img: Image.Image) -> Image.Image:
    alpha = img.getchannel("A")
    rgb = img.convert("RGB")
    rgb = ImageEnhance.Contrast(rgb).enhance(1.10)
    rgb = ImageEnhance.Color(rgb).enhance(1.18)
    rgb = ImageEnhance.Brightness(rgb).enhance(1.04)
    rgb.putalpha(alpha)
    return rgb


def resized(master: Image.Image, size: int) -> Image.Image:
    img = master.resize((size, size), Image.Resampling.LANCZOS)
    if size <= 64:
        img = img.filter(ImageFilter.UnsharpMask(radius=0.6, percent=60, threshold=1))
    return img


def png_bytes(master: Image.Image, size: int) -> bytes:
    img = resized(master, size).quantize(
        colors=colors_for(size), method=Image.Quantize.FASTOCTREE, dither=Image.Dither.FLOYDSTEINBERG
    )
    buf = io.BytesIO()
    img.save(buf, format="PNG", optimize=True)
    return buf.getvalue()


def write_ico(path: Path, entries: list[tuple[int, bytes]]) -> None:
    """ICONDIR + ICONDIRENTRY table with PNG-compressed images."""
    header = struct.pack("<HHH", 0, 1, len(entries))
    offset = len(header) + 16 * len(entries)
    table, blobs = b"", b""
    for size, data in entries:
        dim = 0 if size >= 256 else size
        table += struct.pack("<BBBBHHII", dim, dim, 0, 0, 1, 32, len(data), offset)
        blobs += data
        offset += len(data)
    path.write_bytes(header + table + blobs)


def main() -> None:
    master = polish(square_crop(cut_out(Image.open(sys.argv[1]))))
    for size in PNG_SIZES:
        (OUT / f"kim-{size}.png").write_bytes(png_bytes(master, size))
    write_ico(OUT / "kim.ico", [(s, png_bytes(master, s)) for s in ICO_SIZES])


if __name__ == "__main__":
    main()
