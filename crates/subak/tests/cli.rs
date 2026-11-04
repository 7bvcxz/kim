//! Integration tests for the `subak` binary's CLI contract surface (Steps 02, 07, 08, 09, 10;
//! docs/CLI_CONTRACT.md): help, version, capabilities, structured usage errors, and the `host`
//! and `terminal` commands' errors and dry runs without a Host. JSON outputs are validated
//! against the shared schemas with the same validator as `cargo xtask schema-check`. The live
//! Host is exercised by `tests/conpty_host.rs` (ConPTY, Step 08), `tests/host_ipc.rs` (IPC v1,
//! Step 09) and `tests/session_lifecycle.rs` (Terminals, Step 10).

#[path = "../../../xtask/src/jsonschema.rs"]
mod jsonschema;

use serde_json::Value;
use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};

fn subak(args: &[&str]) -> Output {
    let mut cmd = Command::new(env!("CARGO_BIN_EXE_subak"));
    cmd.args(args).stdin(Stdio::null());
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        cmd.creation_flags(0x0800_0000); // CREATE_NO_WINDOW
    }
    cmd.output().expect("failed to run subak binary")
}

fn repo() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../..")
}

fn validate(schema: &str, doc: &Value) {
    let path = repo().join(format!("docs/contracts/schemas/{schema}.schema.json"));
    let schema_doc: Value = serde_json::from_slice(&std::fs::read(path).unwrap()).unwrap();
    let errors = jsonschema::Validator::new(&schema_doc)
        .validate(doc)
        .unwrap();
    assert!(errors.is_empty(), "{schema}: {errors:?}\n{doc:#}");
}

/// Runs with `--json`; checks one envelope on stdout, empty stderr, and exit == error.exit_code.
fn json(args: &[&str]) -> (i32, Value) {
    let mut a = args.to_vec();
    a.insert(0, "--json"); // before any `--`, after which everything is the command
    let out = subak(&a);
    assert!(
        out.stderr.is_empty(),
        "{a:?}: --json keeps stderr empty: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let doc: Value = serde_json::from_slice(&out.stdout).expect("one JSON document");
    validate("cli-envelope-v1", &doc);
    let code = out.status.code().unwrap();
    if doc["ok"] == false {
        assert_eq!(i64::from(code), doc["error"]["exit_code"].as_i64().unwrap());
    }
    (code, doc)
}

#[test]
fn version_prints_package_version_and_succeeds() {
    let out = subak(&["--version"]);
    assert!(out.status.success());
    assert_eq!(
        String::from_utf8_lossy(&out.stdout).trim(),
        format!("subak {}", env!("CARGO_PKG_VERSION"))
    );
    assert!(out.stderr.is_empty());
}

#[test]
fn help_is_honest_about_the_prototype_status() {
    let out = subak(&["--help"]);
    assert!(out.status.success());
    let text = String::from_utf8_lossy(&out.stdout);
    assert!(text.contains("pre-alpha; Host, IPC v1, the Terminal lifecycle and its CLI"));
    assert!(text.contains("No command opens a window"));
    assert!(text.contains("no windows/panes splits, persistence or recovery yet"));
    assert!(text.contains("The window is a separate program, `subak-gui`"));
    assert!(text.contains("no Windows service exists"));
    assert!(text.contains("There is no ambiguous `close`"));
    for cmd in [
        "capabilities",
        "new",
        "ls",
        "status",
        "info",
        "attach",
        "detach",
        "rename",
        "stop",
        "kill",
        "rm",
        "capture",
        "history",
        "clear-history",
        "host start",
        "host status",
        "host health",
        "host attach",
        "host stop",
        "host serve",
        "hotkeys list",
        "hotkeys show",
        "hotkeys set",
        "hotkeys disable",
        "hotkeys enable",
        "hotkeys reset",
        "hotkeys check",
        "hotkeys export",
        "hotkeys import",
    ] {
        assert!(text.contains(&format!("subak {cmd}")), "{cmd}");
    }
    assert_eq!(
        subak(&["host", "--help"]).stdout,
        out.stdout,
        "help on every level"
    );
}

#[test]
fn unknown_arguments_fail_with_usage_exit_code_on_stderr() {
    let out = subak(&["frobnicate", "work"]);
    assert_eq!(out.status.code(), Some(2));
    assert!(out.stdout.is_empty(), "errors must not go to stdout");
    let err = String::from_utf8_lossy(&out.stderr);
    assert!(err.starts_with("subak: error:"), "{err}");
    assert!(
        err.contains("commands: new, ls, info, status, attach, detach"),
        "{err}"
    );
}

#[test]
fn capabilities_are_honest_and_follow_the_contract() {
    let (code, doc) = json(&["capabilities"]);
    assert_eq!(code, 0);
    validate("tool-capabilities-v1", &doc["data"]);
    assert_eq!(doc["command"], "capabilities");
    let data = &doc["data"];
    assert_eq!(data["tool"], "subak");
    assert_eq!(data["version"], env!("CARGO_PKG_VERSION"));
    // Every listed command really works (Steps 09–11); what is missing (GUI, splits, recovery)
    // is simply not listed, and the summary says pre-alpha (CLI_CONTRACT §9.1).
    assert_eq!(data["implementation"], "implemented");
    assert!(
        data["summary"].as_str().unwrap().starts_with("Pre-alpha"),
        "{data}"
    );
    let names: Vec<&str> = data["commands"]
        .as_array()
        .unwrap()
        .iter()
        .map(|c| c["name"].as_str().unwrap())
        .collect();
    assert_eq!(
        names,
        [
            "capabilities",
            "new",
            "ls",
            "info",
            "status",
            "attach",
            "detach",
            "rename",
            "stop",
            "kill",
            "rm",
            "capture",
            "history",
            "clear-history",
            "host.start",
            "host.status",
            "host.health",
            "host.attach",
            "host.stop",
            "host.serve",
            "hotkeys.list",
            "hotkeys.show",
            "hotkeys.set",
            "hotkeys.disable",
            "hotkeys.enable",
            "hotkeys.reset",
            "hotkeys.check",
            "hotkeys.export",
            "hotkeys.import"
        ],
        "only commands that really exist"
    );
    for c in data["commands"].as_array().unwrap() {
        let destructive = matches!(
            c["name"].as_str().unwrap(),
            "stop" | "kill" | "rm" | "clear-history" | "host.stop" | "hotkeys.reset"
        );
        assert_eq!(c["effect"] == "destructive", destructive, "{c}");
        assert_eq!(c["requires_yes"] == true, destructive, "{c}");
    }
    assert!(
        data["commands"]
            .as_array()
            .unwrap()
            .iter()
            .all(|c| c["opens_window"] == false && c["network"] == false)
    );

    let example: Value = serde_json::from_slice(
        &std::fs::read(repo().join("docs/contracts/examples/tool-capabilities-v1/subak.json"))
            .unwrap(),
    )
    .unwrap();
    assert_eq!(
        data, &example,
        "copy crates/subak/src/capabilities.json to docs/contracts/examples/tool-capabilities-v1/subak.json"
    );
    let envelope_example: Value = serde_json::from_slice(
        &std::fs::read(
            repo().join("docs/contracts/examples/cli-envelope-v1/subak-capabilities.json"),
        )
        .unwrap(),
    )
    .unwrap();
    assert_eq!(
        doc, envelope_example,
        "refresh docs/contracts/examples/cli-envelope-v1/subak-capabilities.json"
    );

    let human = subak(&["capabilities"]);
    assert!(human.status.success());
    assert!(String::from_utf8_lossy(&human.stdout).contains("named Terminals in the Host"));
}

#[test]
fn usage_errors_are_structured_with_json() {
    for (args, command) in [
        (&["frobnicate"][..], "subak"),
        (&[], "subak"),
        (&["host", "frobnicate"], "host"),
        (&["host", "status", "--yes"], "host.status"),
        (&["host", "start", "--endpoint", "a\\b"], "host.start"),
        (&["host", "attach", "--size", "1x1"], "host.attach"),
        (&["host", "attach"], "host.attach"), // --json streams nothing; only with --dry-run
        (&["host", "stop", "--endpoint"], "host.stop"),
        (&["host", "stop", "--force"], "host.stop"),
        (&["attach"], "attach"), // --json only with --dry-run
        (&["attach", "a", "b"], "attach"),
        (&["new", "--attach"], "new"),
        (&["new", "--", ""], "new"),
        (&["new", "--cwd"], "new"),
        (&["new", "--command-line", "cmd", "--", "cmd.exe"], "new"),
        (&["new", "--read-only"], "new"),
        (&["info"], "info"),
        (&["info", "a", "--", "cmd.exe"], "info"),
        (&["rename", "a"], "rename"),
        (&["rename", "a", "b", "c"], "rename"),
        (&["stop", "a", "--force"], "stop"),
        (&["kill", "a", "--kill-terminals"], "kill"),
        (&["ls", "extra"], "ls"),
        (&["status", "--yes"], "status"),
        (&["detach", "--client", "0"], "detach"),
        (&["detach", "--yes"], "detach"),
        // The deprecated spelling reports the canonical command name (ADR 0027).
        (&["terminal"], "terminal"),
        (&["terminal", "close", "x"], "terminal"),
        (&["terminal", "status"], "terminal"),
        (&["terminal", "info"], "info"),
        (&["terminal", "list", "extra"], "ls"),
        (&["terminal", "attach", "a"], "attach"),
        (&["hotkeys"], "hotkeys"),
        (&["hotkeys", "frobnicate"], "hotkeys"),
        (&["hotkeys", "set", "tab.next"], "hotkeys.set"),
        (&["hotkeys", "reset"], "hotkeys.reset"),
        (&["hotkeys", "list", "--endpoint", "x"], "hotkeys.list"),
    ] {
        let (code, doc) = json(args);
        assert_eq!(code, 2, "{args:?}");
        assert_eq!(doc["error"]["code"], "usage", "{args:?}");
        assert_eq!(doc["command"], command, "{args:?}");
    }
}

/// An endpoint no Host serves.
fn nobody() -> String {
    format!("subak-cli-test-nobody-{}", std::process::id())
}

#[cfg(windows)]
#[test]
fn host_commands_without_a_host_fail_with_host_not_running() {
    let ep = nobody();
    for args in [
        &["host", "status", "--endpoint", &ep][..],
        &["host", "stop", "--endpoint", &ep, "--yes"],
        &["host", "stop", "--endpoint", &ep, "--dry-run"],
        &["host", "attach", "--endpoint", &ep, "--dry-run"],
    ] {
        let (code, doc) = json(args);
        assert_eq!(
            (code, doc["error"]["code"].as_str()),
            (3, Some("host_not_running")),
            "{args:?}"
        );
        assert!(
            doc["error"]["hint"]
                .as_str()
                .unwrap()
                .contains("subak host start")
        );
    }
    // Human mode: the error goes to stderr, nothing to stdout.
    let out = subak(&["host", "status", "--endpoint", &ep]);
    assert_eq!(out.status.code(), Some(3));
    assert!(out.stdout.is_empty());
    assert!(String::from_utf8_lossy(&out.stderr).starts_with("subak: error: no Subak Host"));
}

#[cfg(windows)]
#[test]
fn host_dry_runs_describe_the_plan_and_start_nothing() {
    let ep = nobody();
    let (code, doc) = json(&[
        "host",
        "start",
        "--endpoint",
        &ep,
        "--shell",
        "cmd.exe /d",
        "--dry-run",
    ]);
    assert_eq!(code, 0, "{doc}");
    let plan = &doc["data"]["would_start"];
    assert_eq!(plan["shell"], "cmd.exe /d");
    assert_eq!(plan["endpoint"], format!(r"\\.\pipe\{ep}"));
    assert!(plan["arguments"].to_string().contains("serve"));
    let (code, doc) = json(&["host", "serve", "--endpoint", &ep, "--dry-run"]);
    assert_eq!(code, 0);
    assert_eq!(doc["data"]["dry_run"], true);
    let (code, _) = json(&["host", "status", "--endpoint", &ep]);
    assert_eq!(code, 3, "nothing was started");
    // The default endpoint is the well-known per-user, per-session pipe of ADR 0005, and the
    // start lock is derived from it.
    let out = subak(&["host", "start", "--dry-run", "--json"]);
    let doc: Value = serde_json::from_slice(&out.stdout).unwrap();
    let plan = &doc["data"]["would_start"];
    let default = plan["endpoint"].as_str().unwrap().to_string();
    assert!(
        default.starts_with(r"\\.\pipe\mouv-subak-S-1-5-") && default.contains("-s"),
        "{default}"
    );
    assert_eq!(
        plan["start_lock"].as_str().unwrap(),
        format!(r"Local\mouv-subak-start-{}", &default[r"\\.\pipe\".len()..])
    );
    assert_eq!(plan["environment"], "user (CreateEnvironmentBlock)");
    assert_eq!(plan["idle_exit_s"], 10, "ADR 0003 §4 default grace");
    // The plan reports the job this command runs in (risk R-03).
    assert!(plan["launcher_job"].is_null() || plan["launcher_job"]["kill_on_close"].is_boolean());
    let (code, doc) = json(&[
        "host",
        "start",
        "--endpoint",
        &ep,
        "--no-shell",
        "--dry-run",
    ]);
    assert_eq!(code, 0);
    assert_eq!(doc["data"]["would_start"]["shell"], Value::Null);
    assert!(
        doc["data"]["would_start"]["arguments"]
            .to_string()
            .contains("--no-shell")
    );
}

#[cfg(windows)]
#[test]
fn terminal_commands_without_a_host_start_nothing() {
    let ep = nobody();
    // Listing needs no Host: no Host means no Terminals, and nothing is started to find out.
    let (code, doc) = json(&["ls", "--endpoint", &ep]);
    assert_eq!(code, 0, "{doc}");
    assert_eq!(doc["data"]["terminals"], serde_json::json!([]));
    assert!(doc["data"]["host"].is_null());
    let out = subak(&["ls", "--endpoint", &ep]);
    assert!(String::from_utf8_lossy(&out.stdout).contains("no Terminals (no Host is running)"));
    // `status` reports that no Host runs, with exit 0.
    let (code, doc) = json(&["status", "--endpoint", &ep]);
    assert_eq!((code, &doc["data"]["running"]), (0, &Value::Bool(false)));
    for args in [
        &["info", "work", "--endpoint", &ep][..],
        &["rename", "work", "x", "--endpoint", &ep],
        &["stop", "work", "--endpoint", &ep, "--yes"],
        &["kill", "work", "--endpoint", &ep, "--yes"],
        &["rm", "work", "--endpoint", &ep, "--yes"],
        &["attach", "work", "--endpoint", &ep, "--dry-run"],
        &["detach", "work", "--endpoint", &ep],
        &["new", "--no-start", "--endpoint", &ep],
        &["new", "--no-start", "--endpoint", &ep, "--dry-run"],
        &["terminal", "info", "work", "--endpoint", &ep],
    ] {
        let (code, doc) = json(args);
        assert_eq!(
            (code, doc["error"]["code"].as_str()),
            (3, Some("host_not_running")),
            "{args:?}"
        );
    }
    // A dry run of `new` without a Host validates the name and starts nothing.
    let (code, doc) = json(&[
        "new",
        "서버",
        "--endpoint",
        &ep,
        "--dry-run",
        "--",
        "cmd.exe",
        "/d",
    ]);
    assert_eq!(code, 0, "{doc}");
    let plan = &doc["data"];
    assert_eq!(plan["would_start_host"], true);
    assert_eq!(plan["would_create"]["name"], "서버");
    assert_eq!(plan["would_create"]["command"]["args"][0], "/d");
    let (code, doc) = json(&["new", "a:b", "--endpoint", &ep, "--dry-run"]);
    assert_eq!(
        (code, doc["error"]["code"].as_str()),
        (2, Some("invalid_name")),
        "{doc}"
    );
    let (code, _) = json(&["host", "status", "--endpoint", &ep]);
    assert_eq!(code, 3, "nothing was started");
}

#[cfg(not(windows))]
#[test]
fn host_commands_are_unsupported_off_windows() {
    let (code, doc) = json(&["host", "status", "--endpoint", &nobody()]);
    assert_eq!(
        (code, doc["error"]["code"].as_str()),
        (5, Some("unsupported_platform"))
    );
}
