//! CLI contract regression tests (Step 07, docs/CLI_CONTRACT.md, KIM-FR-010, KIM-FR-011,
//! KIM-NFR-014).
//!
//! Runs the real `kim` binary through every command, success and failure, and checks each
//! `--json` document against docs/contracts/schemas/cli-envelope-v1 and kim-cli-v1 with the
//! same validator `cargo xtask schema-check` uses. Stable outputs are compared with golden files
//! in docs/contracts/examples/kim-cli-v1/ (after replacing machine-specific paths); refresh them
//! with `KIM_BLESS=1 cargo test -p kim --test cli_contract` and review the diff. The documented
//! error-code registry, the schema and the Rust enum must agree.

#[path = "../../../xtask/src/jsonschema.rs"]
mod jsonschema;

mod common;

use common::{Sandbox, Server, file_url, manifest, package, release};
use kim::error::ErrorCode;
use serde_json::Value;
use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

fn repo() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../..")
}

fn schema(name: &str) -> Value {
    let p = repo().join(format!("docs/contracts/schemas/{name}.schema.json"));
    serde_json::from_slice(&std::fs::read(&p).unwrap()).unwrap()
}

/// Validates one document against both CLI schemas and the exit-code rule.
struct Checker {
    envelope: Value,
    kim: Value,
    commands: BTreeSet<String>,
    error_codes: BTreeSet<String>,
}

impl Checker {
    fn new() -> Self {
        Self {
            envelope: schema("cli-envelope-v1"),
            kim: schema("kim-cli-v1"),
            commands: BTreeSet::new(),
            error_codes: BTreeSet::new(),
        }
    }

    fn check(&mut self, args: &[&str], code: i32, doc: &Value) {
        for (name, s) in [
            ("cli-envelope-v1", &self.envelope),
            ("kim-cli-v1", &self.kim),
        ] {
            let errors = jsonschema::Validator::new(s)
                .validate(doc)
                .expect("schema uses supported keywords");
            assert!(
                errors.is_empty(),
                "`kim {}` output violates {name}:\n{}\n{doc:#}",
                args.join(" "),
                errors.join("\n")
            );
        }
        if doc["ok"] == true {
            assert_eq!(
                code,
                0,
                "`kim {}` succeeded with exit {code}",
                args.join(" ")
            );
            self.commands
                .insert(doc["command"].as_str().unwrap().to_string());
        } else {
            assert_eq!(
                i64::from(code),
                doc["error"]["exit_code"].as_i64().unwrap(),
                "`kim {}`: process exit code must equal error.exit_code",
                args.join(" ")
            );
            self.error_codes
                .insert(doc["error"]["code"].as_str().unwrap().to_string());
        }
    }

    /// Runs `kim <args> --json`, validates, and returns (exit code, document).
    fn run(&mut self, sb: &Sandbox, args: &[&str]) -> (i32, Value) {
        let mut full = args.to_vec();
        full.push("--json");
        let (code, doc, out) = sb.json(&full);
        assert!(
            out.stderr.is_empty(),
            "`kim {}` with --json wrote to stderr: {}",
            full.join(" "),
            String::from_utf8_lossy(&out.stderr)
        );
        self.check(&full, code, &doc);
        (code, doc)
    }
}

/// Replaces machine-specific values so outputs can be compared with committed golden files:
/// paths, the loopback port, the Windows build, and the hash/size of test packages (they
/// contain the freshly built kim.exe).
fn normalize(v: &mut Value, sb: &Sandbox, port: Option<u16>) {
    // The same executable can be spelled with `/` or `\` and with a `\\?\` prefix.
    let exe = env!("CARGO_BIN_EXE_kim");
    let canonical = std::fs::canonicalize(exe)
        .map(|p| p.display().to_string())
        .unwrap_or_default();
    let mut exes: Vec<String> = vec![
        exe.to_string(),
        exe.replace('/', "\\"),
        canonical.clone(),
        canonical.trim_start_matches(r"\\?\").to_string(),
    ];
    exes.retain(|e| !e.is_empty());
    exes.sort_by_key(|e| std::cmp::Reverse(e.len()));
    let root = sb.root.display().to_string();
    fn walk(v: &mut Value, f: &dyn Fn(&str, &mut Value)) {
        match v {
            Value::Object(map) => {
                for (k, child) in map.iter_mut() {
                    f(k, child);
                    walk(child, f);
                }
            }
            Value::Array(items) => items.iter_mut().for_each(|c| walk(c, f)),
            _ => {}
        }
    }
    let port = port.map(|p| format!("127.0.0.1:{p}"));
    walk(v, &|key, child| {
        if key == "os_build" && child.is_number() {
            *child = 19045.into();
        }
        if port.is_some() && key == "sha256" {
            *child = "5f0c8e8a0d6b3e1f9a7c2b4d6e8f0a1c3e5b7d9f1a3c5e7b9d1f3a5c7e9b1d3f".into();
        }
        if port.is_some() && key == "size" {
            *child = 4_194_304.into();
        }
        if let Value::String(s) = child {
            let mut t = s.clone();
            for e in &exes {
                t = t.replace(e.as_str(), r"C:\Users\me\bin\kim.exe");
            }
            if let Some(p) = &port {
                t = t.replace(p.as_str(), "127.0.0.1:8080");
            }
            *s = t.replace(&root, r"C:\Users\me\AppData");
        }
    });
}

fn golden(name: &str, actual: &Value) {
    if !cfg!(windows) {
        // Golden files hold Windows-shaped paths; elsewhere only the schemas are checked.
        return;
    }
    let path = repo().join(format!("docs/contracts/examples/kim-cli-v1/{name}.json"));
    let rendered = format!("{}\n", serde_json::to_string_pretty(actual).unwrap());
    if std::env::var_os("KIM_BLESS").is_some() {
        std::fs::write(&path, &rendered).unwrap();
        return;
    }
    let expected: Value = std::fs::read(&path)
        .ok()
        .and_then(|b| serde_json::from_slice(&b).ok())
        .unwrap_or(Value::Null);
    assert_eq!(
        &expected,
        actual,
        "{} differs from the real output. If the change is intended, run \
         `KIM_BLESS=1 cargo test -p kim --test cli_contract`, review the diff, and note the \
         change (additive vs breaking, docs/CLI_CONTRACT.md §8).\nactual:\n{rendered}",
        path.display()
    );
}

#[test]
fn every_command_matches_the_schemas_and_stable_outputs_match_golden_files() {
    let mut ck = Checker::new();

    // --- a fresh machine: bundled catalog only, nothing installed -----------------------------
    let sb = Sandbox::new("contract-fresh");
    let before = sb.snapshot();
    for (args, gold) in [
        (&["capabilities"][..], Some("capabilities")),
        (&["version"], Some("version")),
        (&["status"], Some("status")),
        (&["tools", "list"], Some("tools-list")),
        (&["tools", "describe", "kim"], None),
        (&["catalog", "list"], Some("catalog-list")),
        (&["catalog", "list", "--installed"], None),
        (&["catalog", "show", "subak"], Some("catalog-show")),
        (&["catalog", "status"], Some("catalog-status")),
        (&["settings", "show"], Some("settings-show")),
        (
            &["settings", "set", "offline_mode", "true", "--dry-run"],
            Some("settings-set-dry-run"),
        ),
        (&["update", "check"], Some("update-check")),
        (&["catalog", "frobnicate"], Some("error-usage")),
        (&["tools", "describe", "subak"], Some("error-not-installed")),
        (&["install", "nope"], None),
        (&["uninstall", "nope", "--yes"], None),
    ] {
        let (_, mut doc) = ck.run(&sb, args);
        if let Some(name) = gold {
            normalize(&mut doc, &sb, None);
            golden(name, &doc);
        }
    }
    assert_eq!(
        sb.snapshot(),
        before,
        "read-only commands and dry runs must not write anything"
    );
    let catalog = repo().join("catalog/catalog.json");
    let catalog = catalog.to_str().unwrap();
    ck.run(&sb, &["catalog", "verify", catalog]);
    let url = file_url(&repo().join("catalog/catalog.json"));
    let (_, doc) = ck.run(&sb, &["catalog", "refresh", "--source", &url, "--dry-run"]);
    assert_eq!(doc["data"]["dry_run"], true);
    assert!(
        !sb.cache_dir().exists(),
        "refresh --dry-run writes no cache"
    );
    ck.run(
        &sb,
        &[
            "update",
            "check",
            "--refresh",
            "--source",
            &url,
            "--dry-run",
        ],
    );
    assert!(!sb.cache_dir().exists());
    ck.run(&sb, &["catalog", "refresh", "--source", &url]);
    assert!(sb.cache_dir().join("catalog.json").exists());
    ck.run(&sb, &["settings", "set", "offline_mode", "false"]);

    // --- a machine with an installed tool (test-signed catalog, loopback downloads) -----------
    let sb = Sandbox::new("contract-tool");
    let srv = Server::start();
    let v1 = package(&[(
        "capabilities.json",
        &manifest("example-tool", "1.0.0", &["capabilities", "greet"]),
    )]);
    let u1 = srv.put("/1.zip", v1.clone());
    sb.publish(100, &[release("1.0.0", &u1, &v1, "tool.exe")]);
    let gold = |ck: &mut Checker, args: &[&str], name: Option<&str>| {
        let (code, mut doc) = ck.run(&sb, args);
        if let Some(name) = name {
            let mut normalized = doc.clone();
            normalize(&mut normalized, &sb, Some(srv.port));
            golden(name, &normalized);
        }
        doc["exit"] = code.into();
        doc
    };
    let doc = gold(
        &mut ck,
        &["install", "example-tool", "--dry-run"],
        Some("install-dry-run"),
    );
    assert_eq!(
        (&doc["exit"], &doc["data"]["result"]),
        (&0.into(), &Value::Null)
    );
    assert!(!sb.tool_dir("example-tool").exists());
    let doc = gold(&mut ck, &["install", "example-tool"], Some("install"));
    assert_eq!(doc["exit"], 0);
    let doc = gold(
        &mut ck,
        &["install", "example-tool"],
        Some("error-already-installed"),
    );
    assert_eq!(
        (&doc["exit"], doc["error"]["code"].as_str()),
        (&7.into(), Some("already_installed"))
    );
    let doc = gold(&mut ck, &["tools", "list"], Some("tools-list-installed"));
    let entry = &doc["data"]["tools"][1];
    assert_eq!(entry["id"], "example-tool");
    assert_eq!(entry["capabilities"]["source"], "manifest");
    assert_eq!(entry["capabilities"]["commands"], 2);
    let doc = gold(
        &mut ck,
        &[
            "tools",
            "describe",
            "example-tool",
            "--require-command",
            "greet",
            "--require-version",
            ">=1.0",
        ],
        Some("tools-describe"),
    );
    assert_eq!(doc["data"]["requirements"]["satisfied"], true);
    assert_eq!(doc["data"]["capabilities_source"], "manifest");
    gold(&mut ck, &["catalog", "list", "--installed"], None);
    gold(&mut ck, &["status"], None);

    let v2 = package(&[]);
    let u2 = srv.put("/2.zip", v2.clone());
    sb.publish(
        101,
        &[
            release("1.0.0", &u1, &v1, "tool.exe"),
            release("2.0.0", &u2, &v2, "tool.exe"),
        ],
    );
    let doc = gold(
        &mut ck,
        &["update", "check"],
        Some("update-check-available"),
    );
    assert_eq!(doc["data"]["updates"][0]["available"], "2.0.0");
    let doc = gold(&mut ck, &["update", "example-tool", "--dry-run"], None);
    assert_eq!(doc["data"]["plan"]["action"], "update");
    let doc = gold(&mut ck, &["update", "example-tool"], Some("update"));
    assert_eq!(doc["exit"], 0);
    gold(
        &mut ck,
        &["uninstall", "example-tool", "--dry-run"],
        Some("uninstall-dry-run"),
    );
    let doc = gold(
        &mut ck,
        &["uninstall", "example-tool"],
        Some("error-confirmation-required"),
    );
    assert_eq!(doc["exit"], 7, "no --yes when not interactive");
    let doc = gold(
        &mut ck,
        &["uninstall", "example-tool", "--yes"],
        Some("uninstall"),
    );
    assert_eq!(doc["exit"], 0);

    // Every command of `kim capabilities` produced at least one schema-valid success document.
    let (_, caps, _) = Sandbox::new("contract-caps").json(&["capabilities", "--json"]);
    let advertised: BTreeSet<String> = caps["data"]["commands"]
        .as_array()
        .unwrap()
        .iter()
        .map(|c| c["name"].as_str().unwrap().to_string())
        .collect();
    assert_eq!(
        ck.commands, advertised,
        "every advertised command needs a validated success example"
    );
    for code in [
        "usage",
        "not_installed",
        "tool_not_found",
        "already_installed",
    ] {
        assert!(ck.error_codes.contains(code), "{code} was not exercised");
    }
}

#[test]
fn committed_examples_and_schemas_agree_with_the_code() {
    // Error registry: Rust enum == schema enum == schema exit-code mapping.
    let s = schema("kim-cli-v1");
    let enum_codes: BTreeSet<&str> = s["$defs"]["error_code"]["enum"]
        .as_array()
        .unwrap()
        .iter()
        .map(|v| v.as_str().unwrap())
        .collect();
    let rust_codes: BTreeSet<&str> = ErrorCode::ALL.iter().map(|c| c.as_str()).collect();
    assert_eq!(enum_codes, rust_codes, "kim-cli-v1 error_code enum");
    for alt in s["$defs"]["error"]["oneOf"].as_array().unwrap() {
        let exit = alt["properties"]["exit_code"]["const"].as_u64().unwrap();
        for c in alt["properties"]["code"]["enum"].as_array().unwrap() {
            let code = ErrorCode::ALL
                .iter()
                .find(|e| e.as_str() == c.as_str().unwrap())
                .unwrap();
            assert_eq!(u64::from(code.exit_code()), exit, "{c} exit code in schema");
        }
    }

    // The capabilities definitions embedded in kim-cli-v1 equal tool-capabilities-v1.
    let caps = schema("tool-capabilities-v1");
    for (name, def) in caps["$defs"].as_object().unwrap() {
        assert_eq!(&s["$defs"][name], def, "kim-cli-v1 $defs.{name}");
    }
    let mut root = caps;
    for k in ["$schema", "$id", "title", "description", "$defs"] {
        root.as_object_mut().unwrap().remove(k);
    }
    assert_eq!(
        s["$defs"]["capabilities"], root,
        "kim-cli-v1 $defs.capabilities"
    );
}

#[test]
fn the_documented_error_registry_matches_the_code() {
    // docs/CLI_CONTRACT.md §6 has one table row per code: | `code` | exit | meaning |
    let doc = std::fs::read_to_string(repo().join("docs/CLI_CONTRACT.md")).unwrap();
    let mut documented = BTreeSet::new();
    for line in doc.lines() {
        let cells: Vec<&str> = line
            .trim()
            .trim_matches('|')
            .split('|')
            .map(str::trim)
            .collect();
        if cells.len() >= 3
            && let Some(code) = cells[0].strip_prefix('`').and_then(|c| c.strip_suffix('`'))
            && let Ok(exit) = cells[1].parse::<u8>()
            && let Some(e) = ErrorCode::ALL.iter().find(|e| e.as_str() == code)
        {
            assert_eq!(
                e.exit_code(),
                exit,
                "CLI_CONTRACT.md: exit code of `{code}`"
            );
            documented.insert(code.to_string());
        }
    }
    let all: BTreeSet<String> = ErrorCode::ALL
        .iter()
        .map(|c| c.as_str().to_string())
        .collect();
    assert_eq!(
        documented, all,
        "every error code is documented exactly once in §6"
    );
}

#[test]
fn kim_capabilities_match_the_published_tool_capabilities_example() {
    let sb = Sandbox::new("contract-kimcaps");
    let (code, doc, _) = sb.json(&["capabilities", "--json"]);
    assert_eq!(code, 0);
    let example: Value = serde_json::from_slice(
        &std::fs::read(repo().join("docs/contracts/examples/tool-capabilities-v1/kim.json"))
            .unwrap(),
    )
    .unwrap();
    assert_eq!(
        doc["data"], example,
        "refresh docs/contracts/examples/tool-capabilities-v1/kim.json from `kim capabilities --json`"
    );
    let errors = jsonschema::Validator::new(&schema("tool-capabilities-v1"))
        .validate(&doc["data"])
        .unwrap();
    assert!(errors.is_empty(), "{errors:?}");
}

#[test]
fn kim_source_spawns_processes_only_in_the_documented_places() {
    // Inspection test for KIM-FR-012: the headless binary never starts a GUI. The only process
    // spawns in crates/kim/src are the install self-check (`install.rs`) and the opt-in
    // capabilities probe (`capabilities.rs`); neither can name a GUI program.
    let src = Path::new(env!("CARGO_MANIFEST_DIR")).join("src");
    let mut spawn_sites = Vec::new();
    let mut stack = vec![src.clone()];
    while let Some(dir) = stack.pop() {
        for e in std::fs::read_dir(&dir).unwrap().flatten() {
            let p = e.path();
            if p.is_dir() {
                stack.push(p);
                continue;
            }
            let text = std::fs::read_to_string(&p).unwrap();
            let rel = p
                .strip_prefix(&src)
                .unwrap()
                .display()
                .to_string()
                .replace('\\', "/");
            // Strip `#[cfg(test)]` modules: tests may start helper processes.
            let code = text.split("#[cfg(test)]").next().unwrap_or("");
            for needle in ["Command::new(", "ShellExecute", "CreateProcess", "WinExec"] {
                if code.contains(needle) {
                    spawn_sites.push(format!("{rel}: {needle}"));
                }
            }
            for gui in [
                "kim-manager",
                "subak-gui",
                "explorer.exe",
                "cmd /c start",
                "rundll32",
            ] {
                assert!(
                    !code.contains(gui),
                    "{rel} mentions `{gui}`: headless kim must never start a GUI"
                );
            }
        }
    }
    spawn_sites.sort();
    assert_eq!(
        spawn_sites,
        [
            "capabilities.rs: Command::new(",
            "install.rs: Command::new("
        ],
        "a new process spawn in kim needs review against KIM-FR-012 and an update of this list"
    );
}
