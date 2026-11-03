//! Loading and saving `hotkeys.json` (docs/HOTKEY_CONTRACT.md §8.1, §8.3, §8.4).
//!
//! - A missing file means defaults.
//! - An unreadable or invalid file puts the tool in **safe mode**: defaults for `app`/`context`,
//!   nothing global, and the file is never rewritten automatically (`KIM-SEC-023`).
//! - A file from a newer `schema_version` is read-only (`hotkeys_from_newer_version`).
//! - An older file is migrated by a chain of pure `vN → vN+1` functions; the first write after a
//!   migration keeps a backup (`hotkeys.json.bak`). Version 1 is the first version, so the
//!   shipped chain is empty; the mechanism is tested with a test chain.
//! - Writes are atomic: a temporary file is written and flushed, then renamed over the old file
//!   (ADR 0006).

use crate::layer::{LAYER_VERSION, UserLayer};
use crate::manifest::Manifest;
use serde_json::Value;
use std::io::Write;
use std::path::{Path, PathBuf};

/// File name of the user layer in a tool's roaming data folder.
pub const FILE_NAME: &str = "hotkeys.json";
/// The backup kept before a reset, an import or a migrated write.
pub const BACKUP_SUFFIX: &str = ".bak";

/// `config.state` of `hotkeys list` (§10.2).
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum ConfigState {
    /// No file: defaults apply.
    Default,
    /// A valid user layer.
    User,
    /// Unreadable or invalid: safe mode.
    Invalid,
    /// Written by a newer version: read-only, safe mode.
    NewerVersion,
}

impl ConfigState {
    pub fn as_str(self) -> &'static str {
        match self {
            ConfigState::Default => "default",
            ConfigState::User => "user",
            ConfigState::Invalid => "invalid",
            ConfigState::NewerVersion => "newer_version",
        }
    }

    /// Whether the tool must run in safe mode.
    pub fn safe_mode(self) -> bool {
        matches!(self, ConfigState::Invalid | ConfigState::NewerVersion)
    }
}

/// One migration step `vN → vN+1` of the raw document.
pub type Migration = fn(Value) -> Result<Value, String>;

/// The shipped migration chain. Empty: `schema_version` 1 is the first version.
pub const MIGRATIONS: &[Migration] = &[];

/// A loaded user layer.
#[derive(Clone, PartialEq, Debug)]
pub struct Loaded {
    pub path: PathBuf,
    pub state: ConfigState,
    /// The valid layer (`None` for no file and in safe mode).
    pub layer: Option<UserLayer>,
    /// Why the file is invalid or read-only (`path: problem` lines).
    pub problems: Vec<String>,
    /// The version the file had before it was migrated in memory, if it was.
    pub migrated_from: Option<u64>,
}

impl Loaded {
    /// The backup path next to the file.
    pub fn backup_path(&self) -> PathBuf {
        backup_path(&self.path)
    }
}

pub fn backup_path(path: &Path) -> PathBuf {
    let mut s = path.as_os_str().to_owned();
    s.push(BACKUP_SUFFIX);
    PathBuf::from(s)
}

/// Loads `path` for `manifest`'s tool with the shipped migrations.
pub fn load(path: &Path, manifest: &Manifest) -> Loaded {
    load_with(path, manifest, MIGRATIONS)
}

/// Loads `path`; `chain[i]` migrates version `LAYER_VERSION - chain.len() + i` to the next.
pub fn load_with(path: &Path, manifest: &Manifest, chain: &[Migration]) -> Loaded {
    let mut out = Loaded {
        path: path.to_path_buf(),
        state: ConfigState::Default,
        layer: None,
        problems: Vec::new(),
        migrated_from: None,
    };
    let invalid = |mut out: Loaded, why: String| {
        out.state = ConfigState::Invalid;
        out.problems.push(why);
        out
    };
    let bytes = match std::fs::read(path) {
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return out,
        Err(e) => return invalid(out, format!("cannot read the file: {e}")),
        Ok(b) => b,
    };
    let mut doc: Value = match serde_json::from_slice(&bytes) {
        Ok(v) => v,
        Err(e) => return invalid(out, format!("not valid JSON: {e}")),
    };
    let Some(version) = doc.get("schema_version").and_then(Value::as_u64) else {
        return invalid(out, "schema_version: missing or not a number".into());
    };
    if version > LAYER_VERSION {
        out.state = ConfigState::NewerVersion;
        out.problems.push(format!(
            "schema_version {version} was written by a newer version (this one reads {LAYER_VERSION}); the file is read-only"
        ));
        return out;
    }
    let oldest = LAYER_VERSION.saturating_sub(chain.len() as u64);
    if version < oldest {
        return invalid(out, format!("schema_version {version} is not supported"));
    }
    if version < LAYER_VERSION {
        for (i, step) in chain.iter().enumerate().skip((version - oldest) as usize) {
            match step(doc) {
                Ok(d) => doc = d,
                Err(e) => {
                    return invalid(
                        out,
                        format!("migrating from schema_version {}: {e}", oldest + i as u64),
                    );
                }
            }
        }
        out.migrated_from = Some(version);
    }
    let layer: UserLayer = match serde_json::from_value(doc) {
        Ok(l) => l,
        Err(e) => return invalid(out, e.to_string()),
    };
    let problems = layer.problems(&manifest.tool, Some(manifest));
    if !problems.is_empty() {
        out.state = ConfigState::Invalid;
        out.problems = problems;
        return out;
    }
    out.state = ConfigState::User;
    out.layer = Some(layer);
    out
}

/// Reloads a running tool's file after a change notification (§8.3 5). A valid (or removed)
/// file replaces `current`. An invalid or newer file is reported and the previous effective
/// bindings stay: the result is `current` again, with the new problems returned separately.
pub fn reload(current: &Loaded, manifest: &Manifest) -> (Loaded, Vec<String>) {
    let next = load(&current.path, manifest);
    if next.state.safe_mode() && !current.state.safe_mode() {
        let problems = next.problems;
        (current.clone(), problems)
    } else {
        (next, Vec::new())
    }
}

/// Writes `layer` to `path` atomically. With `backup`, an existing file is first copied to
/// `hotkeys.json.bak` (replacing an older backup). Returns the backup path when one was made.
pub fn save(path: &Path, layer: &UserLayer, backup: bool) -> std::io::Result<Option<PathBuf>> {
    let text = serde_json::to_vec_pretty(layer).map_err(std::io::Error::other)?;
    write_atomic(path, &text, backup)
}

/// Writes raw bytes atomically (also used to keep an imported document as given).
pub fn write_atomic(path: &Path, bytes: &[u8], backup: bool) -> std::io::Result<Option<PathBuf>> {
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)?;
    }
    let mut made = None;
    if backup && path.exists() {
        let b = backup_path(path);
        std::fs::copy(path, &b)?;
        made = Some(b);
    }
    let mut tmp = path.as_os_str().to_owned();
    tmp.push(".tmp");
    let tmp = PathBuf::from(tmp);
    let result = (|| {
        let mut f = std::fs::File::create(&tmp)?;
        f.write_all(bytes)?;
        f.write_all(b"\n")?;
        f.sync_all()?;
        drop(f);
        std::fs::rename(&tmp, path)
    })();
    if result.is_err() {
        // The previous file stays exactly as it was.
        let _ = std::fs::remove_file(&tmp);
    }
    result.map(|()| made)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::layer::Override;
    use serde_json::json;

    fn manifest() -> Manifest {
        let p = Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../docs/contracts/examples/kim-hotkey-manifest-v1/subak.json");
        Manifest::parse(&std::fs::read(p).unwrap()).unwrap()
    }

    struct Dir(PathBuf);
    impl Drop for Dir {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }
    fn dir(tag: &str) -> Dir {
        let d =
            std::env::temp_dir().join(format!("kim-hotkeys-store-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        Dir(d)
    }

    #[test]
    fn missing_valid_invalid_and_newer_files() {
        let d = dir("states");
        let p = d.0.join(FILE_NAME);
        let m = manifest();
        assert_eq!(load(&p, &m).state, ConfigState::Default);
        std::fs::write(&p, b"{not json").unwrap();
        let l = load(&p, &m);
        assert_eq!(l.state, ConfigState::Invalid);
        assert!(l.layer.is_none() && !l.problems.is_empty());
        // Duplicate overrides and undeclared leaders are invalid (safe mode).
        std::fs::write(
            &p,
            json!({"schema": "kim.hotkeys", "schema_version": 1, "tool": "subak",
                   "overrides": [{"action": "tab.next", "disabled": true},
                                 {"action": "tab.next", "disabled": true}]})
            .to_string(),
        )
        .unwrap();
        assert_eq!(load(&p, &m).state, ConfigState::Invalid);
        std::fs::write(
            &p,
            json!({"schema": "kim.hotkeys", "schema_version": 1, "tool": "subak",
                   "leaders": {"nope": "Ctrl+A"}})
            .to_string(),
        )
        .unwrap();
        assert_eq!(load(&p, &m).state, ConfigState::Invalid);
        std::fs::write(
            &p,
            json!({"schema": "kim.hotkeys", "schema_version": 2, "tool": "subak"}).to_string(),
        )
        .unwrap();
        let l = load(&p, &m);
        assert_eq!(l.state, ConfigState::NewerVersion);
        assert!(l.state.safe_mode());
        // Nothing above rewrote the file.
        assert!(
            std::fs::read_to_string(&p)
                .unwrap()
                .contains("\"schema_version\":2")
        );
    }

    #[test]
    fn atomic_save_with_backup() {
        let d = dir("save");
        let p = d.0.join("sub").join(FILE_NAME);
        let mut layer = UserLayer::new("subak");
        layer.set_override(None, "tab.next", Some(Override::disabled("tab.next")));
        assert_eq!(
            save(&p, &layer, true).unwrap(),
            None,
            "nothing to back up yet"
        );
        let first = std::fs::read(&p).unwrap();
        layer.enabled = Some(false);
        let b = save(&p, &layer, true).unwrap().expect("backup");
        assert_eq!(std::fs::read(&b).unwrap(), first);
        assert_eq!(load(&p, &manifest()).layer.unwrap(), layer);
        assert!(!d.0.join("sub").join("hotkeys.json.tmp").exists());
    }

    #[test]
    fn migrations_run_as_a_pure_chain() {
        fn v0_to_v1(mut v: Value) -> Result<Value, String> {
            // A fictional v0 named the list `keys`.
            let keys = v
                .as_object_mut()
                .unwrap()
                .remove("keys")
                .unwrap_or(json!([]));
            v["overrides"] = keys;
            v["schema_version"] = json!(1);
            Ok(v)
        }
        let d = dir("migrate");
        let p = d.0.join(FILE_NAME);
        let raw = json!({"schema": "kim.hotkeys", "schema_version": 0, "tool": "subak",
                         "keys": [{"action": "tab.next", "disabled": true}]});
        std::fs::write(&p, raw.to_string()).unwrap();
        // Without a chain, version 0 is not supported.
        assert_eq!(load(&p, &manifest()).state, ConfigState::Invalid);
        let l = load_with(&p, &manifest(), &[v0_to_v1]);
        assert_eq!(l.state, ConfigState::User);
        assert_eq!(l.migrated_from, Some(0));
        let layer = l.layer.unwrap();
        assert!(layer.overrides_in(None)[0].is_disabled());
        // Loading never writes; the first save after a migration keeps the old file.
        assert_eq!(std::fs::read_to_string(&p).unwrap(), raw.to_string());
        let b = save(&p, &layer, l.migrated_from.is_some())
            .unwrap()
            .unwrap();
        assert_eq!(std::fs::read_to_string(b).unwrap(), raw.to_string());
    }
}
