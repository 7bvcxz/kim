//! E2E — Step 46: shared hotkey configuration and conflict detection (docs/HOTKEY_CONTRACT.md,
//! docs/KIM_CLI.md §5). Requirements: KIM-FR-041…046, KIM-SEC-020, KIM-SEC-023.
//!
//! Scenario A (release `kim`, isolated profile): a user lists Kim Manager's shortcuts, changes
//! one, sees a same-tool collision refused without any write, resolves it explicitly, turns all
//! shortcuts off and on, and restores the defaults with a backup — no reinstall, no window, every
//! `--json` document valid against cli-envelope-v1, kim-cli-v1 and hotkeys-cli-v1.
//!
//! Scenario B (release `kim`, simulated installs of Noeul and Ddalgak with shipped hotkey
//! manifests): `kim hotkeys check --all-tools` reports a cross-tool global collision as an
//! actionable error, reads files only (the "programs" are not executable), and changes nothing.
//!
//! Scenario C: a broken `hotkeys.json` puts Kim in safe mode; it is never overwritten until the
//! user runs the documented recovery, which keeps a backup.
//!
//! The machine is in use while e2e runs: only obscure chords (`Ctrl+Alt+Shift+F23/F24`) are used
//! as new bindings, and `kim` never registers a hotkey anyway.

#[path = "../../xtask/src/jsonschema.rs"]
mod jsonschema;

use kim_e2e::{TempDir, bin, run_sandboxed};
use serde_json::{Value, json};
use std::path::{Path, PathBuf};

fn schema(name: &str) -> Value {
    let p = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("..")
        .join("docs/contracts/schemas")
        .join(format!("{name}.schema.json"));
    serde_json::from_slice(&std::fs::read(p).unwrap()).unwrap()
}

/// `kim hotkeys <args> --json` in the sandbox; validated against all three schemas.
fn hk(sb: &TempDir, args: &[&str]) -> (i32, Value) {
    let mut full = vec!["hotkeys"];
    full.extend_from_slice(args);
    full.push("--json");
    let out = run_sandboxed(&bin("kim"), &full, sb);
    let doc: Value =
        serde_json::from_slice(&out.stdout).unwrap_or_else(|e| panic!("{full:?}: not JSON ({e})"));
    for name in ["cli-envelope-v1", "kim-cli-v1", "hotkeys-cli-v1"] {
        let s = schema(name);
        let errors = jsonschema::Validator::new(&s).validate(&doc).unwrap();
        assert!(errors.is_empty(), "{full:?} violates {name}: {errors:?}");
    }
    let code = out.status.code().unwrap_or(-1);
    if doc["ok"] == false {
        assert_eq!(i64::from(code), doc["error"]["exit_code"].as_i64().unwrap());
    } else {
        assert_eq!(code, 0);
    }
    (code, doc)
}

fn layer(sb: &TempDir) -> PathBuf {
    #[cfg(windows)]
    return sb
        .path()
        .join("Roaming")
        .join("mouv")
        .join("kim")
        .join("hotkeys.json");
    #[cfg(not(windows))]
    return sb
        .path()
        .join(".config")
        .join("mouv")
        .join("kim")
        .join("hotkeys.json");
}

#[test]
#[ignore = "e2e: run via `cargo xtask e2e` only (AGENTS.md §4)"]
fn a_user_lists_changes_disables_and_restores_shortcuts_without_reinstalling() {
    let sb = TempDir::new("step46-cycle");
    let (_, doc) = hk(&sb, &["list"]);
    assert_eq!(doc["data"]["tool"], "kim");
    assert_eq!(doc["data"]["actions"][0]["bindings"][0]["keys"], "F5");

    let (_, doc) = hk(&sb, &["set", "catalog.refresh", "ctrl+alt+shift+f24"]);
    assert_eq!(
        doc["data"]["changes"][0]["after"]["bindings"],
        json!(["Ctrl+Alt+Shift+F24"])
    );
    hk(&sb, &["set", "tool.install", "Ctrl+Alt+Shift+F23"]);
    let before = std::fs::read(layer(&sb)).unwrap();
    let (code, doc) = hk(&sb, &["set", "tool.update", "Ctrl+Alt+Shift+F23"]);
    assert_eq!(
        (code, doc["error"]["code"].as_str()),
        (7, Some("hotkey_conflict"))
    );
    assert_eq!(
        std::fs::read(layer(&sb)).unwrap(),
        before,
        "a refused change writes nothing"
    );
    let (code, _) = hk(&sb, &["set", "help.show", "Win+L"]);
    assert_eq!(code, 2);
    let (_, doc) = hk(
        &sb,
        &["set", "tool.update", "Ctrl+Alt+Shift+F23", "--replace"],
    );
    assert_eq!(doc["data"]["changes"].as_array().unwrap().len(), 2);

    hk(&sb, &["disable", "--all"]);
    let (_, doc) = hk(&sb, &["list"]);
    assert_eq!(doc["data"]["enabled"], false);
    hk(&sb, &["enable", "--all"]);
    let (code, _) = hk(&sb, &["reset", "--all"]);
    assert_eq!(code, 7, "needs --yes when Kim cannot ask");
    hk(&sb, &["reset", "--all", "--yes"]);
    assert!(layer(&sb).with_extension("json.bak").exists());
    let (_, doc) = hk(&sb, &["show", "catalog.refresh"]);
    assert_eq!(doc["data"]["action"]["source"], "default");
}

fn fake_install(sb: &TempDir, tool: &str) {
    #[cfg(windows)]
    let programs = sb.path().join("Local").join("Programs").join("mouv");
    #[cfg(not(windows))]
    let programs = sb.path().join(".local/share").join("mouv").join("programs");
    let vdir = programs.join(tool).join("versions").join("0.1.0");
    std::fs::create_dir_all(&vdir).unwrap();
    let record = json!({
        "schema": "kim.install", "schema_version": 1, "tool": tool, "version": "0.1.0",
        "entrypoint": format!("versions/0.1.0/{tool}.exe"), "installed_at": "2026-11-04T00:00:00Z",
        "sha256": "00", "source_url": "https://example.invalid/x.zip", "catalog_version": 1,
        "previous_version": null
    });
    std::fs::write(programs.join(tool).join("current.json"), record.to_string()).unwrap();
    std::fs::write(vdir.join(format!("{tool}.exe")), b"not a program").unwrap();
    let manifest = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../docs/contracts/examples/kim-hotkey-manifest-v1")
        .join(format!("{tool}.json"));
    std::fs::copy(manifest, vdir.join("hotkeys.manifest.json")).unwrap();
}

#[test]
#[ignore = "e2e: run via `cargo xtask e2e` only (AGENTS.md §4)"]
fn cross_tool_collisions_are_reported_from_files_only() {
    let sb = TempDir::new("step46-all");
    fake_install(&sb, "noeul");
    fake_install(&sb, "ddalgak");
    let dl = layer(&sb)
        .parent()
        .unwrap()
        .parent()
        .unwrap()
        .join("ddalgak");
    std::fs::create_dir_all(&dl).unwrap();
    std::fs::write(
        dl.join("hotkeys.json"),
        json!({"schema": "kim.hotkeys", "schema_version": 1, "tool": "ddalgak",
               "overrides": [{"action": "engine.toggle", "bindings": ["F3"]}]})
        .to_string(),
    )
    .unwrap();
    let entries = sb.entry_count();
    let (code, doc) = hk(&sb, &["check", "--all-tools"]);
    assert_eq!(code, 0);
    assert_eq!(sb.entry_count(), entries, "nothing written");
    let conflicts = doc["data"]["conflicts"].as_array().unwrap();
    let e = conflicts
        .iter()
        .find(|c| c["code"] == "cross_tool_global")
        .expect("cross-tool error");
    assert_eq!(
        (e["tool"].as_str(), e["action"].as_str()),
        (Some("ddalgak"), Some("engine.toggle"))
    );
    assert!(
        e["offers"]
            .as_array()
            .unwrap()
            .iter()
            .any(|o| o["kind"] == "rebind")
    );
    assert!(
        conflicts.iter().any(|c| c["code"] == "global_shadows_app"
            && c["tool"] == "kim"
            && c["action"] == "help.show"),
        "Noeul's global F1 shadows Kim Manager's F1"
    );
    let out = run_sandboxed(&bin("kim"), &["hotkeys", "check", "--all-tools"], &sb);
    assert!(String::from_utf8_lossy(&out.stdout).contains("ddalgak hotkeys set engine.toggle"));
}

#[test]
#[ignore = "e2e: run via `cargo xtask e2e` only (AGENTS.md §4)"]
fn a_broken_file_means_safe_mode_and_explicit_recovery() {
    let sb = TempDir::new("step46-safe");
    std::fs::create_dir_all(layer(&sb).parent().unwrap()).unwrap();
    std::fs::write(layer(&sb), b"{ broken").unwrap();
    let (_, doc) = hk(&sb, &["list"]);
    assert_eq!(doc["data"]["config"]["state"], "invalid");
    let (code, _) = hk(&sb, &["set", "help.show", "Ctrl+Alt+Shift+F24"]);
    assert_eq!(code, 4);
    assert_eq!(std::fs::read(layer(&sb)).unwrap(), b"{ broken");
    hk(&sb, &["reset", "--all", "--yes"]);
    assert_eq!(
        std::fs::read(layer(&sb).with_extension("json.bak")).unwrap(),
        b"{ broken"
    );
    let (_, doc) = hk(&sb, &["list"]);
    assert_eq!(doc["data"]["config"]["state"], "user");
}
