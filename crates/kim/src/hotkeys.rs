//! Kim Manager's keyboard shortcuts and read-only cross-tool hotkey discovery
//! (docs/HOTKEY_CONTRACT.md §3.4, §8.1, §9.2, §13.2; ADR 0017).
//!
//! The rules live in the shared `kim-hotkeys` library. This module adds what is specific to
//! Kim: the compiled-in manifest of Kim Manager (tool ID `kim`, backend `focus`), where Kim's
//! `hotkeys.json` lives, and how `kim hotkeys check --all-tools` finds the other installed tools'
//! manifests and user layers. Discovery reads files only: it never starts a tool, never
//! registers a hotkey, never installs a hook and runs no daemon (`KIM-FR-045`, `KIM-NFR-020`).

use crate::inventory;
use crate::paths::Paths;
use kim_hotkeys::check::{ToolEntry, ToolState};
use kim_hotkeys::manifest::{MANIFEST_FILE, Manifest};
use kim_hotkeys::store::{self, ConfigState, Loaded};
use std::path::PathBuf;

pub use kim_hotkeys;

/// Kim Manager's hotkey manifest as shipped (`hotkeys.manifest.json`; release packaging copies
/// this file next to `capabilities.json`). Its `version` must equal the crate version (a test
/// checks it).
pub const MANIFEST_JSON: &str = include_str!("hotkeys.manifest.json");

/// The tool ID of Kim's own shortcuts (§13.2).
pub const TOOL: &str = "kim";

/// Kim Manager's manifest.
pub fn manifest() -> Manifest {
    Manifest::parse(MANIFEST_JSON.as_bytes()).expect("the compiled-in hotkey manifest is valid")
}

/// `%APPDATA%\mouv\<tool>\hotkeys.json` (Linux: `$XDG_CONFIG_HOME/mouv/<tool>/hotkeys.json`).
pub fn layer_path(paths: &Paths, tool: &str) -> PathBuf {
    paths.roaming_data.join(tool).join(store::FILE_NAME)
}

/// Kim's user layer and the tool state the checks use (safe mode for a broken or newer file).
pub fn load(paths: &Paths) -> (ToolState, Loaded) {
    let m = manifest();
    let loaded = store::load(&layer_path(paths, TOOL), &m);
    (state(m, &loaded), loaded)
}

fn state(manifest: Manifest, loaded: &Loaded) -> ToolState {
    let mut s = ToolState::new(manifest, loaded.layer.clone());
    s.safe_mode = loaded.state.safe_mode();
    s
}

/// A human sentence for a file that puts a tool in safe mode, or `None`.
pub fn safe_mode_warning(tool: &str, loaded: &Loaded) -> Option<String> {
    match loaded.state {
        ConfigState::Invalid => Some(format!(
            "{} is invalid ({}); {tool} uses its default shortcuts and registers nothing global until it is fixed or reset with `{tool} hotkeys reset --all --yes`",
            loaded.path.display(),
            loaded
                .problems
                .first()
                .map_or("unknown problem", String::as_str)
        )),
        ConfigState::NewerVersion => Some(format!(
            "{} was written by a newer version and is read-only; {tool} uses its default shortcuts",
            loaded.path.display()
        )),
        _ => None,
    }
}

/// Another installed tool, as read from its files.
#[derive(Debug, Clone)]
pub struct Discovered {
    pub entry: ToolEntry,
    pub state: ToolState,
}

/// Every installed tool that ships a valid hotkey manifest, with its user layer, plus a warning
/// for each tool that could not be checked. Reads install records, manifests and
/// `hotkeys.json` files only.
pub fn installed(paths: &Paths) -> (Vec<Discovered>, Vec<String>) {
    let mut found = Vec::new();
    let mut warnings = Vec::new();
    for t in inventory::installed_tools(paths) {
        if t.id == TOOL {
            continue;
        }
        match discover(paths, &t) {
            Ok((d, warning)) => {
                warnings.extend(warning);
                found.push(d);
            }
            Err(w) => warnings.push(w),
        }
    }
    (found, warnings)
}

/// The shortcuts of one installed tool, from its installed `hotkeys.manifest.json` (which must
/// describe exactly this tool and version) and its `hotkeys.json`. `Err` says why they cannot be
/// read; the `Ok` warning names a user layer in safe mode. Reads files only.
pub fn discover(
    paths: &Paths,
    t: &inventory::InstalledTool,
) -> Result<(Discovered, Option<String>), String> {
    let (Some(version), Some(dir)) = (t.version.as_deref(), t.version_dir.as_ref()) else {
        return Err(format!(
            "{}: damaged install; its shortcuts were not checked ({})",
            t.id,
            t.damaged.as_deref().unwrap_or("no install record")
        ));
    };
    let path = dir.join(MANIFEST_FILE);
    let bytes = match std::fs::read(&path) {
        Ok(b) => b,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            return Err(format!(
                "{} {version} ships no hotkey manifest; its shortcuts were not checked",
                t.id
            ));
        }
        Err(e) => return Err(format!("{}: cannot read {}: {e}", t.id, path.display())),
    };
    let m = Manifest::parse(&bytes).map_err(|e| {
        format!(
            "{}: {} is not a usable hotkey manifest ({e}); its shortcuts were not checked",
            t.id,
            path.display()
        )
    })?;
    if m.tool != t.id || m.version != version {
        return Err(format!(
            "{}: {} describes {} {} instead of {} {version}; it is not trusted",
            t.id,
            path.display(),
            m.tool,
            m.version,
            t.id
        ));
    }
    let loaded = store::load(&layer_path(paths, &t.id), &m);
    let warning = safe_mode_warning(&t.id, &loaded);
    let state = state(m, &loaded);
    Ok((
        Discovered {
            entry: ToolEntry {
                tool: t.id.clone(),
                version: Some(version.to_string()),
                source: "manifest",
                enabled: state.enabled(),
            },
            state,
        },
        warning,
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::Value;

    #[test]
    fn the_manifest_is_the_contract_example_with_this_version() {
        let m = manifest();
        assert_eq!(
            m.version,
            crate::KIM_VERSION,
            "bump hotkeys.manifest.json with the crate"
        );
        let example: Value = serde_json::from_slice(
            &std::fs::read(
                std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
                    .join("../../docs/contracts/examples/kim-hotkey-manifest-v1/kim.json"),
            )
            .unwrap(),
        )
        .unwrap();
        let mut ours: Value = serde_json::from_str(MANIFEST_JSON).unwrap();
        ours["version"] = example["version"].clone();
        assert_eq!(
            ours, example,
            "Kim Manager's actions are those of the contract (§13.2)"
        );
    }
}
