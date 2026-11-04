//! Subak's keyboard shortcuts on the shared `kim-hotkeys` library (docs/HOTKEY_CONTRACT.md §13.1,
//! ADR 0009, ADR 0017; `SUBAK-FR-033`, `SUBAK-FR-034`). Used by `subak hotkeys …` (headless)
//! and by the window (`subak-gui`, focus backend).
//!
//! - **Manifest.** The final manifest of every Subak action is the contract example
//!   `docs/contracts/examples/kim-hotkey-manifest-v1/subak.json`. A build ships only the actions
//!   it implements, under the same IDs and defaults: this build's subset is [`MANIFEST_JSON`]
//!   (`hotkeys.manifest.json`, shipped next to `capabilities.json`). A test keeps it equal to the
//!   final manifest filtered by [`IMPLEMENTED`], with the `prefix` leader and every `@prefix`
//!   binding left out until the prefix table exists (Step 23): a shipped binding always does
//!   what it says.
//! - **Scopes.** The tab actions are `app` actions (the window has focus); the Step 16 copy,
//!   paste, zoom and scroll keys are actions of the `terminal` context (passthrough: keys no
//!   binding uses go to the program); the Terminal manager's keys (Step 18) are actions of the
//!   `manager` context. Subak registers no global hotkey yet: `gui.show` (no default) is not
//!   shipped until a Step registers it; the floating UI is reached by a click, the tray, or
//!   `subak-gui --manager` (a shortcut key on a Start-menu link works without registering).
//! - **The user layer** is `%APPDATA%\mouv\subak\hotkeys.json` (next to the window's `gui.json`).
//!   A broken or newer file means safe mode: the defaults apply and the file is never rewritten
//!   implicitly (§8.3).

use kim_hotkeys::check::{ToolEntry, ToolState};
use kim_hotkeys::manifest::Manifest;
use kim_hotkeys::store::{self, ConfigState, Loaded};
use serde_json::Value;
use std::path::{Path, PathBuf};

/// This build's hotkey manifest (shipped as `hotkeys.manifest.json`; `version` = crate version).
pub const MANIFEST_JSON: &str = include_str!("hotkeys.manifest.json");

/// The tool ID of Subak's shortcuts.
pub const TOOL: &str = "subak";

/// The context of the terminal pane (passthrough).
pub const TERMINAL_CONTEXT: &str = "terminal";

/// The context of the Terminal manager (Step 18).
pub const MANAGER_CONTEXT: &str = "manager";

/// The actions this build implements, in manifest order.
pub const IMPLEMENTED: &[&str] = &[
    "terminal.new",
    "terminal.close",
    "tab.next",
    "tab.previous",
    "tab.hide",
    "tab.move-left",
    "tab.move-right",
    "terminal.rename",
    "clipboard.copy",
    "clipboard.paste",
    "clipboard.copy-html",
    "clipboard.paste-one-line",
    "selection.all",
    "selection.mark",
    "search.open",
    "output.save",
    "view.zoom-in",
    "view.zoom-out",
    "view.zoom-reset",
    "scroll.page-up",
    "scroll.page-down",
    "scroll.line-up",
    "scroll.line-down",
    "scroll.top",
    "scroll.bottom",
    "manager.open",
    "manager.new",
    "manager.rename",
    "manager.delete",
    "manager.search",
    "manager.pin",
    "manager.close",
];

/// The compiled-in manifest.
pub fn manifest() -> Manifest {
    Manifest::parse(MANIFEST_JSON.as_bytes()).expect("the compiled-in hotkey manifest is valid")
}

fn var(name: &str) -> Option<PathBuf> {
    std::env::var_os(name)
        .filter(|v| !v.is_empty())
        .map(PathBuf::from)
}

/// `%APPDATA%\mouv` (Linux, M4: `$XDG_CONFIG_HOME/mouv`): every Kim tool's settings folder.
pub fn roaming_root() -> Option<PathBuf> {
    let base = if cfg!(windows) {
        var("APPDATA")
    } else {
        var("XDG_CONFIG_HOME")
    }?;
    Some(base.join("mouv"))
}

/// `%APPDATA%\mouv\subak` (Linux, M4: `$XDG_CONFIG_HOME/mouv/subak`), from the environment, so a
/// test can point it at a sandbox. `None` when the variable is not set (nothing is persisted).
pub fn config_dir() -> Option<PathBuf> {
    roaming_root().map(|r| r.join(TOOL))
}

/// Kim's per-user tool installs (`%LOCALAPPDATA%\Programs\mouv`; ADR 0006), read by
/// `hotkeys check --all-tools`.
pub fn programs_dir() -> Option<PathBuf> {
    if cfg!(windows) {
        var("LOCALAPPDATA").map(|d| d.join("Programs").join("mouv"))
    } else {
        var("XDG_DATA_HOME").map(|d| d.join("mouv").join("programs"))
    }
}

/// `hotkeys.json` in `dir`.
pub fn layer_path(dir: &Path) -> PathBuf {
    dir.join(store::FILE_NAME)
}

/// The tool state the checks and the window use (safe mode for a broken or newer file).
pub fn state(manifest: Manifest, loaded: &Loaded) -> ToolState {
    let mut s = ToolState::new(manifest, loaded.layer.clone());
    s.safe_mode = loaded.state.safe_mode();
    s
}

/// Subak's user layer from `dir` (`None`: no settings folder, the defaults apply) and its state.
pub fn load(dir: Option<&Path>) -> (ToolState, Loaded) {
    let m = manifest();
    let loaded = match dir {
        Some(d) => store::load(&layer_path(d), &m),
        // No settings folder at all: the defaults, without touching the file system.
        None => Loaded {
            path: PathBuf::from(store::FILE_NAME),
            state: ConfigState::Default,
            layer: None,
            problems: Vec::new(),
            migrated_from: None,
        },
    };
    (state(m, &loaded), loaded)
}

/// A sentence for a file that puts Subak's shortcuts in safe mode, or `None`.
pub fn safe_mode_warning(loaded: &Loaded) -> Option<String> {
    match loaded.state {
        ConfigState::Invalid => Some(format!(
            "{} is invalid ({}); Subak uses its default shortcuts until it is fixed or reset with `subak hotkeys reset --all --yes`",
            loaded.path.display(),
            loaded
                .problems
                .first()
                .map_or("unknown problem", String::as_str)
        )),
        ConfigState::NewerVersion => Some(format!(
            "{} was written by a newer version and is read-only; Subak uses its default shortcuts",
            loaded.path.display()
        )),
        _ => None,
    }
}

/// Why every shortcut is off for this run (`--no-hotkeys`, `KIM_NO_HOTKEYS`; §8.3 4), if it is.
pub fn off_reason(no_hotkeys_flag: bool) -> Option<&'static str> {
    if no_hotkeys_flag {
        Some("--no-hotkeys")
    } else if kim_hotkeys::no_hotkeys_env() {
        Some("KIM_NO_HOTKEYS")
    } else {
        None
    }
}

/// Another installed Kim tool, as read from its files.
#[derive(Debug, Clone)]
pub struct Discovered {
    pub entry: ToolEntry,
    pub state: ToolState,
}

/// Every other installed Kim tool that ships a valid hotkey manifest, with its user layer, plus
/// a warning per tool that could not be checked (`hotkeys check --all-tools`, §9.2). Reads
/// Kim's install records (`<programs>\<tool>\current.json`), the installed version's
/// `hotkeys.manifest.json` and the tool's `hotkeys.json`; starts nothing, registers nothing.
pub fn installed(programs: &Path, roaming: &Path) -> (Vec<Discovered>, Vec<String>) {
    let mut found = Vec::new();
    let mut warnings = Vec::new();
    let Ok(rd) = std::fs::read_dir(programs) else {
        return (found, warnings);
    };
    let mut ids: Vec<String> = rd
        .flatten()
        .filter(|e| e.path().is_dir())
        .map(|e| e.file_name().to_string_lossy().into_owned())
        .filter(|id| !id.starts_with('.') && id != "bin" && id != TOOL)
        .collect();
    ids.sort();
    for id in ids {
        let dir = programs.join(&id);
        let record = match std::fs::read(dir.join("current.json")) {
            Ok(b) => b,
            Err(_) if dir.join("versions").is_dir() => {
                warnings.push(format!(
                    "{id}: damaged install; its shortcuts were not checked"
                ));
                continue;
            }
            Err(_) => continue,
        };
        let Some(version) = serde_json::from_slice::<Value>(&record)
            .ok()
            .and_then(|v| v["version"].as_str().map(str::to_string))
        else {
            warnings.push(format!(
                "{id}: unreadable install record; its shortcuts were not checked"
            ));
            continue;
        };
        let path = dir
            .join("versions")
            .join(&version)
            .join(kim_hotkeys::manifest::MANIFEST_FILE);
        let Ok(bytes) = std::fs::read(&path) else {
            warnings.push(format!(
                "{id} {version} ships no hotkey manifest; its shortcuts were not checked"
            ));
            continue;
        };
        let m = match Manifest::parse(&bytes) {
            Ok(m) if m.tool == id && m.version == version => m,
            Ok(m) => {
                warnings.push(format!(
                    "{id}: {} describes {} {} instead of {id} {version}; it is not trusted",
                    path.display(),
                    m.tool,
                    m.version
                ));
                continue;
            }
            Err(e) => {
                warnings.push(format!(
                    "{id}: {} is not a usable hotkey manifest ({e}); its shortcuts were not checked",
                    path.display()
                ));
                continue;
            }
        };
        let loaded = store::load(&roaming.join(&id).join(store::FILE_NAME), &m);
        let st = state(m, &loaded);
        found.push(Discovered {
            entry: ToolEntry {
                tool: id.clone(),
                version: Some(version),
                source: "manifest",
                enabled: st.enabled(),
            },
            state: st,
        });
    }
    (found, warnings)
}

#[cfg(test)]
mod tests {
    use super::*;
    use kim_hotkeys::check::{Severity, analyze};
    use serde_json::json;

    fn final_manifest() -> Value {
        serde_json::from_slice(
            &std::fs::read(
                Path::new(env!("CARGO_MANIFEST_DIR"))
                    .join("../../docs/contracts/examples/kim-hotkey-manifest-v1/subak.json"),
            )
            .unwrap(),
        )
        .unwrap()
    }

    #[test]
    fn the_shipped_manifest_is_the_final_one_filtered_to_what_this_build_implements() {
        let m = manifest();
        assert_eq!(
            m.version,
            env!("CARGO_PKG_VERSION"),
            "bump hotkeys.manifest.json with the crate"
        );
        let fin = final_manifest();
        let leader_free = |d: &Value| {
            let keys = d.as_str().map_or_else(|| d["keys"].as_str(), Some);
            !keys.unwrap_or("").contains('@')
        };
        let actions: Vec<Value> = fin["actions"]
            .as_array()
            .unwrap()
            .iter()
            .filter(|a| IMPLEMENTED.contains(&a["id"].as_str().unwrap()))
            .map(|a| {
                let mut a = a.clone();
                let d: Vec<Value> = a["defaults"]
                    .as_array()
                    .unwrap()
                    .iter()
                    .filter(|d| leader_free(d))
                    .cloned()
                    .collect();
                a["defaults"] = Value::Array(d);
                a
            })
            .collect();
        let contexts: Vec<Value> = fin["contexts"]
            .as_array()
            .unwrap()
            .iter()
            .filter(|c| c["passthrough"] == true || actions.iter().any(|a| a["context"] == c["id"]))
            .cloned()
            .collect();
        let mut want = fin;
        want["version"] = json!(env!("CARGO_PKG_VERSION"));
        want["actions"] = Value::Array(actions);
        want["contexts"] = Value::Array(contexts);
        want.as_object_mut().unwrap().remove("leaders");
        let ours: Value = serde_json::from_str(MANIFEST_JSON).unwrap();
        assert_eq!(
            ours, want,
            "shipped = final manifest ∩ IMPLEMENTED, no @prefix yet"
        );
        let ids: Vec<&str> = m.actions.iter().map(|a| a.id.as_str()).collect();
        assert_eq!(ids, IMPLEMENTED);
        assert!(m.leaders.is_empty(), "the prefix leader is Step 23's");
        assert!(m.has_passthrough());
    }

    #[test]
    fn the_defaults_have_no_error_or_warning_and_report_the_shadowed_terminal_keys() {
        for m in [
            manifest(),
            Manifest::from_value(final_manifest()).expect("final manifest"),
        ] {
            let a = analyze(&[ToolState::new(m.clone(), None)], &[]);
            assert_eq!(a.count(Severity::Error), 0, "{:#?}", a.conflicts);
            assert_eq!(a.count(Severity::Warning), 0, "{:#?}", a.conflicts);
            let shadowed: Vec<&str> = a
                .conflicts
                .iter()
                .filter(|c| c.code.as_str() == "shadows_terminal_input")
                .map(|c| c.action.as_str())
                .collect();
            assert!(shadowed.contains(&"terminal.new"), "{shadowed:?}");
            assert!(shadowed.contains(&"terminal.close"), "{shadowed:?}");
            // Every default of the shipped manifest is active.
            for decl in m
                .actions
                .iter()
                .filter(|d| IMPLEMENTED.contains(&d.id.as_str()))
            {
                for spec in &decl.defaults {
                    if !spec.keys().contains('@') {
                        assert!(
                            a.is_active(TOOL, &decl.id, spec.keys()),
                            "{} {}",
                            decl.id,
                            spec.keys()
                        );
                    }
                }
            }
        }
    }

    #[test]
    fn a_missing_settings_folder_means_defaults_and_a_broken_file_safe_mode() {
        let (st, loaded) = load(None);
        assert_eq!(loaded.state, ConfigState::Default);
        assert!(!st.safe_mode);
        let dir = std::env::temp_dir().join(format!("subak-hotkeys-unit-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(layer_path(&dir), "{ not json").unwrap();
        let (st, loaded) = load(Some(&dir));
        assert_eq!(loaded.state, ConfigState::Invalid);
        assert!(st.safe_mode);
        assert!(
            safe_mode_warning(&loaded)
                .unwrap()
                .contains("reset --all --yes")
        );
        std::fs::write(
            layer_path(&dir),
            json!({"schema": "kim.hotkeys", "schema_version": 1, "tool": "subak",
                "overrides": [{"action": "terminal.close", "disabled": true}]})
            .to_string(),
        )
        .unwrap();
        let (st, loaded) = load(Some(&dir));
        assert_eq!(loaded.state, ConfigState::User);
        let a = analyze(&[st], &[]);
        assert!(!a.is_active(TOOL, "terminal.close", "Ctrl+W"));
        assert!(a.is_active(TOOL, "terminal.new", "Ctrl+T"));
        let _ = std::fs::remove_dir_all(&dir);
        assert_eq!(off_reason(true), Some("--no-hotkeys"));
    }
}
