//! Step 10 — the Terminal session lifecycle on real Windows (docs/SESSION_MODEL.md, ADR 0020,
//! docs/contracts/IPC_PROTOCOL.md). `SUBAK-FR-002`, `SUBAK-FR-003`, `SUBAK-FR-004`,
//! `SUBAK-FR-010`…`SUBAK-FR-016`, `SUBAK-SEC-001`.
//!
//! Real `subak.exe` Hosts, each on its **own** endpoint (a user's Host is never touched), driven by
//! the `subak terminal` CLI and by raw IPC v1 clients. Every control message a raw client receives
//! is validated against `docs/contracts/schemas/ipc-v1.schema.json`, and method results against
//! their `result_*` definitions. Every test ends what it started: the [`Session`] guard stops the
//! Host with `--kill-terminals --yes` (or kills it), then checks that the Host, every pane
//! process it saw and every pseudo console host are gone.
//!
//! Headless: nothing here opens a window. Lines starting with `EVIDENCE` record what was measured;
//! run with `--nocapture` to see them.
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

/// A Host for one test, with every pane process it reported; on drop it stops the Host (ending
/// its Terminals) and checks that nothing it started survives.
struct Session {
    ep: String,
    host: Proc,
    seen: Mutex<Vec<u32>>,
}

impl Session {
    /// `host start --no-shell`: a Host that owns no Terminal, as start on demand uses.
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

    /// `subak --endpoint <ours> <args>`: the endpoint comes first, because everything after a
    /// `--` in `args` is the command of a new Terminal (and must never fall back to the user's
    /// default endpoint).
    fn cli(&self, args: &[&str]) -> (i32, Value) {
        let mut a = vec!["--endpoint", self.ep.as_str()];
        a.extend_from_slice(args);
        run(&a)
    }

    /// `terminal new` that must succeed; returns its `data` (the `terminal.create` result).
    fn new_terminal(&self, args: &[&str]) -> Value {
        let mut a = vec!["terminal", "new"];
        a.extend_from_slice(args);
        let (code, doc) = self.cli(&a);
        assert_eq!(code, 0, "{a:?}: {doc}");
        let d = doc["data"].clone();
        let mut r = d.clone();
        if let Some(o) = r.as_object_mut() {
            o.remove("host");
        }
        ipc_def("result_terminal_create", &r);
        self.remember(&d["terminal"]);
        d
    }

    fn remember(&self, terminal: &Value) {
        if let Some(pid) = terminal["windows"][0]["panes"][0]["pid"].as_u64() {
            self.seen.lock().unwrap().push(u32::try_from(pid).unwrap());
        }
    }

    fn info(&self, target: &str) -> Value {
        let (code, doc) = self.cli(&["terminal", "info", target]);
        assert_eq!(code, 0, "{doc}");
        ipc_def("result_terminal_info", &doc["data"]);
        doc["data"]["terminal"].clone()
    }

    fn list(&self) -> Vec<Value> {
        let (code, doc) = self.cli(&["terminal", "list"]);
        assert_eq!(code, 0, "{doc}");
        let mut r = doc["data"].clone();
        if let Some(o) = r.as_object_mut() {
            o.remove("host");
        }
        ipc_def("result_terminal_list", &r);
        doc["data"]["terminals"].as_array().unwrap().clone()
    }

    fn names(&self) -> BTreeSet<String> {
        self.list()
            .iter()
            .map(|t| t["name"].as_str().unwrap().to_string())
            .collect()
    }

    fn raw(&self, kind: &str) -> Raw {
        Raw::handshake(&self.ep, kind)
    }

    /// Pseudo console hosts (conhost.exe) of this Host: one per running pane.
    fn conhosts(&self) -> Vec<u32> {
        children(self.host.pid, "conhost.exe")
    }
}

impl Drop for Session {
    fn drop(&mut self) {
        let consoles: Vec<Proc> = if self.host.alive() {
            self.conhosts().into_iter().filter_map(try_open).collect()
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
            if let Some(p) = try_open(*pid)
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

fn try_open(pid: u32) -> Option<Proc> {
    Proc::try_open(pid)
}

fn children(parent: u32, name: &str) -> Vec<u32> {
    processes()
        .into_iter()
        .filter(|(_, pp, n)| *pp == parent && n == name)
        .map(|(p, _, _)| p)
        .collect()
}

fn alive(pid: u32) -> bool {
    try_open(pid).is_some_and(|p| p.alive())
}

fn wait_until(what: &str, mut f: impl FnMut() -> bool) {
    let until = Instant::now() + LONG;
    while !f() {
        assert!(Instant::now() < until, "timed out waiting for: {what}");
        std::thread::sleep(Duration::from_millis(50));
    }
}

fn pane_pid(t: &Value) -> u32 {
    pid_of(&t["windows"][0]["panes"][0]["pid"])
}

fn error(doc: &Value) -> (i64, &str) {
    assert_eq!(doc["ok"], false, "{doc}");
    (
        doc["error"]["exit_code"].as_i64().unwrap(),
        doc["error"]["code"].as_str().unwrap(),
    )
}

/// Collects the events a raw connection receives until `until` holds or the time runs out.
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

fn has_event(events: &[Value], name: &str, f: impl Fn(&Value) -> bool) -> bool {
    events.iter().any(|e| e["event"] == name && f(&e["data"]))
}

// ---------------------------------------------------------------------------------------------

#[test]
fn zero_one_and_many_terminals_have_isolated_process_trees() {
    let s = Session::start("many");
    // Zero: the Host owns nothing, over the CLI and over IPC (also for an mcp client).
    assert!(s.list().is_empty());
    let mcp = s.raw("mcp");
    let r = mcp.request(1, "terminal.list", &json!({}));
    assert_eq!(r["result"]["terminals"], json!([]), "{r}");
    assert!(
        s.conhosts().is_empty(),
        "no pseudo console without a Terminal"
    );
    let watcher = s.raw("gui");
    assert_eq!(
        watcher.request(
            1,
            "events.subscribe",
            &json!({"topics": ["terminals", "panes"]})
        )["ok"],
        true
    );

    // One.
    let one = s.new_terminal(&["one"]);
    let one_id = one["terminal_id"].as_str().unwrap().to_string();
    assert_eq!(one["terminal"]["state"], "running");
    assert_eq!(one["terminal"]["windows"].as_array().unwrap().len(), 1);
    assert_eq!(
        one["terminal"]["windows"][0]["panes"]
            .as_array()
            .unwrap()
            .len(),
        1,
        "Terminal -> one Window -> one Pane"
    );
    let evs = events_until(&watcher, |e| {
        has_event(e, "terminal.state_changed", |d| {
            d["terminal_id"] == one_id.as_str() && d["state"] == "running"
        })
    });
    assert!(
        has_event(&evs, "terminal.created", |d| d["terminal"]["id"]
            == one_id.as_str()
            && d["terminal"]["state"] == "starting"),
        "{evs:?}"
    );

    // Many: default names are the lowest free numbers.
    let mut ids = vec![one_id.clone()];
    for want in ["1", "2", "3", "4"] {
        let t = s.new_terminal(&[]);
        assert_eq!(t["name"], want);
        ids.push(t["terminal_id"].as_str().unwrap().to_string());
    }
    let listed = s.list();
    assert_eq!(listed.len(), 5);
    assert_eq!(
        listed.iter().map(|t| t["id"].clone()).collect::<Vec<_>>(),
        ids.iter().map(|i| json!(i)).collect::<Vec<_>>(),
        "creation order"
    );
    let pids: BTreeSet<u32> = listed.iter().map(|t| pid_of(&t["pid"])).collect();
    assert_eq!(pids.len(), 5, "five distinct root processes");
    assert_eq!(s.conhosts().len(), 5, "one pseudo console per pane");
    // With several Terminals, an attach without a target is ambiguous and names the candidates.
    let r = s.raw("cli").request(1, "pane.attach", &json!({}));
    assert_eq!(error_code(&r), "ambiguous_target", "{r}");
    assert_eq!(r["error"]["details"]["candidates"].as_array().unwrap().len(), 5);
    let (_, doc) = s.cli(&["host", "attach", "--dry-run"]);
    assert_eq!(doc["ok"], true, "a dry run only describes");
    // Each Terminal is reported in full by id and by name (names ignore case).
    let info = s.info("ONE");
    assert_eq!(info["id"], one_id.as_str());
    assert_eq!(s.info(&ids[2])["name"], "2");
    let st = s.cli(&["host", "status"]).1;
    assert_eq!(
        (
            st["data"]["terminals"].as_u64(),
            st["data"]["terminals_total"].as_u64()
        ),
        (Some(5), Some(5))
    );

    // Isolation: a grandchild in "one" and one in "2" each live in their own Terminal's job.
    let mut a = Attach::terminal(&s.ep, "one");
    a.send(b"start /b ping -n 600 127.0.0.1 >nul & echo started-a-mark^er\r");
    a.expect("started-a-marker");
    let mut b = Attach::terminal(&s.ep, "2");
    b.send(b"start /b ping -n 600 127.0.0.1 >nul & echo started-b-mark^er\r");
    b.expect("started-b-marker");
    let ping_of = |shell: u32| {
        let mut p = Vec::new();
        wait_until("ping started", || {
            p = children(shell, "ping.exe");
            !p.is_empty()
        });
        p[0]
    };
    let (shell_a, shell_b) = (pane_pid(&info), pane_pid(&s.info("2")));
    let (ping_a, ping_b) = (ping_of(shell_a), ping_of(shell_b));
    assert!(s.info("one")["busy"] == true && s.info("2")["busy"] == true);
    let (code, doc) = s.cli(&["terminal", "kill", "one", "--yes"]);
    assert_eq!(code, 0, "{doc}");
    ipc_def("result_terminal_end", &doc["data"]);
    assert_eq!(doc["data"]["removed"], true);
    assert_eq!(doc["data"]["affected"][0]["forced"], true);
    wait_until("one's tree ended", || !alive(shell_a) && !alive(ping_a));
    assert!(
        alive(shell_b) && alive(ping_b),
        "killing one Terminal leaves the others' process trees alone"
    );
    assert_eq!(s.conhosts().len(), 4);
    assert_eq!(s.list().len(), 4);
    let evs = events_until(&watcher, |e| {
        has_event(e, "terminal.removed", |d| {
            d["terminal_id"] == one_id.as_str()
        })
    });
    assert!(has_event(&evs, "pane.exited", |d| d["terminal_id"] == one_id.as_str()));
    assert!(has_event(&evs, "terminal.removed", |d| d["reason"] == "killed"));
    drop((a, b));
    eprintln!(
        "EVIDENCE many: host={} 5 Terminals, 5 conhosts, root pids {pids:?}; kill `one` ended shell {shell_a} and ping {ping_a}; `2` kept shell {shell_b} and ping {ping_b}",
        s.host.pid
    );
}

#[test]
fn names_are_unique_ignoring_case_and_composition_and_renames_keep_ids() {
    let s = Session::start("names");
    let work = s.new_terminal(&["Work"]);
    let id = work["terminal_id"].as_str().unwrap().to_string();
    for clash in ["work", "WORK", " Work "] {
        let (code, doc) = s.cli(&["terminal", "new", clash]);
        assert_eq!(error(&doc), (7, "name_conflict"), "{clash}: {doc}");
        assert_eq!(code, 7);
        assert!(doc["error"]["details"].to_string().contains(&id), "{doc}");
    }
    assert_eq!(s.list().len(), 1, "a refused create leaves nothing");
    assert_eq!(s.conhosts().len(), 1, "and starts no pseudo console");
    for bad in [
        "a:b",
        "a.b",
        "t-0123456789ab",
        "P-ABCDEF012345",
        "   ",
        "tab\there",
    ] {
        let (_, doc) = s.cli(&["terminal", "new", bad]);
        assert_eq!(error(&doc), (2, "invalid_name"), "{bad:?}: {doc}");
    }
    // (`-x` on the command line is an unknown option; the Host refuses it as a name too.)
    let r = s
        .raw("cli")
        .request(1, "terminal.create", &json!({"name": "-x"}));
    assert_eq!(error_code(&r), "invalid_name", "{r}");
    // Korean, composed and decomposed: one name. (Its pane gets the size it asked for and the
    // working directory it named: tmux `new-session -x -y -c`.)
    let windir = std::env::var("SystemRoot").unwrap();
    let seoul = s.new_terminal(&["서버", "--size", "100x40", "--cwd", &windir]);
    let pane = &seoul["terminal"]["windows"][0]["panes"][0];
    assert_eq!(pane["size"], json!({"cols": 100, "rows": 40}));
    assert_eq!(pane["cwd"], windir.as_str());
    let mut a = Attach::terminal(&s.ep, "서버");
    a.send(b"cd & echo cwd-mark^er\r");
    a.expect("cwd-marker");
    let text = a.text().to_lowercase();
    assert!(text.contains(&windir.to_lowercase()), "{text}");
    drop(a);
    let (_, doc) = s.cli(&["terminal", "new", "\u{1109}\u{1165}\u{1107}\u{1165}"]);
    assert_eq!(error(&doc), (7, "name_conflict"), "NFC: {doc}");
    assert_eq!(s.info("서버")["id"], seoul["terminal_id"]);

    // Renames: case changes of one's own name are fine; another Terminal's name is not.
    let (code, doc) = s.cli(&["terminal", "rename", "work", "work"]);
    assert_eq!(code, 0, "{doc}");
    ipc_def("result_terminal_rename", &doc["data"]);
    assert_eq!(
        (
            doc["data"]["previous_name"].as_str(),
            doc["data"]["name"].as_str()
        ),
        (Some("Work"), Some("work"))
    );
    let (_, doc) = s.cli(&["terminal", "rename", "work", "서버"]);
    assert_eq!(error(&doc), (7, "name_conflict"));
    let (code, doc) = s.cli(&["terminal", "rename", &id, "build", "--dry-run"]);
    assert_eq!(code, 0);
    ipc_def("result_terminal_rename", &doc["data"]);
    assert_eq!(s.info(&id)["name"], "work", "a dry run renames nothing");
    let watcher = s.raw("gui");
    watcher.request(1, "events.subscribe", &json!({"topics": ["terminals"]}));
    let (code, _) = s.cli(&["terminal", "rename", "WORK", "build"]);
    assert_eq!(code, 0);
    let t = s.info("build");
    assert_eq!(t["id"], id.as_str(), "the id survives a rename");
    assert_eq!(
        pane_pid(&t),
        pane_pid(&work["terminal"]),
        "and so do the processes"
    );
    let evs = events_until(&watcher, |e| !e.is_empty());
    assert_eq!(evs[0]["event"], "terminal.renamed");
    assert_eq!(evs[0]["data"]["previous_name"], "work");
    let (_, doc) = s.cli(&["terminal", "info", "work"]);
    assert_eq!(error(&doc), (3, "target_not_found"), "the old name is free");
    let (_, doc) = s.cli(&["terminal", "rename", "nope", "x"]);
    assert_eq!(error(&doc), (3, "target_not_found"));
    // A freed name can be taken; nothing is ever suffixed.
    let again = s.new_terminal(&["work"]);
    assert_ne!(again["terminal_id"], id.as_str());
    assert_eq!(
        s.names(),
        BTreeSet::from(["build", "work", "서버"].map(String::from))
    );
    // Targets with window and pane parts reach the pane, but Terminal operations want a Terminal.
    let c = s.raw("cli");
    let pane = again["pane_id"].as_str().unwrap();
    let r = c.request(1, "terminal.stop", &json!({"target": pane, "force": true}));
    assert_eq!(error_code(&r), "invalid_params", "{r}");
    let r = c.request(
        2,
        "pane.attach",
        &json!({"target": "work:0.0", "interactive": false}),
    );
    assert_eq!(r["ok"], true, "{r}");
    ipc_def("result_pane_attach", &r["result"]);
    assert_eq!(r["result"]["pane_id"], pane);
    eprintln!(
        "EVIDENCE names: Work/work/WORK/' Work ' -> one Terminal {id}; 서버 composed = decomposed; rename kept id and pid {}",
        pane_pid(&t)
    );
}

#[test]
fn concurrent_clients_create_and_rename_without_duplicates_or_orphans() {
    let s = Session::start("race");
    let barrier = Arc::new(Barrier::new(8));
    let results: Vec<Value> = std::thread::scope(|scope| {
        let hs: Vec<_> = (0..8)
            .map(|i| {
                let barrier = Arc::clone(&barrier);
                let ep = s.ep.clone();
                scope.spawn(move || {
                    let c = Raw::handshake(&ep, if i % 2 == 0 { "cli" } else { "gui" });
                    barrier.wait();
                    c.request(1, "terminal.create", &json!({"name": "race"}))
                })
            })
            .collect();
        hs.into_iter().map(|h| h.join().unwrap()).collect()
    });
    let ok: Vec<&Value> = results.iter().filter(|r| r["ok"] == true).collect();
    assert_eq!(ok.len(), 1, "exactly one create wins: {results:?}");
    ipc_def("result_terminal_create", &ok[0]["result"]);
    s.remember(&ok[0]["result"]["terminal"]);
    for r in results.iter().filter(|r| r["ok"] == false) {
        assert_eq!(error_code(r), "name_conflict", "{r}");
    }
    assert_eq!(s.names(), BTreeSet::from(["race".to_string()]));
    assert_eq!(
        s.conhosts().len(),
        1,
        "losers started nothing (no orphan PTY)"
    );

    // Concurrent creates without names get distinct default names.
    let barrier = Arc::new(Barrier::new(6));
    let created: Vec<Value> = std::thread::scope(|scope| {
        let hs: Vec<_> = (0..6)
            .map(|_| {
                let barrier = Arc::clone(&barrier);
                let ep = s.ep.clone();
                scope.spawn(move || {
                    let c = Raw::handshake(&ep, "cli");
                    barrier.wait();
                    c.request(1, "terminal.create", &json!({}))
                })
            })
            .collect();
        hs.into_iter().map(|h| h.join().unwrap()).collect()
    });
    let mut defaults = BTreeSet::new();
    for r in &created {
        assert_eq!(r["ok"], true, "{r}");
        s.remember(&r["result"]["terminal"]);
        defaults.insert(r["result"]["name"].as_str().unwrap().to_string());
    }
    assert_eq!(
        defaults,
        (1..=6).map(|n| n.to_string()).collect::<BTreeSet<_>>()
    );

    // Concurrent renames of two Terminals to one name, and a create of that name: one wins.
    // (Renames target ids: a Terminal renamed by the winner no longer answers to its old name.)
    let one = s.info("1")["id"].as_str().unwrap().to_string();
    let two = s.info("2")["id"].as_str().unwrap().to_string();
    let barrier = Arc::new(Barrier::new(7));
    let outcomes: Vec<(String, Value)> = std::thread::scope(|scope| {
        let hs: Vec<_> = (0..7)
            .map(|i| {
                let barrier = Arc::clone(&barrier);
                let ep = s.ep.clone();
                let (one, two) = (one.clone(), two.clone());
                scope.spawn(move || {
                    let c = Raw::handshake(&ep, "cli");
                    barrier.wait();
                    match i % 3 {
                        0 => (
                            "rename 1".to_string(),
                            c.request(
                                1,
                                "terminal.rename",
                                &json!({"target": one, "name": "Same"}),
                            ),
                        ),
                        1 => (
                            "rename 2".to_string(),
                            c.request(
                                1,
                                "terminal.rename",
                                &json!({"target": two, "name": "same"}),
                            ),
                        ),
                        _ => (
                            "create".to_string(),
                            c.request(1, "terminal.create", &json!({"name": "SAME"})),
                        ),
                    }
                })
            })
            .collect();
        hs.into_iter().map(|h| h.join().unwrap()).collect()
    });
    let winners: BTreeSet<&str> = outcomes
        .iter()
        .filter(|(_, r)| r["ok"] == true)
        .map(|(who, _)| who.as_str())
        .collect();
    assert_eq!(winners.len(), 1, "one kind of request won: {outcomes:?}");
    for (who, r) in &outcomes {
        if r["ok"] == false {
            assert_eq!(error_code(r), "name_conflict", "{who}: {r}");
        } else if who == "create" {
            s.remember(&r["result"]["terminal"]);
        }
    }
    let holders: Vec<Value> = s
        .list()
        .into_iter()
        .filter(|t| t["name"].as_str().unwrap().eq_ignore_ascii_case("same"))
        .collect();
    assert_eq!(
        holders.len(),
        1,
        "exactly one Terminal is named same: {holders:?}"
    );
    let total = s.list().len();
    assert_eq!(
        s.conhosts().len(),
        total,
        "one pseudo console per Terminal, no orphans"
    );

    // A client killed in the middle of a create leaves a consistent Terminal, never half of one.
    let c = s.raw("cli");
    c.send(&json!({"type": "request", "id": 9, "method": "terminal.create", "params": {"name": "orphan-check"}}));
    wait_until("the Host took the create", || {
        s.list().iter().any(|t| t["name"] == "orphan-check")
    });
    drop(c); // gone before it read its answer
    wait_until("the create completed without its client", || {
        s.list()
            .iter()
            .any(|t| t["name"] == "orphan-check" && t["state"] == "running")
    });
    s.remember(&s.info("orphan-check"));
    assert_eq!(s.conhosts().len(), total + 1);
    wait_until("the crashed client is forgotten", || {
        s.cli(&["host", "status"]).1["data"]["clients"]["connected"] == 1
    });
    eprintln!(
        "EVIDENCE race: 8 concurrent creates of `race` -> 1 Terminal, 7 name_conflict; 6 unnamed -> {defaults:?}; rename/rename/create of `same` -> winner {winners:?}; {} Terminals = {} conhosts",
        total + 1,
        s.conhosts().len()
    );
}

#[test]
fn failed_creations_leave_no_pty_job_process_or_record() {
    let s = Session::start("fail");
    let before = s.raw("cli").request(1, "host.stats", &json!({}))["result"].clone();
    let missing_dir = format!(r"C:\subak-missing-{}", std::process::id());
    let cases: Vec<(Vec<&str>, &str)> = vec![
        (vec!["--", r"C:\subak-nope\missing.exe"], "not_found"),
        (vec!["--", "definitely-not-a-program-subak"], "not_found"),
        (vec!["--cwd", &missing_dir], "bad_cwd"),
        (
            vec!["--cwd", r"C:\Windows\win.ini", "--", "cmd.exe"],
            "bad_cwd",
        ),
    ];
    let mut failures = 0;
    for round in 0..5 {
        for (args, reason) in &cases {
            let mut a = vec!["terminal", "new"];
            a.extend_from_slice(args);
            let (code, doc) = s.cli(&a);
            assert_eq!(code, 1, "{a:?}: {doc}");
            assert_eq!(doc["error"]["code"], "spawn_failed");
            assert!(
                doc["error"]["details"]
                    .to_string()
                    .contains(&format!("reason: {reason}")),
                "round {round} {a:?}: {doc}"
            );
            failures += 1;
        }
    }
    let (_, doc) = s.cli(&["terminal", "new", "--cwd", "relative\\dir"]);
    assert_eq!(error(&doc), (2, "usage"), "a relative cwd is a usage error");
    assert!(s.list().is_empty(), "no half-created Terminal remains");
    assert!(s.conhosts().is_empty(), "no pseudo console remains");
    let kids: Vec<_> = processes()
        .into_iter()
        .filter(|(_, parent, _)| *parent == s.host.pid)
        .collect();
    assert!(kids.is_empty(), "the Host has no child process: {kids:?}");
    let after = s.raw("cli").request(1, "host.stats", &json!({}))["result"].clone();
    assert_eq!(
        after["sessions"]["spawn_failures_total"].as_u64(),
        Some(failures)
    );
    let handles = |v: &Value| v["host"]["handle_count"].as_u64().unwrap();
    assert!(
        handles(&after) <= handles(&before) + 24,
        "handles {} -> {} after {failures} failed starts",
        handles(&before),
        handles(&after)
    );
    // A dry run hits the same error without creating anything.
    let (_, doc) = s.cli(&["terminal", "new", "--dry-run", "--", r"C:\subak-nope\x.exe"]);
    assert_eq!(error(&doc), (1, "spawn_failed"));

    // keep_on_failure keeps a `failed` record, which rm removes without --force.
    let (code, doc) = s.cli(&[
        "terminal",
        "new",
        "kept",
        "--keep-on-failure",
        "--",
        r"C:\subak-nope\missing.exe",
    ]);
    assert_eq!(code, 1, "{doc}");
    let t = s.info("kept");
    assert_eq!(t["state"], "failed");
    assert_eq!(t["failure"]["reason"], "not_found");
    let (_, doc) = s.cli(&["terminal", "rm", "kept", "--no-input"]);
    assert_eq!(
        error(&doc),
        (7, "confirmation_required"),
        "rm deletes data: consent"
    );
    let (code, doc) = s.cli(&["terminal", "rm", "kept", "--yes"]);
    assert_eq!(code, 0, "{doc}");
    assert_eq!(doc["data"]["removed"], true);
    assert!(s.list().is_empty());
    eprintln!(
        "EVIDENCE failed-create: {failures} failed starts (missing program, unknown program, missing and non-directory cwd): 0 Terminals, 0 conhosts, 0 children of host {}; handles {} -> {}",
        s.host.pid,
        handles(&before),
        handles(&after)
    );
}

#[test]
fn stop_kill_and_rm_have_distinct_explicit_guarded_meanings() {
    let s = Session::start("verbs");
    // An idle shell: the Host stops it without `force` (nothing but the shell runs).
    let idle = s.new_terminal(&["idle"]);
    let c = s.raw("cli");
    let r = c.request(1, "terminal.stop", &json!({"target": "idle"}));
    assert_eq!(r["ok"], true, "{r}");
    ipc_def("result_terminal_end", &r["result"]);
    assert_eq!(r["result"]["terminal"]["state"], "stopped");
    assert_eq!(r["result"]["terminal"]["last_exit"]["cause"], "stopped");
    assert!(!alive(pane_pid(&idle["terminal"])));
    let r = c.request(2, "terminal.stop", &json!({"target": "idle"}));
    assert_eq!(
        r["result"]["affected"],
        json!([]),
        "stopping a stopped Terminal is a no-op"
    );

    // A busy one: the Host refuses without force; the CLI asks or needs --yes.
    let busy = s.new_terminal(&["busy"]);
    let shell = pane_pid(&busy["terminal"]);
    let mut a = Attach::terminal(&s.ep, "busy");
    a.send(b"start /b ping -n 600 127.0.0.1 >nul & echo busy-mark^er\r");
    a.expect("busy-marker");
    let mut ping = Vec::new();
    wait_until("ping started", || {
        ping = children(shell, "ping.exe");
        !ping.is_empty()
    });
    let r = c.request(3, "terminal.stop", &json!({"target": "busy"}));
    assert_eq!(error_code(&r), "busy", "{r}");
    ipc_def("end_plan", &r["error"]["details"]);
    assert!(r["error"]["details"]["processes"].as_u64().unwrap() >= 2);
    for args in [
        &["terminal", "stop", "busy", "--no-input"][..],
        &["terminal", "kill", "busy", "--no-input"],
        &["terminal", "rm", "busy", "--force", "--no-input"],
    ] {
        let (_, doc) = s.cli(args);
        assert_eq!(error(&doc), (7, "confirmation_required"), "{args:?}");
    }
    let (_, doc) = s.cli(&["terminal", "rm", "busy", "--yes"]);
    assert_eq!(
        error(&doc),
        (7, "invalid_state"),
        "rm never ends a running Terminal"
    );
    let r = c.request(4, "terminal.remove", &json!({"target": "busy"}));
    assert_eq!(error_code(&r), "invalid_state");
    let (code, doc) = s.cli(&["terminal", "stop", "busy", "--dry-run"]);
    assert_eq!(code, 0);
    ipc_def("result_terminal_end", &doc["data"]);
    assert_eq!(doc["data"]["would_stop"]["busy"], true);
    assert!(
        alive(shell) && alive(ping[0]),
        "refusals and dry runs end nothing"
    );
    let (code, doc) = s.cli(&["terminal", "stop", "busy", "--yes"]);
    assert_eq!(code, 0, "{doc}");
    assert_eq!(doc["data"]["removed"], false, "stop keeps the Terminal");
    assert_eq!(doc["data"]["terminal"]["state"], "stopped");
    wait_until("busy's tree ended", || !alive(shell) && !alive(ping[0]));
    drop(a);
    assert!(s.names().contains("busy"), "listed as stopped");
    let (_, doc) = s.cli(&["terminal", "attach", "busy", "--dry-run"]);
    assert_eq!(doc["ok"], true, "a dry run only describes");
    let r = c.request(5, "pane.attach", &json!({"target": "busy"}));
    assert_eq!(
        error_code(&r),
        "invalid_state",
        "a stopped Terminal cannot be attached"
    );
    let (code, doc) = s.cli(&["terminal", "rm", "busy", "--yes"]);
    assert_eq!(code, 0, "{doc}");
    assert!(!s.names().contains("busy"));

    // kill: ends at once and removes; rm --force on a running Terminal does the same.
    let k = s.new_terminal(&["k"]);
    let (code, doc) = s.cli(&["terminal", "kill", "k", "--dry-run"]);
    assert_eq!(code, 0);
    assert_eq!(doc["data"]["would_kill"]["removes_terminal"], true);
    let t = Instant::now();
    let (code, doc) = s.cli(&["terminal", "kill", "k", "--yes"]);
    assert_eq!(code, 0, "{doc}");
    let kill_took = t.elapsed();
    assert!(!alive(pane_pid(&k["terminal"])));
    let f = s.new_terminal(&["f"]);
    let (code, doc) = s.cli(&["terminal", "rm", "f", "--force", "--yes"]);
    assert_eq!(code, 0, "{doc}");
    assert_eq!(doc["data"]["removed"], true);
    assert!(!alive(pane_pid(&f["terminal"])));

    // Two clients at once: a stop and a kill of one Terminal. Whatever the order, the kill
    // decides (removed), the stop either shares the teardown or finds nothing left, and
    // nothing is left running.
    let e = s.new_terminal(&["esc", "--", "ping", "-n", "600", "127.0.0.1"]);
    let race = |a: (&'static str, &'static str), b: (&'static str, &'static str), target: &str| {
        let barrier = Barrier::new(2);
        let ep = s.ep.clone();
        let go = |(kind, method): (&'static str, &'static str)| {
            let c = Raw::handshake(&ep, kind);
            barrier.wait();
            c.request(1, method, &json!({"target": target, "force": true}))
        };
        std::thread::scope(|scope| {
            let x = scope.spawn(|| go(a));
            let y = scope.spawn(|| go(b));
            (x.join().unwrap(), y.join().unwrap())
        })
    };
    let (stop, kill) = race(("cli", "terminal.stop"), ("gui", "terminal.kill"), "esc");
    assert_eq!(kill["ok"], true, "{kill}");
    assert!(
        stop["ok"] == true || error_code(&stop) == "target_not_found",
        "{stop}"
    );
    for r in [&stop, &kill] {
        if r["ok"] == true {
            ipc_def("result_terminal_end", &r["result"]);
        }
    }
    assert!(!s.names().contains("esc"), "the kill decided: removed");
    assert!(!alive(pane_pid(&e["terminal"])));
    // Two stops at once: one teardown, both answered, the Terminal kept as stopped.
    let twice = s.new_terminal(&["twice", "--", "ping", "-n", "600", "127.0.0.1"]);
    let (x, y) = race(("cli", "terminal.stop"), ("gui", "terminal.stop"), "twice");
    assert_eq!(
        (x["ok"].clone(), y["ok"].clone()),
        (json!(true), json!(true)),
        "{x}\n{y}"
    );
    assert_eq!(s.info("twice")["state"], "stopped");
    assert!(!alive(pane_pid(&twice["terminal"])));
    eprintln!(
        "EVIDENCE verbs: idle stop without force ok; busy refused (busy), CLI confirmation_required x3, rm invalid_state; stop --yes ended shell {shell} and ping {}, kept `busy` as stopped; kill took {kill_took:?}; stop+kill race -> removed",
        ping[0]
    );
}

#[test]
fn detach_disconnect_and_client_crash_never_stop_a_terminal() {
    let s = Session::start("detach");
    let t = s.new_terminal(&["keep"]);
    let tid = t["terminal_id"].as_str().unwrap().to_string();
    let shell = pane_pid(&t["terminal"]);
    let watcher = s.raw("cli");
    watcher.request(
        1,
        "events.subscribe",
        &json!({"topics": ["terminals", "panes"]}),
    );

    // A GUI-style client: attaches, types, and disappears without a word (a closed window or a
    // crash look the same to the Host).
    let gui = s.raw("gui");
    let r = gui.request(
        1,
        "pane.attach",
        &json!({"target": "keep", "size": {"cols": 90, "rows": 25}}),
    );
    ipc_def("result_pane_attach", &r["result"]);
    let stream = u32::try_from(r["result"]["stream_id"].as_u64().unwrap()).unwrap();
    gui.write(&data(stream, 0, 2, b"echo gui-mark^er-1\r"))
        .unwrap();
    let until = Instant::now() + LONG;
    let mut seen = Vec::new();
    while !String::from_utf8_lossy(&seen).contains("gui-marker-1") {
        assert!(Instant::now() < until, "the GUI client saw no output");
        if let Got::Data(b) = gui.next(Duration::from_millis(200)) {
            seen.extend(b);
        }
    }
    drop(gui);
    // An explicit detach (tab hidden, window closed politely).
    let gui2 = s.raw("gui");
    let r = gui2.request(
        1,
        "pane.attach",
        &json!({"target": &tid, "interactive": false}),
    );
    assert_eq!(r["ok"], true);
    let r = gui2.request(2, "pane.detach", &json!({"stream_id": stream}));
    assert_eq!(r["ok"], true, "{r}");
    drop(gui2);
    // A CLI attach that detaches with Ctrl+B d, and one that is killed.
    let mut a = Attach::terminal(&s.ep, "keep");
    a.send(b"echo cli-mark^er-2\r");
    a.expect("cli-marker-2");
    a.send(&[0x02, b'd']);
    assert!(a.child.wait().unwrap().success(), "Ctrl+B d detaches");
    let b = Attach::terminal(&s.ep, "keep");
    drop(b); // TerminateProcess
    wait_until("clients gone", || {
        s.cli(&["host", "status"]).1["data"]["clients"]["connected"] == 2 // watcher + status
    });
    let now = s.info("keep");
    assert_eq!(now["state"], "running");
    assert_eq!(pane_pid(&now), shell, "the same shell");
    assert!(alive(shell));
    // Reconnect: a new client sees what happened before it (replay) in the same Terminal.
    let mut c = Attach::terminal(&s.ep, &tid);
    c.expect("cli-marker-2");
    c.send(b"echo after-reconnect-mark^er\r");
    c.expect("after-reconnect-marker");
    drop(c);
    // Nothing in all of this changed the Terminal's state.
    let mut stray = Vec::new();
    while let Got::Control(m) = watcher.next_control(Duration::from_millis(500)) {
        if m["type"] == "event" {
            stray.push(m);
        }
    }
    assert!(stray.is_empty(), "no lifecycle event: {stray:?}");
    eprintln!(
        "EVIDENCE detach: gui attach+type+disconnect, gui detach, cli Ctrl+B d, cli killed, reconnect with replay: Terminal {tid} shell {shell} running throughout, 0 lifecycle events"
    );
}

#[test]
fn exits_restarts_and_reconnects_leave_no_stale_state() {
    let s = Session::start("restart");
    let watcher = s.raw("gui");
    watcher.request(
        1,
        "events.subscribe",
        &json!({"topics": ["terminals", "panes"]}),
    );
    // A program that exits by itself: its Terminal is removed (tmux semantics)...
    let gone = s.new_terminal(&["gone", "--", "cmd.exe", "/c", "exit 3"]);
    let gone_id = gone["terminal_id"].as_str().unwrap().to_string();
    let evs = events_until(&watcher, |e| {
        has_event(e, "terminal.removed", |d| {
            d["terminal_id"] == gone_id.as_str()
        })
    });
    assert!(has_event(&evs, "pane.exited", |d| d["terminal_id"]
        == gone_id.as_str()
        && d["exit_code"] == 3));
    assert!(has_event(&evs, "terminal.removed", |d| d["reason"] == "exited"));
    // ...unless it was created with remain-on-exit: then it stays as stopped with its exit code.
    let kept = s.new_terminal(&["kept", "--remain-on-exit", "--", "cmd.exe", "/c", "exit 4"]);
    let kept_id = kept["terminal_id"].as_str().unwrap().to_string();
    events_until(&watcher, |e| {
        has_event(e, "terminal.state_changed", |d| {
            d["terminal_id"] == kept_id.as_str() && d["state"] == "stopped"
        })
    });
    let t = s.info("kept");
    assert_eq!(t["state"], "stopped");
    assert_eq!(
        (
            t["last_exit"]["exit_code"].as_u64(),
            t["last_exit"]["cause"].as_str()
        ),
        (Some(4), Some("exited"))
    );

    // Many reconnects see the same Terminals, ids and processes.
    let live = s.new_terminal(&["live"]);
    let snapshot = s.list();
    for _ in 0..10 {
        assert_eq!(s.list(), snapshot, "every new client sees the same state");
    }
    // host stop refuses live Terminals without --kill-terminals; then it ends them all.
    let (_, doc) = s.cli(&["host", "stop"]);
    assert_eq!(error(&doc), (7, "terminals_running"));
    assert!(alive(pane_pid(&live["terminal"])));
    let (_, doc) = s.cli(&["host", "stop", "--kill-terminals", "--no-input"]);
    assert_eq!(error(&doc), (7, "confirmation_required"));
    let (code, doc) = s.cli(&["host", "stop", "--kill-terminals", "--yes"]);
    assert_eq!(code, 0, "{doc}");
    assert!(s.host.wait(LONG), "the Host exited");
    assert!(
        !alive(pane_pid(&live["terminal"])),
        "its Terminal ended with it"
    );
    let until = Instant::now() + LONG;
    loop {
        match watcher.next_control(LONG) {
            Got::Eof => break,
            Got::Control(_) => {}
            other => panic!("clients see the Host go, got {other:?}"),
        }
        assert!(Instant::now() < until, "the Host connection stayed open");
    }
    // A Host started afterwards (on demand) owns nothing stale: no Terminal, no old id.
    let (code, doc) = s.cli(&["terminal", "list"]);
    assert_eq!(code, 0);
    assert_eq!(doc["data"]["terminals"], json!([]));
    let fresh = s.new_terminal(&["live"]);
    assert_eq!(fresh["host"]["started"], true, "started on demand");
    let new_host = Proc::open(pid_of(&fresh["host"]["pid"]));
    assert_ne!(new_host.pid, s.host.pid);
    let old: BTreeSet<String> = snapshot
        .iter()
        .map(|t| t["id"].as_str().unwrap().to_string())
        .collect();
    assert!(
        !old.contains(fresh["terminal_id"].as_str().unwrap()),
        "a new id"
    );
    assert_eq!(s.list().len(), 1);
    // The new Host is owned by this test too.
    let s2 = Session {
        ep: s.ep.clone(),
        host: new_host,
        seen: Mutex::new(vec![pane_pid(&fresh["terminal"])]),
    };
    eprintln!(
        "EVIDENCE restart: exit 3 -> removed (exited); remain-on-exit exit 4 -> stopped; 10 reconnects same state; host stop --kill-terminals ended host {}; new host {} started on demand with only the new Terminal {}",
        s.host.pid, s2.host.pid, fresh["terminal_id"]
    );
    drop(s2);
}
