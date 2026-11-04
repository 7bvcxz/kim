//! E2E — Step 15: the native terminal emulator (docs/TERMINAL_EMULATOR.md, ADR 0036).
//! `SUBAK-FR-004`, `SUBAK-FR-015`, `SUBAK-FR-031`, `SUBAK-NFR-001`, `KIM-NFR-013`.
//!
//! Scenario A (release `subak` + the `subak-term` library): a user creates a Terminal with
//! cmd.exe, attaches with the real `subak attach` (pipes, no window) and types a Korean command.
//! Everything `subak attach` writes — the Host's snapshot and the live stream — is fed into a
//! client view (`TermView`), exactly as the Subak GUI will: the view's screen equals the Host's
//! `subak capture`, Korean syllables sit in two cells each, and the view renders headlessly
//! with the system fonts (Malgun Gothic) with ink in both cells of every syllable.
//!
//! Scenario B (release `subak`): a second client attaches at another size: the pane resizes,
//! ConPTY repaints, and a view of the new size fed with that client's output equals the Host's
//! capture again; `more` paging a file shows its prompt on the last row of both.
//!
//! Windows only. Every scenario uses its own endpoint and stops what it started. Nothing here
//! opens a window.
#![cfg(windows)]

use kim_e2e::{TempDir, bin};
use serde_json::Value;
use std::io::{Read, Write};
use std::os::windows::process::CommandExt;
use std::path::Path;
use std::process::{Child, Command, Stdio};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};
use subak_term::TermView;
use subak_term::render::{RenderOptions, Renderer, Theme};
use subak_term::screen::CellWidth;

const CREATE_NO_WINDOW: u32 = 0x0800_0000;
const LONG: Duration = Duration::from_secs(30);

fn endpoint(tag: &str) -> String {
    format!("subak-e2e15-{tag}-{}", std::process::id())
}

/// `subak --json --endpoint <ep> <args>`.
fn json(exe: &Path, ep: &str, args: &[&str], sb: &TempDir) -> (i32, Value) {
    let out = Command::new(exe)
        .args(["--json", "--endpoint", ep])
        .args(args)
        .env("USERPROFILE", sb.path())
        .stdin(Stdio::null())
        .creation_flags(CREATE_NO_WINDOW)
        .output()
        .expect("run subak");
    let doc: Value = serde_json::from_slice(&out.stdout).unwrap_or_else(|e| {
        panic!(
            "{args:?}: not JSON ({e}): {}",
            String::from_utf8_lossy(&out.stdout)
        )
    });
    (out.status.code().unwrap_or(-1), doc)
}

/// The Host's screen as `subak capture --no-redact` reports it.
fn capture(exe: &Path, ep: &str, target: &str, sb: &TempDir) -> Vec<String> {
    let (code, doc) = json(exe, ep, &["capture", target, "--no-redact"], sb);
    assert_eq!(code, 0, "{doc}");
    doc["data"]["lines"]
        .as_array()
        .unwrap()
        .iter()
        .map(|l| l.as_str().unwrap().to_string())
        .collect()
}

/// Stops the Host and its Terminals however the scenario ends.
struct Cleanup<'a> {
    exe: &'a Path,
    ep: &'a str,
}

impl Drop for Cleanup<'_> {
    fn drop(&mut self) {
        let _ = Command::new(self.exe)
            .args(["--json", "--endpoint", self.ep, "host", "stop"])
            .args(["--kill-terminals", "--yes"])
            .stdin(Stdio::null())
            .creation_flags(CREATE_NO_WINDOW)
            .output();
    }
}

/// `subak attach` with piped input and output; its standard output is the terminal byte stream
/// a view consumes.
struct Attach {
    child: Child,
    out: Arc<Mutex<Vec<u8>>>,
}

impl Attach {
    fn new(exe: &Path, ep: &str, target: &str, size: &str) -> Self {
        let mut child = Command::new(exe)
            .args(["--endpoint", ep, "attach", target, "--size", size])
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .creation_flags(CREATE_NO_WINDOW)
            .spawn()
            .expect("attach");
        let out = Arc::new(Mutex::new(Vec::new()));
        let mut stdout = child.stdout.take().unwrap();
        let sink = Arc::clone(&out);
        std::thread::spawn(move || {
            let mut buf = [0u8; 8192];
            while let Ok(n) = stdout.read(&mut buf) {
                if n == 0 {
                    break;
                }
                sink.lock().unwrap().extend_from_slice(&buf[..n]);
            }
        });
        Self { child, out }
    }

    fn send(&mut self, bytes: &[u8]) {
        let stdin = self.child.stdin.as_mut().unwrap();
        stdin.write_all(bytes).unwrap();
        stdin.flush().unwrap();
    }

    /// A view of `cols × rows` fed with everything received so far.
    fn view(&self, cols: u16, rows: u16) -> TermView {
        let mut v = TermView::new(cols, rows, 2000);
        v.feed(&self.out.lock().unwrap());
        v
    }

    fn wait_screen(&self, cols: u16, rows: u16, needle: &str) -> TermView {
        let until = Instant::now() + LONG;
        loop {
            let v = self.view(cols, rows);
            if text(&v).iter().any(|l| l.contains(needle)) {
                return v;
            }
            assert!(
                Instant::now() < until,
                "`{needle}` not on the view:\n{}",
                text(&v).join("\n")
            );
            std::thread::sleep(Duration::from_millis(100));
        }
    }

    fn detach(&mut self) {
        self.send(&[0x02, b'd']);
        let until = Instant::now() + LONG;
        while self.child.try_wait().unwrap().is_none() {
            assert!(Instant::now() < until, "Ctrl+B d did not detach");
            std::thread::sleep(Duration::from_millis(50));
        }
    }
}

impl Drop for Attach {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

fn text(v: &TermView) -> Vec<String> {
    (0..v.size().1)
        .map(|r| v.row_at(r).text().trim_end().to_string())
        .collect()
}

/// Waits until the view of what `a` received equals the Host's capture.
fn converge(
    a: &Attach,
    cols: u16,
    rows: u16,
    subak: &Path,
    ep: &str,
    t: &str,
    sb: &TempDir,
) -> TermView {
    let until = Instant::now() + LONG;
    loop {
        let v = a.view(cols, rows);
        let host = capture(subak, ep, t, sb);
        if text(&v) == host {
            return v;
        }
        assert!(
            Instant::now() < until,
            "view and Host differ:\n{:?}\n{:?}",
            text(&v),
            host
        );
        std::thread::sleep(Duration::from_millis(200));
    }
}

fn ink(r: &Renderer, col: u16, row: u16) -> usize {
    let m = r.metrics();
    let (x0, y0) = (
        r.padding() + u32::from(col) * m.width,
        r.padding() + u32::from(row) * m.height,
    );
    let bg = r.frame().pixel(x0, y0);
    (y0..y0 + m.height)
        .flat_map(|y| (x0..x0 + m.width).map(move |x| (x, y)))
        .filter(|&(x, y)| r.frame().pixel(x, y) != bg)
        .count()
}

#[test]
#[ignore = "e2e: run via `cargo xtask e2e` only (AGENTS.md §4)"]
fn a_client_view_of_subak_attach_renders_korean_like_the_host_model() {
    let subak = bin("subak");
    let sb = TempDir::new("e2e15-view");
    let ep = endpoint("view");
    let _cleanup = Cleanup {
        exe: &subak,
        ep: &ep,
    };
    let (code, doc) = json(
        &subak,
        &ep,
        &[
            "new", "work", "--size", "80x24", "--", "cmd.exe", "/d", "/q",
        ],
        &sb,
    );
    assert_eq!(code, 0, "{doc}");
    let mut a = Attach::new(&subak, &ep, "work", "80x24");
    a.wait_screen(80, 24, ">");
    a.send("echo 한글-수박 e2e15-mark^er\r".as_bytes());
    a.wait_screen(80, 24, "한글-수박 e2e15-marker");
    let v = converge(&a, 80, 24, &subak, &ep, "work", &sb);
    let row = u16::try_from(
        text(&v)
            .iter()
            .position(|l| l == "한글-수박 e2e15-marker")
            .unwrap(),
    )
    .unwrap();
    let r = v.row_at(row);
    assert_eq!((r.cell(0).ch, r.cell(0).width), ('한', CellWidth::Wide));
    assert_eq!(r.cell(1).width, CellWidth::Tail);
    assert_eq!((r.cell(4).ch, r.cell(5).ch), ('-', '수'));
    drop(r);
    let mut renderer = Renderer::with_system_fonts(Theme::kim_dark(), RenderOptions::default())
        .expect("system fonts");
    renderer.render(&v);
    assert_eq!(renderer.face_for('한').family, "Malgun Gothic");
    for col in [0, 2, 5, 7] {
        assert!(
            ink(&renderer, col, row) > 0 && ink(&renderer, col + 1, row) > 0,
            "{col}"
        );
    }
    a.detach();
}

#[test]
#[ignore = "e2e: run via `cargo xtask e2e` only (AGENTS.md §4)"]
fn a_view_of_another_size_follows_the_resize_and_more() {
    let subak = bin("subak");
    let sb = TempDir::new("e2e15-resize");
    let ep = endpoint("resize");
    let _cleanup = Cleanup {
        exe: &subak,
        ep: &ep,
    };
    let dir = TempDir::new("e2e15-more");
    let file = dir.path().join("lines.txt");
    let body: String = (1..=100).map(|i| format!("e2e15-line-{i}\r\n")).collect();
    std::fs::write(&file, body).unwrap();
    let (code, doc) = json(
        &subak,
        &ep,
        &[
            "new", "pager", "--size", "80x24", "--", "cmd.exe", "/d", "/q",
        ],
        &sb,
    );
    assert_eq!(code, 0, "{doc}");
    let mut a = Attach::new(&subak, &ep, "pager", "80x24");
    a.wait_screen(80, 24, ">");
    a.detach();
    // Another client at 60x20: the pane takes its size and ConPTY repaints.
    let mut b = Attach::new(&subak, &ep, "pager", "60x20");
    b.wait_screen(60, 20, ">");
    b.send(format!("more \"{}\"\r", file.display()).as_bytes());
    b.wait_screen(60, 20, "-- More");
    let v = converge(&b, 60, 20, &subak, &ep, "pager", &sb);
    assert!(text(&v)[19].starts_with("-- More"), "{:?}", text(&v));
    let mut renderer = Renderer::with_system_fonts(Theme::kim_dark(), RenderOptions::default())
        .expect("system fonts");
    renderer.render(&v);
    let m = renderer.metrics();
    assert_eq!(
        renderer.frame().width,
        60 * m.width + 2 * renderer.padding()
    );
    assert!(ink(&renderer, 0, 19) > 0);
    b.send(b"q");
    b.detach();
}
