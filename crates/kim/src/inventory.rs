//! Which Kim tools are installed for this user (`kim tools list`, `kim status`). Pure reads of
//! install records under `%LOCALAPPDATA%\Programs\mouv`; starts nothing and writes nothing.

use crate::install::InstallRecord;
use crate::paths::Paths;
use serde::Serialize;
use std::path::PathBuf;

/// One tool installed by Kim.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct InstalledTool {
    pub id: String,
    /// Active version; `None` when the install is damaged.
    pub version: Option<String>,
    /// Absolute path of the active version's executable.
    pub entrypoint: Option<PathBuf>,
    /// Directory of the active version (where a `capabilities.json` manifest would be).
    pub version_dir: Option<PathBuf>,
    pub installed_at: Option<String>,
    /// Why the install record cannot be used, when it cannot.
    pub damaged: Option<String>,
}

/// Every tool with an install record (or leftover versions) under the programs directory,
/// sorted by id. Kim itself is not listed here; it is the running program.
pub fn installed_tools(paths: &Paths) -> Vec<InstalledTool> {
    let Ok(rd) = std::fs::read_dir(&paths.programs) else {
        return Vec::new();
    };
    let mut tools: Vec<InstalledTool> = rd
        .flatten()
        .filter(|e| e.path().is_dir())
        .filter_map(|e| {
            let id = e.file_name().to_string_lossy().into_owned();
            // `.staging` and other dot-dirs are Kim's scratch space; `bin` holds launchers.
            if id.starts_with('.') || id == "bin" {
                return None;
            }
            installed_tool(paths, &id)
        })
        .collect();
    tools.sort_by(|a, b| a.id.cmp(&b.id));
    tools
}

/// The install state of one tool, or `None` if nothing of it is installed.
pub fn installed_tool(paths: &Paths, id: &str) -> Option<InstalledTool> {
    let dir = paths.tool_dir(id);
    let record_path = paths.install_record(id);
    let damaged = |why: String| InstalledTool {
        id: id.to_string(),
        version: None,
        entrypoint: None,
        version_dir: None,
        installed_at: None,
        damaged: Some(why),
    };
    match std::fs::read(&record_path) {
        Ok(bytes) => match serde_json::from_slice::<InstallRecord>(&bytes) {
            Ok(r) if semver::Version::parse(&r.version).is_ok() => Some(InstalledTool {
                id: id.to_string(),
                entrypoint: Some(crate::install::entrypoint_path(&dir, &r.entrypoint)),
                version_dir: Some(dir.join("versions").join(&r.version)),
                installed_at: Some(r.installed_at),
                version: Some(r.version),
                damaged: None,
            }),
            Ok(r) => Some(damaged(format!(
                "{}: version `{}` is not SemVer",
                record_path.display(),
                r.version
            ))),
            Err(e) => Some(damaged(format!("{}: {e}", record_path.display()))),
        },
        Err(_) if dir.join("versions").is_dir() => Some(damaged(format!(
            "{} is missing (interrupted install or uninstall)",
            record_path.display()
        ))),
        Err(_) => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lists_installed_and_damaged_tools_and_ignores_scratch_dirs() {
        let root = std::env::temp_dir().join(format!("kim-inventory-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let paths = Paths::under(&root);
        assert!(installed_tools(&paths).is_empty(), "no programs dir yet");

        let record = serde_json::json!({
            "schema": "kim.install", "schema_version": 1, "tool": "good", "version": "1.2.3",
            "entrypoint": "versions/1.2.3/good.exe", "installed_at": "2026-11-04T00:00:00Z",
            "sha256": "00", "source_url": "https://x/y.zip", "catalog_version": 1,
            "previous_version": null
        });
        std::fs::create_dir_all(paths.tool_dir("good")).unwrap();
        std::fs::write(paths.install_record("good"), record.to_string()).unwrap();
        std::fs::create_dir_all(paths.tool_dir("broken")).unwrap();
        std::fs::write(paths.install_record("broken"), b"{").unwrap();
        std::fs::create_dir_all(paths.tool_dir("half").join("versions")).unwrap();
        std::fs::create_dir_all(paths.tool_dir("empty")).unwrap();
        std::fs::create_dir_all(paths.staging_dir()).unwrap();

        let tools = installed_tools(&paths);
        let ids: Vec<&str> = tools.iter().map(|t| t.id.as_str()).collect();
        assert_eq!(ids, ["broken", "good", "half"]);
        let good = &tools[1];
        assert_eq!(good.version.as_deref(), Some("1.2.3"));
        assert!(good.entrypoint.as_ref().unwrap().ends_with("good.exe"));
        assert!(good.version_dir.as_ref().unwrap().ends_with("1.2.3"));
        assert!(tools[0].damaged.is_some() && tools[2].damaged.is_some());
        assert!(installed_tool(&paths, "empty").is_none());
        let _ = std::fs::remove_dir_all(root);
    }
}
