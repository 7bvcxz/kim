"""Render static design mock-ups from design/tokens.json (developer tool, not shipped).

    python -I design/mockups/render.py

Requires Pillow and the Windows fonts named in the tokens (Segoe UI, Cascadia Mono, Malgun Gothic).
Writes PNGs next to this script at 1x and 2x scale. The mock-ups illustrate the design system;
they are not screenshots of implemented software.
"""

import json
import sys
from pathlib import Path

from PIL import Image, ImageDraw, ImageFont

HERE = Path(__file__).resolve().parent
TOKENS = json.loads((HERE.parent / "tokens.json").read_text(encoding="utf-8"))
C = TOKENS["themes"]["kim-dark"]["color"]
FONTS = Path(r"C:\Windows\Fonts")


def rgb(token, alpha=255):
    h = C[token].lstrip("#")
    return tuple(int(h[i : i + 2], 16) for i in (0, 2, 4)) + (alpha,)


class Canvas:
    def __init__(self, w, h, scale):
        self.s = scale
        self.img = Image.new("RGBA", (w * scale, h * scale), rgb("bg.base"))
        self.d = ImageDraw.Draw(self.img)
        self._fonts = {}

    def font(self, kind, size, bold=False):
        key = (kind, size, bold)
        if key not in self._fonts:
            name = {
                ("ui", False): "segoeui.ttf",
                ("ui", True): "segoeuib.ttf",
                ("mono", False): "CascadiaMono.ttf",
                ("mono", True): "CascadiaMono.ttf",
                ("ko", False): "malgun.ttf",
                ("ko", True): "malgunbd.ttf",
            }[(kind, bold)]
            self._fonts[key] = ImageFont.truetype(str(FONTS / name), int(size * self.s))
        return self._fonts[key]

    def rect(self, x, y, w, h, fill, radius=0, outline=None, width=1):
        s = self.s
        box = [x * s, y * s, (x + w) * s - 1, (y + h) * s - 1]
        self.d.rounded_rectangle(box, radius=radius * s, fill=fill, outline=outline, width=width * s if outline else 0)

    def text(self, x, y, s, color, size=12, kind="ui", bold=False):
        # Hangul falls back to Malgun Gothic, as the font chain in the tokens specifies.
        cx = x
        for run, is_ko in split_runs(s):
            f = self.font("ko" if is_ko else kind, size, bold)
            self.d.text((cx * self.s, y * self.s), run, font=f, fill=color)
            cx += self.d.textlength(run, font=f) / self.s
        return cx

    def dot(self, cx, cy, state):
        s = self.s
        r = 4
        box = [(cx - r) * s, (cy - r) * s, (cx + r) * s, (cy + r) * s]
        if state == "unknown":
            self.d.ellipse(box, outline=rgb("claude.unknown"), width=max(1, int(1.5 * s)))
        elif state == "error":
            self.d.polygon([(cx * s, (cy - r) * s), ((cx + r) * s, (cy + r) * s), ((cx - r) * s, (cy + r) * s)], fill=rgb("claude.error"))
        else:
            self.d.ellipse(box, fill=rgb("claude." + state))
            if state == "need-input":
                o = r + 2
                self.d.ellipse([(cx - o) * s, (cy - o) * s, (cx + o) * s, (cy + o) * s], outline=rgb("claude.need-input"), width=max(1, s))

    def save(self, name):
        self.img.convert("RGB").save(HERE / f"{name}@{self.s}x.png", optimize=True)


def split_runs(s):
    runs, cur, cur_ko = [], "", None
    for ch in s:
        ko = "\uac00" <= ch <= "\ud7a3" or "\u3130" <= ch <= "\u318f"
        if cur and ko != cur_ko:
            runs.append((cur, cur_ko))
            cur = ""
        cur += ch
        cur_ko = ko
    if cur:
        runs.append((cur, cur_ko))
    return runs


def terminal_window(scale):
    W, H = 900, 520
    c = Canvas(W, H, scale)
    c.rect(0, 0, W, 32, rgb("bg.raised"))
    tabs = [("work", "in-progress", True), ("서버", "none", False), ("claude", "need-input", False), ("notes", "done", False)]
    x = 8
    for name, state, active in tabs:
        w = 150
        c.rect(x, 4, w, 28, rgb("bg.base") if active else rgb("bg.raised"), radius=6)
        if active:
            c.rect(x + 6, 4, w - 12, 2, rgb("accent"))
        if state != "none":
            c.dot(x + 14, 18, state)
        c.text(x + 26, 9, name, rgb("text.primary") if active else rgb("text.secondary"), 12, bold=active)
        if active:
            c.text(x + w - 18, 8, "×", rgb("text.muted"), 13)
        x += w + 2
    c.text(x + 8, 7, "+", rgb("text.secondary"), 15)
    for i, glyph in enumerate(["–", "□", "×"]):
        c.text(W - 46 * (3 - i) + 18, 8, glyph, rgb("text.secondary"), 12)
    lines = [
        ("PS C:\\Users\\me\\src\\kim> ", "ansi.green", "cargo test --workspace", "terminal.foreground"),
        ("   Compiling ", "ansi.green", "subak v0.1.0", "terminal.foreground"),
        ("test ", "terminal.foreground", "session::tests::rename_conflict ... ok", "ansi.bright-black"),
        ("warning: ", "ansi.yellow", "unused variable `pane`", "terminal.foreground"),
        ("error[E0308]: ", "ansi.red", "mismatched types", "terminal.foreground"),
        ("안녕하세요 ", "terminal.foreground", "— 한글은 두 칸 너비로 정렬됩니다", "ansi.cyan"),
        ("╭─ ", "ansi.magenta", "claude  ·  in progress", "ansi.magenta"),
        ("│ ", "ansi.magenta", "Reading crates/subak/src/host.rs", "ansi.bright-black"),
        ("╰─ ", "ansi.magenta", "", "ansi.magenta"),
    ]
    y = 44
    for a, ca, b, cb in lines:
        xe = c.text(16, y, a, rgb(ca), 13, kind="mono")
        c.text(xe, y, b, rgb(cb), 13, kind="mono")
        y += 16
    c.text(16, y, "PS C:\\Users\\me\\src\\kim> ", rgb("ansi.green"), 13, kind="mono")
    c.rect(16 + 200, y + 1, 8, 15, rgb("terminal.cursor"))
    # split pane on the right
    c.rect(560, 32, 1, H - 54, rgb("border.subtle"))
    c.text(576, 44, "~/logs> tail -f host.log", rgb("terminal.foreground"), 13, kind="mono")
    c.text(576, 60, "attach t-3fa9c0d21b7e ok", rgb("ansi.bright-black"), 13, kind="mono")
    c.rect(0, H - 22, W, 22, rgb("bg.raised"))
    c.text(10, H - 18, "[work] 0:pwsh*  1:logs-  ·  2 panes  ·  host ok", rgb("text.muted"), 11, kind="mono")
    c.save("subak-terminal")


def catalog(scale):
    W, H = 820, 480
    c = Canvas(W, H, scale)
    c.rect(0, 0, W, 40, rgb("bg.raised"))
    c.text(16, 11, "Kim", rgb("text.primary"), 15, bold=True)
    c.text(60, 14, "Tool catalog · offline cache from 2026-11-03", rgb("text.muted"), 11)
    c.rect(0, 40, 340, H - 40, rgb("bg.raised"))
    rows = [("Subak", "Persistent Windows terminal", "0.1.0", "Available", "status.info"),
            ("Kim", "Catalog and explicit installer", "0.1.0", "Installed", "status.success"),
            ("Example", "Not available for this platform", "1.2.0", "Incompatible", "status.warning")]
    y = 48
    for i, (name, desc, ver, status, col) in enumerate(rows):
        sel = i == 0
        if sel:
            c.rect(8, y, 324, 48, rgb("accent.soft"), radius=6)
            c.rect(8, y + 10, 2, 28, rgb("accent"))
        c.rect(18, y + 8, 32, 32, rgb("bg.overlay"), radius=6)
        if name == "Kim":
            icon = Image.open(HERE.parent.parent / "assets" / "icon" / f"kim-{32 * scale}.png").convert("RGBA")
            c.img.alpha_composite(icon, (18 * scale, (y + 8) * scale))
        c.text(60, y + 7, name, rgb("text.primary"), 13, bold=True)
        c.text(60, y + 26, desc, rgb("text.muted"), 11)
        c.text(270, y + 8, ver, rgb("text.muted"), 11, kind="mono")
        c.text(258, y + 26, status, rgb(col), 11)
        y += 54
    c.text(364, 60, "Subak", rgb("text.primary"), 15, bold=True)
    c.text(364, 84, "Persistent, Windows-first terminal. Sessions live in a background", rgb("text.secondary"), 12)
    c.text(364, 102, "Host and survive closing every window.", rgb("text.secondary"), 12)
    meta = [("Version", "0.1.0 (stable)"), ("License", "MIT OR Apache-2.0"), ("Platform", "Windows x86_64"), ("Signature", "verified · mouv release key")]
    y = 134
    for k, v in meta:
        c.text(364, y, k, rgb("text.muted"), 11)
        c.text(470, y, v, rgb("text.primary"), 12)
        y += 22
    c.text(364, 236, "Downloading 12.4 / 38.0 MB", rgb("text.secondary"), 12)
    c.rect(364, 258, 420, 4, rgb("bg.selected"), radius=2)
    c.rect(364, 258, int(420 * 12.4 / 38), 4, rgb("accent"), radius=2)
    c.rect(364, 278, 96, 28, rgb("bg.base"), radius=4, outline=rgb("border.strong"))
    c.text(392, 283, "Cancel", rgb("text.primary"), 12)
    c.rect(364, 330, 420, 64, rgb("bg.overlay"), radius=6, outline=rgb("status.danger"))
    c.text(378, 340, "Install failed · signature_invalid", rgb("status.danger"), 12, bold=True)
    c.text(378, 362, "Nothing was changed on this computer. Retry or copy details.", rgb("text.secondary"), 11)
    c.rect(668, 410, 116, 28, rgb("accent"), radius=4)
    c.text(700, 415, "Install", rgb("text.on-accent"), 12, bold=True)
    c.save("kim-catalog")


def components(scale):
    W, H = 760, 300
    c = Canvas(W, H, scale)
    c.text(16, 12, "Claude indicator", rgb("text.secondary"), 12, bold=True)
    states = ["idle", "in-progress", "need-input", "error", "done", "unknown"]
    for i, st in enumerate(states):
        x = 24 + i * 120
        c.dot(x, 48, st)
        c.text(x + 12, 40, st, rgb("text.primary"), 12)
    # dialog
    c.rect(16, 80, 440, 200, rgb("bg.overlay"), radius=10, outline=rgb("border.subtle"))
    c.text(36, 98, "Close Terminal “work”?", rgb("text.primary"), 15, bold=True)
    c.text(36, 128, "This ends 2 running processes:", rgb("text.secondary"), 12)
    c.text(52, 150, "cargo run   ·   claude", rgb("text.primary"), 12, kind="mono")
    c.text(36, 178, "Closing the window instead keeps everything running.", rgb("text.muted"), 11)
    c.rect(248, 230, 92, 30, rgb("bg.base"), radius=4, outline=rgb("focus.ring"), width=2)
    c.text(272, 236, "Cancel", rgb("text.primary"), 12)
    c.rect(348, 230, 92, 30, rgb("bg.overlay"), radius=4, outline=rgb("status.danger"))
    c.text(358, 236, "End Terminal", rgb("status.danger"), 12)
    # floating icon + compact manager
    c.rect(500, 90, 40, 40, rgb("bg.overlay"), radius=10, outline=rgb("border.subtle"))
    c.text(510, 99, "수", rgb("accent"), 15, bold=True)
    c.dot(534, 124, "need-input")
    c.rect(500, 140, 240, 140, rgb("bg.overlay"), radius=10, outline=rgb("border.subtle"))
    c.rect(510, 150, 220, 24, rgb("bg.base"), radius=4, outline=rgb("border.strong"))
    c.text(518, 154, "Search terminals", rgb("text.muted"), 11)
    for i, (n, st) in enumerate([("work", "in-progress"), ("claude", "need-input"), ("서버", "idle")]):
        y = 186 + i * 28
        c.dot(522, y + 9, st)
        c.text(534, y + 1, n, rgb("text.primary"), 12)
        c.text(660, y + 2, "running", rgb("text.muted"), 11)
    c.save("components")


if __name__ == "__main__":
    for s in (1, 2):
        terminal_window(s)
        catalog(s)
        components(s)
    print("mock-ups written to", HERE)
