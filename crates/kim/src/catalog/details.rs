//! What Kim shows about one catalog tool beyond its status (docs/CATALOG_FORMAT.md §2.1, §7).
//!
//! Two kinds of information meet here:
//! * what the **signed catalog declares**: features, how the program runs, entry points, links,
//!   a summary of the default shortcuts and the permissions it needs (`KIM-FR-050`);
//! * what **Kim decides itself** and a catalog can never change: where the tool is installed
//!   (per user, fixed by tool id), that installs and updates are manual only, and, for an
//!   installed tool, the shortcuts its *installed* hotkey manifest and the user's `hotkeys.json`
//!   define (`KIM-FR-052`, HOTKEY_CONTRACT §9.2 discovery rules).
//!
//! Everything here is a pure read of files: nothing is started, registered or written.

use super::{EntryPoint, KNOWN_PERMISSIONS, KNOWN_RUNS, Link, Shortcuts, Tool};
use crate::hotkeys;
use crate::inventory;
use crate::paths::Paths;
use kim_hotkeys::check::ToolState;
use kim_hotkeys::manifest::{ActionDecl, MANIFEST_FILE, ScopeKind};
use kim_hotkeys::view;
use serde::Serialize;
use std::path::PathBuf;

/// Kim's label for a permission id, or `None` when this Kim does not know the id (the catalog's
/// `detail` is then all there is to show).
pub fn permission_label(id: &str) -> Option<&'static str> {
    KNOWN_PERMISSIONS
        .iter()
        .find(|(k, _)| *k == id)
        .map(|(_, l)| *l)
}

/// Kim's description of a `runs` value, or `None` for a value this Kim does not know.
pub fn runs_label(runs: &str) -> Option<&'static str> {
    KNOWN_RUNS.iter().find(|(k, _)| *k == runs).map(|(_, l)| *l)
}

/// A declared permission as Kim shows it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct PermissionView {
    pub id: String,
    /// Kim's own wording; `null` for an id this Kim does not know.
    pub label: Option<&'static str>,
    pub detail: String,
    pub opt_in: bool,
}

/// The permissions a tool declares, in catalog order.
pub fn permissions(tool: &Tool) -> Vec<PermissionView> {
    tool.permissions
        .iter()
        .map(|p| PermissionView {
            id: p.id.clone(),
            label: permission_label(&p.id),
            detail: p.detail.clone(),
            opt_in: p.opt_in,
        })
        .collect()
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct RunsView {
    pub id: String,
    pub label: Option<&'static str>,
}

/// Where Kim puts a tool. Fixed by Kim for each tool id and never taken from catalog data (a
/// catalog cannot choose a path); always per user, never elevated (`KIM-SEC-004`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct InstallLocations {
    /// Program files (`%LOCALAPPDATA%\Programs\mouv\<tool>`), removed by an uninstall.
    pub program_dir: PathBuf,
    /// Settings and state, kept by an uninstall unless the user also confirms deleting them
    /// (`KIM-SEC-005`).
    pub user_data_dirs: Vec<PathBuf>,
    pub per_user: bool,
    pub elevation: bool,
}

/// `None` for Kim itself, which Kim does not install.
pub fn install_locations(paths: &Paths, tool_id: &str) -> Option<InstallLocations> {
    (tool_id != hotkeys::TOOL).then(|| InstallLocations {
        program_dir: paths.tool_dir(tool_id),
        user_data_dirs: paths.user_data_dirs(tool_id),
        per_user: true,
        elevation: false,
    })
}

/// The only ways a tool is installed, updated or removed: these commands, or the matching
/// buttons in Kim Manager. Nothing is automatic (`KIM-SEC-001`, `KIM-SEC-002`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ManualActions {
    /// Always `false`: no Kim tool is installed or updated by itself.
    pub automatic_updates: bool,
    pub install: String,
    pub update: String,
    pub uninstall: String,
    /// Shows available updates; never downloads a package.
    pub check: String,
}

/// `None` for Kim itself, which never installs, updates or removes itself.
pub fn manual_actions(tool_id: &str) -> Option<ManualActions> {
    (tool_id != hotkeys::TOOL).then(|| ManualActions {
        automatic_updates: false,
        install: format!("kim install {tool_id}"),
        update: format!("kim update {tool_id}"),
        uninstall: format!("kim uninstall {tool_id}"),
        check: "kim update check".into(),
    })
}

/// One action of a shortcut list.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ShortcutRow {
    pub action: String,
    pub title: String,
    /// `global`, `app` or `context:<id>`.
    pub scope: String,
    /// Current bindings (canonical key syntax); empty when unbound or disabled.
    pub keys: Vec<String>,
    pub bindable: bool,
    /// `default`, `user`, `profile`, `disabled` or `unbound` (HOTKEY_CONTRACT §10.2).
    pub state: String,
}

/// A tool's shortcuts: from its installed manifest and the user's `hotkeys.json` when it is
/// installed (`source: "installed"`), otherwise the catalog's summary (`source: "catalog"`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ShortcutsView {
    pub source: &'static str,
    pub manifest: String,
    pub backend: String,
    /// The tool-wide switch (`<tool> hotkeys disable --all` turns it off).
    pub enabled: bool,
    pub actions: Vec<ShortcutRow>,
    /// Why the installed shortcuts could not be read, or that they are off or in safe mode.
    pub note: Option<String>,
}

/// The shortcuts to show for `tool`, or `None` when neither the catalog nor an installed
/// manifest declares any. Kim's own come from its compiled-in manifest.
pub fn shortcuts(paths: &Paths, tool: &Tool) -> Option<ShortcutsView> {
    let installed = if tool.id == hotkeys::TOOL {
        let (state, loaded) = hotkeys::load(paths);
        Some(Ok((
            state,
            hotkeys::safe_mode_warning(hotkeys::TOOL, &loaded),
        )))
    } else {
        inventory::installed_tool(paths, &tool.id)
            .map(|t| hotkeys::discover(paths, &t).map(|(d, w)| (d.state, w)))
    };
    match installed {
        Some(Ok((state, warning))) => Some(from_state(&state, warning)),
        Some(Err(why)) => tool.shortcuts.as_ref().map(|s| ShortcutsView {
            note: Some(format!("{why}; showing the catalog's defaults")),
            ..from_catalog(s)
        }),
        None => tool.shortcuts.as_ref().map(from_catalog),
    }
}

fn from_catalog(s: &Shortcuts) -> ShortcutsView {
    ShortcutsView {
        source: "catalog",
        manifest: s.manifest.clone(),
        backend: s.backend.clone(),
        enabled: true,
        actions: s
            .actions
            .iter()
            .map(|a| ShortcutRow {
                action: a.action.clone(),
                title: a.title.clone(),
                scope: a.scope.clone(),
                keys: a.keys.clone(),
                bindable: a.bindable,
                state: if a.keys.is_empty() {
                    "unbound"
                } else {
                    "default"
                }
                .into(),
            })
            .collect(),
        note: None,
    }
}

/// The scope an action's defaults use: its context, or its first (default) scope.
pub fn declared_scope(decl: &ActionDecl) -> String {
    match (&decl.context, decl.scopes.first()) {
        (Some(c), _) => format!("context:{c}"),
        (None, Some(ScopeKind::Global)) => "global".into(),
        (None, _) => "app".into(),
    }
}

fn from_state(state: &ToolState, warning: Option<String>) -> ShortcutsView {
    let id = state.id().to_string();
    let actions = state
        .manifest
        .actions
        .iter()
        // Keys inside a capture or pin window are listed by `<tool> hotkeys list`.
        .filter(|a| a.context.is_none())
        .map(|decl| ShortcutRow {
            action: decl.id.clone(),
            title: decl.title.clone(),
            scope: declared_scope(decl),
            keys: state
                .specs(decl)
                .map(|(specs, _)| specs.iter().map(|s| s.keys().to_string()).collect())
                .unwrap_or_default(),
            bindable: decl.bindable,
            state: view::action_source(state, decl).into(),
        })
        .collect();
    let note = warning.or_else(|| {
        (!state.enabled()).then(|| {
            format!("all of {id}'s shortcuts are turned off (`{id} hotkeys enable --all`)")
        })
    });
    ShortcutsView {
        source: "installed",
        manifest: MANIFEST_FILE.into(),
        backend: serde_json::to_value(state.manifest.backend)
            .ok()
            .and_then(|v| v.as_str().map(str::to_string))
            .unwrap_or_default(),
        enabled: state.enabled(),
        actions,
        note,
    }
}

/// Everything `kim catalog show` and Kim Manager show about a tool besides its status.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Details {
    pub features: Vec<String>,
    pub runs: Option<RunsView>,
    pub entry_points: Vec<EntryPoint>,
    pub links: Vec<Link>,
    pub permissions: Vec<PermissionView>,
    pub shortcuts: Option<ShortcutsView>,
    pub install: Option<InstallLocations>,
    pub manage: Option<ManualActions>,
}

pub fn details(paths: &Paths, tool: &Tool) -> Details {
    Details {
        features: tool.features.clone(),
        runs: tool.runs.as_ref().map(|r| RunsView {
            id: r.clone(),
            label: runs_label(r),
        }),
        entry_points: tool.entry_points.clone(),
        links: tool.links.clone(),
        permissions: permissions(tool),
        shortcuts: shortcuts(paths, tool),
        install: install_locations(paths, &tool.id),
        manage: manual_actions(&tool.id),
    }
}

/// A one-line hint for a running tool that Kim refuses to uninstall: how the user stops it
/// themselves, from how the catalog says it runs. Kim never stops it (`KIM-FR-034`).
pub fn stop_hint(tool: &str, runs: Option<&str>) -> String {
    match runs {
        Some("tray") => format!(
            "{tool} keeps running in the notification area; quit it from its tray icon yourself, then retry"
        ),
        Some("host") => {
            format!("{tool} keeps a background host; stop it yourself first, then retry")
        }
        _ if tool == "subak" => {
            "stop it yourself first (e.g. end your Subak sessions), then retry".to_string()
        }
        _ => format!("close {tool} first, then retry"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::catalog::{Permission, ShortcutSummary, Support};

    fn tool(id: &str) -> Tool {
        Tool {
            id: id.into(),
            name: "Noeul".into(),
            summary: "s".into(),
            description: "d".into(),
            homepage: None,
            license: "MIT".into(),
            support: Support::InDevelopment,
            releases: vec![],
            features: vec!["F1 capture".into()],
            runs: Some("tray".into()),
            entry_points: vec![],
            links: vec![],
            shortcuts: Some(Shortcuts {
                manifest: MANIFEST_FILE.into(),
                backend: "register".into(),
                actions: vec![
                    ShortcutSummary {
                        action: "capture.region".into(),
                        title: "Capture a region".into(),
                        scope: "global".into(),
                        keys: vec!["F1".into()],
                        bindable: true,
                    },
                    ShortcutSummary {
                        action: "editor.open".into(),
                        title: "Open".into(),
                        scope: "app".into(),
                        keys: vec![],
                        bindable: true,
                    },
                ],
            }),
            permissions: vec![
                Permission {
                    id: "screen-capture".into(),
                    detail: "only while you capture".into(),
                    opt_in: false,
                },
                Permission {
                    id: "teleportation".into(),
                    detail: "a permission this Kim does not know".into(),
                    opt_in: true,
                },
            ],
        }
    }

    #[test]
    fn declared_metadata_and_kim_decided_facts_are_combined() {
        let root = std::env::temp_dir().join(format!("kim-details-{}", std::process::id()));
        let paths = Paths::under(&root);
        let d = details(&paths, &tool("noeul"));
        assert_eq!(d.runs.as_ref().unwrap().label, runs_label("tray"));
        assert_eq!(d.permissions[0].label, Some("Captures the screen"));
        assert_eq!(d.permissions[1].label, None, "unknown ids are shown as is");
        assert!(d.permissions[1].opt_in);
        let s = d.shortcuts.unwrap();
        assert_eq!((s.source, s.actions.len()), ("catalog", 2));
        assert_eq!(s.actions[0].state, "default");
        assert_eq!(s.actions[1].state, "unbound");
        let install = d.install.unwrap();
        assert_eq!(install.program_dir, paths.tool_dir("noeul"));
        assert!(install.per_user && !install.elevation);
        let manage = d.manage.unwrap();
        assert!(!manage.automatic_updates);
        assert_eq!(manage.update, "kim update noeul");
        assert!(!root.exists(), "computing details writes nothing");

        // Kim itself: not installed by Kim, never updated by Kim; its shortcuts are its own.
        let k = details(
            &paths,
            &Tool {
                shortcuts: None,
                ..tool("kim")
            },
        );
        assert!(k.install.is_none() && k.manage.is_none());
        let ks = k.shortcuts.unwrap();
        assert_eq!(ks.source, "installed");
        assert!(ks.actions.iter().any(|a| a.action == "catalog.refresh"));
        assert!(!root.exists());
    }

    #[test]
    fn stop_hints_follow_how_the_tool_runs() {
        assert!(stop_hint("noeul", Some("tray")).contains("tray icon"));
        assert!(stop_hint("subak", None).contains("Subak sessions"));
        assert!(stop_hint("x", Some("window")).contains("close x"));
    }
}
