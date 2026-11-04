//! Step 13 — scrollback and terminal replay on real Windows ConPTY (docs/SCROLLBACK.md,
//! ADR 0030). `SUBAK-FR-001`, `SUBAK-FR-004`, `SUBAK-FR-012`, `SUBAK-FR-015`, `SUBAK-FR-042`,
//! `SUBAK-SEC-010`, `SUBAK-SEC-011`, `KIM-SEC-013`, `KIM-NFR-001`.
//!
//! Real `subak.exe` Hosts on private endpoints, real `cmd.exe` panes and a small full-screen
//! test program (this test binary, started in a pane, see [`tui_helper`]). Every viewer is a raw
//! IPC v1 client whose control messages are schema-checked; it feeds the snapshot and the live
//! output into a client-side copy of the Host's terminal model (`src/screen.rs`), the way a
//! terminal would, and checks sequence numbers frame by frame. "Coherent" is measured, not
//! eyeballed: a client that attached late ends up with the same history, screen, attributes and
//! cursor as one that watched from the start, and as the Host's own `pane.capture`.
//!
//! Headless: pseudo consoles have no window and every helper runs with `CREATE_NO_WINDOW`. Each
//! test stops (or kills) the Host it started. Lines starting with `EVIDENCE` record what was
//! measured; run with `--nocapture` to see them.
#![cfg(windows)]
#![allow(unsafe_code)]

#[path = "../../../xtask/src/jsonschema.rs"]
#[allow(dead_code)]
mod jsonschema;

/// The Host's terminal model, used here as the client terminal.
#[path = "../src/screen.rs"]
#[allow(dead_code)]
mod screen;

mod common;

use common::*;
use screen::{Limits, Pos, Screen};
use serde_json::{Value, json};
use std::io::Write;
use std::time::{Duration, Instant};
use windows_sys::Win32::Foundation::FILETIME;
use windows_sys::Win32::System::Diagnostics::ToolHelp::{
    CreateToolhelp32Snapshot, TH32CS_SNAPTHREAD, THREADENTRY32, Thread32First, Thread32Next,
};
use windows_sys::Win32::System::Threading::GetProcessTimes;

const SNAPSHOT: u8 = 1;
const INPUT: u8 = 2;

/// `subak --json --endpoint <ep> <args>`.
fn cli(ep: &str, args: &[&str]) -> (i32, Value) {
    let mut a = vec!["--endpoint", ep];
    a.extend_from_slice(args);
    run(&a)
}

fn ok(ep: &str, args: &[&str]) -> Value {
    let (code, doc) = cli(ep, args);
    assert_eq!(code, 0, "{args:?}: {doc}");
    validate("subak-cli-v1", &doc);
    doc["data"].clone()
}

/// A Host of its own with no Terminal (stopped and checked on drop by [`Host`]).
fn host(tag: &str) -> Host {
    Host::start(&format!("sb-{tag}"), &["--no-shell", "--idle-exit", "60"])
}

/// A raw `cli` request on a fresh connection.
fn request(ep: &str, method: &str, params: &Value) -> Value {
    Raw::handshake(ep, "cli").request(1, method, params)
}

/// The Host's own capture of a pane: every history line and the screen, with escapes.
fn host_capture(ep: &str, target: &str) -> Vec<String> {
    host_capture_from(ep, target, &json!("-"))
}

/// The Host's own capture from line `start` to the end of the screen, with escapes.
fn host_capture_from(ep: &str, target: &str, start: &Value) -> Vec<String> {
    let r = request(
        ep,
        "pane.capture",
        &json!({"target": target, "start": start, "end": "-", "format": "vt", "redact": false}),
    );
    assert_eq!(r["ok"], true, "{r}");
    ipc_def("result_pane_capture", &r["result"]);
    assert_eq!(r["result"]["truncated"], false);
    r["result"]["lines"]
        .as_array()
        .unwrap()
        .iter()
        .map(|l| l.as_str().unwrap().to_string())
        .collect()
}

fn output_bytes(ep: &str, target: &str) -> u64 {
    let r = request(ep, "terminal.info", &json!({"target": target}));
    r["result"]["terminal"]["windows"][0]["panes"][0]["output_bytes"]
        .as_u64()
        .unwrap()
}

/// A client terminal: a raw IPC connection attached to a pane, feeding a terminal model.
struct Viewer {
    raw: Raw,
    stream: u32,
    screen: Screen,
    /// The sequence number the next live frame must carry.
    next: u64,
    snapshots: usize,
    resyncs: usize,
    result: Value,
    /// The last frame was part of a snapshot (one may span several frames).
    in_snapshot: bool,
}

impl Viewer {
    fn attach(ep: &str, target: &str, interactive: bool, size: Option<(u16, u16)>) -> Self {
        Self::attach_with(ep, target, interactive, size, &Value::Null)
    }

    fn attach_with(
        ep: &str,
        target: &str,
        interactive: bool,
        size: Option<(u16, u16)>,
        history: &Value,
    ) -> Self {
        let raw = Raw::handshake(ep, "gui");
        let r = raw.request(1, "events.subscribe", &json!({"topics": ["panes"]}));
        assert_eq!(r["ok"], true, "{r}");
        let size = size.map_or(Value::Null, |(c, r)| json!({"cols": c, "rows": r}));
        let r = raw.request(
            2,
            "pane.attach",
            &json!({"target": target, "interactive": interactive, "size": size,
                    "history_lines": history}),
        );
        assert_eq!(r["ok"], true, "{r}");
        let res = r["result"].clone();
        ipc_def("result_pane_attach", &res);
        let cols = u16::try_from(res["snapshot"]["size"]["cols"].as_u64().unwrap()).unwrap();
        let rows = u16::try_from(res["snapshot"]["size"]["rows"].as_u64().unwrap()).unwrap();
        let seq = res["seq"].as_u64().unwrap();
        assert_eq!(res["replay_bytes"], res["snapshot"]["bytes"]);
        let mut v = Self {
            raw,
            stream: u32::try_from(res["stream_id"].as_u64().unwrap()).unwrap(),
            screen: Screen::new(
                cols,
                rows,
                Limits {
                    lines: 100_000,
                    bytes: usize::MAX / 4,
                },
            ),
            next: seq,
            snapshots: 0,
            resyncs: 0,
            result: res.clone(),
            in_snapshot: true,
        };
        // The snapshot follows the response at once, in frames flagged as such, all at `seq`.
        let want = res["snapshot"]["bytes"].as_u64().unwrap();
        let mut got = 0u64;
        let mut frames = 0;
        while got < want {
            match v.raw.next(LONG) {
                Got::Data(b) => {
                    let (stream, s, flags) = v.raw.last_data.get().unwrap();
                    assert_eq!(
                        (stream, s, flags),
                        (v.stream, seq, SNAPSHOT),
                        "snapshot frame"
                    );
                    got += b.len() as u64;
                    frames += 1;
                    v.screen.feed(&b);
                }
                other => panic!("expected the snapshot, got {other:?}"),
            }
        }
        assert_eq!(got, want);
        assert_eq!(frames, res["snapshot"]["frames"].as_u64().unwrap());
        v.snapshots = 1;
        v
    }

    /// Reads what arrives within `wait`.
    fn pump(&mut self, wait: Duration) {
        let until = Instant::now() + wait;
        loop {
            let left = until.saturating_duration_since(Instant::now());
            if left.is_zero() {
                return;
            }
            match self.raw.next(left) {
                Got::Data(b) => {
                    let (stream, seq, flags) = self.raw.last_data.get().unwrap();
                    assert_eq!(stream, self.stream);
                    if flags == SNAPSHOT {
                        // A resync: a fresh snapshot; live output continues at its seq.
                        if !self.in_snapshot {
                            self.snapshots += 1;
                        }
                        self.in_snapshot = true;
                        self.next = seq;
                        self.screen.feed(&b);
                        continue;
                    }
                    self.in_snapshot = false;
                    assert_eq!(flags, 0);
                    assert_eq!(seq, self.next, "no gap, no duplicate, no reordering");
                    self.next += b.len() as u64;
                    self.screen.feed(&b);
                }
                Got::Control(m) if m["event"] == "pane.resync" => {
                    self.resyncs += 1;
                    self.in_snapshot = false;
                }
                Got::Control(_) => {}
                Got::Timeout => return,
                Got::Eof => panic!("viewer disconnected"),
            }
        }
    }

    fn send(&self, bytes: &[u8]) {
        self.raw.write(&data(self.stream, 0, INPUT, bytes)).unwrap();
    }

    /// The screen rows as text.
    fn text(&self) -> Vec<String> {
        self.screen
            .capture(Pos::Line(0), Pos::Edge, false, false)
            .lines
    }

    /// History and screen with escapes, as `pane.capture` returns them.
    fn all(&self) -> Vec<String> {
        self.screen.capture(Pos::Edge, Pos::Edge, true, false).lines
    }

    fn history(&self) -> Vec<String> {
        self.screen
            .capture(Pos::Edge, Pos::Line(-1), false, false)
            .lines
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
}

/// Waits until the pane's output stops and every viewer has read all of it.
fn settle(ep: &str, target: &str, viewers: &mut [&mut Viewer]) {
    let until = Instant::now() + Duration::from_secs(60);
    loop {
        let total = output_bytes(ep, target);
        for v in viewers.iter_mut() {
            while v.next < total {
                assert!(
                    Instant::now() < until,
                    "viewer stuck at {} of {total}",
                    v.next
                );
                v.pump(Duration::from_millis(100));
            }
        }
        std::thread::sleep(Duration::from_millis(600));
        if output_bytes(ep, target) == total && viewers.iter().all(|v| v.next == total) {
            return;
        }
        assert!(Instant::now() < until, "the pane never went quiet");
    }
}

fn new_terminal(ep: &str, name: &str, extra: &[&str]) -> Value {
    let mut args = vec!["new", name];
    args.extend_from_slice(extra);
    ok(ep, &args)
}

// ---------------------------------------------------------------------------------------------
// A small full-screen program for the panes: alternate screen, colours, a scroll region,
// absolute cursor moves, bracketed paste, a hidden cursor. Runs only inside a pane, when
// `SUBAK_TEST_TUI` is set; it waits for one line of input, then restores the screen.

#[test]
fn tui_helper() {
    if std::env::var_os("SUBAK_TEST_TUI").is_none() {
        return;
    }
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
    let mut o = std::io::stdout();
    write!(o, "\x1b[?1049h\x1b[2J\x1b[H\x1b[?25l").unwrap();
    write!(o, "\x1b[1;31;44mTOP-LEFT\x1b[0m").unwrap();
    write!(o, "\x1b[5;10H\x1b[38;5;208mORANGE-AT-5-10\x1b[0m").unwrap();
    write!(o, "\x1b[3;20r\x1b[20;1Hscroll-a\nscroll-b\nscroll-c").unwrap();
    write!(o, "\x1b[r\x1b[24;1H\x1b[7mSTATUS-BAR\x1b[0m\x1b[?2004h").unwrap();
    write!(o, "\x1b[12;40H\x1b[32mTUI-READY\x1b[0m").unwrap();
    o.flush().unwrap();
    let mut line = String::new();
    let _ = std::io::stdin().read_line(&mut line);
    write!(o, "\x1b[?2004l\x1b[?25h\x1b[?1049l").unwrap();
    o.flush().unwrap();
}

/// The pane command line that runs [`tui_helper`].
fn tui_command_line() -> String {
    format!(
        "\"{}\" tui_helper --exact --nocapture --test-threads=1",
        std::env::current_exe().unwrap().display()
    )
}

// ---------------------------------------------------------------------------------------------

/// Reattach after shell output and scrolling: the late client gets the same history, screen,
/// attributes and cursor as one attached from the start, and as the Host's capture.
#[test]
fn reattach_after_shell_output_and_scrolling_reproduces_history_and_screen() {
    let h = host("scroll");
    let ep = h.ep.as_str();
    new_terminal(ep, "sh", &["--size", "80x24", "--", "cmd.exe", "/d", "/q"]);
    let mut a = Viewer::attach(ep, "sh", true, Some((80, 24)));
    a.wait_for(">", LONG);
    a.send(b"for /L %i in (1,1,150) do @echo line-%i\r");
    a.wait_for("line-150", LONG);
    a.send(b"echo done-mark^er\r");
    a.wait_for("done-marker", LONG);
    // A second client attaches after the scrolling; a third after it detached and came back.
    let mut b = Viewer::attach(ep, "sh", false, None);
    settle(ep, "sh", &mut [&mut a, &mut b]);
    let host_view = host_capture(ep, "sh");
    assert_eq!(b.all(), a.all(), "late client == client from the start");
    assert_eq!(b.all(), host_view, "late client == Host capture");
    assert_eq!(b.screen.cursor(), a.screen.cursor());
    let hist = b.history();
    assert!(hist.iter().any(|l| l == "line-1"), "{hist:?}");
    let first = hist.iter().position(|l| l == "line-1").unwrap();
    for (i, l) in hist[first..].iter().take(120).enumerate() {
        assert_eq!(l, &format!("line-{}", i + 1), "history in order");
    }
    // Detach, more output while nobody watches, reattach.
    drop(b);
    a.send(b"for /L %i in (151,1,190) do @echo line-%i\r");
    a.wait_for("line-190", LONG);
    drop(a);
    std::thread::sleep(Duration::from_millis(500));
    let mut c = Viewer::attach(ep, "sh", false, None);
    settle(ep, "sh", &mut [&mut c]);
    assert_eq!(c.all(), host_capture(ep, "sh"));
    assert!(c.history().iter().any(|l| l == "line-160"));
    assert!(c.text().iter().any(|l| l == "line-190"));
    // The history counters agree.
    let info = ok(ep, &["history", "sh"]);
    let lines = info["history"]["lines"].as_u64().unwrap();
    assert_eq!(lines as usize, c.history().len());
    eprintln!(
        "EVIDENCE scroll: 190 lines through ConPTY at 80x24; late attach got {lines} history lines \
         and the same screen as the continuous client and the Host ({} snapshot bytes)",
        c.result["snapshot"]["bytes"]
    );
}

/// A full-screen program: a client attaching while it runs sees the program's screen exactly
/// (text, colours, positions, modes); after the program exits, the shell's screen is back.
#[test]
fn reattach_during_and_after_a_full_screen_program() {
    let h = host("tui");
    let ep = h.ep.as_str();
    new_terminal(
        ep,
        "tui",
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
    let mut a = Viewer::attach(ep, "tui", true, Some((80, 24)));
    a.wait_for(">", LONG);
    a.send(b"echo before-tui-mark^er\r");
    a.wait_for("before-tui-marker", LONG);
    a.send(format!("{}\r", tui_command_line()).as_bytes());
    a.wait_for("TUI-READY", LONG);
    let mut b = Viewer::attach(ep, "tui", false, None);
    settle(ep, "tui", &mut [&mut a, &mut b]);
    let t = b.text();
    assert!(t[0].starts_with("TOP-LEFT"), "{t:?}");
    // ORANGE was drawn at row 5 inside the 3..20 scroll region, which then scrolled twice.
    assert_eq!(t[2], "         ORANGE-AT-5-10");
    assert_eq!(t[11], format!("{}TUI-READY", " ".repeat(39)));
    assert_eq!(t[23], "STATUS-BAR");
    assert!(t.iter().any(|l| l == "scroll-c"), "{t:?}");
    assert_eq!(b.all(), a.all(), "colours and attributes too");
    assert_eq!(b.all(), host_capture(ep, "tui"));
    assert_eq!(b.screen.cursor(), a.screen.cursor());
    assert!(!b.screen.cursor().visible, "the program hid the cursor");
    // Bracketed paste, which conhost passes through, is replayed to the late client.
    let snap = Raw::handshake(ep, "gui");
    let r = snap.request(
        1,
        "pane.attach",
        &json!({"target": "tui", "interactive": false}),
    );
    assert_eq!(r["ok"], true);
    let mut bytes = Vec::new();
    while (bytes.len() as u64) < r["result"]["snapshot"]["bytes"].as_u64().unwrap() {
        if let Got::Data(d) = snap.next(LONG) {
            bytes.extend(d);
        }
    }
    let s = String::from_utf8_lossy(&bytes);
    assert!(s.contains("\x1b[?2004h"), "mode replayed");
    assert!(s.contains("44m"), "the blue background");
    drop(snap);
    // The program ends: the shell's screen comes back, for old and new clients alike.
    a.send(b"\r");
    a.wait_for("before-tui-marker", LONG);
    let mut c = Viewer::attach(ep, "tui", false, None);
    settle(ep, "tui", &mut [&mut a, &mut b, &mut c]);
    assert!(c.text().iter().any(|l| l.contains("before-tui-marker")));
    assert!(!c.text().iter().any(|l| l.contains("TUI-READY")));
    assert_eq!(c.all(), a.all());
    assert_eq!(b.all(), a.all());
    assert_eq!(c.all(), host_capture(ep, "tui"));
    eprintln!(
        "EVIDENCE tui: alternate-screen program reproduced on a late attach (text, SGR, positions, \
         hidden cursor, bracketed paste); after it exited the shell screen returned on all 3 clients"
    );
}

/// `more` (ships with Windows) paging a file: a late client sees the same page and prompt.
#[test]
fn reattach_while_more_pages_a_file() {
    let h = host("more");
    let ep = h.ep.as_str();
    let dir = std::env::temp_dir().join(format!("subak-step13-more-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let file = dir.join("lines.txt");
    let body: String = (1..=100).map(|i| format!("more-line-{i}\r\n")).collect();
    std::fs::write(&file, body).unwrap();
    new_terminal(
        ep,
        "pager",
        &["--size", "80x24", "--", "cmd.exe", "/d", "/q"],
    );
    let mut a = Viewer::attach(ep, "pager", true, Some((80, 24)));
    a.wait_for(">", LONG);
    a.send(format!("more \"{}\"\r", file.display()).as_bytes());
    a.wait_for("-- More", LONG);
    let mut b = Viewer::attach(ep, "pager", false, None);
    settle(ep, "pager", &mut [&mut a, &mut b]);
    assert_eq!(b.all(), a.all());
    assert!(b.text().iter().any(|l| l.starts_with("-- More")));
    a.send(b" ");
    a.wait_for("more-line-45", LONG);
    let mut c = Viewer::attach(ep, "pager", false, None);
    settle(ep, "pager", &mut [&mut a, &mut b, &mut c]);
    assert_eq!(c.all(), a.all());
    assert_eq!(b.all(), a.all());
    a.send(b"q");
    let _ = std::fs::remove_dir_all(&dir);
    eprintln!(
        "EVIDENCE more: late clients saw the same page and prompt of `more`, before and after paging"
    );
}

/// Clients attach again and again while the pane prints numbered lines as fast as cmd can:
/// every one of them reconstructs exactly the same sequence (each line once, in order), and its
/// live frames continue the snapshot without a gap.
#[test]
fn concurrent_output_during_attach_is_never_lost_duplicated_or_reordered() {
    let h = host("race");
    let ep = h.ep.as_str();
    new_terminal(
        ep,
        "race",
        &[
            "--size",
            "80x24",
            "--history-limit",
            "20000",
            "--",
            "cmd.exe",
            "/d",
            "/q",
        ],
    );
    let mut a = Viewer::attach(ep, "race", true, Some((80, 24)));
    a.wait_for(">", LONG);
    a.send(b"for /L %i in (1,1,10000) do @echo n-%i\r");
    let mut late = Vec::new();
    let t0 = Instant::now();
    while late.len() < 8 {
        a.pump(Duration::from_millis(60 + 10 * late.len() as u64));
        late.push(Viewer::attach(ep, "race", false, None));
    }
    let attach_window = t0.elapsed();
    a.wait_for("n-10000", Duration::from_secs(180));
    {
        let mut all: Vec<&mut Viewer> = late.iter_mut().collect();
        all.push(&mut a);
        settle(ep, "race", &mut all);
    }
    let numbers = |v: &Viewer| -> Vec<u32> {
        v.screen
            .capture(Pos::Edge, Pos::Edge, false, false)
            .lines
            .iter()
            .filter_map(|l| l.strip_prefix("n-").and_then(|n| n.parse().ok()))
            .collect()
    };
    let reference = numbers(&a);
    assert_eq!(
        reference,
        (1..=10000).collect::<Vec<_>>(),
        "the continuous client saw every line once, in order"
    );
    let host_view = host_capture(ep, "race");
    for (i, v) in late.iter().enumerate() {
        assert_eq!(numbers(v), reference, "late client {i}");
        assert_eq!(v.all(), host_view, "late client {i} == Host");
        assert_eq!(v.snapshots, 1);
    }
    let attached_at: Vec<u64> = late
        .iter()
        .map(|v| v.result["seq"].as_u64().unwrap())
        .collect();
    assert!(
        attached_at.windows(2).filter(|w| w[0] < w[1]).count() >= 4,
        "the attaches happened while output flowed: {attached_at:?}"
    );
    eprintln!(
        "EVIDENCE race: 8 clients attached at seq {attached_at:?} within {attach_window:?} while \
         10000 lines were printed; each reconstructed n-1..n-10000 exactly once and in order, and \
         equals the Host's capture"
    );
}

/// One size per pane: a read-only viewer never resizes it and gets snapshots at the pane's
/// size; an interactive client's resize reaches the shell, ConPTY repaints, and every client
/// converges; history rows re-wrap at a narrower client's width.
#[test]
fn resize_and_readers_with_different_sizes_follow_the_pane_size() {
    let h = host("resize");
    let ep = h.ep.as_str();
    new_terminal(ep, "rs", &["--size", "100x30", "--", "cmd.exe", "/d", "/q"]);
    let mut a = Viewer::attach(ep, "rs", true, Some((100, 30)));
    a.wait_for(">", LONG);
    let long = "W".repeat(95);
    for i in 0..40 {
        a.send(format!("echo {i:02}-{long}\r").as_bytes());
    }
    a.wait_for("39-WWW", LONG);
    // A viewer asking for another size gets the pane's size: viewers never resize.
    let mut v = Viewer::attach(ep, "rs", false, Some((60, 20)));
    assert_eq!(
        v.result["snapshot"]["size"],
        json!({"cols": 100, "rows": 30})
    );
    settle(ep, "rs", &mut [&mut a, &mut v]);
    assert_eq!(v.all(), a.all());
    // The interactive client resizes; the shell sees it; everything converges after the repaint.
    let r = a.raw.request(
        9,
        "pane.resize",
        &json!({"cols": 80, "rows": 24, "stream_id": a.stream}),
    );
    assert_eq!(r["ok"], true, "{r}");
    a.screen.resize(80, 24);
    v.screen.resize(80, 24);
    a.send(b"mode con | find \"Col\"\r");
    a.wait_for("Columns:", LONG);
    settle(ep, "rs", &mut [&mut a, &mut v]);
    let cols_line = a
        .text()
        .into_iter()
        .find(|l| l.contains("Columns:"))
        .unwrap();
    assert!(cols_line.trim_end().ends_with("80"), "{cols_line}");
    let host_view = host_capture(ep, "rs");
    let screen_rows = |lines: &[String]| lines[lines.len() - 24..].to_vec();
    assert_eq!(screen_rows(&a.all()), screen_rows(&host_view));
    assert_eq!(screen_rows(&v.all()), screen_rows(&host_view));
    // A new client gets the new size; the 100-column history rows wrap at its 60 columns and
    // join back to the same lines.
    let c = Viewer::attach(ep, "rs", false, None);
    assert_eq!(
        c.result["snapshot"]["size"],
        json!({"cols": 80, "rows": 24})
    );
    let st = ok(ep, &["info", "rs"]);
    assert_eq!(
        st["terminal"]["windows"][0]["panes"][0]["size"],
        json!({"cols": 80, "rows": 24})
    );
    let r = request(
        ep,
        "pane.capture",
        &json!({"target": "rs", "start": "-", "end": -1}),
    );
    let host_hist: Vec<String> = r["result"]["lines"]
        .as_array()
        .unwrap()
        .iter()
        .map(|l| l.as_str().unwrap().to_string())
        .collect();
    let mut narrow = Screen::new(
        80,
        24,
        Limits {
            lines: 100_000,
            bytes: usize::MAX / 4,
        },
    );
    let raw = Raw::handshake(ep, "gui");
    let r = raw.request(
        1,
        "pane.attach",
        &json!({"target": "rs", "interactive": false}),
    );
    let want = r["result"]["snapshot"]["bytes"].as_u64().unwrap();
    let mut got = 0;
    while got < want {
        if let Got::Data(d) = raw.next(LONG) {
            got += d.len() as u64;
            narrow.feed(&d);
        }
    }
    let joined = narrow.capture(Pos::Edge, Pos::Line(-1), false, true).lines;
    assert!(
        joined.len() >= host_hist.len() && joined.ends_with(&host_hist),
        "re-wrapped history joins back:\n{joined:?}\n{host_hist:?}"
    );
    assert!(
        narrow
            .capture(Pos::Edge, Pos::Line(-1), false, false)
            .lines
            .len()
            > joined.len(),
        "100-column rows from before the resize wrapped at 80 columns"
    );
    eprintln!(
        "EVIDENCE resize: viewer asked 60x20, got the pane's 100x30; resize to 80x24 reached cmd \
         (`{}`); all clients equal the Host after the repaint; {} history rows from before the \
         resize re-wrapped at 80 columns in a new client and joined back",
        cols_line.trim(),
        host_hist.len()
    );
}

/// Deterministic eviction at the line limit, `clear-history`, and a limit of 0.
#[test]
fn history_is_bounded_evicts_the_oldest_lines_and_clears_on_request() {
    let h = host("evict");
    let ep = h.ep.as_str();
    new_terminal(
        ep,
        "small",
        &[
            "--size",
            "80x10",
            "--history-limit",
            "50",
            "--",
            "cmd.exe",
            "/d",
            "/q",
        ],
    );
    let mut a = Viewer::attach(ep, "small", true, Some((80, 10)));
    a.wait_for(">", LONG);
    a.send(b"for /L %i in (1,1,200) do @echo e-%i\r");
    a.wait_for("e-200", LONG);
    settle(ep, "small", &mut [&mut a]);
    let cap = ok(ep, &["capture", "small", "--history", "--end", "-1"]);
    let hist: Vec<String> = cap["lines"]
        .as_array()
        .unwrap()
        .iter()
        .map(|l| l.as_str().unwrap().to_string())
        .collect();
    assert_eq!(hist.len(), 50, "{hist:?}");
    let nums: Vec<u32> = hist
        .iter()
        .filter_map(|l| l.strip_prefix("e-").and_then(|n| n.parse().ok()))
        .collect();
    assert!(nums.windows(2).all(|w| w[1] == w[0] + 1), "{nums:?}");
    let screen = ok(ep, &["capture", "small"]);
    let first_on_screen: u32 = screen["lines"]
        .as_array()
        .unwrap()
        .iter()
        .find_map(|l| {
            l.as_str()
                .unwrap()
                .strip_prefix("e-")
                .and_then(|n| n.parse().ok())
        })
        .unwrap();
    assert_eq!(
        *nums.last().unwrap() + 1,
        first_on_screen,
        "the newest lines are kept"
    );
    let info = ok(ep, &["history", "small"]);
    let hi = &info["history"];
    assert_eq!(hi["lines"], 50);
    assert_eq!(hi["limit_lines"], 50);
    assert_eq!(
        hi["evicted_lines"].as_u64().unwrap(),
        hi["total_lines"].as_u64().unwrap() - 50
    );
    // clear-history: refused without consent when it cannot ask, planned by a dry run.
    let (code, doc) = cli(ep, &["clear-history", "small"]);
    assert_eq!(
        (code, doc["error"]["code"].as_str()),
        (7, Some("confirmation_required")),
        "{doc}"
    );
    let plan = ok(ep, &["clear-history", "small", "--dry-run"]);
    assert_eq!(plan["would_clear"]["lines"], 50);
    let screen_before = ok(ep, &["capture", "small"])["lines"].clone();
    let done = ok(ep, &["clear-history", "small", "--yes"]);
    assert_eq!(done["cleared_lines"], 50);
    assert_eq!(done["history"]["lines"], 0);
    assert_eq!(done["history"]["cleared_lines"], 50);
    assert_eq!(
        ok(ep, &["capture", "small"])["lines"],
        screen_before,
        "the screen stays"
    );
    let empty = ok(ep, &["capture", "small", "--history", "--end", "-1"]);
    assert_eq!(empty["lines"], json!([]));
    let v = Viewer::attach(ep, "small", false, None);
    assert_eq!(v.result["snapshot"]["history_lines"], 0);
    // A limit of 0 keeps no history at all.
    new_terminal(
        ep,
        "none",
        &[
            "--size",
            "80x10",
            "--history-limit",
            "0",
            "--",
            "cmd.exe",
            "/d",
            "/q",
        ],
    );
    let mut z = Viewer::attach(ep, "none", true, Some((80, 10)));
    z.wait_for(">", LONG);
    z.send(b"for /L %i in (1,1,40) do @echo z-%i\r");
    z.wait_for("z-40", LONG);
    settle(ep, "none", &mut [&mut z]);
    let zi = ok(ep, &["history", "none"]);
    assert_eq!(zi["history"]["lines"], 0);
    assert!(zi["history"]["evicted_lines"].as_u64().unwrap() >= 30);
    // A limit outside 0..100000 is a usage error, checked before anything starts.
    let (code, _) = cli(ep, &["new", "big", "--history-limit", "100001"]);
    assert_eq!(code, 2);
    eprintln!(
        "EVIDENCE evict: limit 50 kept e-{}..e-{} ({} evicted of {}); clear-history needed --yes, \
         removed 50 lines and kept the screen; limit 0 kept none",
        nums[0], nums[49], hi["evicted_lines"], hi["total_lines"]
    );
}

/// A client that stops reading during sustained output is not disconnected and costs bounded
/// memory; when it reads again it is resynced to exactly the Host's screen.
#[test]
fn a_stalled_client_during_sustained_output_is_resynced_to_the_same_screen() {
    let h = host("stall");
    let ep = h.ep.as_str();
    let dir = std::env::temp_dir().join(format!("subak-step13-stall-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let file = dir.join("big.txt");
    let line =
        "0123456789abcdefghijklmnopqrstuvwxyz \u{d55c}\u{ae00} ABCDEFGHIJKLMNOPQRSTUVWXYZ\r\n";
    std::fs::write(&file, line.repeat(20 * 1024 * 1024 / line.len())).unwrap();
    new_terminal(
        ep,
        "big",
        &["--size", "100x30", "--", "cmd.exe", "/d", "/q"],
    );
    let mut a = Viewer::attach(ep, "big", true, Some((100, 30)));
    a.wait_for(">", LONG);
    let stalled = Viewer::attach(ep, "big", false, None);
    let mem0 = h.stats()["host"]["memory"]["private_bytes"]
        .as_u64()
        .unwrap();
    a.send(format!("type \"{}\" & echo stall-done-mark^er\r", file.display()).as_bytes());
    let mut peak = mem0;
    let mut sampled = Instant::now();
    let until = Instant::now() + Duration::from_secs(240);
    while !a.text().iter().any(|l| l.contains("stall-done-marker")) {
        assert!(Instant::now() < until, "the output never finished");
        a.pump(Duration::from_millis(200));
        if sampled.elapsed() > Duration::from_secs(2) {
            let m = h.stats()["host"]["memory"]["private_bytes"]
                .as_u64()
                .unwrap();
            peak = peak.max(m);
            sampled = Instant::now();
        }
    }
    let s = h.stats();
    let skipped = s["ipc"]["output_frames_skipped"].as_u64().unwrap();
    assert!(skipped > 0, "the stalled client fell behind: {s}");
    assert_eq!(
        s["ipc"]["lagging_clients_dropped"], 0,
        "and was not dropped"
    );
    // It reads again: the in-flight output, then pane.resync, a snapshot, and live output.
    let mut stalled = stalled;
    settle(ep, "big", &mut [&mut a, &mut stalled]);
    assert!(stalled.resyncs >= 1, "pane.resync arrived");
    assert!(stalled.snapshots >= 2, "a fresh snapshot arrived");
    // (2000 history lines do not fit in one capture answer; the screen is what must match.)
    let host_view = host_capture_from(ep, "big", &json!(0));
    let screen_rows = |v: &Viewer| v.screen.capture(Pos::Line(0), Pos::Edge, true, false).lines;
    assert_eq!(screen_rows(&stalled), host_view);
    assert_eq!(screen_rows(&a), host_view);
    let st = h.stats();
    assert!(st["ipc"]["lagging_resyncs"].as_u64().unwrap() >= 1);
    let grew = peak.saturating_sub(mem0);
    assert!(grew < 64 << 20, "bounded: private bytes grew by {grew}");
    let hist = &request(ep, "terminal.info", &json!({"target": "big"}))["result"]["terminal"]["windows"]
        [0]["panes"][0]["history"];
    assert!(hist["bytes"].as_u64().unwrap() <= screen::HISTORY_BYTES as u64);
    assert_eq!(
        hist["lines"], 2000,
        "the default limit holds under sustained output"
    );
    let _ = std::fs::remove_dir_all(&dir);
    eprintln!(
        "EVIDENCE stall: 20 MiB through ConPTY; a client that stopped reading stayed connected \
         ({skipped} frames skipped, {} resyncs), then matched the Host's screen exactly; Host \
         private bytes peak +{:.1} MiB; history held at {} lines / {} bytes",
        st["ipc"]["lagging_resyncs"],
        grew as f64 / 1_048_576.0,
        hist["lines"],
        hist["bytes"]
    );
}

fn cpu_ms(p: &Proc) -> u64 {
    let mut t = [FILETIME::default(); 4];
    // SAFETY: valid process handle with query access; four out-parameters of the right type.
    let ok = unsafe {
        GetProcessTimes(
            std::os::windows::io::AsRawHandle::as_raw_handle(&p.h),
            &raw mut t[0],
            &raw mut t[1],
            &raw mut t[2],
            &raw mut t[3],
        )
    };
    assert_ne!(ok, 0);
    let ft = |f: FILETIME| (u64::from(f.dwHighDateTime) << 32) | u64::from(f.dwLowDateTime);
    (ft(t[2]) + ft(t[3])) / 10_000
}

fn threads_of(pid: u32) -> usize {
    // SAFETY: Toolhelp snapshot, closed below.
    let snap = unsafe { CreateToolhelp32Snapshot(TH32CS_SNAPTHREAD, 0) };
    let mut e = THREADENTRY32 {
        dwSize: u32::try_from(size_of::<THREADENTRY32>()).unwrap(),
        ..Default::default()
    };
    let mut n = 0;
    // SAFETY: `e.dwSize` is set.
    let mut more = unsafe { Thread32First(snap, &raw mut e) } != 0;
    while more {
        if e.th32OwnerProcessID == pid {
            n += 1;
        }
        // SAFETY: same snapshot and entry.
        more = unsafe { Thread32Next(snap, &raw mut e) } != 0;
    }
    // SAFETY: valid snapshot handle.
    unsafe { windows_sys::Win32::Foundation::CloseHandle(snap) };
    n
}

/// Idle: two panes with their screens and history and two attached clients cost the Host no
/// CPU and no extra threads (no timers, no polling); snapshots and captures leave no threads.
#[test]
fn an_idle_host_with_screens_and_clients_does_no_work() {
    let h = host("idle");
    let ep = h.ep.as_str();
    for name in ["one", "two"] {
        new_terminal(ep, name, &["--size", "80x24", "--", "cmd.exe", "/d", "/q"]);
    }
    let mut a = Viewer::attach(ep, "one", true, Some((80, 24)));
    let mut b = Viewer::attach(ep, "two", false, None);
    a.wait_for(">", LONG);
    a.send(b"for /L %i in (1,1,60) do @echo idle-%i\r");
    a.wait_for("idle-60", LONG);
    settle(ep, "one", &mut [&mut a]);
    settle(ep, "two", &mut [&mut b]);
    std::thread::sleep(Duration::from_secs(1));
    let threads0 = threads_of(h.proc.pid);
    let cpu0 = cpu_ms(&h.proc);
    std::thread::sleep(Duration::from_secs(10));
    let cpu1 = cpu_ms(&h.proc);
    let threads1 = threads_of(h.proc.pid);
    assert!(
        cpu1 - cpu0 <= 32,
        "idle Host used {} ms of CPU in 10 s",
        cpu1 - cpu0
    );
    assert_eq!(threads1, threads0, "no thread appeared while idle");
    // Snapshots and captures are work on request only; they leave nothing running.
    for _ in 0..10 {
        let v = Viewer::attach(ep, "one", false, None);
        drop(v);
        let _ = ok(ep, &["capture", "one", "--history"]);
    }
    let until = Instant::now() + LONG;
    while threads_of(h.proc.pid) != threads0 {
        assert!(
            Instant::now() < until,
            "threads did not return to {threads0}"
        );
        std::thread::sleep(Duration::from_millis(100));
    }
    let st = h.status();
    validate(
        "subak-cli-v1",
        &json!({"ok": true, "command": "host.status", "schema_version": 1,
        "data": st, "warnings": []}),
    );
    assert_eq!(st["scrollback"]["panes"], 2);
    assert_eq!(st["scrollback"]["persisted"], false);
    eprintln!(
        "EVIDENCE idle: 2 panes, 2 attached clients, {} history lines kept: Host CPU {} ms over 10 s, \
         threads {threads0} -> {threads1}; 10 attach+capture cycles left {threads0} threads",
        st["scrollback"]["lines"],
        cpu1 - cpu0
    );
}

/// Terminal content is user data: only cli/gui clients may read or clear it, captures hide
/// secret-looking values unless asked not to, and no status, list or counter carries content.
#[test]
fn history_access_is_restricted_and_secrets_stay_out_of_json() {
    let h = host("secret");
    let ep = h.ep.as_str();
    let token = format!("ghp_{}", "Zx9".repeat(12));
    let password = "hunter2-step13-never-shown";
    // Wide enough that the command line does not wrap (a secret split over two rows would be
    // two half secrets; see docs/SCROLLBACK.md, known limits).
    new_terminal(
        ep,
        "sec",
        &["--size", "200x20", "--", "cmd.exe", "/d", "/q"],
    );
    let mut a = Viewer::attach(ep, "sec", true, Some((200, 20)));
    a.wait_for(">", LONG);
    a.send(format!("echo {token}& echo password={password}& echo secret-mark^er\r").as_bytes());
    a.wait_for("secret-marker", LONG);
    settle(ep, "sec", &mut [&mut a]);
    let leaks = |what: &str, s: &str| {
        assert!(!s.contains(&token), "{what} leaked the token");
        assert!(!s.contains(password), "{what} leaked the password");
    };
    // Redacted by default, in human output too.
    let cap = ok(ep, &["capture", "sec"]);
    leaks("capture --json", &cap.to_string());
    assert!(cap["redacted"].as_u64().unwrap() >= 2, "{cap}");
    assert!(cap.to_string().contains("ghp_***"));
    let human = std::process::Command::new(subak())
        .args(["--endpoint", ep, "capture", "sec"])
        .stdin(std::process::Stdio::null())
        .output()
        .unwrap();
    leaks("capture", &String::from_utf8_lossy(&human.stdout));
    leaks("capture (stderr)", &String::from_utf8_lossy(&human.stderr));
    // Shown only when asked for.
    let raw = ok(ep, &["capture", "sec", "--no-redact"]);
    assert!(raw.to_string().contains(&token));
    // No other document carries content (the pane title, which cmd sets to the command, is not
    // reported either).
    for args in [
        &["history", "sec"][..],
        &["info", "sec"],
        &["ls"],
        &["status"],
        &["host", "status"],
    ] {
        let d = ok(ep, args);
        leaks(&format!("{args:?}"), &d.to_string());
    }
    let info = ok(ep, &["info", "sec"]);
    assert_eq!(
        info["terminal"]["windows"][0]["panes"][0]["title"],
        Value::Null
    );
    leaks("host.stats", &h.stats().to_string());
    // Only cli and gui clients may read or clear history (IPC_PROTOCOL §4).
    for kind in ["hook", "mcp"] {
        let c = Raw::handshake(ep, kind);
        for (i, method) in ["pane.capture", "pane.clear_history"].iter().enumerate() {
            let r = c.request(i as u64 + 1, method, &json!({"target": "sec"}));
            assert_eq!(error_code(&r), "unauthorized", "{kind} {method}");
            leaks(kind, &r.to_string());
        }
        let r = c.request(9, "pane.attach", &json!({"target": "sec"}));
        assert_eq!(error_code(&r), "unauthorized", "{kind} attach");
    }
    // A raw IPC capture is redacted by default too.
    let r = request(ep, "pane.capture", &json!({"target": "sec"}));
    leaks("pane.capture", &r.to_string());
    assert_eq!(r["result"]["redact"], true);
    eprintln!(
        "EVIDENCE secrets: capture hid {} values by default (shown with --no-redact only); history, \
         info, ls, status, host status and host.stats carry no content; hook and mcp clients \
         were refused pane.capture, pane.clear_history and pane.attach",
        cap["redacted"]
    );
}
