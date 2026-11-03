//! Integration tests for the `subak` binary's CLI contract surface (Step 02, Step 07, Step 08;
//! docs/CLI_CONTRACT.md): help, version, capabilities, structured usage errors, and the Step 08
//! `host` prototype's errors and dry runs. JSON outputs are validated against the shared schemas
//! with the same validator as `cargo xtask schema-check`. The live Host is exercised by
//! `tests/conpty_host.rs`.

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
    a.push("--json");
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
    assert!(text.contains("pre-alpha; Step 08 ConPTY Host prototype"));
    assert!(text.contains("No command opens a window"));
    assert!(text.contains("no Terminals, panes, GUI or recovery yet"));
    for cmd in [
        "capabilities",
        "host start",
        "host status",
        "host attach",
        "host stop",
        "host serve",
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
    let out = subak(&["new", "work"]);
    assert_eq!(out.status.code(), Some(2));
    assert!(out.stdout.is_empty(), "errors must not go to stdout");
    let err = String::from_utf8_lossy(&out.stderr);
    assert!(err.starts_with("subak: error:"), "{err}");
    assert!(
        err.contains("only `capabilities` and the Step 08 `host` prototype exist"),
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
    assert_eq!(
        data["implementation"], "placeholder",
        "no Terminals or sessions yet: the Host is a Step 08 prototype"
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
            "host.start",
            "host.status",
            "host.attach",
            "host.stop",
            "host.serve"
        ],
        "only commands that really exist"
    );
    let stop = &data["commands"][4];
    assert_eq!(
        (stop["effect"].as_str(), &stop["requires_yes"]),
        (Some("destructive"), &Value::Bool(true))
    );
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
    assert!(String::from_utf8_lossy(&human.stdout).contains("Step 08 ConPTY Host prototype"));
}

#[test]
fn usage_errors_are_structured_with_json() {
    for (args, command) in [
        (&["attach"][..], "subak"),
        (&[], "subak"),
        (&["host", "frobnicate"], "host"),
        (&["host", "status", "--yes"], "host.status"),
        (&["host", "start", "--endpoint", "a\\b"], "host.start"),
        (&["host", "attach", "--size", "1x1"], "host.attach"),
        (&["host", "attach"], "host.attach"), // --json streams nothing; only with --dry-run
        (&["host", "stop", "--endpoint"], "host.stop"),
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
    // The default endpoint is per user and session, and marked as the prototype's.
    let out = subak(&["host", "start", "--dry-run", "--json"]);
    let doc: Value = serde_json::from_slice(&out.stdout).unwrap();
    let default = doc["data"]["would_start"]["endpoint"]
        .as_str()
        .unwrap()
        .to_string();
    assert!(
        default.starts_with(r"\\.\pipe\mouv-subak-proto-S-1-5-"),
        "{default}"
    );
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
