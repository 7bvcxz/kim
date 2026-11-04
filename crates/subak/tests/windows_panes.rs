//! Step 21 — internal Windows and Panes of a Terminal on real Windows (docs/SESSION_MODEL.md,
//! ADR 0054, docs/contracts/IPC_PROTOCOL.md §4.4). `SUBAK-FR-010`, `SUBAK-FR-012`,
//! `SUBAK-FR-013`, `SUBAK-FR-014`, `SUBAK-FR-015`, `SUBAK-FR-016`, `SUBAK-FR-017`,
//! `SUBAK-FR-004`.
//!
//! Real `subak.exe` Hosts, each on its **own** endpoint (the user's Host is never touched), driven
//! by the `subak` CLI (every `--json` document validated against `subak-cli-v1`, every result
//! against its `ipc-v1` definition) and by raw IPC v1 clients (every control message validated).
//! Each test ends what it started: the [`Session`] guard stops the Host with
//! `--kill-terminals --yes` (or kills it) and checks that every pane process it saw and every
//! pseudo console host are gone. Nothing opens a window; `EVIDENCE` lines record measurements
//! (run with `--nocapture`).
#![cfg(windows)]
#![allow(unsafe_code)]

#[path = "../../../xtask/src/jsonschema.rs"]
#[allow(dead_code)]
mod jsonschema;

mod common;

use common::*;
use serde_json::{Value, json};
use std::collections::BTreeSet;
use std::sync::{Arc, Barrier, Mutex};
use std::time::{Duration, Instant};

/// A Host for one test that remembers every pane process it reported; on drop it stops the Host
/// (ending its Terminals) and checks that nothing it started survives.
struct Session {
    ep: String,
    host: Proc,
    seen: Mutex<Vec<u32>>,
}

impl Session {
    fn start(tag: &str) -> Self {
        let ep = endpoint(tag);
        let (code, doc) = run(&[
            "host",
            "start",
            "--endpoint",
            &ep,
            "--no-shell",
            "--idle-exit",
            "60",
        ]);
        assert_eq!(code, 0, "{doc}");
        Self {
            host: Proc::open(pid_of(&doc["data"]["host"]["pid"])),
            ep,
            seen: Mutex::new(Vec::new()),
        }
    }

    /// `subak --json --endpoint <ours> <args>`, validated against subak-cli-v1.
    fn cli(&self, args: &[&str]) -> (i32, Value) {
        let mut a = vec!["--endpoint", self.ep.as_str()];
        a.extend_from_slice(args);
        let (code, doc) = run(&a);
        validate("subak-cli-v1", &doc);
        (code, doc)
    }

    /// A command that must succeed; returns its `data`, checked against `def` when given.
    fn ok(&self, args: &[&str], def: Option<&str>) -> Value {
        let (code, doc) = self.cli(args);
        assert_eq!(code, 0, "{args:?}: {doc}");
        let mut d = doc["data"].clone();
        if let Some(o) = d.as_object_mut() {
            o.remove("host");
        }
        if let Some(def) = def {
            ipc_def(def, &d);
        }
        self.remember(&d);
        d
    }

    /// A command that must fail with `code`; returns the error.
    fn fails(&self, args: &[&str], code: &str) -> Value {
        let (exit, doc) = self.cli(args);
        assert_ne!(exit, 0, "{args:?} must fail: {doc}");
        assert_eq!(doc["error"]["code"], code, "{args:?}: {doc}");
        doc["error"].clone()
    }

    /// Remembers every pane pid in a result (Terminal or Window objects anywhere in it).
    fn remember(&self, v: &Value) {
        match v {
            Value::Object(m) => {
                if m.contains_key("stream_id")
                    && let Some(pid) = m.get("pid").and_then(Value::as_u64)
                {
                    self.seen.lock().unwrap().push(u32::try_from(pid).unwrap());
                }
                for c in m.values() {
                    self.remember(c);
                }
            }
            Value::Array(a) => a.iter().for_each(|c| self.remember(c)),
            _ => {}
        }
    }

    fn info(&self, target: &str) -> Value {
        self.ok(&["info", target], Some("result_terminal_info"))["terminal"].clone()
    }

    /// Every pane of a Terminal as (window index, pane index, pane object), in order.
    fn panes(&self, target: &str) -> Vec<(u64, u64, Value)> {
        let d = self.ok(&["pane", "ls", target], Some("result_topology"));
        let mut out = Vec::new();
        for w in d["windows"].as_array().unwrap() {
            for p in w["panes"].as_array().unwrap() {
                out.push((
                    w["index"].as_u64().unwrap(),
                    p["index"].as_u64().unwrap(),
                    p.clone(),
                ));
            }
        }
        out
    }

    fn conhosts(&self) -> Vec<u32> {
        processes()
            .into_iter()
            .filter(|(_, pp, n)| *pp == self.host.pid && n == "conhost.exe")
            .map(|(p, _, _)| p)
            .collect()
    }
}

impl Drop for Session {
    fn drop(&mut self) {
        let consoles: Vec<Proc> = if self.host.alive() {
            self.conhosts()
                .into_iter()
                .filter_map(Proc::try_open)
                .collect()
        } else {
            Vec::new()
        };
        if self.host.alive() {
            let _ = run(&[
                "host",
                "stop",
                "--endpoint",
                &self.ep,
                "--kill-terminals",
                "--yes",
            ]);
            if !self.host.wait(Duration::from_secs(20)) {
                self.host.kill();
            }
        }
        let mut survivors = Vec::new();
        for pid in self.seen.lock().unwrap().iter() {
            if let Some(p) = Proc::try_open(*pid)
                && !p.wait(Duration::from_secs(10))
            {
                p.kill();
                survivors.push(*pid);
            }
        }
        for c in &consoles {
            if !c.wait(Duration::from_secs(10)) {
                c.kill();
                survivors.push(c.pid);
            }
        }
        if !std::thread::panicking() {
            assert!(
                survivors.is_empty(),
                "processes outlived the test Host: {survivors:?}"
            );
        }
    }
}

fn alive(pid: u32) -> bool {
    Proc::try_open(pid).is_some_and(|p| p.alive())
}

fn wait_until(what: &str, mut f: impl FnMut() -> bool) {
    let until = Instant::now() + LONG;
    while !f() {
        assert!(Instant::now() < until, "timed out waiting for: {what}");
        std::thread::sleep(Duration::from_millis(50));
    }
}

/// Collects the events a raw connection receives until `until` holds.
fn events_until(c: &Raw, mut until: impl FnMut(&[Value]) -> bool) -> Vec<Value> {
    let deadline = Instant::now() + LONG;
    let mut got = Vec::new();
    while !until(&got) {
        assert!(
            Instant::now() < deadline,
            "events so far: {:#}",
            Value::from(got.clone())
        );
        if let Got::Control(m) = c.next_control(Duration::from_millis(200))
            && m["type"] == "event"
        {
            got.push(m);
        }
    }
    got
}

fn has(events: &[Value], name: &str, f: impl Fn(&Value) -> bool) -> bool {
    events.iter().any(|e| e["event"] == name && f(&e["data"]))
}

fn subscribe(s: &Session) -> Raw {
    let c = Raw::handshake(&s.ep, "cli");
    let r = c.request(
        1,
        "events.subscribe",
        &json!({"topics": ["terminals", "panes"]}),
    );
    assert_eq!(r["ok"], true, "{r}");
    c
}

/// A long-running program that is not a shell: a pane running it is busy.
const PING: &[&str] = &["--", "ping.exe", "-n", "600", "127.0.0.1"];

// ---------------------------------------------------------------------------------------------

/// Acceptance 1 and 3: one Terminal holds several internal windows, each with several panes
/// whose processes run independently in their own pseudo consoles; the targets name exactly
/// one level, and a window or pane of another Terminal can never be hit.
#[test]
fn a_terminal_holds_windows_with_independent_pane_processes() {
    let s = Session::start("w21-model");
    let work = s.ok(&["new", "work"], Some("result_terminal_create"));
    let tid = work["terminal_id"].as_str().unwrap().to_string();
    let events = subscribe(&s);
    // Window 1 `build` (selected), window 2 `srv` (not selected), panes in both.
    let build = s.ok(
        &["window", "new", "work", "--name", "build"],
        Some("result_window_create"),
    );
    assert_eq!(build["window"]["index"], 1);
    assert_eq!(build["window"]["name"], "build");
    assert_eq!(build["selected"], true);
    let srv = s.ok(
        &["window", "new", "work", "--name", "srv", "--no-select"],
        Some("result_window_create"),
    );
    assert_eq!(srv["window"]["index"], 2);
    // A verbatim command line prints a marker: the pane's own screen keeps it.
    let marked = s.ok(
        &[
            "pane",
            "new",
            "work:build",
            "--command-line",
            "cmd.exe /d /k echo PANE-MARK-B1",
        ],
        Some("result_window_create"),
    );
    let pinged = s.ok(
        &[&["pane", "new", "work:srv", "--no-select"][..], PING].concat(),
        Some("result_window_create"),
    );
    // tmux numbering: windows 0, 1, 2; panes 0..n per window.
    let panes = s.panes("work");
    let layout: Vec<(u64, u64)> = panes.iter().map(|(w, p, _)| (*w, *p)).collect();
    assert_eq!(layout, [(0, 0), (1, 0), (1, 1), (2, 0), (2, 1)]);
    // Every pane runs its own process, under its own pseudo console host.
    let pids: BTreeSet<u32> = panes.iter().map(|(_, _, p)| pid_of(&p["pid"])).collect();
    assert_eq!(pids.len(), 5, "five distinct pane processes");
    assert!(pids.iter().all(|p| alive(*p)));
    wait_until("one conhost per pane", || s.conhosts().len() == 5);
    let streams: BTreeSet<u64> = panes
        .iter()
        .map(|(_, _, p)| p["stream_id"].as_u64().unwrap())
        .collect();
    assert_eq!(streams.len(), 5, "one data stream per pane");
    // The selection is the Host's: build's new pane is active, srv's pane was not selected.
    let t = s.info("work");
    assert_eq!(t["active_window"], build["window_id"]);
    assert_eq!(t["windows"][1]["active_pane"], marked["pane_id"]);
    assert_eq!(t["windows"][2]["active_pane"], srv["pane_id"]);
    assert_eq!(t["busy"], true, "the ping pane makes the Terminal busy");
    let summary = s.ok(&["ls"], None)["terminals"][0].clone();
    assert_eq!(
        (summary["windows"].as_u64(), summary["panes"].as_u64()),
        (Some(3), Some(5))
    );
    // Events: created windows and panes, in the order they happened, every one schema-valid.
    let got = events_until(&events, |e| {
        has(e, "pane.created", |d| d["pane"]["id"] == pinged["pane_id"])
    });
    assert!(has(&got, "window.created", |d| d["window"]["name"] == "build"));
    assert!(has(&got, "window.selected", |d| d["window_id"]
        == build["window_id"]));
    assert!(has(&got, "pane.selected", |d| d["pane_id"] == marked["pane_id"]));
    assert!(!has(&got, "window.selected", |d| d["window_id"]
        == srv["window_id"]));
    // Per-pane scrollback: the marker is in its pane's capture and in no other.
    let cap = |target: &str| {
        s.ok(&["capture", target], None)["lines"]
            .as_array()
            .unwrap()
            .iter()
            .map(|l| l.as_str().unwrap().to_string())
            .collect::<Vec<_>>()
            .join("\n")
    };
    wait_until("the marker on its pane", || {
        cap("work:build.1").contains("PANE-MARK-B1")
    });
    for other in ["work:0.0", "work:build.0", "work:2.0"] {
        assert!(!cap(other).contains("PANE-MARK-B1"), "{other}");
    }
    // Rename keeps the id; names are not unique, digits-only names and id shapes are refused.
    let r = s.ok(
        &["window", "rename", "work:srv", "servers"],
        Some("result_window_rename"),
    );
    assert_eq!(
        (r["previous_name"].as_str(), r["changed"].as_bool()),
        (Some("srv"), Some(true))
    );
    s.ok(&["window", "rename", "work:0", "build"], None);
    s.fails(&["window", "select", "work:build"], "ambiguous_target");
    s.fails(&["window", "rename", "work:0", "42"], "invalid_name");
    s.fails(&["window", "rename", "work:0", "a.b"], "invalid_name");
    let w0 = t["windows"][0]["id"].as_str().unwrap().to_string();
    s.ok(&["window", "rename", &w0, "shell"], None);
    assert!(has(
        &events_until(&events, |e| has(e, "window.renamed", |d| d["name"]
            == "shell")),
        "window.renamed",
        |d| d["previous_name"] == "build"
    ));
    // The hierarchy is enforced: each command takes exactly its level.
    for (args, why) in [
        (
            vec!["window", "new", "work:0"],
            "window new takes a Terminal",
        ),
        (vec!["window", "kill", "work"], "never a whole Terminal"),
        (vec!["pane", "kill", "work:0"], "a window is not a pane"),
        (vec!["pane", "kill", "work"], "never a whole Terminal"),
        (
            vec!["window", "select", "work"],
            "a Terminal is not a window",
        ),
        (vec!["pane", "select", "work:1"], "a window is not a pane"),
        (
            vec!["window", "ls", "work:1"],
            "list-windows takes a Terminal",
        ),
        (
            vec!["pane", "ls", "work:1.0"],
            "list-panes takes a Terminal or window",
        ),
    ] {
        let e = s.fails(&args, "usage");
        assert_eq!(e["exit_code"], 2, "{why}: {e}");
    }
    let tid_target = tid.clone();
    s.fails(&["kill", &format!("{tid_target}:1"), "--yes"], "usage");
    // No cross-Terminal mix-up: another Terminal's window or pane id under `work` is not found,
    // and its own targets stay its own.
    let other = s.ok(&["new", "other"], Some("result_terminal_create"));
    let (ow, op) = (
        other["window_id"].as_str().unwrap(),
        other["pane_id"].as_str().unwrap(),
    );
    for t in [
        format!("work:{ow}"),
        format!("work:{ow}.0"),
        "other:1".into(),
    ] {
        s.fails(&["window", "kill", &t, "--yes"], "target_not_found");
        s.fails(&["pane", "select", &format!("{t}.0")], "target_not_found");
    }
    s.fails(
        &["pane", "kill", &format!("work:0.{op}"), "--yes"],
        "target_not_found",
    );
    s.fails(
        &["pane", "select", &format!("work:1.{op}")],
        "target_not_found",
    );
    let e = s.fails(&["pane", "kill", op, "--yes"], "invalid_state");
    assert!(
        e["message"].as_str().unwrap().contains("last running pane"),
        "{e}"
    );
    assert_eq!(s.info("other")["state"], "running");
    // Windows and panes change only in a running Terminal.
    s.ok(&["stop", "other", "--yes"], None);
    s.fails(&["window", "new", "other"], "invalid_state");
    s.fails(&["pane", "new", "other"], "invalid_state");
    println!(
        "EVIDENCE model: Terminal {tid}: 3 windows (0,1,2), 5 panes, 5 distinct pids {pids:?}, 5 conhost.exe, 5 streams; per-pane capture; hierarchy and cross-Terminal targets refused"
    );
}

/// Acceptance 2: switching the selection, detaching, disconnecting or crashing clients never
/// ends a window's or pane's process, and the Host keeps the topology and selection for every
/// client; an intentional kill ends only its target, by id, after the busy guard.
#[test]
fn only_an_intentional_kill_ends_and_only_its_target() {
    let s = Session::start("w21-kill");
    s.ok(&["new", "work"], None);
    s.ok(&["new", "bystander"], None);
    let a = s.ok(&["window", "new", "work", "--name", "a"], None);
    let busy = s.ok(&[&["pane", "new", "work:a"][..], PING].concat(), None);
    s.ok(&["pane", "new", "work:a"], None);
    let b = s.ok(&["window", "new", "work", "--name", "b"], None);
    let all = |s: &Session| -> Vec<(u64, u64, Value)> { s.panes("work") };
    let pid_of_pane = |id: &Value| {
        all(&s)
            .into_iter()
            .find(|(_, _, p)| p["id"] == *id)
            .map(|(_, _, p)| pid_of(&p["pid"]))
            .unwrap()
    };
    let busy_pid = pid_of_pane(&busy["pane_id"]);
    let bystander = pid_of(&s.info("bystander")["windows"][0]["panes"][0]["pid"]);
    let before: BTreeSet<u32> = all(&s).iter().map(|(_, _, p)| pid_of(&p["pid"])).collect();
    assert_eq!(before.len(), 5);
    // Two clients see one selection; selecting, attaching, detaching and disconnecting end
    // nothing.
    let watcher = subscribe(&s);
    let viewer = Raw::handshake(&s.ep, "gui");
    let att = viewer.request(
        1,
        "pane.attach",
        &json!({"target": "work", "interactive": true, "size": {"cols": 80, "rows": 24}}),
    );
    assert_eq!(
        att["result"]["pane_id"], b["pane_id"],
        "a Terminal target means its active pane"
    );
    let sel = s.ok(&["window", "select", "work:a"], Some("result_select"));
    assert_eq!(sel["changed"], true);
    let got = events_until(&watcher, |e| has(e, "window.selected", |_| true));
    assert!(has(&got, "window.selected", |d| d["window_id"]
        == a["window_id"]));
    assert_eq!(s.info("work")["active_window"], a["window_id"]);
    let dry = s.ok(
        &["pane", "select", "work:a.0", "--dry-run"],
        Some("result_select"),
    );
    assert_eq!(dry["would_select"]["changed"], true);
    s.ok(&["pane", "select", "work:a.0"], Some("result_select"));
    s.ok(&["detach", "work"], None);
    drop(viewer);
    let crash = Raw::handshake(&s.ep, "gui");
    crash.request(
        1,
        "pane.attach",
        &json!({"target": "work:a.1", "interactive": true}),
    );
    drop(crash);
    std::thread::sleep(Duration::from_millis(300));
    for p in &before {
        assert!(
            alive(*p),
            "pid {p} must survive selection and client changes"
        );
    }
    // The topology and selection are the Host's: a new client sees exactly what was left.
    let again = Raw::handshake(&s.ep, "cli");
    let topo = again.request(2, "window.list", &json!({"target": "work"}));
    ipc_def("result_topology", &topo["result"]);
    assert_eq!(topo["result"]["active_window"], a["window_id"]);
    assert_eq!(topo["result"]["windows"].as_array().unwrap().len(), 3);
    // The busy guard: the CLI cannot ask here, so it needs --yes; the Host needs `force`.
    let e = s.fails(&["pane", "kill", "work:a.1"], "confirmation_required");
    assert_eq!(e["exit_code"], 7);
    let r = again.request(3, "pane.kill", &json!({"target": busy["pane_id"]}));
    assert_eq!(error_code(&r), "busy");
    ipc_def("close_plan", &r["error"]["details"]);
    let plan = s.ok(
        &["pane", "kill", "work:a.1", "--dry-run"],
        Some("result_close"),
    );
    assert_eq!(plan["would_kill"]["busy"], true);
    assert!(alive(busy_pid), "a dry run ends nothing");
    // The intentional kill ends exactly that pane, by id.
    let done = s.ok(&["pane", "kill", "work:a.1", "--yes"], Some("result_close"));
    assert_eq!(done["affected"].as_array().unwrap().len(), 1);
    assert_eq!(done["affected"][0]["pane_id"], busy["pane_id"]);
    assert_eq!(done["window_removed"], false);
    wait_until("the busy pane's process ended", || !alive(busy_pid));
    for p in before.iter().filter(|p| **p != busy_pid) {
        assert!(alive(*p), "pid {p} kept running");
    }
    assert!(alive(bystander));
    // Its window renumbers its panes (tmux); the ids stay.
    let a_panes: Vec<(u64, u64)> = all(&s)
        .iter()
        .filter(|(w, _, _)| *w == 1)
        .map(|(w, p, _)| (*w, *p))
        .collect();
    assert_eq!(a_panes, [(1, 0), (1, 1)]);
    let got = events_until(&watcher, |e| has(e, "pane.removed", |_| true));
    assert!(has(&got, "pane.exited", |d| d["pane_id"] == busy["pane_id"]));
    assert!(has(&got, "pane.removed", |d| d["reason"] == "killed"));
    // A window kill ends its panes only, and the window goes; then the index is free again.
    let a_pids: Vec<u32> = all(&s)
        .iter()
        .filter(|(w, _, _)| *w == 1)
        .map(|(_, _, p)| pid_of(&p["pid"]))
        .collect();
    let done = s.ok(&["window", "kill", "work:a", "--yes"], Some("result_close"));
    assert_eq!(done["affected"].as_array().unwrap().len(), 2);
    assert_eq!(done["window_removed"], true);
    for p in &a_pids {
        wait_until("window a's panes ended", || !alive(*p));
    }
    let left: Vec<u64> = all(&s).iter().map(|(w, _, _)| *w).collect();
    assert_eq!(left, [0, 2]);
    let got = events_until(&watcher, |e| has(e, "window.removed", |_| true));
    assert!(has(&got, "window.removed", |d| d["window_id"] == a["window_id"]));
    let c = s.ok(&["window", "new", "work", "--no-select"], None);
    assert_eq!(c["window"]["index"], 1, "the lowest free index is reused");
    // The last running window or pane is refused: only `kill` ends a Terminal.
    s.ok(&["window", "kill", "work:1", "--yes"], None);
    s.ok(&["window", "kill", "work:2", "--yes"], None);
    let e = s.fails(&["window", "kill", "work:0", "--yes"], "invalid_state");
    assert!(
        e["message"]
            .as_str()
            .unwrap()
            .contains("last running window"),
        "{e}"
    );
    s.fails(&["pane", "kill", "work:0.0", "--yes"], "invalid_state");
    assert_eq!(s.info("work")["state"], "running");
    assert!(alive(bystander));
    println!(
        "EVIDENCE kill: selection/attach/detach/disconnect kept {} pids; pane kill ended only pid {busy_pid}; window kill ended only {a_pids:?}; bystander {bystander} untouched",
        before.len()
    );
}

/// A pane whose program exits leaves its window while the others run; the exit of the last
/// running pane ends the Terminal as before; stop keeps every window and pane; Host stop and
/// Terminal kill end every pane.
#[test]
fn exits_stop_and_kill_follow_the_hierarchy() {
    let s = Session::start("w21-exit");
    s.ok(&["new", "work", "--remain-on-exit"], None);
    let events = subscribe(&s);
    // A pane whose program ends by itself: removed with reason `exited`, the Terminal runs on.
    let quick = s.ok(
        &[
            "window",
            "new",
            "work",
            "--command-line",
            "cmd.exe /d /c ping -n 2 127.0.0.1 >nul & exit 3",
        ],
        None,
    );
    let got = events_until(&events, |e| has(e, "window.removed", |_| true));
    assert!(has(&got, "pane.exited", |d| d["pane_id"]
        == quick["pane_id"]
        && d["exit_code"] == 3));
    assert!(has(&got, "pane.removed", |d| d["reason"] == "exited"));
    assert!(has(&got, "window.removed", |d| d["window_id"]
        == quick["window_id"]));
    assert!(
        has(&got, "window.selected", |_| true),
        "the active window went: another is selected"
    );
    let t = s.info("work");
    assert_eq!(t["state"], "running");
    assert_eq!(t["windows"].as_array().unwrap().len(), 1);
    // Stop keeps every window and pane (with their screens), ends every process.
    s.ok(&["window", "new", "work"], None);
    s.ok(&["pane", "new", "work:1"], None);
    let pids: Vec<u32> = s
        .panes("work")
        .iter()
        .map(|(_, _, p)| pid_of(&p["pid"]))
        .collect();
    assert_eq!(pids.len(), 3);
    let stopped = s.ok(&["stop", "work", "--yes"], Some("result_terminal_end"));
    assert_eq!(stopped["affected"].as_array().unwrap().len(), 3);
    for p in &pids {
        wait_until("every pane ended on stop", || !alive(*p));
    }
    let t = s.info("work");
    assert_eq!(t["state"], "stopped");
    let kept: usize = t["windows"]
        .as_array()
        .unwrap()
        .iter()
        .map(|w| w["panes"].as_array().unwrap().len())
        .sum();
    assert_eq!(kept, 3, "a stopped Terminal keeps its windows and panes");
    s.ok(&["capture", "work:1.1"], None);
    s.ok(&["rm", "work", "--yes"], None);
    // The last running pane's own exit ends the Terminal (removed without remain-on-exit).
    s.ok(
        &[
            "new",
            "solo",
            "--command-line",
            "cmd.exe /d /c ping -n 3 127.0.0.1 >nul",
        ],
        None,
    );
    let p2 = s.ok(
        &[
            "pane",
            "new",
            "solo",
            "--command-line",
            "cmd.exe /d /c exit 0",
        ],
        None,
    );
    events_until(&events, |e| {
        has(e, "pane.removed", |d| d["pane_id"] == p2["pane_id"])
    });
    events_until(&events, |e| {
        has(e, "terminal.removed", |d| d["name"] == "solo")
    });
    // Kill ends every pane of every window at once.
    s.ok(&["new", "many"], None);
    for _ in 0..2 {
        s.ok(&["window", "new", "many"], None);
        s.ok(&[&["pane", "new", "many"][..], PING].concat(), None);
    }
    let pids: Vec<u32> = s
        .panes("many")
        .iter()
        .map(|(_, _, p)| pid_of(&p["pid"]))
        .collect();
    assert_eq!(pids.len(), 5);
    let killed = s.ok(&["kill", "many", "--yes"], Some("result_terminal_end"));
    assert_eq!(killed["affected"].as_array().unwrap().len(), 5);
    assert_eq!(killed["removed"], true);
    for p in &pids {
        wait_until("every pane ended on kill", || !alive(*p));
    }
    // Host stop ends every pane too (checked by the Session guard).
    s.ok(&["new", "last"], None);
    s.ok(&["window", "new", "last"], None);
    println!(
        "EVIDENCE exits: pane exit removed only it; stop kept 3 panes as stopped; kill ended 5 panes"
    );
}

/// Concurrency and cleanup: concurrent creates give unique indexes and ids; two clients killing
/// the last two panes at once end exactly one (the other is the last and is refused); a
/// create racing a Terminal kill leaves no process; failed starts leave nothing.
#[test]
fn concurrent_operations_keep_one_consistent_topology() {
    let s = Arc::new(Session::start("w21-race"));
    s.ok(&["new", "work"], None);
    let n = 6;
    let gate = Arc::new(Barrier::new(n));
    let made: Vec<Value> = (0..n)
        .map(|_| {
            let (s, gate) = (Arc::clone(&s), Arc::clone(&gate));
            std::thread::spawn(move || {
                gate.wait();
                s.ok(&["window", "new", "work", "--no-select"], None)
            })
        })
        .collect::<Vec<_>>()
        .into_iter()
        .map(|h| h.join().unwrap())
        .collect();
    let idx: BTreeSet<u64> = made
        .iter()
        .map(|m| m["window"]["index"].as_u64().unwrap())
        .collect();
    assert_eq!(
        idx,
        (1..=6).collect::<BTreeSet<u64>>(),
        "unique, gap-free indexes"
    );
    let ids: BTreeSet<&str> = made
        .iter()
        .map(|m| m["window_id"].as_str().unwrap())
        .collect();
    assert_eq!(ids.len(), n);
    // Leave exactly two windows, then kill both at once from two clients: one wins, the other
    // is refused as the last; the Terminal keeps running with one pane.
    for i in 2..=6 {
        s.ok(&["window", "kill", &format!("work:{i}"), "--yes"], None);
    }
    for round in 0..3 {
        if round > 0 {
            s.ok(&["window", "new", "work"], None);
        }
        let targets: Vec<String> = s
            .panes("work")
            .iter()
            .map(|(_, _, p)| p["id"].as_str().unwrap().to_string())
            .collect();
        assert_eq!(targets.len(), 2);
        let gate = Arc::new(Barrier::new(2));
        let codes: Vec<i32> = targets
            .into_iter()
            .map(|t| {
                let (s, gate) = (Arc::clone(&s), Arc::clone(&gate));
                std::thread::spawn(move || {
                    gate.wait();
                    s.cli(&["pane", "kill", &t, "--yes"]).0
                })
            })
            .collect::<Vec<_>>()
            .into_iter()
            .map(|h| h.join().unwrap())
            .collect();
        let mut sorted = codes.clone();
        sorted.sort_unstable();
        assert_eq!(
            sorted,
            [0, 7],
            "round {round}: one kill, one refusal ({codes:?})"
        );
        assert_eq!(s.panes("work").len(), 1);
        assert_eq!(s.info("work")["state"], "running");
    }
    // A create racing the Terminal's kill: either the pane is created and then ended with the
    // Terminal, or it is refused; no process is left either way.
    for _ in 0..3 {
        s.ok(&["new", "racer"], None);
        let s2 = Arc::clone(&s);
        let creator = std::thread::spawn(move || s2.cli(&["window", "new", "racer"]));
        let (kc, _) = s.cli(&["kill", "racer", "--yes"]);
        let (cc, cd) = creator.join().unwrap();
        assert_eq!(kc, 0);
        if cc == 0 {
            s.remember(&cd["data"]);
        }
        wait_until("racer gone", || s.cli(&["info", "racer"]).0 == 3);
    }
    // Failed starts leave no window, pane or conhost.
    let consoles = s.conhosts().len();
    for _ in 0..5 {
        s.fails(
            &["window", "new", "work", "--", "C:\\no\\such\\program.exe"],
            "spawn_failed",
        );
        s.fails(
            &["pane", "new", "work", "--cwd", "C:\\no\\such\\dir"],
            "spawn_failed",
        );
    }
    assert_eq!(s.panes("work").len(), 1);
    assert_eq!(
        s.conhosts().len(),
        consoles,
        "no pseudo console left by a failed start"
    );
    let stats = s.ok(&["host", "status"], None);
    assert_eq!(stats["terminals"], 1);
    println!(
        "EVIDENCE race: 6 concurrent window creates -> indexes 1..6; 3 rounds of racing last-pane kills -> one each; create/kill races and 10 failed starts left nothing"
    );
}

/// Raw IPC: the methods answer schema-valid results, `mcp` clients may list but not change,
/// and `pane.split` stays for Step 22.
#[test]
fn ipc_methods_results_and_authorization() {
    let s = Session::start("w21-ipc");
    s.ok(&["new", "work"], None);
    let c = Raw::handshake(&s.ep, "cli");
    let created = c.request(
        1,
        "window.create",
        &json!({"target": "work", "name": "logs", "select": false, "dry_run": true}),
    );
    ipc_def("result_window_create", &created["result"]);
    assert_eq!(created["result"]["would_create"]["window_index"], 1);
    assert_eq!(s.panes("work").len(), 1, "a dry run creates nothing");
    let created = c.request(
        2,
        "window.create",
        &json!({"target": "work", "name": "logs"}),
    );
    ipc_def("result_window_create", &created["result"]);
    s.remember(&created["result"]);
    let created = c.request(
        3,
        "pane.create",
        &json!({"target": "work:logs", "select": false}),
    );
    ipc_def("result_window_create", &created["result"]);
    s.remember(&created["result"]);
    for (id, method, params, def) in [
        (
            4,
            "window.list",
            json!({"target": "work"}),
            "result_topology",
        ),
        (
            5,
            "pane.list",
            json!({"target": "work:logs"}),
            "result_topology",
        ),
        (
            6,
            "window.select",
            json!({"target": "work:0"}),
            "result_select",
        ),
        (
            7,
            "pane.select",
            json!({"target": "work:1.1"}),
            "result_select",
        ),
        (
            8,
            "window.rename",
            json!({"target": "work:1", "name": "tail", "dry_run": true}),
            "result_window_rename",
        ),
        (
            9,
            "pane.kill",
            json!({"target": "work:1.1", "dry_run": true}),
            "result_close",
        ),
    ] {
        let r = c.request(id, method, &params);
        assert_eq!(r["ok"], true, "{method}: {r}");
        ipc_def(def, &r["result"]);
    }
    let r = c.request(10, "pane.split", &json!({"target": "work"}));
    assert_eq!(error_code(&r), "unknown_request", "splitting is Step 22");
    let mcp = Raw::handshake(&s.ep, "mcp");
    let r = mcp.request(1, "window.list", &json!({"target": "work"}));
    assert_eq!(r["ok"], true, "listing is read-only");
    for m in [
        "window.create",
        "pane.create",
        "window.kill",
        "pane.kill",
        "window.select",
    ] {
        let r = mcp.request(2, m, &json!({"target": "work"}));
        assert_eq!(error_code(&r), "unauthorized", "{m}");
    }
    let hook = Raw::handshake(&s.ep, "hook");
    assert_eq!(
        error_code(&hook.request(1, "pane.list", &json!({"target": "work"}))),
        "unauthorized"
    );
}
