//! E2E — Step 13: scrollback and terminal replay (docs/SCROLLBACK.md, ADR 0030).
//! `SUBAK-FR-001`, `SUBAK-FR-004`, `SUBAK-FR-012`, `SUBAK-FR-015`, `SUBAK-FR-042`,
//! `SUBAK-SEC-011`, `KIM-SEC-013`.
//!
//! Scenario A (release `subak`): a user prints more lines than the screen holds, detaches, and
//! attaches again from another client: the new attach shows the screen and puts the earlier
//! lines into its own scrollback once (no line twice), then streams live output. `capture`
//! returns the screen, `capture --history` the earlier lines, `history` the counters (never
//! content), and `clear-history` asks for consent (`confirmation_required` without `--yes`),
//! then empties the history while the screen stays. A Terminal created with
//! `--history-limit 20` keeps exactly the newest 20 rows.
//!
//! Scenario B (release `subak`): privacy. A secret printed in a pane is hidden in `capture`
//! (`***`, human and JSON) unless `--no-redact`, and never appears in `info`, `ls`, `status`,
//! `history` or `host status`. A stopped Terminal (`--remain-on-exit`) can still be captured.
//!
//! Windows only. Every scenario uses its own endpoint and stops what it started.
#![cfg(windows)]

use kim_e2e::{TempDir, bin};
use serde_json::Value;
use std::io::{Read, Write};
use std::os::windows::process::CommandExt;
use std::path::Path;
use std::process::{Child, Command, Stdio};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

const CREATE_NO_WINDOW: u32 = 0x0800_0000;
const LONG: Duration = Duration::from_secs(30);

fn endpoint(tag: &str) -> String {
    format!("subak-e2e13-{tag}-{}", std::process::id())
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
    assert!(out.stderr.is_empty(), "{args:?}: --json keeps stderr empty");
    let doc: Value = serde_json::from_slice(&out.stdout).unwrap_or_else(|e| {
        panic!(
            "{args:?}: not JSON ({e}): {}",
            String::from_utf8_lossy(&out.stdout)
        )
    });
    let code = out.status.code().unwrap_or(-1);
    if doc["ok"] == false {
        assert_eq!(Some(i64::from(code)), doc["error"]["exit_code"].as_i64());
    }
    (code, doc)
}

fn lines(doc: &Value) -> Vec<String> {
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

/// `subak attach` with piped stdin and output and no window.
struct Client {
    child: Child,
    out: Arc<Mutex<Vec<u8>>>,
}

impl Client {
    fn attach(exe: &Path, ep: &str, target: &str) -> Self {
        let mut child = Command::new(exe)
            .args(["--endpoint", ep, "attach", target, "--size", "80x24"])
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .creation_flags(CREATE_NO_WINDOW)
            .spawn()
            .expect("attach");
        let out = Arc::new(Mutex::new(Vec::new()));
        for mut s in [
            Box::new(child.stdout.take().unwrap()) as Box<dyn Read + Send>,
            Box::new(child.stderr.take().unwrap()),
        ] {
            let sink = Arc::clone(&out);
            std::thread::spawn(move || {
                let mut buf = [0u8; 8192];
                while let Ok(n) = s.read(&mut buf) {
                    if n == 0 {
                        break;
                    }
                    sink.lock().unwrap().extend_from_slice(&buf[..n]);
                }
            });
        }
        let c = Self { child, out };
        c.expect("Ctrl+B d detaches");
        c
    }

    fn send(&mut self, bytes: &[u8]) {
        let stdin = self.child.stdin.as_mut().unwrap();
        stdin.write_all(bytes).unwrap();
        stdin.flush().unwrap();
    }

    fn text(&self) -> String {
        String::from_utf8_lossy(&self.out.lock().unwrap()).into_owned()
    }

    fn expect(&self, needle: &str) {
        let until = Instant::now() + LONG;
        while !self.text().contains(needle) {
            assert!(
                Instant::now() < until,
                "`{needle}` not seen:\n{}",
                self.text()
            );
            std::thread::sleep(Duration::from_millis(50));
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

impl Drop for Client {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

#[test]
#[ignore = "e2e: run via `cargo xtask e2e` only (AGENTS.md §4)"]
fn reattach_replays_the_screen_and_history_once_and_the_cli_reads_and_clears_it() {
    let subak = bin("subak");
    let sb = TempDir::new("e2e13-replay");
    let ep = endpoint("replay");
    let _cleanup = Cleanup {
        exe: &subak,
        ep: &ep,
    };
    let run = |args: &[&str]| json(&subak, &ep, args, &sb);

    let (code, doc) = run(&[
        "new", "work", "--size", "80x24", "--", "cmd.exe", "/d", "/q",
    ]);
    assert_eq!(code, 0, "{doc}");
    let mut a = Client::attach(&subak, &ep, "work");
    a.send(b"for /L %i in (1,1,60) do @echo e2e13-line-%i\r");
    a.expect("e2e13-line-60");
    a.detach();

    // A new client: the screen and the earlier lines (into its scrollback).
    let mut b = Client::attach(&subak, &ep, "work");
    b.expect("e2e13-line-60");
    let seen = b.text();
    // Lines of the replayed screen are positioned with CUP (`ESC[row;colH`) rather than ended
    // with CR, so count a line followed by anything but another digit.
    let count = |i: u32| {
        let needle = format!("e2e13-line-{i}");
        seen.match_indices(&needle)
            .filter(|(at, _)| {
                !seen[at + needle.len()..]
                    .chars()
                    .next()
                    .is_some_and(|c| c.is_ascii_digit())
            })
            .count()
    };
    // Lines that scrolled into history arrive exactly once: replay never duplicates them.
    for i in [1, 30] {
        assert_eq!(count(i), 1, "history line {i} once:\n{seen}");
    }
    // A line on the screen arrives in the snapshot; ConPTY may repaint the screen after a
    // same-size attach (e2e run 2 saw that; Step 16 removes the no-op resize), which redraws the
    // same cells and is not duplicated content.
    assert!(count(60) >= 1, "screen line 60 shown:\n{seen}");
    b.send(b"echo after-reattach-mark^er\r");
    b.expect("after-reattach-marker");
    b.detach();

    // capture, capture --history, history.
    let (code, screen) = run(&["capture", "work"]);
    assert_eq!(code, 0, "{screen}");
    assert_eq!(lines(&screen).len(), 24);
    assert!(lines(&screen).iter().any(|l| l == "after-reattach-marker"));
    let (_, hist) = run(&["capture", "work", "--history", "--end", "-1"]);
    let h = lines(&hist);
    assert!(h.iter().any(|l| l == "e2e13-line-1"), "{h:?}");
    let (code, info) = run(&["history", "work"]);
    assert_eq!(code, 0, "{info}");
    assert_eq!(
        info["data"]["history"]["lines"].as_u64(),
        Some(h.len() as u64)
    );
    assert_eq!(info["data"]["history"]["persisted"], false);
    assert!(
        !info.to_string().contains("e2e13-line"),
        "history shows no content"
    );

    // clear-history needs consent; the screen stays.
    let (code, doc) = run(&["clear-history", "work"]);
    assert_eq!(
        (code, doc["error"]["code"].as_str()),
        (7, Some("confirmation_required"))
    );
    let (code, doc) = run(&["clear-history", "work", "--yes"]);
    assert_eq!(code, 0, "{doc}");
    assert_eq!(doc["data"]["cleared_lines"].as_u64(), Some(h.len() as u64));
    let (_, empty) = run(&["capture", "work", "--history", "--end", "-1"]);
    assert!(lines(&empty).is_empty());
    let (_, again) = run(&["capture", "work"]);
    assert_eq!(lines(&again), lines(&screen));

    // A small limit keeps exactly the newest rows.
    let (code, doc) = run(&[
        "new",
        "small",
        "--size",
        "80x10",
        "--history-limit",
        "20",
        "--",
        "cmd.exe",
        "/d",
        "/q",
    ]);
    assert_eq!(code, 0, "{doc}");
    let mut s = Client::attach(&subak, &ep, "small");
    s.send(b"for /L %i in (1,1,100) do @echo small-%i\r");
    s.expect("small-100");
    s.detach();
    std::thread::sleep(Duration::from_millis(500));
    let (_, info) = run(&["history", "small"]);
    assert_eq!(info["data"]["history"]["lines"], 20, "{info}");
    assert_eq!(info["data"]["history"]["limit_lines"], 20);
    assert!(info["data"]["history"]["evicted_lines"].as_u64().unwrap() > 0);
}

#[test]
#[ignore = "e2e: run via `cargo xtask e2e` only (AGENTS.md §4)"]
fn captures_hide_secrets_and_stopped_terminals_can_still_be_captured() {
    let subak = bin("subak");
    let sb = TempDir::new("e2e13-privacy");
    let ep = endpoint("privacy");
    let _cleanup = Cleanup {
        exe: &subak,
        ep: &ep,
    };
    let run = |args: &[&str]| json(&subak, &ep, args, &sb);
    let token = format!("ghp_{}", "e2e13".repeat(8));
    let line = format!("cmd.exe /d /c \"echo {token}& echo password=e2e13-hidden-value\"");
    let (code, doc) = run(&[
        "new",
        "sec",
        "--remain-on-exit",
        "--size",
        "200x10",
        "--command-line",
        &line,
    ]);
    assert_eq!(code, 0, "{doc}");
    let until = Instant::now() + LONG;
    while run(&["info", "sec"]).1["data"]["terminal"]["state"] != "stopped" {
        assert!(Instant::now() < until, "the program did not end");
        std::thread::sleep(Duration::from_millis(100));
    }
    // Captured after its program ended, redacted by default.
    let (code, cap) = run(&["capture", "sec"]);
    assert_eq!(code, 0, "{cap}");
    let text = cap.to_string();
    assert!(
        !text.contains(&token) && !text.contains("e2e13-hidden-value"),
        "{text}"
    );
    assert!(text.contains("ghp_***"));
    let human = Command::new(&subak)
        .args(["--endpoint", &ep, "capture", "sec"])
        .stdin(Stdio::null())
        .creation_flags(CREATE_NO_WINDOW)
        .output()
        .unwrap();
    assert!(!String::from_utf8_lossy(&human.stdout).contains(&token));
    let (_, exact) = run(&["capture", "sec", "--no-redact"]);
    assert!(exact.to_string().contains(&token), "shown only on request");
    for args in [
        &["info", "sec"][..],
        &["ls"],
        &["status"],
        &["history", "sec"],
        &["host", "status"],
    ] {
        let (_, d) = run(args);
        assert!(!d.to_string().contains(&token), "{args:?} leaked");
    }
    // Removing the Terminal removes its history.
    let (code, _) = run(&["rm", "sec", "--yes"]);
    assert_eq!(code, 0);
    let (code, doc) = run(&["capture", "sec"]);
    assert_eq!(
        (code, doc["error"]["code"].as_str()),
        (3, Some("target_not_found"))
    );
}
