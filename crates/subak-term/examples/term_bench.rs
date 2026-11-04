//! The terminal emulator benchmark harness (ADR 0036, docs/TERMINAL_EMULATOR.md §7).
//!
//!     cargo run --release -p subak-term --example term_bench -- [--quick] [--json]
//!         [--only parse,render,long,fallback,dpi,theme,latency,idle] [--idle-secs N]
//!         [--shot DIR] [--compare -- <program> [args]]
//!     cargo run --release -p subak-term --example term_bench -- --show
//!         [--attach <terminal> [--endpoint <ep>] [--subak <path>]] [--offscreen] [--exit-after N]
//!
//! Measures, in this process and without a visible window:
//!
//! - `parse` — VT parsing into the model (the Host's and every client view's cost per byte);
//! - `render` — full frames (cold and warm glyph cache), a one-row change and a scroll, at three
//!   grid sizes and three scales, with the system fonts (GDI) and the built-in rasterizer;
//! - `long` — 100 000 lines of output fed in chunks while frames are drawn at most every 16 ms;
//! - `fallback` — which face draws each sample character and what its first rasterization costs;
//! - `dpi` — cell metrics, frame size, full-frame time and glyph cache size per scale;
//! - `theme` — redrawing everything after a theme change;
//! - `latency` — keyboard latency in the client view: key event → bytes for the pane → the echo
//!   fed back → the frame ready (the Host and ConPTY round trip is measured by
//!   `crates/subak/tests/terminal_render.rs` on real panes);
//! - `idle` — a child process that renders a frame, presents it with GDI in a window placed
//!   off-screen (-20000,-20000; not activated, no taskbar button) and then idles: its CPU time,
//!   private bytes, working set, threads and handles are sampled, then it is terminated.
//!   `--compare -- <program>` samples another program the same way (Kim Manager's egui + glow
//!   preview places its own window off-screen and unactivated).
//!
//! `--shot DIR` writes BMP screenshots of the demo screen (system fonts, 100 % and 200 %, both
//! themes) for inspection. `--show` is the maintainer's visible check (docs/TERMINAL_EMULATOR.md
//! §9): a real, per-monitor-DPI-aware window with the demo screen, or with a live `subak attach`
//! session whose keys go through the view's input API; `--offscreen` runs the same window off
//! every monitor, unactivated, and closes it after `--exit-after` seconds. Not shipped.

use std::time::{Duration, Instant};
use subak_term::render::{BoxRasterizer, RenderOptions, Renderer, Theme};
use subak_term::{Key, Mods, TermView};

/// Mixed content: prompt, colours, Korean, CJK, emoji, combining marks, box drawing, blocks.
fn demo_bytes() -> Vec<u8> {
    let mut s = String::new();
    s.push_str(
        "\x1b[1;38;2;183;166;245mSubak\x1b[0m terminal renderer \x1b[2m(Step 15)\x1b[0m\r\n",
    );
    s.push_str("PS C:\\Users\\kim> \x1b[93mGet-ChildItem\x1b[0m \x1b[90m-Force\x1b[0m\r\n");
    s.push_str(
        "\x1b[32mMode\x1b[0m   \x1b[32mLastWriteTime\x1b[0m   \x1b[32mLength\x1b[0m Name\r\n",
    );
    s.push_str("d----  2026-11-04 09:00         \x1b[1;34msrc\x1b[0m\r\n");
    s.push_str("-a---  2026-11-04 09:01   2617  screen.rs\r\n");
    s.push_str("한글 출력: 안녕하세요, 수박입니다. 漢字 かな カナ\r\n");
    s.push_str("Emoji 😀 🍉 ✅ ⚠  symbols ✓ → ← ★  combining: e\u{301} a\u{308} n\u{303}\r\n");
    s.push_str("┌──────┬──────┐ ╔════╗ ┏━━━┓ ▁▂▃▄▅▆▇█ ░▒▓\r\n");
    s.push_str("│ cell │ grid │ ║ ok ║ ┃ H ┃ ▌▐▀▄ ▖▗▘▝\r\n");
    s.push_str("└──────┴──────┘ ╚════╝ ┗━━━┛\r\n");
    s.push_str(
        "\x1b[1mbold\x1b[0m \x1b[3mitalic\x1b[0m \x1b[4munderline\x1b[0m \x1b[21mdouble\x1b[0m ",
    );
    s.push_str("\x1b[9mstrike\x1b[0m \x1b[53moverline\x1b[0m \x1b[7minverse\x1b[0m ");
    s.push_str("\x1b]8;;https://example.com/subak\x1b\\hyperlink\x1b]8;;\x1b\\\r\n");
    for (i, n) in (0..16).enumerate() {
        s.push_str(&format!("\x1b[48;5;{n}m {i:x} "));
    }
    s.push_str("\x1b[0m\r\n");
    for i in 0..32 {
        let v = i * 8;
        s.push_str(&format!("\x1b[48;2;{v};{};{}m ", 255 - v, (v * 3) % 256));
    }
    s.push_str("\x1b[0m truecolour\r\n");
    s.push_str("\x1b[38;5;208mwarning:\x1b[0m long lines wrap at the edge of the grid and ");
    s.push_str("continue on the next row, exactly as the Host's model wraps them.\r\n");
    s.push_str("PS C:\\Users\\kim> \x1b[5 q");
    s.into_bytes()
}

fn view_with(cols: u16, rows: u16, bytes: &[u8]) -> TermView {
    let mut v = TermView::new(cols, rows, 2000);
    v.feed(bytes);
    v
}

fn system_renderer(scale: f32) -> Option<Renderer> {
    let opts = RenderOptions {
        scale,
        ..RenderOptions::default()
    };
    Renderer::with_system_fonts(Theme::kim_dark(), opts).ok()
}

fn builtin_renderer(scale: f32) -> Renderer {
    let opts = RenderOptions {
        scale,
        ..RenderOptions::default()
    };
    Renderer::new(Box::new(BoxRasterizer::new()), Theme::kim_dark(), opts)
}

fn median(mut v: Vec<f64>) -> f64 {
    v.sort_by(f64::total_cmp);
    v[v.len() / 2]
}

fn pct(mut v: Vec<f64>, p: f64) -> f64 {
    v.sort_by(f64::total_cmp);
    let i = ((v.len() as f64 - 1.0) * p).round() as usize;
    v[i.min(v.len() - 1)]
}

fn ms(d: Duration) -> f64 {
    d.as_secs_f64() * 1000.0
}

struct Out {
    json: bool,
    doc: serde_json::Map<String, serde_json::Value>,
}

impl Out {
    fn section(&mut self, name: &str, value: serde_json::Value) {
        self.doc.insert(name.to_string(), value);
    }

    fn say(&self, s: &str) {
        if !self.json {
            println!("{s}");
        }
    }
}

// ---------------------------------------------------------------------------------------------

fn bench_parse(o: &mut Out, quick: bool) {
    let total = if quick { 4 << 20 } else { 32 << 20 };
    let mut work: Vec<(&str, Vec<u8>)> = Vec::new();
    let mut ascii = Vec::new();
    let mut sgr = Vec::new();
    let mut korean = Vec::new();
    let mut emoji = Vec::new();
    for i in 0..2000 {
        ascii.extend(
            format!("line {i:05} the quick brown fox jumps over the lazy dog 0123456789\r\n")
                .bytes(),
        );
        sgr.extend(
            format!(
                "\x1b[32m{i:05}\x1b[0m \x1b[1;38;5;208mwarn\x1b[0m \x1b[38;2;10;200;30mok\x1b[0m \x1b[4mlink\x1b[24m text\r\n"
            )
            .bytes(),
        );
        korean.extend(format!("{i:05} 안녕하세요 수박 터미널입니다 한글 출력 테스트\r\n").bytes());
        emoji.extend(format!("{i:05} 😀 🍉 ✅ e\u{301} a\u{308} ★ → ┌─┐\r\n").bytes());
    }
    // A full-screen repaint as ConPTY sends it: home, each row at an absolute position.
    let mut tui = Vec::new();
    for f in 0..50 {
        tui.extend(b"\x1b[?25l\x1b[H");
        for r in 1..=30 {
            tui.extend(
                format!(
                    "\x1b[{r};1H\x1b[38;5;{}m{:<118}\x1b[K",
                    (r + f) % 256,
                    format!("row {r} frame {f}")
                )
                .bytes(),
            );
        }
        tui.extend(b"\x1b[?25h");
    }
    work.push(("ascii", ascii));
    work.push(("sgr", sgr));
    work.push(("korean", korean));
    work.push(("emoji", emoji));
    work.push(("tui-repaint", tui));
    let mut res = serde_json::Map::new();
    o.say("\n## parse (TermView::feed, 120x30, 2000 history lines)");
    o.say("| workload | MiB/s |");
    o.say("| --- | --- |");
    for (name, chunk) in &work {
        let mut v = TermView::new(120, 30, 2000);
        let t = Instant::now();
        let mut fed = 0;
        while fed < total {
            v.feed(chunk);
            fed += chunk.len();
        }
        let rate = fed as f64 / t.elapsed().as_secs_f64() / 1_048_576.0;
        o.say(&format!("| {name} | {rate:.1} |"));
        res.insert(
            (*name).into(),
            serde_json::json!({"mib_per_s": rate, "bytes": fed}),
        );
    }
    o.section("parse", serde_json::Value::Object(res));
}

fn bench_render(o: &mut Out, quick: bool) {
    let sizes: &[(u16, u16)] = if quick {
        &[(120, 30)]
    } else {
        &[(80, 24), (120, 30), (240, 67)]
    };
    let scales: &[f32] = if quick { &[1.0, 2.0] } else { &[1.0, 1.5, 2.0] };
    let reps = if quick { 5 } else { 15 };
    let demo = demo_bytes();
    let mut rows = Vec::new();
    o.say("\n## render (ms per frame, median)");
    o.say("| rasterizer | grid | scale | frame px | cold full | warm full | one row | scroll | frame MiB | glyph cache KiB |");
    o.say("| --- | --- | --- | --- | --- | --- | --- | --- | --- | --- |");
    for kind in ["gdi", "builtin"] {
        for &(cols, rows_n) in sizes {
            for &scale in scales {
                let make = || {
                    if kind == "gdi" {
                        system_renderer(scale)
                    } else {
                        Some(builtin_renderer(scale))
                    }
                };
                let Some(mut r) = make() else {
                    continue;
                };
                // Fill the whole grid: the demo repeated.
                let mut v = TermView::new(cols, rows_n, 2000);
                while v.screen().history().total_lines < 2 {
                    v.feed(&demo);
                    v.feed(b"\r\n");
                }
                let t = Instant::now();
                r.render(&v);
                let cold = ms(t.elapsed());
                let mut warm = Vec::new();
                for _ in 0..reps {
                    r.set_theme(Theme::kim_dark());
                    let t = Instant::now();
                    r.render(&v);
                    warm.push(ms(t.elapsed()));
                }
                let mut one = Vec::new();
                for i in 0..reps {
                    v.feed(if i % 2 == 0 { b"x" } else { b"\x08 \x08" });
                    let t = Instant::now();
                    let d = r.render(&v);
                    one.push(ms(t.elapsed()));
                    assert!(d.row_count() <= 2, "one row changed: {d:?}");
                }
                let mut scroll = Vec::new();
                for i in 0..reps {
                    v.feed(format!("\r\nscrolled line {i} 한글 😀").as_bytes());
                    let t = Instant::now();
                    r.render(&v);
                    scroll.push(ms(t.elapsed()));
                }
                let f = r.frame();
                let row = serde_json::json!({
                    "rasterizer": kind, "cols": cols, "rows": rows_n, "scale": scale,
                    "frame": [f.width, f.height], "cold_full_ms": cold, "warm_full_ms": median(warm.clone()),
                    "one_row_ms": median(one.clone()), "scroll_ms": median(scroll.clone()),
                    "frame_bytes": f.bytes(), "glyph_cache_bytes": r.glyph_cache().bytes(),
                    "glyphs": r.glyph_cache().len(),
                });
                o.say(&format!(
                    "| {kind} | {cols}x{rows_n} | {scale} | {}x{} | {cold:.2} | {:.2} | {:.3} | {:.2} | {:.1} | {} |",
                    f.width,
                    f.height,
                    median(warm),
                    median(one),
                    median(scroll),
                    f.bytes() as f64 / 1_048_576.0,
                    r.glyph_cache().bytes() / 1024
                ));
                rows.push(row);
            }
        }
    }
    o.section("render", serde_json::Value::Array(rows));
}

fn bench_long(o: &mut Out, quick: bool) {
    let lines = if quick { 20_000 } else { 100_000 };
    let mut bytes = Vec::new();
    for i in 0..lines {
        bytes.extend(format!("\x1b[32m{i:06}\x1b[0m build output line with some text, 한글 and a path C:\\src\\file{i}.rs\r\n").bytes());
    }
    let mut res = serde_json::Map::new();
    o.say("\n## long output (feed in 64 KiB chunks, draw at most every 16 ms, 120x30, 10 000 history lines)");
    for kind in ["gdi", "builtin"] {
        let r = if kind == "gdi" {
            system_renderer(1.0)
        } else {
            Some(builtin_renderer(1.0))
        };
        let Some(mut r) = r else {
            continue;
        };
        let mut v = TermView::new(120, 30, 10_000);
        let t = Instant::now();
        let mut last = Instant::now() - Duration::from_millis(20);
        let mut frames = 0;
        for chunk in bytes.chunks(64 * 1024) {
            v.feed(chunk);
            if last.elapsed() >= Duration::from_millis(16) {
                r.render(&v);
                frames += 1;
                last = Instant::now();
            }
        }
        r.render(&v);
        frames += 1;
        let secs = t.elapsed().as_secs_f64();
        let rate = bytes.len() as f64 / secs / 1_048_576.0;
        let h = v.screen().history();
        o.say(&format!(
            "- {kind}: {lines} lines ({:.1} MiB) in {secs:.2} s = {rate:.1} MiB/s with {frames} frames; history {} lines / {} KiB; model {} KiB; renderer {} KiB",
            bytes.len() as f64 / 1_048_576.0,
            h.lines,
            h.bytes / 1024,
            v.screen().memory_bytes() / 1024,
            r.memory_bytes() / 1024
        ));
        // Scrolling back through the history: rows decoded on demand.
        let t = Instant::now();
        for _ in 0..100 {
            v.scroll_lines(30);
            r.render(&v);
        }
        let scroll_ms = ms(t.elapsed()) / 100.0;
        o.say(&format!(
            "  scrolling back a page at a time: {scroll_ms:.2} ms per frame"
        ));
        res.insert(kind.into(), serde_json::json!({
            "lines": lines, "bytes": bytes.len(), "seconds": secs, "mib_per_s": rate, "frames": frames,
            "history_lines": h.lines, "history_bytes": h.bytes, "model_bytes": v.screen().memory_bytes(),
            "renderer_bytes": r.memory_bytes(), "scrollback_page_ms": scroll_ms,
        }));
    }
    o.section("long_output", serde_json::Value::Object(res));
}

fn bench_fallback(o: &mut Out) {
    let samples: [(&str, &str); 12] = [
        ("latin", "A"),
        ("hangul", "한"),
        ("hanja", "漢"),
        ("hiragana", "あ"),
        ("emoji", "😀"),
        ("emoji-bmp", "✅"),
        ("check", "✓"),
        ("arrow", "→"),
        ("box", "┼"),
        ("block", "▓"),
        ("combining", "e\u{301}"),
        ("unassigned", "\u{10FFFD}"),
    ];
    let mut res = serde_json::Map::new();
    o.say("\n## glyph and font fallback (system fonts, 100 %)");
    let Some(mut r) = system_renderer(1.0) else {
        o.say("system fonts unavailable on this platform");
        return;
    };
    o.say("| sample | char | face | fallback | cells | first draw (us) |");
    o.say("| --- | --- | --- | --- | --- | --- |");
    for (name, s) in samples {
        let ch = s.chars().next().unwrap_or(' ');
        let face = r.face_for(ch);
        let cells = subak_term::screen::char_width(ch).max(1);
        let mut v = TermView::new(4, 1, 0);
        v.feed(s.as_bytes());
        let t = Instant::now();
        r.render(&v);
        let us = t.elapsed().as_secs_f64() * 1e6;
        let fam = if face.found {
            face.family.clone()
        } else {
            "(none: hollow box)".into()
        };
        o.say(&format!(
            "| {name} | {s} | {fam} | {} | {cells} | {us:.0} |",
            face.fallback
        ));
        res.insert(name.into(), serde_json::json!({"char": s, "face": fam, "fallback": face.fallback, "found": face.found, "cells": cells, "first_frame_us": us}));
    }
    o.section("fallback", serde_json::Value::Object(res));
}

fn bench_dpi(o: &mut Out) {
    let demo = demo_bytes();
    let v = view_with(120, 30, &demo);
    let mut rows = Vec::new();
    o.say("\n## high DPI (system fonts, 120x30 demo screen)");
    o.say("| scale | cell | frame px | full frame ms | glyphs | glyph cache KiB | frame MiB |");
    o.say("| --- | --- | --- | --- | --- | --- | --- |");
    for scale in [1.0f32, 1.25, 1.5, 1.75, 2.0] {
        let Some(mut r) = system_renderer(scale) else {
            return;
        };
        let t = Instant::now();
        r.render(&v);
        let full = ms(t.elapsed());
        let m = r.metrics();
        let f = r.frame();
        o.say(&format!(
            "| {scale} | {}x{} | {}x{} | {full:.2} | {} | {} | {:.1} |",
            m.width,
            m.height,
            f.width,
            f.height,
            r.glyph_cache().len(),
            r.glyph_cache().bytes() / 1024,
            f.bytes() as f64 / 1_048_576.0
        ));
        rows.push(serde_json::json!({"scale": scale, "cell": [m.width, m.height], "frame": [f.width, f.height],
            "full_frame_ms": full, "glyphs": r.glyph_cache().len(), "glyph_cache_bytes": r.glyph_cache().bytes()}));
    }
    // A DPI change on a live renderer re-rasterizes at the new size.
    if let Some(mut r) = system_renderer(1.0) {
        r.render(&v);
        let t = Instant::now();
        r.set_scale(1.5);
        r.render(&v);
        let change = ms(t.elapsed());
        o.say(&format!(
            "- DPI change 100 % → 150 % on a live renderer, next frame: {change:.2} ms"
        ));
        o.section(
            "dpi",
            serde_json::json!({"scales": rows, "change_ms": change}),
        );
    }
}

fn bench_theme(o: &mut Out) {
    let demo = demo_bytes();
    let v = view_with(120, 30, &demo);
    let Some(mut r) = system_renderer(1.0) else {
        return;
    };
    r.render(&v);
    let before = r.frame().hash();
    let hc = Theme::high_contrast(
        subak_term::render::Rgb(0, 0, 0),
        subak_term::render::Rgb(255, 255, 255),
        subak_term::render::Rgb(26, 235, 255),
    );
    let mut times = Vec::new();
    for i in 0..10 {
        r.set_theme(if i % 2 == 0 {
            hc.clone()
        } else {
            Theme::kim_dark()
        });
        let t = Instant::now();
        let d = r.render(&v);
        times.push(ms(t.elapsed()));
        assert!(d.full);
    }
    r.set_theme(hc);
    r.render(&v);
    let changed = r.frame().hash() != before;
    let med = median(times);
    o.say(&format!(
        "\n## theme change\n- kim-dark ↔ high-contrast, full redraw of 120x30 with warm glyph cache: {med:.2} ms (median of 10); pixels changed: {changed}"
    ));
    o.section(
        "theme",
        serde_json::json!({"redraw_ms": med, "pixels_changed": changed}),
    );
}

fn bench_latency(o: &mut Out, quick: bool) {
    let n = if quick { 300 } else { 2000 };
    let demo = demo_bytes();
    let mut res = serde_json::Map::new();
    o.say("\n## keyboard latency in the client view (key → bytes → echo fed back → frame ready)");
    for kind in ["gdi", "builtin"] {
        let r = if kind == "gdi" {
            system_renderer(1.0)
        } else {
            Some(builtin_renderer(1.0))
        };
        let Some(mut r) = r else {
            continue;
        };
        let mut v = view_with(120, 30, &demo);
        r.render(&v);
        let mut samples = Vec::with_capacity(n);
        let keys = "the quick brown fox 한글 ";
        for (i, c) in keys.chars().cycle().take(n).enumerate() {
            let t = Instant::now();
            let bytes = if i % 40 == 39 {
                v.key(Key::Enter, Mods::NONE)
            } else {
                v.key(Key::Char(c), Mods::NONE)
            };
            // The program echoes what it got (a shell's line editor does the same).
            let echo: Vec<u8> = if bytes == b"\r" {
                b"\r\n".to_vec()
            } else {
                bytes
            };
            v.feed(&echo);
            let d = r.render(&v);
            samples.push(t.elapsed().as_secs_f64() * 1e6);
            assert!(!d.is_empty());
        }
        let (p50, p95, max) = (
            pct(samples.clone(), 0.5),
            pct(samples.clone(), 0.95),
            pct(samples, 1.0),
        );
        o.say(&format!(
            "- {kind}: {n} keys, p50 {p50:.0} us, p95 {p95:.0} us, max {max:.0} us"
        ));
        res.insert(
            kind.into(),
            serde_json::json!({"keys": n, "p50_us": p50, "p95_us": p95, "max_us": max}),
        );
    }
    o.section("latency", serde_json::Value::Object(res));
}

fn shots(dir: &str) {
    let demo = demo_bytes();
    let mut v = view_with(100, 24, &demo);
    std::fs::create_dir_all(dir).expect("shot dir");
    for (scale, theme) in [
        (1.0f32, "kim-dark"),
        (2.0, "kim-dark"),
        (1.0, "high-contrast"),
    ] {
        let Some(mut r) = system_renderer(scale) else {
            eprintln!("no system fonts");
            return;
        };
        if theme == "high-contrast" {
            r.set_theme(Theme::high_contrast(
                subak_term::render::Rgb(0, 0, 0),
                subak_term::render::Rgb(255, 255, 255),
                subak_term::render::Rgb(26, 235, 255),
            ));
        }
        v.select(0, 5, subak_term::SelectMode::Word);
        v.extend_selection(30, 5);
        r.render(&v);
        let path = format!("{dir}/term-{theme}-{}.bmp", (scale * 100.0) as u32);
        std::fs::write(&path, r.frame().to_bmp()).expect("write shot");
        println!("wrote {path}");
        v.clear_selection();
    }
}

// ---------------------------------------------------------------------------------------------
// Idle resources: an off-screen window that presents rendered frames with GDI.

#[cfg(windows)]
#[allow(unsafe_code)]
mod idle {
    use std::cell::RefCell;
    use std::os::windows::io::AsRawHandle;
    use std::os::windows::process::CommandExt;
    use std::process::{Child, Command, Stdio};
    use std::ptr::{null, null_mut};
    use std::time::Duration;
    use subak_term::TermView;
    use subak_term::render::{Frame, RenderOptions, Renderer, Theme};
    use windows_sys::Win32::Foundation::{FILETIME, HANDLE, HWND, LPARAM, LRESULT, WPARAM};
    use windows_sys::Win32::Graphics::Gdi::{
        BI_RGB, BITMAPINFO, BITMAPINFOHEADER, BeginPaint, DIB_RGB_COLORS, EndPaint, PAINTSTRUCT,
        SetDIBitsToDevice,
    };
    use windows_sys::Win32::System::Diagnostics::ToolHelp::{
        CreateToolhelp32Snapshot, TH32CS_SNAPTHREAD, THREADENTRY32, Thread32First, Thread32Next,
    };
    use windows_sys::Win32::System::LibraryLoader::GetModuleHandleW;
    use windows_sys::Win32::System::ProcessStatus::{
        GetProcessMemoryInfo, PROCESS_MEMORY_COUNTERS_EX,
    };
    use windows_sys::Win32::System::Threading::{
        CREATE_NO_WINDOW, GetProcessHandleCount, GetProcessTimes,
    };
    use windows_sys::Win32::UI::WindowsAndMessaging::{
        CreateWindowExW, DefWindowProcW, DispatchMessageW, GetMessageW, MSG, PostQuitMessage,
        RegisterClassExW, SW_SHOWNOACTIVATE, ShowWindow, TranslateMessage, WM_DESTROY, WM_PAINT,
        WNDCLASSEXW, WS_EX_NOACTIVATE, WS_EX_TOOLWINDOW, WS_POPUP,
    };

    thread_local! {
        /// The view and its renderer, as a Subak GUI window keeps them.
        static SURFACE: RefCell<Option<(TermView, Renderer)>> = const { RefCell::new(None) };
    }

    fn wide(s: &str) -> Vec<u16> {
        s.encode_utf16().chain([0]).collect()
    }

    fn bitmap_info(f: &Frame) -> BITMAPINFO {
        BITMAPINFO {
            bmiHeader: BITMAPINFOHEADER {
                biSize: size_of::<BITMAPINFOHEADER>() as u32,
                biWidth: f.width as i32,
                biHeight: -(f.height as i32),
                biPlanes: 1,
                biBitCount: 32,
                biCompression: BI_RGB,
                ..BITMAPINFOHEADER::default()
            },
            ..BITMAPINFO::default()
        }
    }

    unsafe extern "system" fn wndproc(hwnd: HWND, msg: u32, wp: WPARAM, lp: LPARAM) -> LRESULT {
        match msg {
            WM_PAINT => {
                let mut ps = PAINTSTRUCT::default();
                // SAFETY: standard paint handling on our own window; the frame lives in the
                // thread's surface for the whole call.
                unsafe {
                    let dc = BeginPaint(hwnd, &raw mut ps);
                    SURFACE.with_borrow_mut(|s| {
                        if let Some((view, r)) = s {
                            // Draws what changed (nothing, after the first frame), then presents.
                            r.render(view);
                            let f = r.frame();
                            let bmi = bitmap_info(f);
                            SetDIBitsToDevice(
                                dc,
                                0,
                                0,
                                f.width,
                                f.height,
                                0,
                                0,
                                0,
                                f.height,
                                f.pixels.as_ptr().cast(),
                                &raw const bmi,
                                DIB_RGB_COLORS,
                            );
                        }
                    });
                    EndPaint(hwnd, &raw const ps);
                }
                0
            }
            WM_DESTROY => {
                // SAFETY: ends this thread's message loop.
                unsafe { PostQuitMessage(0) };
                0
            }
            // SAFETY: default handling of every other message.
            _ => unsafe { DefWindowProcW(hwnd, msg, wp, lp) },
        }
    }

    /// The child: a 120x30 view with the demo screen and 2 000 lines of history, its renderer
    /// with the system fonts, and a popup tool window placed far off every monitor, never
    /// activated, that presents the frame on `WM_PAINT`. Prints `ready` and runs its message loop
    /// until the parent ends it.
    pub fn child() {
        let mut v = TermView::new(120, 30, 2000);
        for i in 0..2000 {
            v.feed(format!("history line {i} with some text 한글\r\n").as_bytes());
        }
        v.feed(&super::demo_bytes());
        let mut r = Renderer::with_system_fonts(Theme::kim_dark(), RenderOptions::default())
            .expect("system fonts");
        r.render(&v);
        let (w, h) = (r.frame().width as i32, r.frame().height as i32);
        SURFACE.with_borrow_mut(|s| *s = Some((v, r)));
        let class = wide("SubakTermBenchIdle");
        // SAFETY: registers a window class and creates one popup tool window of this process,
        // placed far off every monitor, never activated; the strings outlive the calls.
        unsafe {
            let hinst = GetModuleHandleW(null());
            let wc = WNDCLASSEXW {
                cbSize: size_of::<WNDCLASSEXW>() as u32,
                lpfnWndProc: Some(wndproc),
                hInstance: hinst,
                lpszClassName: class.as_ptr(),
                ..WNDCLASSEXW::default()
            };
            RegisterClassExW(&raw const wc);
            let title = wide("subak-term idle probe");
            let hwnd = CreateWindowExW(
                WS_EX_TOOLWINDOW | WS_EX_NOACTIVATE,
                class.as_ptr(),
                title.as_ptr(),
                WS_POPUP,
                -20000,
                -20000,
                w,
                h,
                null_mut(),
                null_mut(),
                hinst,
                null(),
            );
            assert!(!hwnd.is_null(), "CreateWindowExW");
            ShowWindow(hwnd, SW_SHOWNOACTIVATE);
            println!("ready {w}x{h}");
            use std::io::Write;
            let _ = std::io::stdout().flush();
            let mut msg = MSG::default();
            while GetMessageW(&raw mut msg, null_mut(), 0, 0) > 0 {
                TranslateMessage(&raw const msg);
                DispatchMessageW(&raw const msg);
            }
        }
    }

    #[derive(Debug, Clone, Copy)]
    pub struct Sample {
        pub cpu_ms: f64,
        pub private: usize,
        pub working_set: usize,
        pub peak_working_set: usize,
        pub handles: u32,
        pub threads: u32,
    }

    fn ft(f: FILETIME) -> f64 {
        ((u64::from(f.dwHighDateTime) << 32) | u64::from(f.dwLowDateTime)) as f64 / 10_000.0
    }

    fn threads_of(pid: u32) -> u32 {
        let mut n = 0;
        // SAFETY: a thread snapshot walked with a correctly sized entry, then closed.
        unsafe {
            let snap = CreateToolhelp32Snapshot(TH32CS_SNAPTHREAD, 0);
            let mut e = THREADENTRY32 {
                dwSize: size_of::<THREADENTRY32>() as u32,
                ..THREADENTRY32::default()
            };
            if Thread32First(snap, &raw mut e) != 0 {
                loop {
                    if e.th32OwnerProcessID == pid {
                        n += 1;
                    }
                    if Thread32Next(snap, &raw mut e) == 0 {
                        break;
                    }
                }
            }
            windows_sys::Win32::Foundation::CloseHandle(snap);
        }
        n
    }

    pub fn sample(h: HANDLE, pid: u32) -> Sample {
        let mut c = FILETIME::default();
        let (mut e, mut k, mut u) = (c, c, c);
        let mut pm = PROCESS_MEMORY_COUNTERS_EX {
            cb: size_of::<PROCESS_MEMORY_COUNTERS_EX>() as u32,
            ..PROCESS_MEMORY_COUNTERS_EX::default()
        };
        let mut handles = 0;
        // SAFETY: queries on a process handle we own; the out-structures are sized correctly.
        unsafe {
            GetProcessTimes(h, &raw mut c, &raw mut e, &raw mut k, &raw mut u);
            GetProcessMemoryInfo(h, (&raw mut pm).cast(), pm.cb);
            GetProcessHandleCount(h, &raw mut handles);
        }
        Sample {
            cpu_ms: ft(k) + ft(u),
            private: pm.PrivateUsage,
            working_set: pm.WorkingSetSize,
            peak_working_set: pm.PeakWorkingSetSize,
            handles,
            threads: threads_of(pid),
        }
    }

    /// Samples `child` after a warm-up and after `idle` seconds, then ends it.
    fn measure_child(mut child: Child, warmup: u64, idle: u64) -> (Sample, Sample) {
        let h = child.as_raw_handle() as HANDLE;
        let pid = child.id();
        std::thread::sleep(Duration::from_secs(warmup));
        let a = sample(h, pid);
        std::thread::sleep(Duration::from_secs(idle));
        let b = sample(h, pid);
        let _ = child.kill();
        let _ = child.wait();
        (a, b)
    }

    /// The renderer's own probe (this executable with `--idle-child`).
    pub fn measure(idle: u64) -> Option<(Sample, Sample, String)> {
        let exe = std::env::current_exe().ok()?;
        let mut child = Command::new(exe)
            .arg("--idle-child")
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .creation_flags(CREATE_NO_WINDOW)
            .spawn()
            .ok()?;
        let mut line = String::new();
        {
            use std::io::BufRead;
            let out = child.stdout.take()?;
            std::io::BufReader::new(out).read_line(&mut line).ok()?;
        }
        let (a, b) = measure_child(child, 3, idle);
        Some((a, b, line.trim().to_string()))
    }

    /// Another program measured the same way (e.g. Kim Manager's egui + glow preview, which
    /// places its window off-screen and unactivated by itself). It must exit by itself or is
    /// ended after sampling.
    pub fn measure_exe(cmd: &[String], warmup: u64, idle: u64) -> Option<(Sample, Sample)> {
        let (exe, args) = cmd.split_first()?;
        let child = Command::new(exe)
            .args(args)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .creation_flags(CREATE_NO_WINDOW)
            .spawn()
            .ok()?;
        Some(measure_child(child, warmup, idle))
    }
}

fn bench_idle(o: &mut Out, secs: u64, compare: &[String]) {
    #[cfg(windows)]
    {
        let mib = |v: usize| v as f64 / 1_048_576.0;
        o.say(&format!(
            "\n## idle resources ({secs} s idle after the warm-up; CPU time, memory, threads, handles of the process)"
        ));
        let Some((a, b, ready)) = idle::measure(secs) else {
            o.say("idle probe failed to start");
            return;
        };
        o.say(&format!(
            "- subak-term CPU renderer + GDI blit, off-screen window {ready}, 120x30 view with 2 000 history lines: CPU over {secs} s idle {:.1} ms; private {:.2} MiB; working set {:.2} MiB (peak {:.2}); threads {}; handles {}",
            b.cpu_ms - a.cpu_ms,
            mib(b.private),
            mib(b.working_set),
            mib(b.peak_working_set),
            b.threads,
            b.handles
        ));
        let mut doc = serde_json::json!({
            "idle_seconds": secs,
            "cpu_renderer_gdi_window": {
                "cpu_ms_idle": b.cpu_ms - a.cpu_ms, "cpu_ms_total": b.cpu_ms,
                "private_bytes": b.private, "working_set_bytes": b.working_set,
                "peak_working_set_bytes": b.peak_working_set, "threads": b.threads,
                "handles": b.handles, "window": ready,
            },
        });
        if !compare.is_empty()
            && let Some((a, b)) = idle::measure_exe(compare, 8, secs)
        {
            o.say(&format!(
                "- comparison `{}`: CPU over {secs} s idle {:.1} ms; private {:.2} MiB; working set {:.2} MiB (peak {:.2}); threads {}; handles {}",
                compare.join(" "),
                b.cpu_ms - a.cpu_ms,
                mib(b.private),
                mib(b.working_set),
                mib(b.peak_working_set),
                b.threads,
                b.handles
            ));
            doc["comparison"] = serde_json::json!({
                "command": compare.join(" "), "cpu_ms_idle": b.cpu_ms - a.cpu_ms,
                "private_bytes": b.private, "working_set_bytes": b.working_set,
                "peak_working_set_bytes": b.peak_working_set, "threads": b.threads,
                "handles": b.handles,
            });
        }
        o.section("idle", doc);
    }
    #[cfg(not(windows))]
    {
        let _ = (secs, compare);
        o.say("\n## idle resources: not measured (Windows only)");
    }
}

// ---------------------------------------------------------------------------------------------
// The manual check: the renderer in a real window (docs/TERMINAL_EMULATOR.md §9).

#[cfg(windows)]
#[allow(unsafe_code)]
mod show {
    use std::cell::RefCell;
    use std::io::{Read, Write};
    use std::os::windows::process::CommandExt;
    use std::process::{Child, ChildStdin, Command, Stdio};
    use std::ptr::{null, null_mut};
    use std::sync::{Arc, Mutex};
    use subak_term::render::{RenderOptions, Renderer, Theme};
    use subak_term::{Key, Mods, TermView};
    use windows_sys::Win32::Foundation::{HWND, LPARAM, LRESULT, RECT, WPARAM};
    use windows_sys::Win32::Graphics::Gdi::{
        BI_RGB, BITMAPINFO, BITMAPINFOHEADER, BeginPaint, DIB_RGB_COLORS, EndPaint, InvalidateRect,
        PAINTSTRUCT, SetDIBitsToDevice,
    };
    use windows_sys::Win32::System::LibraryLoader::GetModuleHandleW;
    use windows_sys::Win32::System::Threading::CREATE_NO_WINDOW;
    use windows_sys::Win32::UI::HiDpi::{
        AdjustWindowRectExForDpi, DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2, GetDpiForWindow,
        SetProcessDpiAwarenessContext,
    };
    use windows_sys::Win32::UI::Input::KeyboardAndMouse::GetKeyState;
    use windows_sys::Win32::UI::WindowsAndMessaging::{
        CW_USEDEFAULT, CreateWindowExW, DefWindowProcW, DestroyWindow, DispatchMessageW,
        GetMessageW, MSG, PostMessageW, PostQuitMessage, RegisterClassExW, SW_SHOW,
        SW_SHOWNOACTIVATE, SWP_NOACTIVATE, SWP_NOMOVE, SWP_NOZORDER, SetTimer, SetWindowPos,
        ShowWindow, TranslateMessage, WM_APP, WM_CHAR, WM_DESTROY, WM_DPICHANGED, WM_KEYDOWN,
        WM_KILLFOCUS, WM_PAINT, WM_SETFOCUS, WM_SYSCHAR, WM_SYSKEYDOWN, WM_TIMER, WNDCLASSEXW,
        WS_CAPTION, WS_EX_NOACTIVATE, WS_EX_TOOLWINDOW, WS_MINIMIZEBOX, WS_OVERLAPPED, WS_POPUP,
        WS_SYSMENU,
    };

    const WM_OUTPUT: u32 = WM_APP + 1;
    const WM_ENDED: u32 = WM_APP + 2;

    struct App {
        view: TermView,
        renderer: Renderer,
        stdin: Option<ChildStdin>,
        child: Option<Child>,
        incoming: Arc<Mutex<Vec<u8>>>,
        high_surrogate: Option<u16>,
        bytes: usize,
        frames: usize,
    }

    thread_local! {
        static APP: RefCell<Option<App>> = const { RefCell::new(None) };
    }

    fn wide(s: &str) -> Vec<u16> {
        s.encode_utf16().chain([0]).collect()
    }

    fn style() -> u32 {
        WS_OVERLAPPED | WS_CAPTION | WS_SYSMENU | WS_MINIMIZEBOX
    }

    /// The outer window size for the renderer's frame at `dpi`.
    fn outer(r: &Renderer, dpi: u32) -> (i32, i32) {
        let f = r.frame();
        let mut rc = RECT {
            left: 0,
            top: 0,
            right: f.width as i32,
            bottom: f.height as i32,
        };
        // SAFETY: a plain RECT computation.
        unsafe { AdjustWindowRectExForDpi(&raw mut rc, style(), 0, 0, dpi) };
        (rc.right - rc.left, rc.bottom - rc.top)
    }

    fn mods() -> Mods {
        // SAFETY: reads this thread's keyboard state.
        let down = |vk: i32| unsafe { GetKeyState(vk) } < 0;
        Mods {
            shift: down(0x10),
            ctrl: down(0x11),
            alt: down(0x12),
        }
    }

    fn send(app: &mut App, bytes: &[u8]) {
        if bytes.is_empty() {
            return;
        }
        if let Some(stdin) = &mut app.stdin {
            let _ = stdin.write_all(bytes);
            let _ = stdin.flush();
        }
    }

    fn redraw(hwnd: HWND, app: &mut App) {
        let d = app.renderer.render(&app.view);
        if !d.is_empty() {
            app.frames += 1;
            // SAFETY: invalidates our own window's client area.
            unsafe { InvalidateRect(hwnd, null(), 0) };
        }
    }

    unsafe extern "system" fn wndproc(hwnd: HWND, msg: u32, wp: WPARAM, lp: LPARAM) -> LRESULT {
        // Calls below can send messages synchronously (re-entering this procedure while the
        // state is borrowed): those fall through to the default handling, and closing the
        // window waits until the borrow ends.
        let mut destroy = false;
        let handled = APP.with(|cell| {
            let Ok(mut slot) = cell.try_borrow_mut() else {
                return None;
            };
            let app = slot.as_mut()?;
            match msg {
                WM_PAINT => {
                    let mut ps = PAINTSTRUCT::default();
                    // SAFETY: standard paint handling on our own window with our own frame.
                    unsafe {
                        let dc = BeginPaint(hwnd, &raw mut ps);
                        let f = app.renderer.frame();
                        let bmi = BITMAPINFO {
                            bmiHeader: BITMAPINFOHEADER {
                                biSize: size_of::<BITMAPINFOHEADER>() as u32,
                                biWidth: f.width as i32,
                                biHeight: -(f.height as i32),
                                biPlanes: 1,
                                biBitCount: 32,
                                biCompression: BI_RGB,
                                ..BITMAPINFOHEADER::default()
                            },
                            ..BITMAPINFO::default()
                        };
                        SetDIBitsToDevice(
                            dc,
                            0,
                            0,
                            f.width,
                            f.height,
                            0,
                            0,
                            0,
                            f.height,
                            f.pixels.as_ptr().cast(),
                            &raw const bmi,
                            DIB_RGB_COLORS,
                        );
                        EndPaint(hwnd, &raw const ps);
                    }
                    Some(0)
                }
                WM_OUTPUT => {
                    let bytes = std::mem::take(&mut *app.incoming.lock().unwrap());
                    app.bytes += bytes.len();
                    app.view.feed(&bytes);
                    let replies = app.view.take_replies();
                    send(app, &replies);
                    redraw(hwnd, app);
                    Some(0)
                }
                WM_KEYDOWN | WM_SYSKEYDOWN => {
                    let key = match wp {
                        0x26 => Key::Up,
                        0x28 => Key::Down,
                        0x25 => Key::Left,
                        0x27 => Key::Right,
                        0x24 => Key::Home,
                        0x23 => Key::End,
                        0x21 => Key::PageUp,
                        0x22 => Key::PageDown,
                        0x2D => Key::Insert,
                        0x2E => Key::Delete,
                        0x70..=0x7B => Key::F((wp - 0x70 + 1) as u8),
                        0x1B if app.stdin.is_none() => {
                            destroy = true;
                            return Some(0);
                        }
                        _ => return None, // characters arrive as WM_CHAR
                    };
                    let bytes = app.view.key(key, mods());
                    send(app, &bytes);
                    redraw(hwnd, app);
                    Some(0)
                }
                WM_CHAR | WM_SYSCHAR => {
                    let unit = wp as u16;
                    let c = if (0xD800..0xDC00).contains(&unit) {
                        app.high_surrogate = Some(unit);
                        return Some(0);
                    } else if (0xDC00..0xE000).contains(&unit) {
                        let hi = app.high_surrogate.take()?;
                        char::decode_utf16([hi, unit]).next()?.ok()?
                    } else {
                        char::from_u32(u32::from(unit))?
                    };
                    let m = Mods {
                        alt: msg == WM_SYSCHAR,
                        ..Mods::NONE
                    };
                    let bytes = match c {
                        '\r' => app.view.key(Key::Enter, m),
                        '\t' => app.view.key(Key::Tab, mods()),
                        '\u{8}' => app.view.key(Key::Backspace, m),
                        '\u{1b}' => app.view.key(Key::Escape, m),
                        // Ctrl+letter arrives already as its C0 control.
                        c if (c as u32) < 0x20 => vec![c as u8],
                        c => app.view.key(Key::Char(c), m),
                    };
                    send(app, &bytes);
                    redraw(hwnd, app);
                    Some(0)
                }
                WM_SETFOCUS | WM_KILLFOCUS => {
                    let focused = msg == WM_SETFOCUS;
                    app.renderer.set_focused(focused);
                    if let Some(report) = app.view.focus(focused) {
                        send(app, &report);
                    }
                    redraw(hwnd, app);
                    Some(0)
                }
                WM_DPICHANGED => {
                    // Re-rasterize at the new scale (never a bitmap scale) and take the size
                    // Windows suggests for the new monitor's DPI.
                    let dpi = (wp & 0xffff) as u32;
                    app.renderer.set_scale(dpi as f32 / 96.0);
                    app.renderer.render(&app.view);
                    let (w, h) = outer(&app.renderer, dpi);
                    // SAFETY: resizes our own window; lp points at the suggested RECT.
                    unsafe {
                        let rc = &*(lp as *const RECT);
                        SetWindowPos(
                            hwnd,
                            null_mut(),
                            rc.left,
                            rc.top,
                            w,
                            h,
                            SWP_NOZORDER | SWP_NOACTIVATE,
                        );
                        InvalidateRect(hwnd, null(), 0);
                    }
                    Some(0)
                }
                WM_TIMER | WM_ENDED => {
                    destroy = true;
                    Some(0)
                }
                WM_DESTROY => {
                    if let Some(mut child) = app.child.take() {
                        // Ending `subak attach` is a client exit: the Terminal keeps running.
                        let _ = child.kill();
                        let _ = child.wait();
                    }
                    println!(
                        "show: {} bytes received, {} frames drawn; screen:\n{}",
                        app.bytes,
                        app.frames,
                        (0..app.view.size().1)
                            .map(|r| app.view.row_at(r).text().trim_end().to_string())
                            .filter(|l| !l.is_empty())
                            .collect::<Vec<_>>()
                            .join("\n")
                    );
                    // SAFETY: ends this thread's message loop.
                    unsafe { PostQuitMessage(0) };
                    Some(0)
                }
                _ => None,
            }
        });
        if destroy {
            // SAFETY: closes our own window (WM_DESTROY then runs with the state free).
            unsafe { DestroyWindow(hwnd) };
            return 0;
        }
        // SAFETY: default handling of everything not handled above.
        handled.unwrap_or_else(|| unsafe { DefWindowProcW(hwnd, msg, wp, lp) })
    }

    /// Opens a real window with the demo screen, or with a live `subak attach <target>` session
    /// when `attach` is given. `offscreen` places it far off every monitor, never activated, and
    /// closes it after `exit_after` seconds (for an automated smoke run that shows nothing).
    pub fn run(
        attach: Option<&(String, Option<String>, String)>,
        offscreen: bool,
        exit_after: u32,
    ) {
        // SAFETY: opts this process into per-monitor DPI awareness before any window exists.
        unsafe { SetProcessDpiAwarenessContext(DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2) };
        let (cols, rows) = (100u16, 30u16);
        let mut view = TermView::new(cols, rows, 2000);
        let renderer = Renderer::with_system_fonts(Theme::kim_dark(), RenderOptions::default())
            .expect("system fonts");
        let incoming = Arc::new(Mutex::new(Vec::new()));
        let (mut stdin, mut child, mut stdout) = (None, None, None);
        match attach {
            None => view.feed(&super::demo_bytes()),
            Some((target, endpoint, subak)) => {
                view.set_answer_reports(true);
                let mut args: Vec<String> = Vec::new();
                if let Some(ep) = endpoint {
                    args.extend(["--endpoint".into(), ep.clone()]);
                }
                args.extend([
                    "attach".into(),
                    target.clone(),
                    "--size".into(),
                    format!("{cols}x{rows}"),
                ]);
                let mut c = Command::new(subak)
                    .args(&args)
                    .stdin(Stdio::piped())
                    .stdout(Stdio::piped())
                    .stderr(Stdio::null())
                    .creation_flags(CREATE_NO_WINDOW)
                    .spawn()
                    .expect("run subak attach");
                stdin = c.stdin.take();
                stdout = c.stdout.take();
                child = Some(c);
            }
        }
        let class = wide("SubakTermBenchShow");
        // SAFETY: registers a class and creates one window of this process; with `offscreen` it
        // is a tool window far off every monitor and never activated.
        let hwnd = unsafe {
            let hinst = GetModuleHandleW(null());
            let wc = WNDCLASSEXW {
                cbSize: size_of::<WNDCLASSEXW>() as u32,
                lpfnWndProc: Some(wndproc),
                hInstance: hinst,
                lpszClassName: class.as_ptr(),
                ..WNDCLASSEXW::default()
            };
            RegisterClassExW(&raw const wc);
            let title = wide(match attach {
                None => "Subak terminal renderer (manual check, Step 15)",
                Some(_) => "Subak terminal renderer: live session (manual check, Step 15)",
            });
            let (ex, st, x, y) = if offscreen {
                (
                    WS_EX_TOOLWINDOW | WS_EX_NOACTIVATE,
                    WS_POPUP,
                    -20000,
                    -20000,
                )
            } else {
                (0, style(), CW_USEDEFAULT, CW_USEDEFAULT)
            };
            CreateWindowExW(
                ex,
                class.as_ptr(),
                title.as_ptr(),
                st,
                x,
                y,
                400,
                300,
                null_mut(),
                null_mut(),
                hinst,
                null(),
            )
        };
        assert!(!hwnd.is_null(), "CreateWindowExW");
        // SAFETY: our own window.
        let dpi = unsafe { GetDpiForWindow(hwnd) }.max(96);
        let mut renderer = renderer;
        renderer.set_scale(dpi as f32 / 96.0);
        renderer.render(&view);
        let (w, h) = outer(&renderer, dpi);
        APP.with_borrow_mut(|slot| {
            *slot = Some(App {
                view,
                renderer,
                stdin,
                child,
                incoming: Arc::clone(&incoming),
                high_surrogate: None,
                bytes: 0,
                frames: 1,
            });
        });
        if let Some(mut out) = stdout {
            let hw = hwnd as isize;
            std::thread::spawn(move || {
                let mut buf = [0u8; 16 * 1024];
                loop {
                    match out.read(&mut buf) {
                        Ok(0) | Err(_) => break,
                        Ok(n) => {
                            incoming.lock().unwrap().extend_from_slice(&buf[..n]);
                            // SAFETY: posts to the UI thread's window (thread-safe API).
                            unsafe { PostMessageW(hw as HWND, WM_OUTPUT, 0, 0) };
                        }
                    }
                }
                // SAFETY: as above.
                unsafe { PostMessageW(hw as HWND, WM_ENDED, 0, 0) };
            });
        }
        // SAFETY: sizes and shows our own window, then runs this thread's message loop.
        unsafe {
            SetWindowPos(
                hwnd,
                null_mut(),
                0,
                0,
                w,
                h,
                SWP_NOMOVE | SWP_NOZORDER | SWP_NOACTIVATE,
            );
            ShowWindow(
                hwnd,
                if offscreen {
                    SW_SHOWNOACTIVATE
                } else {
                    SW_SHOW
                },
            );
            if offscreen || exit_after > 0 {
                SetTimer(hwnd, 1, exit_after.max(1) * 1000, None);
            }
            let mut msg = MSG::default();
            while GetMessageW(&raw mut msg, null_mut(), 0, 0) > 0 {
                TranslateMessage(&raw const msg);
                DispatchMessageW(&raw const msg);
            }
        }
    }
}

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    if args.iter().any(|a| a == "--idle-child") {
        #[cfg(windows)]
        idle::child();
        return;
    }
    let flag = |f: &str| args.iter().any(|a| a == f);
    let value = |f: &str| {
        args.iter()
            .position(|a| a == f)
            .and_then(|i| args.get(i + 1))
            .cloned()
    };
    if let Some(dir) = value("--shot") {
        shots(&dir);
        return;
    }
    if flag("--show") {
        // The manual check (docs/TERMINAL_EMULATOR.md §9): a real window. Never run by tests,
        // except `--offscreen`, which places it off every monitor and closes it by itself.
        #[cfg(windows)]
        {
            let attach = value("--attach").map(|t| {
                (
                    t,
                    value("--endpoint"),
                    value("--subak").unwrap_or_else(|| "subak".into()),
                )
            });
            let exit_after = value("--exit-after")
                .and_then(|s| s.parse().ok())
                .unwrap_or(if flag("--offscreen") { 5 } else { 0 });
            show::run(attach.as_ref(), flag("--offscreen"), exit_after);
        }
        return;
    }
    let quick = flag("--quick");
    let only: Vec<String> = value("--only")
        .map(|s| s.split(',').map(str::to_string).collect())
        .unwrap_or_default();
    let want = |s: &str| only.is_empty() || only.iter().any(|o| o == s);
    let idle_secs: u64 = value("--idle-secs")
        .and_then(|s| s.parse().ok())
        .unwrap_or(if quick { 10 } else { 30 });
    let mut o = Out {
        json: flag("--json"),
        doc: serde_json::Map::new(),
    };
    o.say(&format!(
        "# subak-term benchmark ({} build, {})",
        if cfg!(debug_assertions) {
            "debug"
        } else {
            "release"
        },
        std::env::consts::OS
    ));
    if want("parse") {
        bench_parse(&mut o, quick);
    }
    if want("render") {
        bench_render(&mut o, quick);
    }
    if want("long") {
        bench_long(&mut o, quick);
    }
    if want("fallback") {
        bench_fallback(&mut o);
    }
    if want("dpi") {
        bench_dpi(&mut o);
    }
    if want("theme") {
        bench_theme(&mut o);
    }
    if want("latency") {
        bench_latency(&mut o, quick);
    }
    if want("idle") {
        // `--compare -- <program> [args]`: another program measured the same way.
        let compare: Vec<String> = args
            .iter()
            .position(|a| a == "--compare")
            .map(|i| {
                args[i + 1..]
                    .iter()
                    .skip_while(|a| *a == "--")
                    .cloned()
                    .collect()
            })
            .unwrap_or_default();
        bench_idle(&mut o, idle_secs, &compare);
    }
    if o.json {
        o.doc.insert(
            "build".into(),
            serde_json::json!(if cfg!(debug_assertions) {
                "debug"
            } else {
                "release"
            }),
        );
        println!("{}", serde_json::Value::Object(o.doc));
    }
}
