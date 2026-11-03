//! `kim hotkeys …` through the real binary (Step 46, docs/HOTKEY_CONTRACT.md §8–§10).
//! Requirements: KIM-FR-041, KIM-FR-043, KIM-FR-044, KIM-FR-045, KIM-FR-046, KIM-SEC-020,
//! KIM-SEC-023.
//!
//! Every `--json` document is validated against cli-envelope-v1, kim-cli-v1 and hotkeys-cli-v1
//! with the validator `cargo xtask schema-check` uses. Other installed tools are simulated with
//! install records and shipped `hotkeys.manifest.json` files (the contract's Noeul, Subak and
//! Ddalgak examples); none of them is ever started.

#[path = "../../../xtask/src/jsonschema.rs"]
mod jsonschema;

mod common;

use common::Sandbox;
use serde_json::{Value, json};
use std::path::{Path, PathBuf};

fn repo() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../..")
}

fn schema(name: &str) -> Value {
    serde_json::from_slice(
        &std::fs::read(repo().join(format!("docs/contracts/schemas/{name}.schema.json"))).unwrap(),
    )
    .unwrap()
}

struct Hk {
    sb: Sandbox,
    schemas: Vec<(&'static str, Value)>,
}

impl Hk {
    fn new(tag: &str) -> Hk {
        Hk {
            sb: Sandbox::new(&format!("hk-{tag}")),
            schemas: ["cli-envelope-v1", "kim-cli-v1", "hotkeys-cli-v1"]
                .iter()
                .map(|n| (*n, schema(n)))
                .collect(),
        }
    }

    /// `kim hotkeys <args> --json`: validated, exit code checked against the document.
    fn json(&self, args: &[&str]) -> (i32, Value) {
        let mut full = vec!["hotkeys"];
        full.extend_from_slice(args);
        full.push("--json");
        let (code, doc, out) = self.sb.json(&full);
        assert!(out.stderr.is_empty(), "{full:?} wrote to stderr");
        for (name, s) in &self.schemas {
            let errors = jsonschema::Validator::new(s).validate(&doc).unwrap();
            assert!(
                errors.is_empty(),
                "{full:?} violates {name}: {errors:?}\n{doc:#}"
            );
        }
        let want = if doc["ok"] == true {
            0
        } else {
            doc["error"]["exit_code"].as_i64().unwrap()
        };
        assert_eq!(i64::from(code), want, "{full:?}");
        (code, doc)
    }

    fn layer_file(&self) -> PathBuf {
        #[cfg(windows)]
        return self
            .sb
            .roaming()
            .join("mouv")
            .join("kim")
            .join("hotkeys.json");
        #[cfg(not(windows))]
        return self
            .sb
            .root
            .join("xdgc")
            .join("mouv")
            .join("kim")
            .join("hotkeys.json");
    }

    fn tool_layer(&self, tool: &str) -> PathBuf {
        self.layer_file()
            .parent()
            .unwrap()
            .parent()
            .unwrap()
            .join(tool)
            .join("hotkeys.json")
    }

    /// Simulates an installed tool that ships a hotkey manifest (Kim never starts it).
    fn install_fake(&self, tool: &str, manifest: Option<&str>) {
        let version = "0.1.0";
        let dir = self.sb.tool_dir(tool);
        let vdir = dir.join("versions").join(version);
        std::fs::create_dir_all(&vdir).unwrap();
        let record = json!({
            "schema": "kim.install", "schema_version": 1, "tool": tool, "version": version,
            "entrypoint": format!("versions/{version}/{tool}.exe"),
            "installed_at": "2026-11-04T00:00:00Z", "sha256": "00",
            "source_url": "https://example.invalid/x.zip", "catalog_version": 1,
            "previous_version": null
        });
        std::fs::write(dir.join("current.json"), record.to_string()).unwrap();
        // The "program" is not executable: starting it would fail loudly.
        std::fs::write(vdir.join(format!("{tool}.exe")), b"not a program").unwrap();
        if let Some(m) = manifest {
            let src = repo().join(format!(
                "docs/contracts/examples/kim-hotkey-manifest-v1/{m}.json"
            ));
            std::fs::copy(src, vdir.join("hotkeys.manifest.json")).unwrap();
        }
    }
}

#[test]
fn list_change_disable_and_restore_without_reinstalling() {
    let h = Hk::new("cycle");
    let (_, doc) = h.json(&["list"]);
    assert_eq!(doc["data"]["config"]["state"], "default");
    let ids: Vec<&str> = doc["data"]["actions"]
        .as_array()
        .unwrap()
        .iter()
        .map(|a| a["id"].as_str().unwrap())
        .collect();
    for id in [
        "catalog.refresh",
        "help.show",
        "tool.install",
        "tool.update",
        "tool.uninstall",
    ] {
        assert!(ids.contains(&id), "{id}");
    }
    assert!(
        ids.contains(&"nav.next-control") && ids.contains(&"window.close"),
        "reserved keys are listed"
    );

    // A dry run checks and shows the change, and writes nothing.
    let (_, doc) = h.json(&["set", "catalog.refresh", "ctrl+r", "--dry-run"]);
    assert_eq!(
        (
            doc["data"]["dry_run"].clone(),
            doc["data"]["written"].clone()
        ),
        (json!(true), json!(false))
    );
    assert_eq!(
        doc["data"]["changes"][0]["after"]["bindings"],
        json!(["Ctrl+R"]),
        "aliases are canonicalized"
    );
    assert!(!h.layer_file().exists());

    // Change, disable, restore.
    let (_, doc) = h.json(&["set", "catalog.refresh", "Ctrl+R"]);
    assert_eq!(doc["data"]["written"], true);
    let (_, doc) = h.json(&["disable", "tool.uninstall"]);
    assert_eq!(doc["data"]["written"], true);
    let (_, doc) = h.json(&["show", "catalog.refresh"]);
    assert_eq!(doc["data"]["action"]["source"], "user");
    assert_eq!(doc["data"]["action"]["bindings"][0]["keys"], "Ctrl+R");
    let (_, doc) = h.json(&["reset", "catalog.refresh"]);
    assert_eq!(
        doc["data"]["changes"][0]["after"],
        json!({"source": "default", "bindings": ["F5"]})
    );
    let (_, doc) = h.json(&["export"]);
    assert_eq!(
        doc["data"]["document"]["overrides"],
        json!([{"action": "tool.uninstall", "disabled": true}])
    );

    // The tool-wide switch and scopes.
    let (_, doc) = h.json(&["disable", "--all"]);
    assert!(doc["data"]["changes"].as_array().unwrap().len() >= 2);
    let (_, doc) = h.json(&["list"]);
    assert_eq!(doc["data"]["enabled"], false);
    assert!(doc["data"]["actions"][0]["bindings"][0]["active"] == false);
    h.json(&["enable", "--all"]);
    let (_, doc) = h.json(&["disable", "--scope", "app"]);
    assert_eq!(doc["data"]["written"], true);
    let (_, doc) = h.json(&["list"]);
    assert_eq!(doc["data"]["disabled_scopes"], json!(["app"]));
    h.json(&["enable", "--scope", "app"]);

    // reset --all needs --yes when Kim cannot ask, keeps a backup, and restores every default.
    let before = std::fs::read(h.layer_file()).unwrap();
    let (code, doc) = h.json(&["reset", "--all"]);
    assert_eq!(
        (code, doc["error"]["code"].as_str()),
        (7, Some("confirmation_required"))
    );
    assert_eq!(std::fs::read(h.layer_file()).unwrap(), before);
    let (_, doc) = h.json(&["reset", "--all", "--yes"]);
    assert_eq!(
        doc["warnings"],
        json!(["the previous file was kept as hotkeys.json.bak"])
    );
    let backup = h.layer_file().with_extension("json.bak");
    assert_eq!(std::fs::read(backup).unwrap(), before);
    let (_, doc) = h.json(&["list"]);
    assert_eq!(
        doc["data"]["actions"][4]["source"], "unbound",
        "tool.uninstall is back to its default"
    );

    // import replaces the user layer and keeps a backup.
    let file = h.sb.root.join("mine.json");
    std::fs::write(
        &file,
        json!({"schema": "kim.hotkeys", "schema_version": 1, "tool": "kim",
               "overrides": [{"action": "help.show", "bindings": ["Shift+F1"]}]})
        .to_string(),
    )
    .unwrap();
    let (_, doc) = h.json(&["import", file.to_str().unwrap()]);
    assert_eq!(doc["data"]["changes"][0]["action"], "help.show");
    std::fs::write(&file, b"{\"schema\": \"kim.hotkeys\"}").unwrap();
    let (code, doc) = h.json(&["import", file.to_str().unwrap()]);
    assert_eq!(
        (code, doc["error"]["code"].as_str()),
        (4, Some("hotkeys_invalid"))
    );
}

#[test]
fn refused_changes_never_touch_working_bindings() {
    let h = Hk::new("refuse");
    h.json(&["set", "tool.install", "Ctrl+I"]);
    let before = std::fs::read(h.layer_file()).unwrap();
    for (args, code, err) in [
        (&["set", "help.show", "Win+L"][..], 2, "hotkey_reserved"),
        (
            &["set", "help.show", "Ctrl+Alt+Delete"],
            2,
            "hotkey_reserved",
        ),
        (&["set", "help.show", "F6"], 2, "hotkey_reserved"),
        (&["set", "help.show", "Hyper+K"], 2, "hotkey_invalid"),
        (
            &["set", "help.show", "F3", "--scope", "global"],
            2,
            "hotkey_invalid",
        ),
        (&["set", "nope.action", "F2"], 3, "action_not_found"),
        (&["set", "tool.update", "Ctrl+I"], 7, "hotkey_conflict"),
    ] {
        let (c, doc) = h.json(args);
        assert_eq!(
            (c, doc["error"]["code"].as_str()),
            (code, Some(err)),
            "{args:?}"
        );
        assert_eq!(
            std::fs::read(h.layer_file()).unwrap(),
            before,
            "{args:?} wrote"
        );
    }
    // The refusal is actionable: what conflicts, and how to resolve it.
    let (_, doc) = h.json(&["set", "tool.update", "Ctrl+I"]);
    assert!(
        doc["error"]["details"][0]
            .as_str()
            .unwrap()
            .starts_with("duplicate_binding")
    );
    assert!(doc["error"]["hint"].as_str().unwrap().contains("--replace"));
    // Human output: error and hint on stderr, nothing on stdout.
    let out = h.sb.run(&["hotkeys", "set", "tool.update", "Ctrl+I"]);
    assert!(out.stdout.is_empty());
    let err = String::from_utf8_lossy(&out.stderr);
    assert!(
        err.contains("kim: error: Ctrl+I conflicts with another binding") && err.contains("hint:"),
        "{err}"
    );
    // An explicit --replace resolves it and lists both actions.
    let (_, doc) = h.json(&["set", "tool.update", "Ctrl+I", "--replace"]);
    let changed: Vec<&str> = doc["data"]["changes"]
        .as_array()
        .unwrap()
        .iter()
        .map(|c| c["action"].as_str().unwrap())
        .collect();
    assert_eq!(changed, ["tool.update", "tool.install"]);
}

#[test]
fn broken_and_newer_files_are_never_overwritten_implicitly() {
    let h = Hk::new("safe");
    std::fs::create_dir_all(h.layer_file().parent().unwrap()).unwrap();
    std::fs::write(h.layer_file(), b"{ broken").unwrap();
    let (_, doc) = h.json(&["list"]);
    assert_eq!(doc["data"]["config"]["state"], "invalid");
    assert!(doc["warnings"][0].as_str().unwrap().contains("is invalid"));
    assert_eq!(
        doc["data"]["actions"][0]["bindings"][0]["keys"], "F5",
        "defaults apply"
    );
    let (code, doc) = h.json(&["set", "help.show", "F2"]);
    assert_eq!(
        (code, doc["error"]["code"].as_str()),
        (4, Some("hotkeys_invalid"))
    );
    assert_eq!(std::fs::read(h.layer_file()).unwrap(), b"{ broken");
    // The documented recovery keeps the broken file as the backup.
    h.json(&["reset", "--all", "--yes"]);
    assert_eq!(
        std::fs::read(h.layer_file().with_extension("json.bak")).unwrap(),
        b"{ broken"
    );
    let (_, doc) = h.json(&["list"]);
    assert_eq!(doc["data"]["config"]["state"], "user");

    let newer = json!({"schema": "kim.hotkeys", "schema_version": 2, "tool": "kim"}).to_string();
    std::fs::write(h.layer_file(), &newer).unwrap();
    let (_, doc) = h.json(&["list"]);
    assert_eq!(doc["data"]["config"]["state"], "newer_version");
    for args in [
        &["set", "help.show", "F2"][..],
        &["reset", "--all", "--yes"],
        &["disable", "--all"],
    ] {
        let (code, doc) = h.json(args);
        assert_eq!(
            (code, doc["error"]["code"].as_str()),
            (5, Some("hotkeys_from_newer_version")),
            "{args:?}"
        );
    }
    assert_eq!(std::fs::read_to_string(h.layer_file()).unwrap(), newer);
}

#[test]
fn all_tools_check_reads_files_only_and_reports_cross_tool_conflicts() {
    let h = Hk::new("all");
    h.install_fake("noeul", Some("noeul"));
    h.install_fake("subak", Some("subak"));
    h.install_fake("ddalgak", Some("ddalgak"));
    h.install_fake("plain", None);
    // The user bound Ddalgak's engine toggle to F3, which Noeul uses globally by default.
    let dl = h.tool_layer("ddalgak");
    std::fs::create_dir_all(dl.parent().unwrap()).unwrap();
    std::fs::write(
        &dl,
        json!({"schema": "kim.hotkeys", "schema_version": 1, "tool": "ddalgak",
               "overrides": [{"action": "engine.toggle", "bindings": ["F3"]}]})
        .to_string(),
    )
    .unwrap();
    let snapshot = h.sb.snapshot();
    let (code, doc) = h.json(&["check", "--all-tools"]);
    assert_eq!(code, 0, "a successful check exits 0 even with conflicts");
    assert_eq!(h.sb.snapshot(), snapshot, "the check writes nothing");
    let data = &doc["data"];
    let tools: Vec<(&str, &str)> = data["tools"]
        .as_array()
        .unwrap()
        .iter()
        .map(|t| (t["tool"].as_str().unwrap(), t["source"].as_str().unwrap()))
        .collect();
    assert_eq!(
        tools,
        [
            ("ddalgak", "manifest"),
            ("kim", "self"),
            ("noeul", "manifest"),
            ("subak", "manifest")
        ]
    );
    assert!(
        doc["warnings"][0]
            .as_str()
            .unwrap()
            .contains("plain 0.1.0 ships no hotkey manifest")
    );
    let find = |code: &str, tool: &str, action: &str| {
        data["conflicts"]
            .as_array()
            .unwrap()
            .iter()
            .find(|c| c["code"] == code && c["tool"] == tool && c["action"] == action)
            .cloned()
    };
    // Cross-Kim collision: an actionable error with remap offers; nobody's file changed.
    let e = find("cross_tool_global", "ddalgak", "engine.toggle").expect("cross-tool error");
    assert_eq!(e["severity"], "error");
    assert_eq!(
        e["with"],
        json!({"tool": "noeul", "action": "paste.float", "keys": "F3", "scope": "global"})
    );
    assert_eq!(
        e["offers"][0],
        json!({"kind": "rebind", "keys": "Shift+F3"})
    );
    // Several tools want F1: Noeul's global F1 shadows Kim Manager's and Subak's help.
    assert!(find("global_shadows_app", "kim", "help.show").is_some());
    assert!(find("global_shadows_app", "subak", "help.show").is_some());
    assert!(data["summary"]["errors"].as_u64().unwrap() >= 1);
    // Human output names the exact fix for the tool that owns the binding.
    let out = h.sb.run(&["hotkeys", "check", "--all-tools"]);
    let text = String::from_utf8_lossy(&out.stdout);
    assert!(
        text.contains("`ddalgak hotkeys set engine.toggle Shift+F3`"),
        "{text}"
    );
    // --min-severity filters the report.
    let (_, doc) = h.json(&["check", "--all-tools", "--min-severity", "error"]);
    assert!(
        doc["data"]["conflicts"]
            .as_array()
            .unwrap()
            .iter()
            .all(|c| c["severity"] == "error")
    );
    // A Kim change shows cross-tool warnings but is not blocked by them.
    let (_, doc) = h.json(&["set", "catalog.refresh", "F3"]);
    assert!(
        doc["data"]["conflicts"]
            .as_array()
            .unwrap()
            .iter()
            .any(|c| c["code"] == "global_shadows_app")
    );
    // A manifest that names another tool or version is not trusted.
    let bad =
        h.sb.tool_dir("subak")
            .join("versions/0.1.0/hotkeys.manifest.json");
    let text = std::fs::read_to_string(&bad)
        .unwrap()
        .replace("\"0.1.0\"", "\"9.9.9\"");
    std::fs::write(&bad, text).unwrap();
    let (_, doc) = h.json(&["check", "--all-tools"]);
    assert!(
        doc["warnings"]
            .as_array()
            .unwrap()
            .iter()
            .any(|w| w.as_str().unwrap().contains("not trusted"))
    );
}
