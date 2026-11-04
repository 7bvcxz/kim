//! E2E — Step 17: Subak's keyboard shortcuts from the command line (docs/SUBAK_CLI.md §3.12,
//! docs/HOTKEY_CONTRACT.md §10, §13.1). Requirements: `SUBAK-FR-033`, `SUBAK-FR-034`,
//! `KIM-FR-041`…`KIM-FR-046`, `KIM-SEC-023`.
//!
//! Scenario A (release `subak`, isolated profile, no Host): a user lists the window's shortcuts
//! (the four requested tab keys), lets programs receive `Ctrl+W` by disabling "Close Terminal",
//! moves "Next tab" to `Ctrl+PageDown`, sees a same-tool collision refused without any write,
//! resolves it with `--replace`, and restores everything with a backup — every `--json` document
//! valid against cli-envelope-v1 and hotkeys-cli-v1, and no Host started.
//!
//! Scenario B: a broken `hotkeys.json` is never overwritten implicitly (safe mode); the
//! documented recovery keeps it as the backup. A simulated Noeul install is read, never started,
//! by `check --all-tools`.

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

/// `subak hotkeys <args> --json` in the sandbox; validated against both schemas.
fn hk(sb: &TempDir, args: &[&str]) -> (i32, Value) {
    let mut full = vec!["hotkeys"];
    full.extend_from_slice(args);
    full.push("--json");
    let out = run_sandboxed(&bin("subak"), &full, sb);
    assert!(
        out.stderr.is_empty(),
        "{full:?}: stderr stays empty with --json"
    );
    let doc: Value =
        serde_json::from_slice(&out.stdout).unwrap_or_else(|e| panic!("{full:?}: not JSON ({e})"));
    for name in ["cli-envelope-v1", "hotkeys-cli-v1"] {
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
    if cfg!(windows) {
        sb.path()
            .join("Roaming")
            .join("mouv")
            .join("subak")
            .join("hotkeys.json")
    } else {
        sb.path()
            .join(".config")
            .join("mouv")
            .join("subak")
            .join("hotkeys.json")
    }
}

fn keys_of(list: &Value, id: &str) -> Vec<String> {
    list["data"]["actions"]
        .as_array()
        .unwrap()
        .iter()
        .find(|a| a["id"] == id)
        .unwrap_or_else(|| panic!("no action {id}"))["bindings"]
        .as_array()
        .unwrap()
        .iter()
        .map(|b| b["keys"].as_str().unwrap().to_string())
        .collect()
}

#[test]
#[ignore = "e2e: run via `cargo xtask e2e` only (AGENTS.md §4)"]
fn scenario_a_a_user_rebinds_and_disables_the_tab_keys_headlessly() {
    let sb = TempDir::new("step17-hotkeys-a");
    let (_, list) = hk(&sb, &["list"]);
    for (id, want) in [
        ("terminal.new", "Ctrl+T"),
        ("terminal.close", "Ctrl+W"),
        ("tab.next", "Ctrl+Tab"),
        ("tab.previous", "Ctrl+Shift+Tab"),
    ] {
        assert_eq!(keys_of(&list, id), [want], "{id}");
    }
    // Let programs receive Ctrl+W (readline's delete-word) by disabling "Close Terminal".
    let (_, off) = hk(&sb, &["disable", "terminal.close"]);
    assert_eq!(off["data"]["written"], true);
    let (_, show) = hk(&sb, &["show", "terminal.close"]);
    assert_eq!(show["data"]["action"]["source"], "disabled");
    // Move "Next tab" and preview first.
    let (_, dry) = hk(&sb, &["set", "tab.next", "ctrl+pagedown", "--dry-run"]);
    assert_eq!(dry["data"]["written"], false);
    hk(&sb, &["set", "tab.next", "Ctrl+PageDown"]);
    let before = std::fs::read_to_string(layer(&sb)).unwrap();
    // A second user binding on the same chord is refused, nothing written.
    let (code, e) = hk(&sb, &["set", "tab.previous", "Ctrl+PageDown"]);
    assert_eq!(code, 7);
    assert_eq!(e["error"]["code"], "hotkey_conflict");
    assert_eq!(std::fs::read_to_string(layer(&sb)).unwrap(), before);
    // Resolved explicitly.
    let (_, r) = hk(&sb, &["set", "tab.previous", "Ctrl+PageDown", "--replace"]);
    assert_eq!(r["data"]["changes"].as_array().unwrap().len(), 2, "{r}");
    // Everything back, with a backup; --yes is required.
    let (code, _) = hk(&sb, &["reset", "--all"]);
    assert_eq!(code, 7);
    let (_, reset) = hk(&sb, &["reset", "--all", "--yes"]);
    assert_eq!(reset["data"]["written"], true);
    assert!(layer(&sb).with_extension("json.bak").exists());
    let (_, list) = hk(&sb, &["list"]);
    assert_eq!(keys_of(&list, "terminal.close"), ["Ctrl+W"]);
    assert_eq!(keys_of(&list, "tab.next"), ["Ctrl+Tab"]);
}

#[test]
#[ignore = "e2e: run via `cargo xtask e2e` only (AGENTS.md §4)"]
fn scenario_b_safe_mode_and_cross_tool_checks_read_files_only() {
    let sb = TempDir::new("step17-hotkeys-b");
    std::fs::create_dir_all(layer(&sb).parent().unwrap()).unwrap();
    std::fs::write(layer(&sb), "{ broken").unwrap();
    let (_, list) = hk(&sb, &["list"]);
    assert_eq!(list["data"]["config"]["state"], "invalid");
    let (code, e) = hk(&sb, &["set", "tab.next", "Ctrl+PageDown"]);
    assert_eq!(code, 4);
    assert_eq!(e["error"]["code"], "hotkeys_invalid");
    assert_eq!(std::fs::read_to_string(layer(&sb)).unwrap(), "{ broken");
    hk(&sb, &["reset", "--all", "--yes"]);
    assert_eq!(
        std::fs::read_to_string(layer(&sb).with_extension("json.bak")).unwrap(),
        "{ broken"
    );
    // A simulated Noeul install: manifest and record only; its "program" is not executable.
    let manifest: Value = serde_json::from_slice(
        &std::fs::read(
            Path::new(env!("CARGO_MANIFEST_DIR")).join("../crates/noeul/src/hotkeys.manifest.json"),
        )
        .unwrap(),
    )
    .unwrap();
    let version = manifest["version"].as_str().unwrap().to_string();
    let programs = if cfg!(windows) {
        sb.path().join("Local").join("Programs").join("mouv")
    } else {
        sb.path().join(".local/share").join("mouv").join("programs")
    };
    let v = programs.join("noeul").join("versions").join(&version);
    std::fs::create_dir_all(&v).unwrap();
    std::fs::write(
        programs.join("noeul").join("current.json"),
        json!({"version": version}).to_string(),
    )
    .unwrap();
    std::fs::write(v.join("hotkeys.manifest.json"), manifest.to_string()).unwrap();
    std::fs::write(v.join("noeul.exe"), "not a program").unwrap();
    let (_, report) = hk(&sb, &["check", "--all-tools"]);
    let tools: Vec<&str> = report["data"]["tools"]
        .as_array()
        .unwrap()
        .iter()
        .map(|t| t["tool"].as_str().unwrap())
        .collect();
    assert_eq!(tools, ["noeul", "subak"]);
    assert_eq!(report["data"]["summary"]["errors"], 0);
}
