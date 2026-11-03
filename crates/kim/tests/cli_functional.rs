//! CLI functional tests (Step 07): malformed arguments, missing and damaged tools, permission
//! problems, partial downloads, offline mode, non-interactive execution, secret redaction,
//! stdout/stderr discipline and capability negotiation. Every case runs the real `kim` binary in
//! a sandbox; downloads come from a loopback server; nothing touches the real network.

#[path = "../../../xtask/src/jsonschema.rs"]
mod jsonschema;

mod common;

use common::{Sandbox, Server, file_url, manifest, package, release};
use serde_json::Value;
use std::path::Path;
use std::time::{Duration, Instant};

fn assert_valid(doc: &Value) {
    let p = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../docs/contracts/schemas/kim-cli-v1.schema.json");
    let schema: Value = serde_json::from_slice(&std::fs::read(p).unwrap()).unwrap();
    let errors = jsonschema::Validator::new(&schema).validate(doc).unwrap();
    assert!(errors.is_empty(), "{errors:?}\n{doc:#}");
}

fn err_code(doc: &Value) -> &str {
    doc["error"]["code"].as_str().unwrap_or("<none>")
}

/// A sandbox with `example-tool` 1.0.0 (shipping a capabilities manifest) published and served.
fn with_tool(tag: &str) -> (Sandbox, Server, Vec<u8>, String) {
    let sb = Sandbox::new(tag);
    let srv = Server::start();
    let body = package(&[(
        "capabilities.json",
        &manifest("example-tool", "1.0.0", &["capabilities", "greet"]),
    )]);
    let url = srv.put("/1.zip", body.clone());
    sb.publish(100, &[release("1.0.0", &url, &body, "tool.exe")]);
    (sb, srv, body, url)
}

fn staging_is_empty(sb: &Sandbox) -> bool {
    std::fs::read_dir(sb.programs().join(".staging")).map_or(true, |mut rd| rd.next().is_none())
}

fn record_version(sb: &Sandbox) -> Option<String> {
    let rec: Value = serde_json::from_slice(
        &std::fs::read(sb.tool_dir("example-tool").join("current.json")).ok()?,
    )
    .ok()?;
    rec["version"].as_str().map(str::to_string)
}

// --- malformed arguments ---------------------------------------------------------------------

#[test]
fn malformed_arguments_exit_2_with_a_structured_usage_error() {
    let sb = Sandbox::new("usage");
    let cases: &[(&[&str], &str)] = &[
        (&["frobnicate"], "kim"),
        (&["catalog"], "catalog"),
        (&["catalog", "list", "--bogus"], "catalog.list"),
        (&["catalog", "list", "--channel", "nightly"], "catalog.list"),
        (&["catalog", "list", "--quiet", "--verbose"], "catalog.list"),
        (&["install"], "install"),
        (
            &["tools", "describe", "kim", "--timeout", "0"],
            "tools.describe",
        ),
        (
            &["tools", "describe", "kim", "--require-version", "not a req"],
            "tools.describe",
        ),
        (&["settings", "set", "nope", "true"], "settings.set"),
        (
            &["settings", "set", "offline_mode", "maybe"],
            "settings.set",
        ),
        (
            &["update", "check", "--source", "file:///x"],
            "update.check",
        ),
    ];
    for (args, command) in cases {
        let human = sb.run(args);
        assert_eq!(human.status.code(), Some(2), "{args:?}");
        assert!(
            human.stdout.is_empty(),
            "{args:?}: errors never go to stdout"
        );
        assert!(
            !human.stderr.is_empty(),
            "{args:?}: the error is explained on stderr"
        );

        let mut with_json = args.to_vec();
        with_json.push("--json");
        let (code, doc, out) = sb.json(&with_json);
        assert_eq!(code, 2, "{args:?}");
        assert_eq!(err_code(&doc), "usage", "{args:?}");
        assert_eq!(doc["command"], *command, "{args:?}");
        assert!(
            out.stderr.is_empty(),
            "{args:?}: --json errors are on stdout only"
        );
        assert_valid(&doc);
    }
    // A refused source URL is a usage problem too, with its own code.
    let (code, doc, _) = sb.json(&["catalog", "refresh", "--source", "ftp://x/c.json", "--json"]);
    assert_eq!((code, err_code(&doc)), (2, "unsupported_source"));
    // `--help` and `--version` are human text by contract, even with --json.
    let help = sb.run(&["install", "--help"]);
    assert!(help.status.success());
    assert!(String::from_utf8_lossy(&help.stdout).contains("--dry-run"));
    assert_eq!(sb.snapshot().len(), 0, "usage errors write nothing");
}

// --- missing and damaged tools ---------------------------------------------------------------

#[test]
fn missing_and_damaged_tools_are_reported_not_guessed() {
    let sb = Sandbox::new("missing");
    for (args, code, error) in [
        (&["tools", "describe", "nope"][..], 3, "not_installed"),
        (&["tools", "describe", "subak"], 3, "not_installed"),
        (&["install", "nope"], 3, "tool_not_found"),
        (&["update", "nope"], 3, "not_installed"),
        (&["uninstall", "nope", "--yes"], 3, "not_installed"),
        (&["catalog", "show", "nope"], 3, "tool_not_found"),
    ] {
        let mut a = args.to_vec();
        a.push("--json");
        let (c, doc, _) = sb.json(&a);
        assert_eq!((c, err_code(&doc)), (code, error), "{args:?}");
        assert_valid(&doc);
    }
    let (_, doc, _) = sb.json(&["tools", "describe", "subak", "--json"]);
    assert!(
        doc["error"]["hint"]
            .as_str()
            .unwrap()
            .contains("kim install subak")
    );

    // A damaged install record is shown as damaged, never trusted.
    let dir = sb.tool_dir("broken");
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(dir.join("current.json"), b"{ not json").unwrap();
    let (code, doc, _) = sb.json(&["tools", "list", "--json"]);
    assert_eq!(code, 0);
    let broken = doc["data"]["tools"]
        .as_array()
        .unwrap()
        .iter()
        .find(|t| t["id"] == "broken")
        .expect("damaged tools are listed");
    assert_eq!(broken["status"], "damaged");
    assert!(broken["damaged"].as_str().unwrap().contains("current.json"));
    let (code, doc, _) = sb.json(&["tools", "describe", "broken", "--probe", "--json"]);
    assert_eq!((code, err_code(&doc)), (3, "not_installed"));
    let (code, doc, _) = sb.json(&[
        "tools",
        "describe",
        "broken",
        "--require-version",
        ">=1",
        "--json",
    ]);
    assert_eq!((code, err_code(&doc)), (5, "tool_incompatible"));
    let (_, doc, _) = sb.json(&["status", "--json"]);
    assert!(
        doc["data"]["problems"]
            .as_array()
            .unwrap()
            .iter()
            .any(|p| p.as_str().unwrap().contains("broken is damaged"))
    );
    assert_valid(&doc);
}

// --- permission problems (Windows ACLs, no administrator rights) ----------------------------

/// Denies specific rights (for Everyone) on a path; restored on drop. Works without elevation
/// because the current user owns the sandbox and may always change its DACL. Only *specific*
/// rights are used (`WD`, `AD`, `DE`, `DC`, …): icacls' simple rights such as `W` or `D` include
/// SYNCHRONIZE and would block reading as well.
#[cfg(windows)]
struct Deny(std::path::PathBuf);

/// "Read-only directory": no new files or subdirectories, no attribute changes; reads work.
#[cfg(windows)]
const READ_ONLY_DIR: &str = "(OI)(CI)(WD,AD,WEA,WA)";

#[cfg(windows)]
impl Deny {
    fn new(path: &Path, rights: &str) -> Self {
        icacls(path, &["/deny", &format!("*S-1-1-0:{rights}")]);
        Self(path.to_path_buf())
    }
}

#[cfg(windows)]
impl Drop for Deny {
    fn drop(&mut self) {
        icacls(&self.0, &["/remove:d", "*S-1-1-0"]);
    }
}

#[cfg(windows)]
fn icacls(path: &Path, args: &[&str]) {
    use std::os::windows::process::CommandExt;
    let root = std::env::var("SystemRoot").unwrap_or_else(|_| r"C:\Windows".into());
    let out = std::process::Command::new(Path::new(&root).join("System32").join("icacls.exe"))
        .arg(path)
        .args(args)
        .creation_flags(0x0800_0000) // CREATE_NO_WINDOW
        .output()
        .expect("icacls");
    assert!(
        out.status.success(),
        "icacls {args:?}: {}",
        String::from_utf8_lossy(&out.stdout)
    );
}

#[test]
#[cfg(windows)]
fn a_read_only_install_directory_fails_with_permission_denied_and_changes_nothing() {
    let (sb, _srv, _, _) = with_tool("perm-dir");
    std::fs::create_dir_all(sb.programs()).unwrap();
    {
        let _deny = Deny::new(&sb.programs(), READ_ONLY_DIR);
        let (code, doc, _) = sb.json(&["install", "example-tool", "--json"]);
        assert_eq!((code, err_code(&doc)), (1, "permission_denied"), "{doc}");
        assert!(
            doc["error"]["hint"]
                .as_str()
                .unwrap()
                .contains("never elevates")
        );
        assert_valid(&doc);
        assert!(!sb.tool_dir("example-tool").exists(), "nothing was created");
        // Planning still works: --dry-run only reads.
        let (code, _, _) = sb.json(&["install", "example-tool", "--dry-run", "--json"]);
        assert_eq!(code, 0);
        let human = sb.run(&["install", "example-tool"]);
        assert_eq!(human.status.code(), Some(1));
        assert!(String::from_utf8_lossy(&human.stderr).contains("access denied"));
    }
    // Same command succeeds once the user fixed the permissions: it was the only problem.
    let (code, doc, _) = sb.json(&["install", "example-tool", "--json"]);
    assert_eq!(code, 0, "{doc}");
}

#[test]
#[cfg(windows)]
fn a_protected_install_record_aborts_the_update_at_the_commit_point_and_rolls_back() {
    let (sb, srv, v1, u1) = with_tool("perm-record");
    let (code, _, _) = sb.json(&["install", "example-tool", "--json"]);
    assert_eq!(code, 0);
    let v2 = package(&[]);
    let u2 = srv.put("/2.zip", v2.clone());
    sb.publish(
        101,
        &[
            release("1.0.0", &u1, &v1, "tool.exe"),
            release("2.0.0", &u2, &v2, "tool.exe"),
        ],
    );
    let record = sb.tool_dir("example-tool").join("current.json");
    {
        // Replacing current.json needs DELETE on it or DELETE_CHILD on its directory. Everything
        // else (reading it, the lock file, staging, versions\) stays allowed, so the update runs
        // up to the commit point and fails exactly there.
        let _file = Deny::new(&record, "(DE)");
        let _dir = Deny::new(&sb.tool_dir("example-tool"), "(DC)");
        let (code, doc, _) = sb.json(&["update", "example-tool", "--json"]);
        assert_eq!((code, err_code(&doc)), (1, "permission_denied"), "{doc}");
    }
    assert_eq!(
        record_version(&sb).as_deref(),
        Some("1.0.0"),
        "1.0.0 stays active"
    );
    let versions = sb.tool_dir("example-tool").join("versions");
    assert!(versions.join("1.0.0").exists());
    assert!(
        !versions.join("2.0.0").exists(),
        "the half-activated version was removed"
    );
    assert!(
        !sb.tool_dir("example-tool")
            .join("current.json.tmp")
            .exists()
    );
    assert!(!sb.tool_dir("example-tool").join(".kim.lock").exists());
    assert!(staging_is_empty(&sb));
    let (code, _, _) = sb.json(&["update", "example-tool", "--json"]);
    assert_eq!(code, 0, "retry works once access is restored");
}

#[test]
#[cfg(windows)]
fn protected_settings_are_reported_and_left_unchanged() {
    let sb = Sandbox::new("perm-settings");
    let (code, _, _) = sb.json(&["settings", "set", "offline_mode", "true", "--json"]);
    assert_eq!(code, 0);
    let file = sb.settings_file();
    let before = std::fs::read(&file).unwrap();
    {
        // The directory is read-only: the new settings cannot be written.
        let _dir = Deny::new(file.parent().unwrap(), READ_ONLY_DIR);
        let _file = Deny::new(&file, "(DE)");
        let (code, doc, _) = sb.json(&["settings", "set", "offline_mode", "false", "--json"]);
        assert_eq!((code, err_code(&doc)), (1, "permission_denied"), "{doc}");
        assert_valid(&doc);
    }
    assert_eq!(std::fs::read(&file).unwrap(), before, "settings unchanged");
    assert!(!file.with_extension("json.tmp").exists());
    {
        // The file cannot even be read: Kim must not "change" a value it cannot see.
        let _file = Deny::new(&file, "(RD)");
        let (code, doc, _) = sb.json(&["settings", "set", "offline_mode", "false", "--json"]);
        assert_eq!((code, err_code(&doc)), (1, "permission_denied"), "{doc}");
        // Reading commands still work, with a warning instead of a crash.
        let (code, doc, _) = sb.json(&["settings", "show", "--json"]);
        assert_eq!(code, 0);
        assert!(!doc["warnings"].as_array().unwrap().is_empty());
    }
    assert_eq!(std::fs::read(&file).unwrap(), before, "settings unchanged");
}

// --- partial downloads -----------------------------------------------------------------------

#[test]
fn a_partial_download_is_refused_cleaned_up_and_retry_succeeds() {
    let (sb, srv, body, _) = with_tool("partial");
    // A crashed earlier operation left staging behind.
    let leftover = sb.programs().join(".staging").join("example-tool-999999-1");
    std::fs::create_dir_all(&leftover).unwrap();
    std::fs::write(leftover.join("package"), b"half a zip").unwrap();

    *srv.truncate_at.lock().unwrap() = Some(body.len() / 2);
    let (code, doc, _) = sb.json(&["install", "example-tool", "--json"]);
    assert_eq!(
        (code, err_code(&doc)),
        (6, "network_unavailable"),
        "a connection closed early is a network failure: {doc}"
    );
    assert_valid(&doc);
    assert!(record_version(&sb).is_none(), "nothing was activated");
    assert!(!sb.tool_dir("example-tool").join("versions").exists());
    assert!(!sb.tool_dir("example-tool").join(".kim.lock").exists());
    assert!(
        staging_is_empty(&sb),
        "partial download and old leftovers removed"
    );

    *srv.truncate_at.lock().unwrap() = None;
    let (code, doc, _) = sb.json(&["install", "example-tool", "--json"]);
    assert_eq!(code, 0, "{doc}");
    assert_eq!(record_version(&sb).as_deref(), Some("1.0.0"));
    assert!(staging_is_empty(&sb));
}

// --- offline ---------------------------------------------------------------------------------

#[test]
fn offline_mode_refuses_every_network_command_and_allows_reads_and_dry_runs() {
    let (sb, srv, _, _) = with_tool("offline");
    let source =
        file_url(&Path::new(env!("CARGO_MANIFEST_DIR")).join("../../catalog/catalog.json"));
    let (code, _, _) = sb.json(&["settings", "set", "offline_mode", "true", "--json"]);
    assert_eq!(code, 0);
    let hits = *srv.hits.lock().unwrap();
    let before = sb.snapshot();
    for args in [
        &["install", "example-tool"][..],
        &["update", "example-tool"],
        &["catalog", "refresh", "--source", &source],
        &["catalog", "refresh", "--source", &source, "--dry-run"],
        &["update", "check", "--refresh", "--source", &source],
    ] {
        let mut a = args.to_vec();
        a.push("--json");
        let (code, doc, _) = sb.json(&a);
        // `update example-tool` fails earlier: it is not installed.
        let expected = if args[0] == "update" && args[1] == "example-tool" {
            (3, "not_installed")
        } else {
            (6, "offline_mode")
        };
        assert_eq!((code, err_code(&doc)), expected, "{args:?}");
    }
    for args in [
        &["install", "example-tool", "--dry-run"][..],
        &["catalog", "list"],
        &["update", "check"],
        &["tools", "list"],
        &["status"],
        &["version"],
    ] {
        let mut a = args.to_vec();
        a.push("--json");
        let (code, doc, _) = sb.json(&a);
        assert_eq!(code, 0, "{args:?}: {doc}");
    }
    assert_eq!(
        *srv.hits.lock().unwrap(),
        hits,
        "no request reached the server"
    );
    assert_eq!(sb.snapshot(), before, "nothing was written while offline");
    let (_, doc, _) = sb.json(&["status", "--json"]);
    assert_eq!(doc["data"]["settings"]["offline_mode"], true);

    // Without offline mode, an unreachable source is `network_unavailable`, not a crash.
    let (code, _, _) = sb.json(&["settings", "set", "offline_mode", "false", "--json"]);
    assert_eq!(code, 0);
    let (code, doc, _) = sb.json(&[
        "catalog",
        "refresh",
        "--source",
        "http://127.0.0.1:9/catalog.json",
        "--json",
    ]);
    assert_eq!((code, err_code(&doc)), (6, "network_unavailable"));
}

// --- non-interactive execution ---------------------------------------------------------------

/// Runs kim and fails the test if it does not finish in time (a prompt would hang forever).
fn run_bounded(sb: &Sandbox, args: &[&str], env: &[(&str, &str)]) -> std::process::Output {
    let mut cmd = sb.cmd(args);
    for (k, v) in env {
        cmd.env(k, v);
    }
    let mut child = cmd
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .unwrap();
    let started = Instant::now();
    while child.try_wait().unwrap().is_none() {
        if started.elapsed() > Duration::from_secs(60) {
            let _ = child.kill();
            panic!("`kim {}` hung (waiting for input?)", args.join(" "));
        }
        std::thread::sleep(Duration::from_millis(20));
    }
    child.wait_with_output().unwrap()
}

#[test]
fn non_interactive_runs_never_prompt_or_hang() {
    let (sb, _srv, _, _) = with_tool("noninteractive");
    let (code, _, _) = sb.json(&["install", "example-tool", "--json"]);
    assert_eq!(code, 0);
    for (args, env) in [
        (&["uninstall", "example-tool"][..], &[][..]),
        (&["uninstall", "example-tool", "--no-input"], &[]),
        (&["uninstall", "example-tool"], &[("KIM_NO_INPUT", "1")]),
        (&["uninstall", "example-tool", "--purge-data"], &[]),
    ] {
        let out = run_bounded(&sb, args, env);
        assert_eq!(out.status.code(), Some(7), "{args:?} {env:?}");
        assert!(out.stdout.is_empty());
        let err = String::from_utf8_lossy(&out.stderr);
        assert!(
            err.contains("needs confirmation") && err.contains("--yes"),
            "{err}"
        );
        assert!(!err.contains("[y/N]"), "no prompt was shown: {err}");
        assert_eq!(
            record_version(&sb).as_deref(),
            Some("1.0.0"),
            "nothing removed"
        );
    }
    let out = run_bounded(&sb, &["uninstall", "example-tool", "--json"], &[]);
    let doc: Value = serde_json::from_slice(&out.stdout).unwrap();
    assert_eq!(err_code(&doc), "confirmation_required");
    // Explicit consent on the command line works without any terminal.
    let out = run_bounded(
        &sb,
        &["uninstall", "example-tool", "--yes", "--no-input"],
        &[],
    );
    assert_eq!(out.status.code(), Some(0));
    assert!(record_version(&sb).is_none());
}

// --- secrets ---------------------------------------------------------------------------------

#[test]
fn secrets_in_urls_never_reach_json_human_output_logs_or_state() {
    let secrets = ["s3cr3t-pw", "tok-123", "proxy-pw", "dl-sig-456"];
    let assert_clean = |what: &str, bytes: &[u8]| {
        let text = String::from_utf8_lossy(bytes);
        for s in secrets {
            assert!(!text.contains(s), "{what} leaks `{s}`:\n{text}");
        }
    };
    let mut sb = Sandbox::new("secrets");
    sb.extra_env.push((
        "HTTPS_PROXY".into(),
        "http://proxyuser:proxy-pw@127.0.0.1:9".into(),
    ));
    let source = "http://alice:s3cr3t-pw@127.0.0.1:9/catalog.json?token=tok-123";
    for verbose in [false, true] {
        let mut args = vec!["catalog", "refresh", "--source", source];
        if verbose {
            args.push("--verbose");
        }
        let human = sb.run(&args);
        assert_eq!(human.status.code(), Some(6));
        assert_clean("stderr", &human.stderr);
        assert_clean("stdout", &human.stdout);
        assert!(String::from_utf8_lossy(&human.stderr).contains("http://***@127.0.0.1:9"));
        args.push("--json");
        let (code, doc, out) = sb.json(&args);
        assert_eq!((code, err_code(&doc)), (6, "network_unavailable"));
        assert_clean("--json stdout", &out.stdout);
        assert_clean("--json stderr", &out.stderr);
        assert!(
            doc["error"]["message"]
                .as_str()
                .unwrap()
                .contains("token=***")
        );
    }
    let (code, doc, out) = sb.json(&[
        "catalog",
        "refresh",
        "--source",
        "https://127.0.0.1:9/catalog.json",
        "--json",
    ]);
    assert_eq!((code, err_code(&doc)), (6, "network_unavailable"));
    assert_clean("proxy failure", &out.stdout);

    // A signed download URL with a secret parameter: plan, progress and the stored install
    // record show it redacted; the download itself still uses the real URL.
    let sb = Sandbox::new("secrets-install");
    let srv = Server::start();
    let body = package(&[]);
    let url = format!("{}?sig=dl-sig-456", srv.put("/1.zip", body.clone()));
    sb.publish(100, &[release("1.0.0", &url, &body, "tool.exe")]);
    let (code, doc, out) = sb.json(&["install", "example-tool", "--dry-run", "--json"]);
    assert_eq!(code, 0, "{doc}");
    assert_clean("dry-run plan", &out.stdout);
    assert!(
        doc["data"]["plan"]["asset"]["url"]
            .as_str()
            .unwrap()
            .ends_with("?sig=***")
    );
    let out = sb.run(&["install", "example-tool", "--verbose"]);
    assert_eq!(
        out.status.code(),
        Some(0),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert_clean("install stdout", &out.stdout);
    assert_clean("install stderr", &out.stderr);
    let record = std::fs::read(sb.tool_dir("example-tool").join("current.json")).unwrap();
    assert_clean("install record", &record);
}

// --- streams, --quiet and --verbose -----------------------------------------------------------

#[test]
fn streams_quiet_and_verbose_follow_the_contract() {
    let sb = Sandbox::new("streams");
    // A corrupt cache produces a warning.
    let cache = sb.cache_dir();
    std::fs::create_dir_all(&cache).unwrap();
    std::fs::write(cache.join("catalog.json"), b"{").unwrap();
    std::fs::write(cache.join("catalog.json.sig"), b"{}").unwrap();

    let normal = sb.run(&["catalog", "list"]);
    assert!(normal.status.success());
    assert!(String::from_utf8_lossy(&normal.stdout).contains("TOOL"));
    assert!(String::from_utf8_lossy(&normal.stderr).starts_with("kim: warning: "));

    let quiet = sb.run(&["catalog", "list", "--quiet"]);
    assert!(quiet.stderr.is_empty(), "--quiet: nothing on stderr");
    assert_eq!(quiet.stdout, normal.stdout, "--quiet never changes results");

    let verbose = sb.run(&["catalog", "list", "-v"]);
    let diag = String::from_utf8_lossy(&verbose.stderr);
    assert!(
        diag.contains("kim: catalog.list: kim ") && diag.contains("finished in"),
        "{diag}"
    );
    assert_eq!(
        verbose.stdout, normal.stdout,
        "--verbose never changes stdout"
    );

    let (code, doc, out) = sb.json(&["catalog", "list", "--json", "--verbose"]);
    assert_eq!(code, 0);
    assert!(
        doc["warnings"][0]
            .as_str()
            .unwrap()
            .contains("ignoring the cached catalog")
    );
    assert!(
        !out.stderr.is_empty(),
        "--verbose diagnostics stay on stderr with --json"
    );

    let failed = sb.run(&["catalog", "show", "nope", "--verbose"]);
    assert_eq!(failed.status.code(), Some(3));
    assert!(failed.stdout.is_empty());
    let err = String::from_utf8_lossy(&failed.stderr);
    assert!(err.contains("kim: error: no tool `nope`"), "{err}");
    assert!(err.contains("hint: known tools:"), "{err}");
    assert!(
        err.contains("error code `tool_not_found` (exit code 3)"),
        "{err}"
    );

    // Bare `kim` shows help; `kim --json` without a command is a usage error document.
    let bare = sb.run(&[]);
    assert!(bare.status.success() && String::from_utf8_lossy(&bare.stdout).contains("Usage"));
    let (code, doc, _) = sb.json(&["--json"]);
    assert_eq!(
        (code, err_code(&doc), doc["command"].as_str()),
        (2, "usage", Some("kim"))
    );
}

// --- capability negotiation ------------------------------------------------------------------

#[test]
fn capability_negotiation_reports_absent_old_and_incompatible_tools() {
    let (sb, srv, v1, u1) = with_tool("negotiate");
    let describe = |extra: &[&str]| {
        let mut a = vec!["tools", "describe", "example-tool", "--json"];
        a.extend_from_slice(extra);
        let (code, doc, _) = sb.json(&a);
        assert_valid(&doc);
        (code, err_code(&doc).to_string(), doc)
    };
    assert_eq!(describe(&[]).1, "not_installed", "absent tool");
    let (code, _, _) = sb.json(&["install", "example-tool", "--json"]);
    assert_eq!(code, 0);

    let (code, _, doc) = describe(&["--require-command", "greet", "--require-version", "^1"]);
    assert_eq!(code, 0);
    assert_eq!(doc["data"]["capabilities"]["commands"][1]["name"], "greet");
    assert_eq!(
        describe(&["--require-command", "teleport"]).1,
        "capability_missing"
    );
    assert_eq!(
        describe(&["--require-version", ">=2"]).1,
        "tool_incompatible"
    );
    // The installed binary is a copy of kim, so it truthfully describes `kim`: refused.
    let (code, err, _) = describe(&["--probe"]);
    assert_eq!((code, err.as_str()), (4, "capabilities_invalid"));

    // A manifest from a newer CLI contract major is incompatible for invocation.
    let manifest_path = sb
        .tool_dir("example-tool")
        .join("versions")
        .join("1.0.0")
        .join("capabilities.json");
    let mut m: Value = serde_json::from_slice(&std::fs::read(&manifest_path).unwrap()).unwrap();
    m["cli_contract"] = 2.into();
    std::fs::write(&manifest_path, serde_json::to_vec(&m).unwrap()).unwrap();
    assert_eq!(
        describe(&["--require-command", "greet"]).1,
        "tool_incompatible"
    );
    // A broken manifest is reported, never trusted.
    std::fs::write(&manifest_path, b"{ broken").unwrap();
    assert_eq!(describe(&[]).1, "capabilities_invalid");
    let (_, list, _) = sb.json(&["tools", "list", "--json"]);
    assert_eq!(
        list["data"]["tools"][1]["capabilities"]["source"],
        "invalid"
    );
    assert!(!list["warnings"].as_array().unwrap().is_empty());

    // An update without a manifest: capabilities are unknown, so requirements cannot pass.
    let v2 = package(&[]);
    let u2 = srv.put("/2.zip", v2.clone());
    sb.publish(
        101,
        &[
            release("1.0.0", &u1, &v1, "tool.exe"),
            release("2.0.0", &u2, &v2, "tool.exe"),
        ],
    );
    let (code, _, _) = sb.json(&["update", "example-tool", "--json"]);
    assert_eq!(code, 0);
    let (code, _, doc) = describe(&[]);
    assert_eq!(
        (code, doc["data"]["capabilities_source"].as_str()),
        (0, Some("none"))
    );
    assert_eq!(
        describe(&["--require-command", "greet"]).1,
        "capabilities_unavailable"
    );

    // The program vanished: probing reports a damaged install instead of guessing.
    std::fs::remove_file(
        sb.tool_dir("example-tool")
            .join("versions")
            .join("2.0.0")
            .join("tool.exe"),
    )
    .unwrap();
    assert_eq!(describe(&["--probe"]).1, "not_installed");

    // Kim negotiates about itself without starting anything.
    let (code, doc, _) = sb.json(&[
        "tools",
        "describe",
        "kim",
        "--require-command",
        "install",
        "--require-command",
        "tools.describe",
        "--json",
    ]);
    assert_eq!(code, 0, "{doc}");
    let (_, doc, _) = sb.json(&[
        "tools",
        "describe",
        "kim",
        "--require-command",
        "nope",
        "--json",
    ]);
    assert_eq!(err_code(&doc), "capability_missing");
}

#[test]
#[cfg(windows)]
fn probing_a_tool_that_predates_the_contract_is_capabilities_unavailable() {
    // Simulate an old tool: its program does not know `capabilities --json` (whoami.exe answers
    // "invalid argument" and exits; no network, no window).
    let sb = Sandbox::new("old-tool");
    let dir = sb.tool_dir("old-tool");
    let vdir = dir.join("versions").join("1.0.0");
    std::fs::create_dir_all(&vdir).unwrap();
    let root = std::env::var("SystemRoot").unwrap_or_else(|_| r"C:\Windows".into());
    std::fs::copy(
        Path::new(&root).join("System32").join("whoami.exe"),
        vdir.join("old-tool.exe"),
    )
    .unwrap();
    std::fs::write(
        dir.join("current.json"),
        serde_json::to_vec(&serde_json::json!({
            "schema": "kim.install", "schema_version": 1, "tool": "old-tool", "version": "1.0.0",
            "entrypoint": "versions/1.0.0/old-tool.exe", "installed_at": "2026-11-01T00:00:00Z",
            "sha256": "00", "source_url": "https://example.invalid/old.zip", "catalog_version": 1,
            "previous_version": null
        }))
        .unwrap(),
    )
    .unwrap();
    let (code, doc, _) = sb.json(&["tools", "describe", "old-tool", "--probe", "--json"]);
    assert_eq!(
        (code, err_code(&doc)),
        (5, "capabilities_unavailable"),
        "{doc}"
    );
    assert_valid(&doc);
    let (code, doc, _) = sb.json(&["tools", "list", "--json"]);
    assert_eq!(code, 0);
    assert_eq!(doc["data"]["tools"][1]["capabilities"]["source"], "none");
}
