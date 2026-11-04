//! `subak hotkeys …` (Step 17; docs/HOTKEY_CONTRACT.md §10, docs/SUBAK_CLI.md §3.12) run as the
//! real `subak` binary in a sandboxed profile: `APPDATA`/`LOCALAPPDATA` (and the XDG variables)
//! point at a fresh temporary folder, so the user's own `hotkeys.json` and Kim installs are never
//! read or written. No Host is started or contacted, no window opens, nothing is registered.
//! Every `--json` document is validated against cli-envelope-v1 and hotkeys-cli-v1 with the
//! validator of `cargo xtask schema-check`.
//!
//! Requirements: `SUBAK-FR-033` (the defaults), `SUBAK-FR-034` (every shortcut rebindable or
//! disabled), `KIM-FR-041`…`KIM-FR-046`, `KIM-SEC-023`.

#[path = "../../../xtask/src/jsonschema.rs"]
mod jsonschema;

use serde_json::{Value, json};
use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};

fn repo() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../..")
}

fn schema(name: &str) -> Value {
    let path = repo().join(format!("docs/contracts/schemas/{name}.schema.json"));
    serde_json::from_slice(&std::fs::read(path).unwrap()).unwrap()
}

/// One sandboxed profile, removed on drop.
struct Profile(PathBuf);

impl Profile {
    fn new(tag: &str) -> Self {
        let d = std::env::temp_dir().join(format!(
            "subak-hotkeys-cli-{tag}-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(&d).unwrap();
        Self(d)
    }

    /// `%APPDATA%\mouv\subak\hotkeys.json` of this profile.
    fn layer(&self) -> PathBuf {
        self.0.join("Roaming/mouv/subak/hotkeys.json")
    }

    /// Kim's per-user install root of this profile.
    fn programs(&self) -> PathBuf {
        if cfg!(windows) {
            self.0.join("Local/Programs/mouv")
        } else {
            self.0.join("Local/mouv/programs")
        }
    }

    fn run(&self, args: &[&str]) -> Output {
        let mut cmd = Command::new(env!("CARGO_BIN_EXE_subak"));
        cmd.args(args)
            .env("APPDATA", self.0.join("Roaming"))
            .env("LOCALAPPDATA", self.0.join("Local"))
            .env("XDG_CONFIG_HOME", self.0.join("Roaming"))
            .env("XDG_DATA_HOME", self.0.join("Local"))
            .env_remove("KIM_NO_INPUT")
            .stdin(Stdio::null());
        #[cfg(windows)]
        {
            use std::os::windows::process::CommandExt;
            cmd.creation_flags(0x0800_0000); // CREATE_NO_WINDOW
        }
        cmd.output().expect("run subak")
    }

    /// `subak --json hotkeys <args>`: one valid document on stdout, nothing on stderr, and the
    /// exit code of its class.
    fn json(&self, args: &[&str]) -> (i32, Value) {
        let mut a = vec!["--json", "hotkeys"];
        a.extend_from_slice(args);
        let out = self.run(&a);
        assert!(
            out.stderr.is_empty(),
            "{a:?}: --json keeps stderr empty: {}",
            String::from_utf8_lossy(&out.stderr)
        );
        let doc: Value = serde_json::from_slice(&out.stdout)
            .unwrap_or_else(|e| panic!("{a:?}: not one JSON document ({e})"));
        for name in ["cli-envelope-v1", "hotkeys-cli-v1"] {
            let s = schema(name);
            let errors = jsonschema::Validator::new(&s).validate(&doc).unwrap();
            assert!(
                errors.is_empty(),
                "{a:?} violates {name}: {errors:?}\n{doc:#}"
            );
        }
        let code = out.status.code().unwrap();
        if doc["ok"] == false {
            assert_eq!(i64::from(code), doc["error"]["exit_code"].as_i64().unwrap());
        } else {
            assert_eq!(code, 0, "{doc}");
        }
        (code, doc)
    }

    fn file(&self) -> Option<String> {
        std::fs::read_to_string(self.layer()).ok()
    }
}

impl Drop for Profile {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

fn action<'a>(list: &'a Value, id: &str) -> &'a Value {
    list["data"]["actions"]
        .as_array()
        .unwrap()
        .iter()
        .find(|a| a["id"] == id)
        .unwrap_or_else(|| panic!("no action {id}"))
}

fn keys(a: &Value) -> Vec<String> {
    a["bindings"]
        .as_array()
        .unwrap()
        .iter()
        .map(|b| b["keys"].as_str().unwrap().to_string())
        .collect()
}

#[test]
fn list_show_and_check_report_the_shipped_defaults_without_a_host_or_a_file() {
    let p = Profile::new("defaults");
    let (_, list) = p.json(&["list"]);
    assert_eq!(list["command"], "hotkeys.list");
    let d = &list["data"];
    assert_eq!(d["tool"], "subak");
    assert_eq!(d["enabled"], true);
    assert_eq!(d["config"]["state"], "default");
    assert_eq!(d["leaders"], json!([]), "the prefix leader is Step 23's");
    // SUBAK-FR-033: the four requested tab keys, exactly.
    for (id, want) in [
        ("terminal.new", "Ctrl+T"),
        ("terminal.close", "Ctrl+W"),
        ("tab.next", "Ctrl+Tab"),
        ("tab.previous", "Ctrl+Shift+Tab"),
        ("terminal.rename", "Ctrl+Shift+R"),
    ] {
        let a = action(&list, id);
        assert_eq!(keys(a), [want], "{id}");
        assert_eq!(a["scope"], "app", "{id}");
        assert_eq!(a["source"], "default", "{id}");
        assert_eq!(a["bindings"][0]["registration"], "not_applicable");
    }
    // The Step 16 window keys are rebindable actions of the terminal context now.
    let copy = action(&list, "clipboard.copy");
    assert_eq!(copy["scope"], "context:terminal");
    assert_eq!(keys(copy), ["Ctrl+Shift+C", "Ctrl+Insert"]);
    assert_eq!(action(&list, "tab.hide")["source"], "unbound");
    assert_eq!(action(&list, "window.close")["source"], "reserved");
    assert!(
        d["actions"]
            .as_array()
            .unwrap()
            .iter()
            .all(|a| !a["id"].as_str().unwrap().starts_with("window.select")),
        "only what this build implements is listed"
    );
    // --scope filters.
    let (_, ctx) = p.json(&["list", "--scope", "context"]);
    assert!(
        ctx["data"]["actions"]
            .as_array()
            .unwrap()
            .iter()
            .all(|a| a["scope"] == "context:terminal" || a["scope"] == "context:manager")
    );
    // Step 18: the Terminal manager's keys, a context of their own.
    for (id, want) in [
        ("manager.open", "Enter"),
        ("manager.new", "Ctrl+N"),
        ("manager.rename", "F2"),
        ("manager.delete", "Delete"),
        ("manager.close", "Escape"),
    ] {
        let a = action(&list, id);
        assert_eq!(keys(a), [want], "{id}");
        assert_eq!(a["scope"], "context:manager", "{id}");
    }
    assert_eq!(keys(action(&list, "manager.search")), ["Ctrl+F", "char:/"]);
    // show: one action and what involves it.
    let (_, show) = p.json(&["show", "terminal.close"]);
    assert_eq!(show["data"]["action"]["id"], "terminal.close");
    assert_eq!(
        show["data"]["conflicts"][0]["code"], "shadows_terminal_input",
        "programs do not receive Ctrl+W: an info with the pass-through hint"
    );
    let (code, missing) = p.json(&["show", "window.select-1"]);
    assert_eq!(code, 3);
    assert_eq!(missing["error"]["code"], "action_not_found");
    // check: no error or warning; the two shadowed terminal keys as infos.
    let (_, check) = p.json(&["check"]);
    assert_eq!(
        check["data"]["summary"],
        json!({"errors": 0, "warnings": 0, "infos": 2})
    );
    let (_, errors) = p.json(&["check", "--min-severity", "warning"]);
    assert_eq!(errors["data"]["conflicts"], json!([]));
    // Nothing was written by reading.
    assert_eq!(p.file(), None);
    // The human list is a table on stdout.
    let out = p.run(&["hotkeys", "list"]);
    assert!(out.status.success());
    let text = String::from_utf8_lossy(&out.stdout);
    assert!(
        text.contains("terminal.close") && text.contains("Ctrl+W"),
        "{text}"
    );
    assert!(text.contains("hotkeys.json (default)"), "{text}");
}

#[test]
fn set_disable_enable_and_reset_round_trip_and_dry_runs_write_nothing() {
    let p = Profile::new("edit");
    // A dry run checks and shows, and writes nothing.
    let (_, dry) = p.json(&["set", "tab.next", "ctrl+pagedown", "--dry-run"]);
    assert_eq!(dry["data"]["dry_run"], true);
    assert_eq!(dry["data"]["written"], false);
    assert_eq!(
        dry["data"]["changes"][0]["after"]["bindings"],
        json!(["Ctrl+PageDown"])
    );
    assert_eq!(p.file(), None, "a dry run writes nothing");
    // The alias form is stored canonically.
    let (_, set) = p.json(&["set", "tab.next", "ctrl+pagedown", "Ctrl+Tab"]);
    assert_eq!(set["data"]["written"], true);
    let stored: Value = serde_json::from_str(&p.file().unwrap()).unwrap();
    assert_eq!(stored["tool"], "subak");
    assert_eq!(
        stored["overrides"],
        json!([{"action": "tab.next", "bindings": ["Ctrl+PageDown", "Ctrl+Tab"]}])
    );
    let (_, list) = p.json(&["list"]);
    assert_eq!(
        keys(action(&list, "tab.next")),
        ["Ctrl+PageDown", "Ctrl+Tab"]
    );
    assert_eq!(list["data"]["config"]["state"], "user");
    // Disable one action: Ctrl+W then reaches the program (until Step 23's pass-through).
    let (_, off) = p.json(&["disable", "terminal.close"]);
    assert_eq!(off["data"]["changes"][0]["after"]["source"], "disabled");
    let (_, check) = p.json(&["check"]);
    assert!(
        check["data"]["conflicts"]
            .as_array()
            .unwrap()
            .iter()
            .all(|c| c["action"] != "terminal.close"),
        "a disabled Ctrl+W shadows nothing: {check}"
    );
    // A sequence and an option object.
    let (_, seq) = p.json(&["set", "tab.hide", "Ctrl+K Ctrl+H"]);
    assert_eq!(seq["data"]["written"], true);
    let (_, rep) = p.json(&["set", "tab.move-left", "Ctrl+Shift+PageUp", "--repeat"]);
    assert_eq!(rep["data"]["written"], true, "{rep}");
    let stored: Value = serde_json::from_str(&p.file().unwrap()).unwrap();
    assert!(stored.to_string().contains("\"repeat\":true"), "{stored}");
    // The context binding Ctrl+Shift+PageUp (scroll.page-up) is in another scope: no conflict.
    let (_, check) = p.json(&["check"]);
    assert_eq!(check["data"]["summary"]["errors"], 0, "{check}");
    // Switches.
    let (_, all_off) = p.json(&["disable", "--all"]);
    assert_eq!(all_off["data"]["written"], true);
    let (_, list) = p.json(&["list"]);
    assert_eq!(list["data"]["enabled"], false);
    p.json(&["enable", "--all"]);
    let (_, ctx_off) = p.json(&["disable", "--scope", "context"]);
    assert_eq!(ctx_off["data"]["written"], true);
    let (_, list) = p.json(&["list"]);
    assert_eq!(list["data"]["disabled_scopes"], json!(["context"]));
    p.json(&["enable", "--scope", "context"]);
    // Reset one action, then everything (destructive: --yes; a backup is kept).
    let (_, r) = p.json(&["reset", "tab.next"]);
    assert_eq!(r["data"]["changes"][0]["after"]["source"], "default");
    let before = p.file().unwrap();
    let (code, refused) = p.json(&["reset", "--all"]);
    assert_eq!(code, 7);
    assert_eq!(refused["error"]["code"], "confirmation_required");
    assert_eq!(p.file().unwrap(), before, "refused: unchanged");
    let (_, preview) = p.json(&["reset", "--all", "--dry-run"]);
    assert_eq!(preview["data"]["written"], false);
    let (_, all) = p.json(&["reset", "--all", "--yes"]);
    assert_eq!(all["data"]["written"], true);
    assert!(
        all["warnings"][0]
            .as_str()
            .unwrap()
            .contains("hotkeys.json.bak")
    );
    let backup = std::fs::read_to_string(p.layer().with_extension("json.bak")).unwrap();
    assert_eq!(backup, before);
    let (_, list) = p.json(&["list"]);
    assert_eq!(keys(action(&list, "terminal.close")), ["Ctrl+W"]);
    // Human output: the change on stdout, what was saved.
    let out = p.run(&["hotkeys", "set", "terminal.new", "Ctrl+Shift+T"]);
    assert!(out.status.success());
    let text = String::from_utf8_lossy(&out.stdout);
    assert!(
        text.contains("terminal.new: Ctrl+T (default) → Ctrl+Shift+T (user)"),
        "{text}"
    );
    assert!(text.contains("applies it when it regains focus"), "{text}");
}

#[test]
fn refused_changes_write_nothing_and_name_the_way_out() {
    let p = Profile::new("refused");
    // Reserved by the OS.
    let (code, e) = p.json(&["set", "tab.next", "Win+L"]);
    assert_eq!(
        (code, e["error"]["code"].as_str()),
        (2, Some("hotkey_reserved"))
    );
    // Reserved navigation in app scope.
    let (code, e) = p.json(&["set", "tab.next", "F6"]);
    assert_eq!(
        (code, e["error"]["code"].as_str()),
        (2, Some("hotkey_reserved"))
    );
    // Bad syntax and an unknown action.
    let (code, e) = p.json(&["set", "tab.next", "Ctrl+Nope"]);
    assert_eq!(
        (code, e["error"]["code"].as_str()),
        (2, Some("hotkey_invalid"))
    );
    let (code, e) = p.json(&["set", "pane.split-right", "Ctrl+Shift+5"]);
    assert_eq!(
        (code, e["error"]["code"].as_str()),
        (3, Some("action_not_found"))
    );
    // A scope the action does not have.
    let (code, e) = p.json(&["set", "tab.next", "Ctrl+F12", "--scope", "global"]);
    assert_eq!(code, 2, "{e}");
    assert_eq!(p.file(), None, "nothing written by any refusal");
    // A user binding beats a default (§9.1, scenario 04): Ctrl+T onto tab.next is accepted and
    // reported, never refused.
    let (_, ok) = p.json(&["set", "tab.next", "Ctrl+T"]);
    assert!(
        ok["data"]["conflicts"]
            .as_array()
            .unwrap()
            .iter()
            .any(|c| c["code"] == "default_suppressed"),
        "{ok}"
    );
    // Two user bindings on one chord: refused, the file keeps what it had.
    let before = p.file().unwrap();
    let (code, e) = p.json(&["set", "terminal.rename", "Ctrl+T"]);
    assert_eq!(
        (code, e["error"]["code"].as_str()),
        (7, Some("hotkey_conflict"))
    );
    assert!(
        e["error"]["hint"].as_str().unwrap().contains("--replace"),
        "{e}"
    );
    assert_eq!(p.file().unwrap(), before);
    // --replace takes it away from the other action of Subak, and lists both changes.
    let (_, r) = p.json(&["set", "terminal.rename", "Ctrl+T", "--replace"]);
    let changed: Vec<&str> = r["data"]["changes"]
        .as_array()
        .unwrap()
        .iter()
        .map(|c| c["action"].as_str().unwrap())
        .collect();
    assert!(
        changed.contains(&"terminal.rename") && changed.contains(&"tab.next"),
        "{r}"
    );
}

#[test]
fn a_broken_or_newer_file_is_safe_mode_and_never_rewritten_implicitly() {
    let p = Profile::new("broken");
    std::fs::create_dir_all(p.layer().parent().unwrap()).unwrap();
    std::fs::write(p.layer(), "{ not json").unwrap();
    let (_, list) = p.json(&["list"]);
    assert_eq!(list["data"]["config"]["state"], "invalid");
    assert!(
        list["warnings"][0]
            .as_str()
            .unwrap()
            .contains("reset --all --yes"),
        "{list}"
    );
    assert_eq!(
        keys(action(&list, "terminal.new")),
        ["Ctrl+T"],
        "defaults apply"
    );
    for args in [
        &["set", "tab.next", "Ctrl+PageDown"][..],
        &["disable", "terminal.close"],
        &["enable", "--all"],
        &["reset", "tab.next"],
        &["export"],
    ] {
        let (code, e) = p.json(args);
        assert_eq!(code, 4, "{args:?}");
        assert_eq!(e["error"]["code"], "hotkeys_invalid", "{args:?}");
    }
    assert_eq!(p.file().as_deref(), Some("{ not json"), "never rewritten");
    // The documented recovery replaces it and keeps the broken file as the backup.
    let (_, r) = p.json(&["reset", "--all", "--yes"]);
    assert_eq!(r["data"]["written"], true);
    assert_eq!(
        std::fs::read_to_string(p.layer().with_extension("json.bak")).unwrap(),
        "{ not json"
    );
    let (_, list) = p.json(&["list"]);
    assert_eq!(list["data"]["config"]["state"], "user");
    // A file of a newer schema version is read-only, even for reset --all.
    let newer = json!({"schema": "kim.hotkeys", "schema_version": 2, "tool": "subak"}).to_string();
    std::fs::write(p.layer(), &newer).unwrap();
    for args in [
        &["set", "tab.next", "Ctrl+PageDown"][..],
        &["reset", "--all", "--yes"],
    ] {
        let (code, e) = p.json(args);
        assert_eq!(code, 5, "{args:?}");
        assert_eq!(e["error"]["code"], "hotkeys_from_newer_version");
    }
    assert_eq!(p.file().unwrap(), newer);
}

#[test]
fn export_and_import_move_the_user_layer_as_a_document() {
    let p = Profile::new("io");
    let (_, empty) = p.json(&["export"]);
    assert_eq!(
        empty["data"]["document"],
        json!({"schema": "kim.hotkeys", "schema_version": 1, "tool": "subak"})
    );
    let doc = p.0.join("mine.json");
    std::fs::write(
        &doc,
        json!({"schema": "kim.hotkeys", "schema_version": 1, "tool": "subak", "future": 1,
            "overrides": [{"action": "terminal.close", "disabled": true},
                          {"action": "tab.next", "bindings": ["Ctrl+PageDown"]}]})
        .to_string(),
    )
    .unwrap();
    let d = doc.display().to_string();
    let (_, dry) = p.json(&["import", &d, "--dry-run"]);
    assert_eq!(dry["data"]["written"], false);
    assert_eq!(p.file(), None);
    let (_, imported) = p.json(&["import", &d]);
    assert_eq!(imported["data"]["written"], true);
    let (_, back) = p.json(&["export"]);
    assert_eq!(
        back["data"]["document"]["future"], 1,
        "unknown fields are kept"
    );
    assert_eq!(
        back["data"]["document"]["overrides"][1]["action"],
        "tab.next"
    );
    // A document for another tool, and a missing file, are refused without a write.
    std::fs::write(
        &doc,
        json!({"schema": "kim.hotkeys", "schema_version": 1, "tool": "noeul"}).to_string(),
    )
    .unwrap();
    let before = p.file();
    let (code, e) = p.json(&["import", &d]);
    assert_eq!(
        (code, e["error"]["code"].as_str()),
        (4, Some("hotkeys_invalid"))
    );
    let (code, _) = p.json(&["import", &p.0.join("none.json").display().to_string()]);
    assert_eq!(code, 1);
    assert_eq!(p.file(), before);
}

#[test]
fn check_all_tools_reads_the_installed_manifests_and_starts_nothing() {
    let p = Profile::new("all-tools");
    // A simulated Kim install of Noeul: its shipped manifest, an install record, and a
    // "program" that is not executable (it is never started).
    let manifest: Value = serde_json::from_slice(
        &std::fs::read(repo().join("crates/noeul/src/hotkeys.manifest.json")).unwrap(),
    )
    .unwrap();
    let version = manifest["version"].as_str().unwrap().to_string();
    let dir = p.programs().join("noeul");
    std::fs::create_dir_all(dir.join("versions").join(&version)).unwrap();
    std::fs::write(
        dir.join("current.json"),
        json!({"version": version}).to_string(),
    )
    .unwrap();
    std::fs::write(
        dir.join("versions")
            .join(&version)
            .join("hotkeys.manifest.json"),
        manifest.to_string(),
    )
    .unwrap();
    std::fs::write(
        dir.join("versions").join(&version).join("noeul.exe"),
        "not a program",
    )
    .unwrap();
    // A damaged install is reported, not guessed.
    std::fs::create_dir_all(p.programs().join("ddalgak").join("versions")).unwrap();
    let (_, report) = p.json(&["check", "--all-tools"]);
    let tools: Vec<&str> = report["data"]["tools"]
        .as_array()
        .unwrap()
        .iter()
        .map(|t| t["tool"].as_str().unwrap())
        .collect();
    assert_eq!(tools, ["noeul", "subak"], "{report}");
    assert_eq!(report["data"]["tools"][1]["source"], "self");
    assert!(
        report["warnings"]
            .as_array()
            .unwrap()
            .iter()
            .any(|w| w.as_str().unwrap().contains("ddalgak: damaged install")),
        "{report}"
    );
    assert_eq!(report["data"]["summary"]["errors"], 0, "{report}");
    // A Subak global binding that collides with Noeul's F1 is a cross-tool error — Subak has
    // no global action yet, so the user cannot even ask for one.
    let (code, e) = p.json(&["set", "terminal.new", "F1", "--scope", "global"]);
    assert_eq!(code, 2, "{e}");
    assert_eq!(p.file(), None);
}
