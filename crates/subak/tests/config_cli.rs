//! Step 20 — `subak config …`, `subak profile …` and `subak new --profile` on real Windows
//! (docs/SUBAK_CLI.md §3.13, docs/SUBAK_GUI.md §13, ADR 0051). `SUBAK-FR-011`, `SUBAK-FR-046`,
//! `SUBAK-FR-048`, `SUBAK-FR-049`, `KIM-FR-012`, `KIM-FR-013`, `KIM-SEC-013`.
//!
//! Every run uses its own `APPDATA` sandbox (the maintainer's settings are never read or
//! written) and, when a Host is needed, its own endpoint. Every `--json` document is validated
//! against `cli-envelope-v1` and `subak-cli-v1`; stable ones are compared with golden files in
//! `docs/contracts/examples/subak-cli-v1/` (`SUBAK_BLESS=1` refreshes them). A shell profile's
//! launch is checked by this test binary started again as the profile's program
//! (`profile_argv_helper`): it writes the arguments, working directory and environment it got.
#![cfg(windows)]

#[path = "../../../xtask/src/jsonschema.rs"]
#[allow(dead_code)]
mod jsonschema;

mod common;

use common::*;
use serde_json::{Value, json};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

fn sandbox(tag: &str) -> PathBuf {
    let d = std::env::temp_dir().join(format!(
        "subak-config-cli-{tag}-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    std::fs::create_dir_all(&d).unwrap();
    d
}

fn cfg_dir(appdata: &Path) -> PathBuf {
    appdata.join("mouv").join("subak")
}

/// `subak --json <args>` with `APPDATA=<appdata>`, validated against subak-cli-v1.
fn sub(appdata: &Path, args: &[&str]) -> (i32, Value) {
    let a = appdata.display().to_string();
    let (code, doc) = run_env(args, &[("APPDATA", a.as_str())]);
    validate("subak-cli-v1", &doc);
    (code, doc)
}

/// Replaces the sandbox, the endpoint, ids, pids and times so a document can be a golden file.
fn normalize(doc: &Value, appdata: &Path, ep: Option<&str>) -> Value {
    let sandbox = appdata.display().to_string();
    let mut text = doc.to_string();
    for form in [sandbox.clone(), sandbox.replace('\\', "\\\\")] {
        text = text.replace(&form, "C:\\\\Users\\\\me\\\\AppData\\\\Roaming");
    }
    if let Some(ep) = ep {
        text = text.replace(ep, "subak-example");
    }
    let re = regex::Regex::new(r"config-\d{8}T\d{6}Z(-\d+)?\.toml").unwrap();
    text = re
        .replace_all(&text, "config-20261104T010203Z.toml")
        .into_owned();
    let re = regex::Regex::new(r"config-invalid-\d{8}T\d{6}Z(-\d+)?\.toml").unwrap();
    text = re
        .replace_all(&text, "config-invalid-20261104T010203Z.toml")
        .into_owned();
    serde_json::from_str(&text).unwrap()
}

fn golden(name: &str, doc: &Value, appdata: &Path) {
    let actual = normalize(doc, appdata, None);
    let path = Path::new(env!("CARGO_MANIFEST_DIR")).join(format!(
        "../../docs/contracts/examples/subak-cli-v1/{name}.json"
    ));
    let rendered = format!("{}\n", serde_json::to_string_pretty(&actual).unwrap());
    if std::env::var_os("SUBAK_BLESS").is_some() {
        std::fs::write(&path, &rendered).unwrap();
        return;
    }
    let expected: Value = std::fs::read(&path)
        .ok()
        .and_then(|b| serde_json::from_slice(&b).ok())
        .unwrap_or(Value::Null);
    assert_eq!(
        expected,
        actual,
        "{} differs from the real output. If intended, run `SUBAK_BLESS=1 cargo test -p subak \
         --test config_cli`, review the diff and note whether it is additive (CLI_CONTRACT §8).\n\
         actual:\n{rendered}",
        path.display()
    );
}

const PROFILES: &str = r#"# hand-written settings
config_version = 1

[appearance]
theme = "kim-light"   # easier in daylight
font_size = 15
future_knob = "kept"

[terminal]
default_profile = "PowerShell"

[[profiles]]
name = "PowerShell"
program = "powershell.exe"
args = ["-NoLogo", "-NoProfile"]
env = { RUST_BACKTRACE = "1" }

[[profiles]]
name = "dev"
program = "cmd.exe"
args = ["/k"]
env = { MODE = "fast", GITHUB_TOKEN = "ghp_abcdefghijklmnopqrstuvwxyz0123456789" }
"#;

#[test]
fn config_commands_edit_validate_back_up_and_never_start_a_host() {
    let appdata = sandbox("commands");
    let dir = cfg_dir(&appdata);
    let file = dir.join("config.toml");
    let ep = endpoint("config-nohost");
    // No file: the defaults, and nothing is created by reading.
    let (code, doc) = sub(&appdata, &["config", "show"]);
    assert_eq!(code, 0, "{doc}");
    assert_eq!(doc["data"]["state"], "missing");
    assert!(
        doc["data"]["settings"]
            .as_array()
            .unwrap()
            .iter()
            .all(|r| r["source"] == "default")
    );
    golden("config-show-defaults", &doc, &appdata);
    assert!(!file.exists(), "reading writes nothing");
    // set: the first edit starts the file; key=value works too; a dry run writes nothing.
    let (code, doc) = sub(
        &appdata,
        &[
            "config",
            "set",
            "appearance.theme",
            "kim-light",
            "--dry-run",
        ],
    );
    assert_eq!(code, 0, "{doc}");
    assert_eq!(doc["data"]["written"], false);
    assert!(!file.exists());
    let (code, doc) = sub(
        &appdata,
        &["config", "set", "appearance.theme", "kim-light"],
    );
    assert_eq!(code, 0, "{doc}");
    golden("config-set", &doc, &appdata);
    let (code, _) = sub(
        &appdata,
        &["config", "set", "terminal.word_separators= -.,"],
    );
    assert_eq!(code, 0);
    let text = std::fs::read_to_string(&file).unwrap();
    assert!(text.contains("theme = \"kim-light\"") && text.contains("word_separators = \" -.,\""));
    // Refusals: usage (exit 2) for an unknown key or a bad value; nothing written.
    let (code, doc) = sub(&appdata, &["config", "set", "appearance.opacity", "20"]);
    assert_eq!(code, 2, "{doc}");
    assert_eq!(doc["error"]["code"], "usage");
    let (code, doc) = sub(&appdata, &["config", "set", "appearance.colour", "red"]);
    assert_eq!(code, 2, "{doc}");
    assert!(
        doc["error"]["hint"]
            .as_str()
            .unwrap()
            .contains("appearance.theme")
    );
    assert_eq!(std::fs::read_to_string(&file).unwrap(), text, "untouched");
    // A hand-written file: comments and unknown keys survive an edit; problems name their line;
    // secret-looking values are never printed.
    std::fs::write(&file, PROFILES).unwrap();
    let (code, doc) = sub(&appdata, &["config", "set", "appearance.font_size", "16"]);
    assert_eq!(code, 0, "{doc}");
    let text = std::fs::read_to_string(&file).unwrap();
    assert!(text.starts_with("# hand-written settings\n"));
    assert!(text.contains("theme = \"kim-light\"   # easier in daylight"));
    assert!(text.contains("future_knob = \"kept\"") && text.contains("font_size = 16\n"));
    let (code, doc) = sub(&appdata, &["config", "check"]);
    assert_eq!(code, 0, "{doc}");
    assert_eq!(doc["data"]["valid"], true);
    assert_eq!(doc["data"]["unknown"], json!(["appearance.future_knob"]));
    assert_eq!(doc["data"]["problems"][0]["severity"], "warning");
    assert_eq!(doc["data"]["problems"][0]["line"], 22);
    golden("config-check", &doc, &appdata);
    let (code, doc) = sub(&appdata, &["profile", "list"]);
    assert_eq!(code, 0, "{doc}");
    assert!(!doc.to_string().contains("ghp_abc"), "{doc}");
    assert_eq!(doc["data"]["default"], "PowerShell");
    golden("profile-list", &doc, &appdata);
    let (code, doc) = sub(&appdata, &["profile", "show", "DEV"]);
    assert_eq!(code, 0, "{doc}");
    assert_eq!(doc["data"]["profile"]["env"]["GITHUB_TOKEN"], "***");
    assert_eq!(doc["data"]["profile"]["env"]["MODE"], "fast");
    let (code, doc) = sub(&appdata, &["profile", "show", "nope"]);
    assert_eq!(code, 3, "{doc}");
    assert_eq!(doc["error"]["code"], "profile_not_found");
    golden("error-profile-not-found", &doc, &appdata);
    let (code, doc) = sub(&appdata, &["config", "export"]);
    assert_eq!(code, 0, "{doc}");
    let exported = doc["data"]["text"].as_str().unwrap().to_string();
    assert!(!exported.contains("ghp_abc") && exported.contains("GITHUB_TOKEN = \"***\""));
    assert_eq!(doc["data"]["redacted"], 1);
    golden("config-export", &doc, &appdata);
    // Import refuses secrets (also the *** of an export) and invalid documents.
    let incoming = appdata.join("incoming.toml");
    std::fs::write(&incoming, &exported).unwrap();
    let inc = incoming.display().to_string();
    let (code, doc) = sub(&appdata, &["config", "import", &inc]);
    assert_eq!(code, 4, "{doc}");
    assert_eq!(doc["error"]["code"], "secret_in_config");
    golden("error-secret-in-config", &doc, &appdata);
    std::fs::write(&incoming, "[appearance]\nfont_size = 99\n").unwrap();
    let (code, doc) = sub(&appdata, &["config", "import", &inc]);
    assert_eq!(code, 4, "{doc}");
    assert_eq!(doc["error"]["code"], "config_invalid");
    assert!(
        doc["error"]["details"][0]
            .as_str()
            .unwrap()
            .contains("line 2")
    );
    // A clean import backs the current file up first.
    std::fs::write(
        &incoming,
        "config_version = 1\n[appearance]\nopacity = 80\n[[profiles]]\nname = \"cmd\"\nprogram = \"cmd.exe\"\n",
    )
    .unwrap();
    let (code, doc) = sub(&appdata, &["config", "import", &inc, "--dry-run"]);
    assert_eq!(code, 0, "{doc}");
    assert_eq!(doc["data"]["written"], false);
    golden("config-import-dry-run", &doc, &appdata);
    let before = std::fs::read_to_string(&file).unwrap();
    let (code, doc) = sub(&appdata, &["config", "import", &inc]);
    assert_eq!(code, 0, "{doc}");
    let backup = PathBuf::from(doc["data"]["backup"].as_str().unwrap());
    assert_eq!(
        std::fs::read_to_string(&backup).unwrap(),
        before,
        "the backup is the old file"
    );
    assert!(
        std::fs::read_to_string(&file)
            .unwrap()
            .contains("opacity = 80")
    );
    // reset --all asks (non-interactive: confirmation_required), then backs up and resets.
    let (code, doc) = sub(&appdata, &["config", "reset", "--all"]);
    assert_eq!(code, 7, "{doc}");
    assert_eq!(doc["error"]["code"], "confirmation_required");
    let (code, doc) = sub(&appdata, &["config", "reset", "--all", "--yes"]);
    assert_eq!(code, 0, "{doc}");
    golden("config-reset-all", &doc, &appdata);
    let text = std::fs::read_to_string(&file).unwrap();
    assert!(
        !text.contains("opacity") && text.contains("[[profiles]]"),
        "profiles stay: {text}"
    );
    // A broken file: the defaults, every edit refused (never rewritten), reset --all replaces
    // it after keeping it as a backup.
    std::fs::write(&file, "[appearance\ntheme = ").unwrap();
    let (code, doc) = sub(&appdata, &["config", "set", "appearance.theme", "kim-dark"]);
    assert_eq!(code, 4, "{doc}");
    assert_eq!(doc["error"]["code"], "config_invalid");
    golden("error-config-invalid", &doc, &appdata);
    assert_eq!(
        std::fs::read_to_string(&file).unwrap(),
        "[appearance\ntheme = "
    );
    let (code, doc) = sub(&appdata, &["config", "show"]);
    assert_eq!(code, 0);
    assert_eq!(doc["data"]["state"], "invalid");
    let (code, doc) = sub(&appdata, &["config", "reset", "--all", "--yes"]);
    assert_eq!(code, 0, "{doc}");
    let kept = doc["data"]["backup"].as_str().unwrap();
    assert!(kept.contains("config-invalid-"), "{kept}");
    assert_eq!(
        std::fs::read_to_string(kept).unwrap(),
        "[appearance\ntheme = "
    );
    // A newer file is read-only: set, reset and import refuse with exit 5.
    std::fs::write(
        &file,
        "config_version = 2\n[appearance]\ntheme = \"kim-dark\"\n",
    )
    .unwrap();
    let (code, doc) = sub(&appdata, &["config", "set", "appearance.theme", "auto"]);
    assert_eq!(code, 5, "{doc}");
    assert_eq!(doc["error"]["code"], "config_from_newer_version");
    golden("error-config-from-newer-version", &doc, &appdata);
    let (code, _) = sub(&appdata, &["config", "reset", "--all", "--yes"]);
    assert_eq!(code, 5);
    let (code, _) = sub(&appdata, &["config", "import", &inc]);
    assert_eq!(code, 5);
    assert!(
        std::fs::read_to_string(&file)
            .unwrap()
            .starts_with("config_version = 2")
    );
    // Backups: at most five.
    let n = std::fs::read_dir(dir.join("backups")).unwrap().count();
    assert!((2..=6).contains(&n), "{n}");
    // None of this contacted or started a Host.
    let (code, doc) = run(&["status", "--endpoint", &ep]);
    assert_eq!(code, 0);
    assert_eq!(doc["data"]["running"], false);
    // Human output never shows a secret either.
    let out = std::process::Command::new(subak())
        .args(["profile", "list"])
        .env("APPDATA", &appdata)
        .output()
        .unwrap();
    let human = String::from_utf8_lossy(&out.stdout);
    assert!(!human.contains("ghp_"), "{human}");
    println!(
        "EVIDENCE config: {} backups kept in {}",
        n,
        dir.join("backups").display()
    );
}

/// The child side of `a_profile_launches_its_exact_argv_cwd_and_env`: when started as a
/// profile's program with `SUBAK_PROFILE_ECHO` set, this test binary writes what it received.
#[test]
fn profile_argv_helper() {
    let Some(file) = std::env::var_os("SUBAK_PROFILE_ECHO") else {
        return;
    };
    let doc = json!({
        "args": std::env::args().collect::<Vec<_>>(),
        "cwd": std::env::current_dir().unwrap().display().to_string(),
        "mode": std::env::var("SUBAK_PROFILE_MODE").ok(),
        "greeting": std::env::var("SUBAK_PROFILE_GREETING").ok(),
        "temp": std::env::var("TEMP").ok(),
    });
    std::fs::write(file, doc.to_string()).unwrap();
}

fn wait_for(what: &str, mut f: impl FnMut() -> bool) {
    let until = Instant::now() + Duration::from_secs(30);
    while !f() {
        assert!(Instant::now() < until, "{what}");
        std::thread::sleep(Duration::from_millis(50));
    }
}

/// A TOML literal string (no escapes; the paths and arguments here have no `'`).
fn lit(s: &str) -> String {
    assert!(!s.contains('\''));
    format!("'{s}'")
}

#[test]
fn a_profile_launches_its_exact_argv_cwd_and_env_and_settings_never_touch_running_terminals() {
    let host = Host::start("config-profile", &["--no-shell"]);
    let ep = host.ep.clone();
    let appdata = sandbox("profile");
    let dir = cfg_dir(&appdata);
    std::fs::create_dir_all(&dir).unwrap();
    let work = appdata.join("작업 폴더");
    std::fs::create_dir_all(&work).unwrap();
    let out = appdata.join("argv.json");
    let me = std::env::current_exe().unwrap().display().to_string();
    let tricky = [
        "a b",
        "say \"hi\"",
        "100% ^&|<>()",
        "한글 인자",
        "trailing\\",
    ];
    let args: Vec<String> = ["profile_argv_helper", "--exact", "--test-threads=1", "--"]
        .iter()
        .map(|s| (*s).to_string())
        .chain(tricky.iter().map(|s| (*s).to_string()))
        .collect();
    let toml = format!(
        "config_version = 1\n[terminal]\ndefault_profile = \"echo\"\n\n[[profiles]]\nname = \"echo\"\nprogram = {}\nargs = [{}]\ncwd = {}\nenv = {{ SUBAK_PROFILE_ECHO = {}, SUBAK_PROFILE_MODE = \"fast\", SUBAK_PROFILE_GREETING = \"안녕\" }}\nunset_env = [\"TEMP\"]\n",
        lit(&me),
        args.iter()
            .map(|a| format!("\"{}\"", a.replace('\\', "\\\\").replace('"', "\\\"")))
            .collect::<Vec<_>>()
            .join(", "),
        lit(&work.display().to_string()),
        lit(&out.display().to_string()),
    );
    std::fs::write(dir.join("config.toml"), &toml).unwrap();
    let (code, doc) = sub(&appdata, &["config", "check"]);
    assert_eq!(doc["data"]["valid"], true, "{doc}\n{toml}");
    assert_eq!(code, 0);
    // A dry run names the profile and the resolved program, starting nothing.
    let (code, doc) = sub(
        &appdata,
        &[
            "new",
            "p1",
            "--profile",
            "echo",
            "--dry-run",
            "--endpoint",
            &ep,
        ],
    );
    assert_eq!(code, 0, "{doc}");
    assert_eq!(doc["data"]["profile"], "echo");
    assert_eq!(
        doc["data"]["would_create"]["cwd"],
        work.display().to_string()
    );
    assert!(!out.exists());
    // `subak new` without a program uses the default profile: argv exact, cwd, env.
    let (code, doc) = sub(
        &appdata,
        &["new", "p1", "--remain-on-exit", "--endpoint", &ep],
    );
    assert_eq!(code, 0, "{doc}");
    assert_eq!(doc["data"]["profile"], "echo");
    let tid = doc["data"]["terminal_id"].as_str().unwrap().to_string();
    wait_for("the profile's program wrote its argv", || out.exists());
    let got: Value = serde_json::from_slice(&std::fs::read(&out).unwrap()).unwrap();
    let argv: Vec<String> = got["args"]
        .as_array()
        .unwrap()
        .iter()
        .map(|v| v.as_str().unwrap().to_string())
        .collect();
    assert_eq!(argv[5..], tricky.map(String::from), "argv exact: {argv:?}");
    assert_eq!(got["cwd"], work.display().to_string());
    assert_eq!(got["mode"], "fast");
    assert_eq!(got["greeting"], "안녕");
    assert_eq!(got["temp"], Value::Null, "unset_env removed TEMP");
    // The pane's record names the variables, never their values.
    let (_, info) = sub(&appdata, &["info", &tid, "--endpoint", &ep]);
    let pane = &info["data"]["terminal"]["windows"][0]["panes"][0];
    let set_names = pane["env"]["set"].to_string();
    assert!(set_names.contains("SUBAK_PROFILE_MODE"), "{pane}");
    assert!(!info.to_string().contains("안녕"), "values are never shown");
    // --env and --cwd given on the command line win over the profile's; --no-profile ignores it.
    std::fs::remove_file(&out).unwrap();
    let elsewhere = appdata.join("elsewhere");
    std::fs::create_dir_all(&elsewhere).unwrap();
    let el = elsewhere.display().to_string();
    let (code, doc) = sub(
        &appdata,
        &[
            "new",
            "p2",
            "--profile",
            "ECHO",
            "--cwd",
            &el,
            "--env",
            "SUBAK_PROFILE_MODE=slow",
            "--remain-on-exit",
            "--endpoint",
            &ep,
        ],
    );
    assert_eq!(code, 0, "{doc}");
    wait_for("the second run", || out.exists());
    let got: Value = serde_json::from_slice(&std::fs::read(&out).unwrap()).unwrap();
    assert_eq!(got["cwd"], el);
    assert_eq!(got["mode"], "slow");
    assert_eq!(got["greeting"], "안녕");
    let (code, doc) = sub(
        &appdata,
        &["new", "p3", "--no-profile", "--dry-run", "--endpoint", &ep],
    );
    assert_eq!(code, 0, "{doc}");
    assert_eq!(doc["data"]["profile"], Value::Null);
    let (code, doc) = sub(
        &appdata,
        &["new", "p4", "--profile", "nope", "--endpoint", &ep],
    );
    assert_eq!(code, 3, "{doc}");
    assert_eq!(doc["error"]["code"], "profile_not_found");
    // Settings changes never reach a running Terminal: a running pane keeps its process, its
    // environment names and its size while every setting and the profile change.
    let (code, doc) = sub(
        &appdata,
        &[
            "new",
            "live",
            "--no-profile",
            "--endpoint",
            &ep,
            "--",
            "cmd.exe",
            "/d",
            "/k",
        ],
    );
    assert_eq!(code, 0, "{doc}");
    let pane_of = |name: &str| {
        let (_, i) = sub(&appdata, &["info", name, "--endpoint", &ep]);
        i["data"]["terminal"]["windows"][0]["panes"][0].clone()
    };
    let before = pane_of("live");
    for (k, v) in [
        ("appearance.theme", "kim-dark"),
        ("appearance.font_size", "20"),
        ("appearance.line_height", "1.6"),
        ("appearance.opacity", "70"),
        ("terminal.default_profile", ""),
    ] {
        let (code, doc) = sub(&appdata, &["config", "set", k, v]);
        assert_eq!(code, 0, "{doc}");
    }
    std::fs::write(
        dir.join("config.toml"),
        "config_version = 1\n[[profiles]]\nname = \"echo\"\nprogram = \"pwsh.exe\"\n",
    )
    .unwrap();
    std::thread::sleep(Duration::from_millis(300));
    let after = pane_of("live");
    for k in ["pid", "env", "size", "command", "cwd"] {
        assert_eq!(before[k], after[k], "{k} of a running pane");
    }
    assert_eq!(after["state"], before["state"]);
    // A broken settings file: `new` runs the Host's shell and says why; `--profile` refuses.
    std::fs::write(dir.join("config.toml"), "[terminal\ndefault_profile = ").unwrap();
    let (code, doc) = sub(&appdata, &["new", "p6", "--dry-run", "--endpoint", &ep]);
    assert_eq!(code, 0, "{doc}");
    assert_eq!(doc["data"]["profile"], Value::Null);
    assert!(
        doc["warnings"][0]
            .as_str()
            .unwrap()
            .contains("no default profile is used"),
        "{doc}"
    );
    let (code, doc) = sub(
        &appdata,
        &[
            "new",
            "p7",
            "--profile",
            "echo",
            "--dry-run",
            "--endpoint",
            &ep,
        ],
    );
    assert_eq!(code, 4, "{doc}");
    assert_eq!(doc["error"]["code"], "config_invalid");
    println!(
        "EVIDENCE profile: argv {} words exact, cwd {}, env set/unset; running pane pid {} unchanged",
        tricky.len(),
        work.display(),
        after["pid"]
    );
}
