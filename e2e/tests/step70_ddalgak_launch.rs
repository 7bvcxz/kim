//! E2E — Step 70: Ddalgak program and website launches (docs/DDALGAK_RULE_MODEL.md §5.5, §5.6,
//! §6.2, docs/DDALGAK_ENGINE.md §13, ADR 0037). Requirements: `DDALGAK-FR-013`,
//! `DDALGAK-FR-014`, `DDALGAK-SEC-006`, `DDALGAK-SEC-007`, `DDALGAK-SEC-008`, `DDALGAK-SEC-011`,
//! `DDALGAK-SEC-013`, `DDALGAK-SEC-014`, `KIM-FR-012`.
//!
//! Scenario A (release `ddalgak`, isolated profile, no app): an agent adds a launcher for a
//! program in a folder with spaces and Korean characters (a quoted path as "Copy as path" gives it,
//! two arguments, one with spaces, an explicit environment override) and a website typed without
//! a scheme with a token in its query, each with `--allow launch-programs`. It is refused, with
//! nothing written: `javascript:`, `file:`, `ms-settings:`, a custom protocol, credentials in the
//! URL, `cmd.exe`, a relative path, an alternate data stream, a bad variable name. Without
//! `--allow` a rule is saved disabled (`consent_required`); changing a consented target needs a
//! fresh `--allow`. `rule list`, `rule show`, `rule check` and `status` never show a path, an
//! argument, a URL path or query or a variable value; `rule show --show-targets` does, because the
//! agent asked. A missing program is a `target_missing` warning, never searched for.
//!
//! Scenario B (debug test host of `ddalgak-gui` with `--test-launch-root`: no tray icon, a hook
//! that never consumes a key, programs only from the sandbox and always hidden, websites recorded
//! instead of opened): the engine loads both rules and starts nothing. On an unlocked desktop an
//! injected F15 (an obscure key, sent as another program) starts the hidden test program, which
//! writes its arguments and the override into a marker file, and an injected F16 — pressed twice —
//! is recorded as exactly one `open` hand-off of the normalized URL; on a locked desktop that part
//! is reported as skipped. Then the agent quits the app.
//!
//! Machine safety: sandboxed `%APPDATA%`/`%LOCALAPPDATA%`, own pipe endpoint; the only program
//! ever started is the scenario's own console marker program, built here with `rustc`, started
//! hidden and killed if still running; **no browser is opened** and no other program's window is
//! touched; every app is quit or killed by a guard.
#![cfg(windows)]

#[path = "../../xtask/src/jsonschema.rs"]
mod jsonschema;

#[path = "../../crates/ddalgak/tests/common/marker.rs"]
mod marker;

use kim_e2e::{TempDir, bin, bin_debug, require_e2e};
use serde_json::{Value, json};
use std::os::windows::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

const CREATE_NO_WINDOW: u32 = 0x0800_0000;
const SECRET: &str = "E2E-s3cret-토큰";

fn schema() -> Value {
    let p = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../docs/contracts/schemas/ddalgak-cli-v1.schema.json");
    serde_json::from_slice(&std::fs::read(p).unwrap()).unwrap()
}

struct Env<'a> {
    sb: &'a TempDir,
    cli: PathBuf,
    endpoint: String,
}

impl Env<'_> {
    fn cmd(&self, exe: &Path) -> Command {
        let mut c = Command::new(exe);
        let p = self.sb.path();
        c.env("APPDATA", p.join("Roaming"))
            .env("LOCALAPPDATA", p.join("Local"))
            .env_remove("KIM_NO_HOTKEYS")
            .creation_flags(CREATE_NO_WINDOW);
        c
    }

    fn json(&self, args: &[&str]) -> (i32, Value) {
        let mut c = self.cmd(&self.cli);
        c.args(args).args(["--endpoint", &self.endpoint, "--json"]);
        let out = c.output().unwrap();
        let doc: Value = serde_json::from_slice(&out.stdout)
            .unwrap_or_else(|e| panic!("{args:?}: not JSON ({e})"));
        let errors = jsonschema::Validator::new(&schema())
            .validate(&doc)
            .unwrap();
        assert!(errors.is_empty(), "{args:?}: {errors:?}\n{doc:#}");
        (out.status.code().unwrap_or(-1), doc)
    }

    fn human(&self, args: &[&str]) -> String {
        let mut c = self.cmd(&self.cli);
        c.args(args).args(["--endpoint", &self.endpoint]);
        String::from_utf8_lossy(&c.output().unwrap().stdout).to_string()
    }

    fn rules(&self) -> Value {
        let p = self.sb.path().join("Roaming/mouv/ddalgak/rules.json");
        serde_json::from_slice(&std::fs::read(p).unwrap_or_default()).unwrap_or(Value::Null)
    }

    fn start_host(&self, launch_root: &Path) -> Host {
        let mut c = self.cmd(&bin_debug("ddalgak-gui"));
        c.args([
            "--test-host",
            "--test-max-seconds",
            "90",
            "--endpoint",
            &self.endpoint,
            "--test-launch-root",
        ])
        .arg(launch_root)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null());
        let mut h = Host(c.spawn().unwrap());
        let until = Instant::now() + Duration::from_secs(20);
        loop {
            if self.json(&["status"]).1["data"]["app"]["running"] == true {
                return h;
            }
            assert!(h.0.try_wait().unwrap().is_none(), "the app exited early");
            assert!(Instant::now() < until, "the app never answered");
            std::thread::sleep(Duration::from_millis(100));
        }
    }
}

struct Host(Child);

impl Drop for Host {
    fn drop(&mut self) {
        if self.0.try_wait().ok().flatten().is_none() {
            let _ = self.0.kill();
        }
        let _ = self.0.wait();
    }
}

fn no_secret(what: &str, s: &str) {
    assert!(
        !s.contains("s3cret") && !s.contains("토큰") && !s.contains("결과 marker"),
        "{what} must not show a launch target: {s}"
    );
}

fn env<'a>(sb: &'a TempDir, cli: PathBuf, tag: &str) -> Env<'a> {
    Env {
        sb,
        cli,
        endpoint: format!("kim-e2e-step70-{tag}-{}", std::process::id()),
    }
}

/// The marker program in a folder with spaces and Korean characters.
struct Programs {
    root: PathBuf,
    exe: PathBuf,
    marker: PathBuf,
}

fn programs(sb: &TempDir) -> Programs {
    let built = marker::build(&sb.path().join("build"));
    let root = sb.path().join("테스트 폴더 with spaces");
    std::fs::create_dir_all(&root).unwrap();
    let exe = root.join("마커 app.exe");
    std::fs::copy(&built, &exe).unwrap();
    Programs {
        marker: root.join("결과 marker.txt"),
        root,
        exe,
    }
}

/// Scenario A, shared with B: the rules an agent writes headlessly.
fn build_rules(e: &Env, p: &Programs) -> String {
    for args in [
        ["--action", "open_url", "--url", "javascript:alert(1)"],
        [
            "--action",
            "open_url",
            "--url",
            "file:///C:/Windows/win.ini",
        ],
        [
            "--action",
            "open_url",
            "--url",
            "ms-settings:privacy-webcam",
        ],
        ["--action", "open_url", "--url", "zoommtg://zoom.us/join"],
        [
            "--action",
            "open_url",
            "--url",
            "https://user:s3cret@example.com/",
        ],
        [
            "--action",
            "open_program",
            "--path",
            r"C:\Windows\System32\cmd.exe",
        ],
        ["--action", "open_program", "--path", "notepad.exe"],
        ["--action", "open_program", "--path", r"C:\notes.txt:x.exe"],
    ] {
        let mut a = vec![
            "rule",
            "add",
            "--trigger",
            "F20",
            "--allow",
            "launch-programs",
        ];
        a.extend(args);
        let (code, d) = e.json(&a);
        assert_eq!(
            (code, d["error"]["code"].as_str()),
            (2, Some("usage")),
            "{a:?}: {d:#}"
        );
        no_secret("a refusal", &d.to_string());
    }
    let (code, d) = e.json(&[
        "rule",
        "add",
        "--trigger",
        "F20",
        "--action",
        "open_program",
        "--path",
        r"C:\x.exe",
        "--env",
        "1BAD=x",
    ]);
    assert_eq!(
        (code, d["error"]["code"].as_str()),
        (2, Some("usage")),
        "{d:#}"
    );
    assert!(e.rules().is_null(), "nothing was written");
    let quoted = format!("\"{}\"", p.exe.display());
    let marker_arg = p.marker.display().to_string();
    let env_arg = format!("DDALGAK_TEST_VAR={SECRET}");
    let (code, d) = e.json(&[
        "rule",
        "add",
        "--id",
        "open-marker",
        "--trigger",
        "F15",
        "--accept-injected",
        "--action",
        "open_program",
        "--path",
        &quoted,
        "--arg",
        &marker_arg,
        "--arg",
        "첫 번째 인자",
        "--env",
        &env_arg,
        "--allow",
        "launch-programs",
    ]);
    assert_eq!(code, 0, "{d:#}");
    no_secret("rule add", &d.to_string());
    let typed = format!("Example.COM/reset?token={SECRET}");
    let (code, d) = e.json(&[
        "rule",
        "add",
        "--id",
        "open-site",
        "--trigger",
        "F16",
        "--accept-injected",
        "--action",
        "open_url",
        "--url",
        &typed,
        "--allow",
        "launch-programs",
    ]);
    assert_eq!(code, 0, "{d:#}");
    no_secret("rule add", &d.to_string());
    let (code, d) = e.json(&[
        "rule",
        "add",
        "--id",
        "no-consent",
        "--trigger",
        "F17",
        "--action",
        "open_url",
        "--url",
        "https://example.org/",
    ]);
    assert_eq!(
        (code, d["error"]["code"].as_str()),
        (7, Some("consent_required")),
        "{d:#}"
    );
    let (code, d) = e.json(&[
        "rule",
        "add",
        "--id",
        "gone",
        "--trigger",
        "F18",
        "--action",
        "open_program",
        "--path",
        r"C:\kim-e2e-step70\never installed\tool.exe",
        "--allow",
        "launch-programs",
    ]);
    assert_eq!(code, 0, "{d:#}");
    // The stored form: normalized, consent bound to the exact targets.
    let doc = e.rules();
    let list = doc["profiles"][0]["rules"].as_array().unwrap().clone();
    let rule = |id: &str| list.iter().find(|r| r["id"] == id).unwrap().clone();
    let site = rule("open-site");
    let url = site["steps"][0]["url"].as_str().unwrap().to_string();
    assert!(
        url.starts_with("https://example.com/reset?token=E2E-s3cret-%ED"),
        "{url}"
    );
    assert_eq!(
        rule("open-marker")["steps"][0]["path"],
        p.exe.display().to_string()
    );
    assert_eq!(rule("no-consent")["enabled"], false);
    // A changed target needs a fresh --allow.
    let (code, d) = e.json(&[
        "rule",
        "edit",
        "open-site",
        "--action",
        "open_url",
        "--url",
        "https://other.example/",
    ]);
    assert_eq!(
        (code, d["error"]["code"].as_str()),
        (7, Some("consent_required")),
        "{d:#}"
    );
    assert_eq!(
        e.rules()["profiles"][0]["rules"],
        json!(list),
        "refused: nothing changed"
    );
    // Reading never shows a target, unless asked for.
    for args in [
        &["rule", "list"][..],
        &["rule", "check"],
        &["status"],
        &["rule", "show", "open-site"],
        &["rule", "show", "open-marker"],
    ] {
        let (_, d) = e.json(args);
        no_secret(&args.join(" "), &d.to_string());
        no_secret(&args.join(" "), &e.human(args));
    }
    let (_, d) = e.json(&["rule", "check"]);
    assert!(
        d["data"]["problems"]
            .as_array()
            .unwrap()
            .iter()
            .any(|p| p["code"] == "target_missing" && p["rule"] == "gone"),
        "{d:#}"
    );
    let (_, d) = e.json(&["rule", "show", "open-site", "--show-targets"]);
    assert_eq!(d["data"]["rule"]["steps"][0]["url"], url.as_str());
    url
}

#[test]
#[ignore = "e2e: run via `cargo xtask e2e` only (AGENTS.md §4)"]
fn an_agent_writes_launch_rules_headlessly_and_nothing_starts() {
    require_e2e();
    let sb = TempDir::new("step70-a");
    let e = env(&sb, bin("ddalgak"), "a");
    let p = programs(&sb);
    build_rules(&e, &p);
    // No app runs: nothing could have started, and nothing did.
    let (code, d) = e.json(&["engine", "start"]);
    assert_eq!(
        (code, d["error"]["code"].as_str()),
        (3, Some("engine_not_running"))
    );
    assert!(!p.marker.exists());
}

#[test]
#[ignore = "e2e: run via `cargo xtask e2e` only (AGENTS.md §4)"]
fn the_running_app_starts_only_the_hidden_test_program_and_opens_no_browser() {
    require_e2e();
    let sb = TempDir::new("step70-b");
    let e = env(&sb, bin("ddalgak"), "b");
    let p = programs(&sb);
    let url = build_rules(&e, &p);
    let host = e.start_host(&p.root);
    let record = p.root.join("shell-open.jsonl");
    if ddalgak::win::is_elevated() {
        eprintln!("skipped (elevated): the app refuses to turn its engine on as administrator");
        e.json(&["quit"]);
        return;
    }
    let (_, d) = e.json(&["engine", "start"]);
    assert_eq!(d["data"]["engine"]["state"], "running", "{d:#}");
    let (_, st) = e.json(&["status"]);
    no_secret("status", &st.to_string());
    assert_eq!(
        st["data"]["rules"]["active_rules"],
        json!(["open-marker", "open-site", "gone"])
    );
    std::thread::sleep(Duration::from_millis(300));
    assert!(
        !p.marker.exists() && !record.exists(),
        "loading started nothing"
    );
    let tap = |vk: u16| {
        ddalgak::win::inject::send_as_other_program(&[
            ddalgak::inject::OutKey { vk, up: false },
            ddalgak::inject::OutKey { vk, up: true },
        ]) == 2
    };
    if !ddalgak::win::input_desktop_available() || !tap(0x7E) {
        eprintln!("skipped (locked): the launch triggers need SendInput on an unlocked desktop");
    } else {
        let m = marker::wait(&p.marker, Duration::from_secs(20)).expect("the hidden program ran");
        marker::kill_if_running(m.pid, &p.exe);
        assert_eq!(m.args, ["첫 번째 인자"]);
        assert_eq!(m.env, SECRET);
        assert!(tap(0x7F) && tap(0x7F), "F16 twice");
        std::thread::sleep(Duration::from_millis(800));
        let opened: Vec<Value> = std::fs::read_to_string(&record)
            .unwrap_or_default()
            .lines()
            .map(|l| serde_json::from_str(l).unwrap())
            .collect();
        assert_eq!(
            opened,
            [json!({ "verb": "open", "target": url, "show": "normal" })]
        );
        let (_, st) = e.json(&["status"]);
        no_secret("status after the launches", &st.to_string());
        let outcomes: Vec<String> = st["data"]["activity"]
            .as_array()
            .unwrap()
            .iter()
            .map(|a| {
                format!(
                    "{}:{}",
                    a["rule"].as_str().unwrap(),
                    a["outcome"].as_str().unwrap()
                )
            })
            .collect();
        assert!(
            outcomes.contains(&"open-marker:ok".to_string()),
            "{outcomes:?}"
        );
        assert!(
            outcomes.contains(&"open-site:ok".to_string()),
            "{outcomes:?}"
        );
        assert!(
            outcomes.contains(&"open-site:rate_limited".to_string())
                || outcomes.contains(&"open-site:busy".to_string()),
            "{outcomes:?}"
        );
    }
    let (code, _) = e.json(&["quit"]);
    assert_eq!(code, 0);
    drop(host);
}
