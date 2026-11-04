//! Step 15 — the native terminal emulator on real Windows ConPTY output (ADR 0036,
//! docs/TERMINAL_EMULATOR.md). `SUBAK-FR-004`, `SUBAK-FR-013`, `SUBAK-FR-015`, `SUBAK-FR-031`,
//! `SUBAK-NFR-001`, `KIM-NFR-013`.
//!
//! Each test starts its own `subak.exe` Host on a private endpoint, runs a real program in a pane
//! (cmd.exe, Windows PowerShell 5.1, `more`, a full-screen stand-in built into this test binary
//! and — when installed — `claude --version`), attaches as a GUI client over IPC v1 and feeds the
//! snapshot and live stream into a [`TermView`], the Subak GUI's view state. The view is then
//! rendered **headlessly** into a pixel frame with the system fonts (GDI, no window) and checked
//! against golden screens: the expected text in the expected cells (Korean as two-cell
//! characters), colours as the program set them, ink inside the right cells, and the Host's own
//! capture being equal to what the view shows (the Host model and the client view agree).
//! Keyboard input goes through the view's own input API, never the desktop.
//!
//! `SUBAK_TERM_SHOTS=<dir>` writes each rendered frame as a BMP screenshot for inspection.
//! Lines starting with `EVIDENCE` record what was measured; run with `--nocapture` to see them.
#![cfg(windows)]
#![allow(unsafe_code)]

#[path = "../../../xtask/src/jsonschema.rs"]
#[allow(dead_code)]
mod jsonschema;

mod common;

use common::*;
use serde_json::{Value, json};
use std::io::Write;
use std::time::{Duration, Instant};
use subak_term::render::{RenderOptions, Renderer, Rgb, Theme};
use subak_term::screen::{CellWidth, Color};
use subak_term::{Key, Mods, MouseAction, MouseButton, MouseEvent, MouseOutcome, TermView};

/// Writes an `EVIDENCE` line straight to stderr (the test harness does not capture it), so CI
/// logs show what was measured on the runner's Windows build too.
macro_rules! evidence {
    ($($t:tt)*) => {{
        let mut e = std::io::stderr().lock();
        let _ = writeln!(e, $($t)*);
    }};
}

const SNAPSHOT: u8 = 1;
const INPUT: u8 = 2;

/// A PowerShell's first prompt can take long on a cold start or a busy CI runner (Step 11).
const PS_START: Duration = Duration::from_secs(120);

fn host(tag: &str) -> Host {
    Host::start(&format!("tr-{tag}"), &["--no-shell", "--idle-exit", "60"])
}

fn cli(ep: &str, args: &[&str]) -> (i32, Value) {
    let mut a = vec!["--endpoint", ep];
    a.extend_from_slice(args);
    run(&a)
}

fn new_terminal(ep: &str, name: &str, extra: &[&str]) {
    let mut args = vec!["new", name];
    args.extend_from_slice(extra);
    let (code, doc) = cli(ep, &args);
    assert_eq!(code, 0, "{args:?}: {doc}");
}

/// The Host's own capture of the screen (text, no escapes, no redaction).
fn host_screen(ep: &str, target: &str) -> Vec<String> {
    let r = Raw::handshake(ep, "cli").request(
        1,
        "pane.capture",
        &json!({"target": target, "start": 0, "end": "-", "format": "text", "redact": false}),
    );
    assert_eq!(r["ok"], true, "{r}");
    r["result"]["lines"]
        .as_array()
        .unwrap()
        .iter()
        .map(|l| l.as_str().unwrap().to_string())
        .collect()
}

fn output_bytes(ep: &str, target: &str) -> u64 {
    let r = Raw::handshake(ep, "cli").request(1, "terminal.info", &json!({"target": target}));
    r["result"]["terminal"]["windows"][0]["panes"][0]["output_bytes"]
        .as_u64()
        .unwrap()
}

/// The Subak GUI's view of a pane, without the window: an interactive IPC v1 client feeding a
/// [`TermView`] and a [`Renderer`] with the system fonts.
struct Client {
    raw: Raw,
    stream: u32,
    view: TermView,
    renderer: Renderer,
    next: u64,
    name: String,
}

impl Client {
    fn attach(ep: &str, target: &str, size: (u16, u16)) -> Self {
        let raw = Raw::handshake(ep, "gui");
        let r = raw.request(1, "events.subscribe", &json!({"topics": ["panes"]}));
        assert_eq!(r["ok"], true, "{r}");
        let r = raw.request(
            2,
            "pane.attach",
            &json!({"target": target, "interactive": true,
                    "size": {"cols": size.0, "rows": size.1}}),
        );
        assert_eq!(r["ok"], true, "{r}");
        let res = &r["result"];
        let cols = u16::try_from(res["snapshot"]["size"]["cols"].as_u64().unwrap()).unwrap();
        let rows = u16::try_from(res["snapshot"]["size"]["rows"].as_u64().unwrap()).unwrap();
        let seq = res["seq"].as_u64().unwrap();
        let mut c = Self {
            raw,
            stream: u32::try_from(res["stream_id"].as_u64().unwrap()).unwrap(),
            view: TermView::new(cols, rows, 2000),
            renderer: Renderer::with_system_fonts(Theme::kim_dark(), RenderOptions::default())
                .expect("the Windows terminal fonts"),
            next: seq,
            name: target.to_string(),
        };
        // This view owns the pane's size, so it is the terminal that answers device reports.
        c.view.set_answer_reports(true);
        let want = res["snapshot"]["bytes"].as_u64().unwrap();
        let mut got = 0;
        while got < want {
            match c.raw.next(LONG) {
                Got::Data(b) => {
                    let (_, s, flags) = c.raw.last_data.get().unwrap();
                    assert_eq!((s, flags), (seq, SNAPSHOT));
                    got += b.len() as u64;
                    c.view.feed(&b);
                }
                other => panic!("expected the snapshot, got {other:?}"),
            }
        }
        c
    }

    fn pump(&mut self, wait: Duration) {
        let until = Instant::now() + wait;
        loop {
            let left = until.saturating_duration_since(Instant::now());
            if left.is_zero() {
                return;
            }
            match self.raw.next(left) {
                Got::Data(b) => {
                    let (_, seq, flags) = self.raw.last_data.get().unwrap();
                    if flags == 0 {
                        assert_eq!(seq, self.next, "no gap, no duplicate");
                        self.next += b.len() as u64;
                    }
                    self.view.feed(&b);
                    let replies = self.view.take_replies();
                    if !replies.is_empty() {
                        self.send(&replies);
                    }
                }
                Got::Control(_) => {}
                Got::Timeout => return,
                Got::Eof => panic!("the Host closed the connection"),
            }
        }
    }

    fn send(&self, bytes: &[u8]) {
        self.raw.write(&data(self.stream, 0, INPUT, bytes)).unwrap();
    }

    /// Types text through the view's input API (as the GUI does with key events).
    fn type_text(&mut self, text: &str) {
        let mut bytes = Vec::new();
        for c in text.chars() {
            bytes.extend(if c == '\r' {
                self.view.key(Key::Enter, Mods::NONE)
            } else {
                self.view.key(Key::Char(c), Mods::NONE)
            });
        }
        self.send(&bytes);
    }

    fn text(&self) -> Vec<String> {
        (0..self.view.size().1)
            .map(|r| self.view.row_at(r).text().trim_end().to_string())
            .collect()
    }

    fn wait_for(&mut self, needle: &str, limit: Duration) {
        let until = Instant::now() + limit;
        while !self.text().iter().any(|l| l.contains(needle)) {
            assert!(
                Instant::now() < until,
                "`{needle}` not on screen:\n{}",
                self.text().join("\n")
            );
            self.pump(Duration::from_millis(100));
        }
    }

    /// Reads until the pane is quiet and everything it wrote has been fed to the view.
    fn settle(&mut self, ep: &str) {
        let until = Instant::now() + Duration::from_secs(60);
        loop {
            let total = output_bytes(ep, &self.name);
            while self.next < total {
                assert!(Instant::now() < until, "stuck at {} of {total}", self.next);
                self.pump(Duration::from_millis(100));
            }
            self.pump(Duration::from_millis(400));
            if output_bytes(ep, &self.name) == self.next {
                return;
            }
            assert!(Instant::now() < until, "the pane never went quiet");
        }
    }

    fn resize(&mut self, cols: u16, rows: u16) {
        let r = self.raw.request(
            90,
            "pane.resize",
            &json!({"cols": cols, "rows": rows, "stream_id": self.stream}),
        );
        assert_eq!(r["ok"], true, "{r}");
        self.view.resize(cols, rows);
    }

    /// Renders the view and saves a screenshot when `SUBAK_TERM_SHOTS` names a directory.
    fn render(&mut self, shot: &str) {
        self.renderer.render(&self.view);
        if let Some(dir) = std::env::var_os("SUBAK_TERM_SHOTS") {
            let dir = std::path::PathBuf::from(dir);
            std::fs::create_dir_all(&dir).unwrap();
            let path = dir.join(format!("{shot}.bmp"));
            std::fs::write(&path, self.renderer.frame().to_bmp()).unwrap();
            evidence!("EVIDENCE screenshot {}", path.display());
        }
    }

    /// Pixels of cell (`col`, `row`) that differ from its top-left pixel (its background).
    fn ink(&self, col: u16, row: u16) -> usize {
        let r = &self.renderer;
        let m = r.metrics();
        let (x0, y0) = (
            r.padding() + u32::from(col) * m.width,
            r.padding() + u32::from(row) * m.height,
        );
        let bg = r.frame().pixel(x0, y0);
        let mut n = 0;
        for y in y0..y0 + m.height {
            for x in x0..x0 + m.width {
                n += usize::from(r.frame().pixel(x, y) != bg);
            }
        }
        n
    }

    fn background(&self, col: u16, row: u16) -> Rgb {
        let r = &self.renderer;
        let m = r.metrics();
        r.frame().pixel(
            r.padding() + u32::from(col) * m.width,
            r.padding() + u32::from(row) * m.height,
        )
    }

    /// The first row containing `needle`.
    fn row_of(&self, needle: &str) -> u16 {
        u16::try_from(
            self.text()
                .iter()
                .position(|l| l.contains(needle))
                .unwrap_or_else(|| panic!("`{needle}` not on screen")),
        )
        .unwrap()
    }

    /// The cell where `needle` starts on `row` (counting cells, not characters).
    fn col_of(&self, row: u16, needle: &str) -> u16 {
        let r = self.view.row_at(row);
        let first = needle.chars().next().unwrap();
        for x in 0..r.cols() {
            let c = r.cell(x);
            if c.ch == first && c.width != CellWidth::Tail {
                let mut s = String::new();
                let mut y = x;
                while y < r.cols() && s.chars().count() < needle.chars().count() {
                    let c = r.cell(y);
                    if c.width != CellWidth::Tail {
                        s.push(c.ch);
                    }
                    y += 1;
                }
                if s == needle {
                    return u16::try_from(x).unwrap();
                }
            }
        }
        panic!(
            "`{needle}` not on row {row}: {:?}",
            self.text()[usize::from(row)]
        );
    }
}

// ---------------------------------------------------------------------------------------------
// Programs built into this test binary.

fn enable_vt() {
    use windows_sys::Win32::System::Console::{
        ENABLE_VIRTUAL_TERMINAL_PROCESSING, GetConsoleMode, GetStdHandle, STD_OUTPUT_HANDLE,
        SetConsoleMode,
    };
    // SAFETY: plain console calls on this process's own standard output handle.
    unsafe {
        let h = GetStdHandle(STD_OUTPUT_HANDLE);
        let mut m = 0;
        GetConsoleMode(h, &raw mut m);
        SetConsoleMode(h, m | ENABLE_VIRTUAL_TERMINAL_PROCESSING);
    }
}

/// A full-screen program (the Step 13/14 stand-in): alternate screen, colours, a scroll region,
/// box drawing, Korean, a status bar and a hidden cursor; waits for one line, then restores the
/// screen. Runs only inside a pane, when `SUBAK_TEST_TUI` is set.
#[test]
fn tui_helper() {
    if std::env::var_os("SUBAK_TEST_TUI").is_none() {
        return;
    }
    enable_vt();
    let mut o = std::io::stdout();
    write!(o, "\x1b[?1049h\x1b[2J\x1b[H\x1b[?25l").unwrap();
    write!(o, "\x1b[1;31;44mTOP-LEFT\x1b[0m").unwrap();
    write!(o, "\x1b[3;5H┌──────┐\x1b[4;5H│ 한글 │\x1b[5;5H└──────┘").unwrap();
    write!(o, "\x1b[8;1r\x1b[r\x1b[10;10H\x1b[38;5;208mORANGE\x1b[0m").unwrap();
    write!(o, "\x1b[24;1H\x1b[7mSTATUS-BAR\x1b[0m").unwrap();
    write!(o, "\x1b[12;40H\x1b[32mTUI-READY\x1b[0m").unwrap();
    o.flush().unwrap();
    let mut line = String::new();
    let _ = std::io::stdin().read_line(&mut line);
    write!(o, "\x1b[?25h\x1b[?1049l").unwrap();
    o.flush().unwrap();
}

/// Writes the sequences whose passage through ConPTY is probed (mouse tracking, focus events,
/// bracketed paste, OSC 8), then a marker, and waits for one line.
#[test]
fn probe_helper() {
    if std::env::var_os("SUBAK_TEST_PROBE").is_none() {
        return;
    }
    enable_vt();
    let mut o = std::io::stdout();
    write!(o, "\x1b[?1000h\x1b[?1006h\x1b[?1004h\x1b[?2004h").unwrap();
    write!(
        o,
        "\x1b]8;;https://example.com/subak\x1b\\LINKED\x1b]8;;\x1b\\\r\n"
    )
    .unwrap();
    write!(o, "PROBE-READY\r\n").unwrap();
    o.flush().unwrap();
    let mut line = String::new();
    let _ = std::io::stdin().read_line(&mut line);
    write!(
        o,
        "\x1b[?1000l\x1b[?1006l\x1b[?1004l\x1b[?2004lPROBE-GOT-{}\r\n",
        line.trim().len()
    )
    .unwrap();
    o.flush().unwrap();
}

fn me(test: &str) -> String {
    format!(
        "\"{}\" {test} --exact --nocapture --test-threads=1",
        std::env::current_exe().unwrap().display()
    )
}

// ---------------------------------------------------------------------------------------------

/// cmd.exe: Korean typed through the view's input API comes back from ConPTY as two-cell
/// characters, renders with Malgun Gothic inside its two cells, and the view equals the Host's
/// own model. After a resize ConPTY repaints and both agree again at the new size.
#[test]
fn cmd_renders_korean_wide_glyphs_and_follows_a_resize() {
    let h = host("cmd");
    let ep = h.ep.as_str();
    new_terminal(ep, "c", &["--size", "80x24", "--", "cmd.exe", "/d", "/q"]);
    let mut c = Client::attach(ep, "c", (80, 24));
    c.wait_for(">", LONG);
    c.type_text("echo 한글-수박 wide-mark^er\r");
    c.wait_for("한글-수박 wide-marker", LONG);
    c.settle(ep);
    let row = c.row_of("wide-marker");
    let col = c.col_of(row, "한글-수박");
    let r = c.view.row_at(row);
    // Golden: 한 글 - 수 박, each Hangul syllable a wide cell followed by its tail.
    let cells: Vec<(char, CellWidth)> = (usize::from(col)..usize::from(col) + 9)
        .map(|x| (r.cell(x).ch, r.cell(x).width))
        .collect();
    use CellWidth::{Narrow, Tail, Wide};
    assert_eq!(
        cells,
        [
            ('한', Wide),
            (' ', Tail),
            ('글', Wide),
            (' ', Tail),
            ('-', Narrow),
            ('수', Wide),
            (' ', Tail),
            ('박', Wide),
            (' ', Tail)
        ]
    );
    drop(r);
    assert_eq!(
        c.text(),
        host_screen(ep, "c"),
        "the view equals the Host model"
    );
    c.render("cmd-korean-80x24");
    for x in [col, col + 2, col + 5, col + 7] {
        assert!(c.ink(x, row) > 0, "left half of a syllable at {x}");
        assert!(
            c.ink(x + 1, row) > 0,
            "right half of a syllable at {}",
            x + 1
        );
    }
    assert_eq!(c.renderer.face_for('한').family, "Malgun Gothic");
    // Resize: the view resizes, the Host tells ConPTY, ConPTY repaints.
    c.resize(60, 20);
    c.type_text("echo after-resize-mark^er\r");
    c.wait_for("after-resize-marker", LONG);
    c.settle(ep);
    assert_eq!(c.view.size(), (60, 20));
    assert_eq!(
        c.text(),
        host_screen(ep, "c"),
        "equal again after the resize"
    );
    c.render("cmd-korean-60x20");
    let (w, hpx) = (c.renderer.frame().width, c.renderer.frame().height);
    let m = c.renderer.metrics();
    assert_eq!((w, hpx), (60 * m.width + 16, 20 * m.height + 16));
    let row = c.row_of("wide-marker");
    let col = c.col_of(row, "한글");
    assert!(c.ink(col, row) > 0 && c.ink(col + 1, row) > 0);
    evidence!(
        "EVIDENCE cmd: Korean echoed through ConPTY as wide cells at column {col}, rendered with \
         Malgun Gothic in both cells; view == Host model at 80x24 and after a resize to 60x20 \
         (frame {w}x{hpx}, cell {}x{})",
        m.width,
        m.height
    );
}

/// Windows PowerShell 5.1: the prompt, colours from Write-Host, Korean output and the progress
/// bar of Write-Progress render with the colours ConPTY sends.
#[test]
fn powershell_prompt_colours_korean_and_progress() {
    let h = host("ps");
    let ep = h.ep.as_str();
    new_terminal(
        ep,
        "p",
        &[
            "--size",
            "100x30",
            "--env",
            "POWERSHELL_UPDATECHECK=Off",
            "--env",
            "POWERSHELL_TELEMETRY_OPTOUT=1",
            "--",
            "powershell",
            "-NoLogo",
            "-NoProfile",
        ],
    );
    let mut c = Client::attach(ep, "p", (100, 30));
    c.wait_for("PS ", PS_START);
    c.type_text(
        "Write-Host -ForegroundColor Green 'ps-green-mark'; Write-Host ('한글' + '-ps-mark')\r",
    );
    c.wait_for("한글-ps-mark", LONG);
    c.settle(ep);
    let row = c.text().iter().rposition(|l| l == "ps-green-mark").unwrap();
    let row = u16::try_from(row).unwrap();
    let fg = c.view.row_at(row).cell(0).attrs.fg;
    assert!(
        matches!(fg, Color::Indexed(2 | 10)),
        "Write-Host Green arrives as an ANSI green: {fg:?}"
    );
    c.render("powershell-colours");
    let t = Theme::kim_dark();
    let green = if fg == Color::Indexed(2) {
        t.ansi[2]
    } else {
        t.ansi[10]
    };
    let (m, r) = (c.renderer.metrics(), &c.renderer);
    let (x0, y0) = (r.padding(), r.padding() + u32::from(row) * m.height);
    // The strongest ink of the text (antialiased edges are blends toward the background).
    let strongest = (y0..y0 + m.height)
        .flat_map(|y| (x0..x0 + 13 * m.width).map(move |x| (x, y)))
        .map(|(x, y)| r.frame().pixel(x, y))
        .max_by_key(|p| u32::from(p.1))
        .unwrap();
    assert!(
        strongest.1 > strongest.0 + 25 && strongest.1 > strongest.2 + 15 && strongest.1 >= 120,
        "the text is drawn in the theme's green {green:?}: {strongest:?}"
    );
    let k = c.row_of("한글-ps-mark");
    assert!(c.ink(0, k) > 0 && c.ink(1, k) > 0);
    // Write-Progress draws its bar at the top while the command runs.
    // The markers are built by concatenation so the echoed command line never matches them.
    c.type_text(
        "Write-Progress -Activity ('subak-' + 'progress') -Status 'half' -PercentComplete 50; \
         Start-Sleep -Seconds 4; Write-Host ('progress-' + 'done')\r",
    );
    c.wait_for("subak-progress", LONG);
    c.pump(Duration::from_millis(500));
    let prow = c.row_of("subak-progress");
    let attrs = c.view.row_at(prow).cell(0).attrs;
    c.render("powershell-progress");
    let bg = c.background(0, prow);
    assert_ne!(
        bg, t.background,
        "the progress area has its own background ({attrs:?})"
    );
    c.wait_for("progress-done", LONG);
    evidence!(
        "EVIDENCE powershell: Write-Host Green arrived as {fg:?} and was drawn in the theme green; \
         Korean output in two cells; Write-Progress drew its bar on row {prow} with background \
         {bg:?} ({attrs:?})"
    );
}

/// A full-screen program and `more`: the alternate screen with colours, box drawing, Korean and
/// a status bar renders where the program put it; `more` shows its prompt on the last row.
#[test]
fn full_screen_program_and_more_render() {
    let h = host("tui");
    let ep = h.ep.as_str();
    new_terminal(
        ep,
        "t",
        &[
            "--size",
            "80x24",
            "--env",
            "SUBAK_TEST_TUI=1",
            "--",
            "cmd.exe",
            "/d",
            "/q",
        ],
    );
    let mut c = Client::attach(ep, "t", (80, 24));
    c.wait_for(">", LONG);
    c.type_text(&format!("{}\r", me("tui_helper")));
    c.wait_for("TUI-READY", LONG);
    c.settle(ep);
    let t = c.text();
    assert!(t[0].starts_with("TOP-LEFT"), "{t:?}");
    assert_eq!(t[2], "    ┌──────┐");
    assert_eq!(t[3], "    │ 한글 │");
    assert_eq!(t[9], "         ORANGE");
    assert_eq!(t[11], format!("{}TUI-READY", " ".repeat(39)));
    assert_eq!(t[23], "STATUS-BAR");
    assert_eq!(t, host_screen(ep, "t"));
    let a = c.view.row_at(0).cell(0).attrs;
    assert_eq!(a.bg, Color::Indexed(4), "blue background");
    c.render("tui");
    let th = Theme::kim_dark();
    assert_eq!(c.background(0, 0), th.ansi[4]);
    assert_ne!(c.background(0, 23), th.background, "the inverse status bar");
    assert!(c.ink(5, 2) > 0 && c.ink(6, 3) > 0, "box drawing and Korean");
    assert!(
        !c.view.screen().cursor().visible,
        "the program hid the cursor"
    );
    c.type_text("\r");
    c.wait_for(">", LONG);
    c.settle(ep);
    assert!(!c.text().iter().any(|l| l.contains("TUI-READY")));
    // `more` paging a file.
    let dir = std::env::temp_dir().join(format!("subak-step15-more-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let file = dir.join("lines.txt");
    let body: String = (1..=100).map(|i| format!("more-line-{i}\r\n")).collect();
    std::fs::write(&file, body).unwrap();
    c.type_text(&format!("more \"{}\"\r", file.display()));
    c.wait_for("-- More", LONG);
    c.settle(ep);
    let t = c.text();
    assert!(t[23].starts_with("-- More"), "{t:?}");
    assert_eq!(t, host_screen(ep, "t"));
    c.render("more");
    assert!(c.ink(0, 23) > 0);
    c.type_text("q");
    let _ = std::fs::remove_dir_all(&dir);
    evidence!(
        "EVIDENCE tui+more: alternate screen with colours, box drawing, Korean and status bar \
         rendered at the program's positions; `more` prompt on the last row; view == Host model"
    );
}

/// `claude --version` in a pane (never an interactive session), when Claude Code is installed.
#[test]
fn claude_version_renders_when_installed() {
    use std::os::windows::process::CommandExt;
    let found = std::process::Command::new("where.exe")
        .arg("claude")
        .creation_flags(0x0800_0000)
        .output()
        .is_ok_and(|o| o.status.success());
    if !found {
        evidence!("EVIDENCE claude: not installed on this machine; smoke test skipped");
        return;
    }
    let h = host("claude");
    let ep = h.ep.as_str();
    new_terminal(ep, "cv", &["--size", "100x24", "--", "cmd.exe", "/d", "/q"]);
    let mut c = Client::attach(ep, "cv", (100, 24));
    c.wait_for(">", LONG);
    c.type_text("claude --version & echo claude-version-do^ne\r");
    c.wait_for("claude-version-done", Duration::from_secs(60));
    c.settle(ep);
    let t = c.text();
    let line = t
        .iter()
        .find(|l| l.contains("Claude Code"))
        .unwrap_or_else(|| panic!("no version line: {t:?}"));
    assert!(
        line.chars().next().is_some_and(|ch| ch.is_ascii_digit()),
        "{line}"
    );
    assert_eq!(t, host_screen(ep, "cv"));
    c.render("claude-version");
    evidence!("EVIDENCE claude: `claude --version` rendered: {line}");
}

/// Keyboard latency through the real pipeline, measured in the client view: a key event in the
/// view → bytes to the Host → ConPTY → cmd echoes → Host → the view's model → frame drawn.
#[test]
fn keyboard_latency_through_the_host_and_conpty() {
    let h = host("lat");
    let ep = h.ep.as_str();
    new_terminal(ep, "k", &["--size", "80x24", "--", "cmd.exe", "/d", "/q"]);
    let mut c = Client::attach(ep, "k", (80, 24));
    c.wait_for(">", LONG);
    c.settle(ep);
    c.render("latency-start");
    let mut samples = Vec::new();
    for i in 0..40 {
        let ch = char::from(b'a' + (i % 26) as u8);
        let x0 = c.view.screen().cursor().x;
        let t0 = Instant::now();
        let bytes = c.view.key(Key::Char(ch), Mods::NONE);
        c.send(&bytes);
        let until = Instant::now() + LONG;
        while c.view.screen().cursor().x == x0 {
            assert!(Instant::now() < until, "no echo for key {i}");
            c.pump(Duration::from_millis(1));
        }
        c.renderer.render(&c.view);
        samples.push(t0.elapsed().as_secs_f64() * 1000.0);
    }
    let bytes = c.view.key(Key::Char('c'), Mods::CTRL);
    c.send(&bytes);
    samples.sort_by(f64::total_cmp);
    let p = |q: f64| samples[((samples.len() - 1) as f64 * q).round() as usize];
    evidence!(
        "EVIDENCE latency: key in the view to echoed frame through Host + ConPTY + cmd, 40 keys: \
         p50 {:.1} ms, p95 {:.1} ms, max {:.1} ms",
        p(0.5),
        p(0.95),
        p(1.0)
    );
    assert!(p(0.5) < 250.0, "p50 {:.1} ms", p(0.5));
}

/// What ConPTY passes through to the Host on this Windows build: mouse tracking, focus events,
/// bracketed paste and OSC 8. Whatever arrives is modelled and encoded as the program asked;
/// what does not is recorded (the inbox ConPTY of Windows 10 renders through conhost).
#[test]
fn conpty_passthrough_of_mouse_paste_focus_and_hyperlinks() {
    let h = host("probe");
    let ep = h.ep.as_str();
    new_terminal(
        ep,
        "pr",
        &[
            "--size",
            "80x24",
            "--env",
            "SUBAK_TEST_PROBE=1",
            "--",
            "cmd.exe",
            "/d",
            "/q",
        ],
    );
    let mut c = Client::attach(ep, "pr", (80, 24));
    c.wait_for(">", LONG);
    c.type_text(&format!("{}\r", me("probe_helper")));
    c.wait_for("PROBE-READY", LONG);
    c.settle(ep);
    let m = c.view.screen().modes();
    let row = c.row_of("LINKED");
    let col = c.col_of(row, "LINKED");
    let link = c.view.link_at(col, row).map(|l| l.uri.to_string());
    let mouse = c.view.mouse(&MouseEvent {
        action: MouseAction::Press(MouseButton::Left),
        col: 3,
        row: 4,
        x_px: 0,
        y_px: 0,
        mods: Mods::NONE,
    });
    if m.mouse != subak_term::screen::MouseTracking::Off {
        assert!(
            matches!(mouse, MouseOutcome::Send(ref b) if !b.is_empty()),
            "{mouse:?}"
        );
    } else {
        assert_eq!(mouse, MouseOutcome::Local);
    }
    let paste = c.view.paste("abc");
    if m.bracketed_paste {
        assert_eq!(paste, b"\x1b[200~abc\x1b[201~");
    }
    c.send(&paste);
    c.type_text("\r");
    c.wait_for("PROBE-GOT-", LONG);
    evidence!(
        "EVIDENCE passthrough: mouse tracking {:?} encoding {:?}, focus events {}, bracketed \
         paste {}, OSC 8 link {:?}; the view sent {:?} for a click",
        m.mouse,
        m.mouse_encoding,
        m.focus_events,
        m.bracketed_paste,
        link,
        mouse
    );
}
