//! `kim hotkeys …`: Kim Manager's keyboard shortcuts from the command line
//! (docs/HOTKEY_CONTRACT.md §10, docs/KIM_CLI.md).
//!
//! Headless by construction (`KIM-FR-012`, `KIM-FR-045`): these commands read and write
//! `hotkeys.json` and read other installed tools' manifests and user layers. They never open a
//! window, never register a hotkey, never install a hook and never start a tool. A refused or
//! invalid change writes nothing, so working bindings never change by accident.

use crate::{Ctx, Outcome, table};
use clap::{Args, Subcommand, ValueEnum};
use kim::error::{ErrorCode, KimError};
use kim::hotkeys::{self as kh, Discovered};
use kim::paths::Paths;
use kim_hotkeys::check::{Conflict, Severity, ToolEntry, ToolState, analyze, report_json};
use kim_hotkeys::edit::{self, Applied, Edit, EditError, Options, Refusal, Switch};
use kim_hotkeys::keys::Sequence;
use kim_hotkeys::layer::UserLayer;
use kim_hotkeys::manifest::{BindingObject, BindingSpec, Trigger};
use kim_hotkeys::store::{self, ConfigState, Loaded};
use kim_hotkeys::view;
use serde_json::{Value, json};

#[derive(Debug, Subcommand)]
pub enum HotkeysCommand {
    /// Every action with its keys, scope, source and state (read-only)
    List(ListArgs),
    /// One action with its bindings and every conflict that involves it (read-only)
    Show(ShowArgs),
    /// Replace an action's bindings (one argument per binding; quote sequences: "Ctrl+K Ctrl+R")
    Set(SetArgs),
    /// Disable one action, all shortcuts (--all) or a scope (--scope)
    Disable(DisableArgs),
    /// Turn all shortcuts (--all) or a scope (--scope) back on; per action use `reset`
    Enable(EnableArgs),
    /// Restore an action's default, or discard every change (--all, keeps a backup)
    Reset(ResetArgs),
    /// The conflict report; --all-tools adds every installed Kim tool (reads files only)
    Check(CheckArgs),
    /// Print the user layer (`kim.hotkeys` document) (read-only)
    Export,
    /// Validate a `kim.hotkeys` document and replace the user layer with it (keeps a backup)
    Import(ImportArgs),
}

#[derive(Debug, Args)]
pub struct ListArgs {
    /// Only actions with a binding in this scope: global, app or context[:<id>]
    #[arg(long, value_name = "SCOPE")]
    scope: Option<String>,
    /// Show the bindings with this profile active
    #[arg(long, value_name = "NAME")]
    profile: Option<String>,
}

#[derive(Debug, Args)]
pub struct ShowArgs {
    /// Action ID, e.g. `catalog.refresh`
    action: String,
    /// Show the bindings with this profile active
    #[arg(long, value_name = "NAME")]
    profile: Option<String>,
}

#[derive(Debug, Clone, Copy, ValueEnum)]
enum BindScope {
    Global,
    App,
}

#[derive(Debug, Clone, Copy, ValueEnum)]
enum TriggerOpt {
    Press,
    Release,
    Hold,
}

#[derive(Debug, Args)]
pub struct SetArgs {
    /// Action ID, e.g. `catalog.refresh` (or `@<leader>` for a tool with leaders)
    action: String,
    /// New bindings, e.g. `Ctrl+R` (aliases such as `ctrl+r` or `C-r` are accepted)
    #[arg(required = true, value_name = "KEYS")]
    keys: Vec<String>,
    /// Scope of the new bindings (default: the action's first scope)
    #[arg(long, value_enum)]
    scope: Option<BindScope>,
    /// When the binding fires (default: press)
    #[arg(long, value_enum)]
    trigger: Option<TriggerOpt>,
    /// With --trigger hold: how long to hold, 200-5000 ms
    #[arg(long, value_name = "MS", requires = "trigger")]
    hold_ms: Option<u64>,
    /// Let auto-repeat fire the action again (press only)
    #[arg(long)]
    repeat: bool,
    /// Change this profile's overlay instead of the user layer
    #[arg(long, value_name = "NAME")]
    profile: Option<String>,
    /// Take the keys away from another action of this tool if they collide
    #[arg(long)]
    replace: bool,
    /// Accept a conflict with another tool's global binding (the first to register wins)
    #[arg(long)]
    allow_conflict: bool,
    /// Check and show the change; write nothing
    #[arg(long)]
    dry_run: bool,
}

#[derive(Debug, Args)]
pub struct DisableArgs {
    /// Action ID to disable
    #[arg(required_unless_present_any = ["all", "scope"], conflicts_with_all = ["all", "scope"])]
    action: Option<String>,
    /// Turn every shortcut off (`enabled: false`)
    #[arg(long, conflicts_with = "scope")]
    all: bool,
    /// Turn one scope off: global, app or context
    #[arg(long, value_name = "SCOPE")]
    scope: Option<String>,
    /// Change this profile's overlay (with an action)
    #[arg(long, value_name = "NAME", requires = "action")]
    profile: Option<String>,
    /// Show the change; write nothing
    #[arg(long)]
    dry_run: bool,
}

#[derive(Debug, Args)]
pub struct EnableArgs {
    /// Turn the tool-wide switch back on
    #[arg(long, required_unless_present = "scope", conflicts_with = "scope")]
    all: bool,
    /// Turn one scope back on: global, app or context
    #[arg(long, value_name = "SCOPE")]
    scope: Option<String>,
    /// Show the change; write nothing
    #[arg(long)]
    dry_run: bool,
}

#[derive(Debug, Args)]
pub struct ResetArgs {
    /// Action ID (or `@<leader>`) whose default to restore
    #[arg(required_unless_present = "all", conflicts_with = "all")]
    action: Option<String>,
    /// Discard every change (keeps hotkeys.json.bak); needs --yes when Kim cannot ask
    #[arg(long)]
    all: bool,
    /// Confirm --all without prompting
    #[arg(long, short)]
    yes: bool,
    /// Remove the override from this profile's overlay (with an action)
    #[arg(long, value_name = "NAME", requires = "action")]
    profile: Option<String>,
    /// Show the change; write nothing
    #[arg(long)]
    dry_run: bool,
}

#[derive(Debug, Clone, Copy, ValueEnum)]
enum MinSeverity {
    Error,
    Warning,
    Info,
}

#[derive(Debug, Args)]
pub struct CheckArgs {
    /// Also check every installed Kim tool (manifests and hotkeys.json only; starts nothing)
    #[arg(long)]
    all_tools: bool,
    /// Report only conflicts at least this severe
    #[arg(long, value_enum, default_value_t = MinSeverity::Info)]
    min_severity: MinSeverity,
    /// Check Kim's bindings with this profile active
    #[arg(long, value_name = "NAME")]
    profile: Option<String>,
}

#[derive(Debug, Args)]
pub struct ImportArgs {
    /// A `kim.hotkeys` document for tool `kim`
    file: std::path::PathBuf,
    /// Validate and show the change; write nothing
    #[arg(long)]
    dry_run: bool,
}

pub fn command_name(c: &HotkeysCommand) -> &'static str {
    match c {
        HotkeysCommand::List(_) => "hotkeys.list",
        HotkeysCommand::Show(_) => "hotkeys.show",
        HotkeysCommand::Set(_) => "hotkeys.set",
        HotkeysCommand::Disable(_) => "hotkeys.disable",
        HotkeysCommand::Enable(_) => "hotkeys.enable",
        HotkeysCommand::Reset(_) => "hotkeys.reset",
        HotkeysCommand::Check(_) => "hotkeys.check",
        HotkeysCommand::Export => "hotkeys.export",
        HotkeysCommand::Import(_) => "hotkeys.import",
    }
}

/// Kim's state and every other installed tool's, for checks.
struct World {
    path: String,
    loaded: Loaded,
    tools: Vec<ToolState>,
    entries: Vec<ToolEntry>,
    warnings: Vec<String>,
}

fn world(paths: &Paths, all_tools: bool, profile: Option<&str>) -> World {
    let (mut kim, loaded) = kh::load(paths);
    kim.profile = profile.map(str::to_string);
    let mut warnings: Vec<String> = kh::safe_mode_warning(kh::TOOL, &loaded)
        .into_iter()
        .collect();
    let mut entries = vec![ToolEntry {
        tool: kh::TOOL.into(),
        version: Some(kim::KIM_VERSION.into()),
        source: "self",
        enabled: kim.enabled(),
    }];
    let mut tools = vec![kim];
    if all_tools {
        let (found, w) = kh::installed(paths);
        warnings.extend(w);
        for Discovered { entry, state } in found {
            entries.push(entry);
            tools.push(state);
        }
    }
    World {
        path: loaded.path.display().to_string(),
        loaded,
        tools,
        entries,
        warnings,
    }
}

pub fn run(c: &HotkeysCommand, ctx: Ctx) -> Result<Outcome, KimError> {
    let paths = Paths::from_env()?;
    match c {
        HotkeysCommand::List(a) => list(&paths, a),
        HotkeysCommand::Show(a) => show(&paths, a),
        HotkeysCommand::Set(a) => set(&paths, a),
        HotkeysCommand::Disable(a) => disable(&paths, a),
        HotkeysCommand::Enable(a) => enable(&paths, a),
        HotkeysCommand::Reset(a) => reset(&paths, a, ctx),
        HotkeysCommand::Check(a) => check(&paths, a),
        HotkeysCommand::Export => export(&paths),
        HotkeysCommand::Import(a) => import(&paths, a),
    }
}

/// Kim's CLI cannot ask a running instance about global registrations; Kim Manager has none.
fn registration(_: &str, _: &str) -> &'static str {
    "unknown"
}

fn source_text(v: &Value) -> String {
    let src = v["source"].as_str().unwrap_or("");
    let bindings = v["bindings"].as_array().cloned().unwrap_or_default();
    let inactive = bindings.iter().any(|b| b["active"] == false);
    match (src, inactive) {
        ("reserved", _) => "fixed".into(),
        (s, true) => format!("{s}, inactive"),
        (s, false) => s.into(),
    }
}

fn keys_text(v: &Value) -> String {
    let keys: Vec<&str> = v["bindings"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|b| b["keys"].as_str())
        .collect();
    if keys.is_empty() {
        "—".into()
    } else {
        keys.join(", ")
    }
}

fn list(paths: &Paths, a: &ListArgs) -> Result<Outcome, KimError> {
    let w = world(paths, false, a.profile.as_deref());
    let analysis = analyze(&w.tools, &[]);
    let mut data = view::list_data(
        &w.tools[0],
        &analysis,
        &w.path,
        w.loaded.state,
        &registration,
    );
    if let Some(s) = &a.scope {
        let want = s.as_str();
        if !(matches!(want, "global" | "app" | "context")
            || kim_hotkeys::Scope::parse(want).is_some())
        {
            return Err(KimError::new(
                ErrorCode::Usage,
                format!("`{s}` is not a scope (global, app, context or context:<id>)"),
            ));
        }
        let matches =
            |scope: &str| scope == want || (want == "context" && scope.starts_with("context:"));
        if let Some(actions) = data["actions"].as_array_mut() {
            actions.retain(|v| {
                matches(v["scope"].as_str().unwrap_or(""))
                    || v["bindings"]
                        .as_array()
                        .into_iter()
                        .flatten()
                        .any(|b| matches(b["scope"].as_str().unwrap_or("")))
            });
        }
    }
    let mut rows = vec![vec![
        "ACTION".to_string(),
        "KEYS".into(),
        "SCOPE".into(),
        "STATE".into(),
        "TITLE".into(),
    ]];
    for v in data["actions"].as_array().into_iter().flatten() {
        rows.push(vec![
            v["id"].as_str().unwrap_or("").into(),
            keys_text(v),
            v["scope"].as_str().unwrap_or("").into(),
            source_text(v),
            v["title"].as_str().unwrap_or("").into(),
        ]);
    }
    let mut human = format!(
        "Kim Manager shortcuts{}\n\n",
        if w.tools[0].enabled() {
            ""
        } else {
            " (all off: `kim hotkeys enable --all` turns them back on)"
        }
    );
    human.push_str(&table(&rows));
    human.push_str(&format!(
        "\nFile: {} ({})\nChange with `kim hotkeys set <action> <keys>`, `disable`, `reset`; `kim hotkeys check --all-tools` finds conflicts.\n",
        w.path,
        w.loaded.state.as_str()
    ));
    Ok(Outcome {
        data,
        human,
        warnings: w.warnings,
    })
}

fn conflict_lines(conflicts: &[Conflict]) -> String {
    let mut s = String::new();
    for c in conflicts {
        s.push_str(&format!(
            "  {} {}: {}\n",
            c.severity().as_str(),
            c.code.as_str(),
            c.message
        ));
        let offers: Vec<String> = c
            .offers
            .iter()
            .map(|o| match o {
                kim_hotkeys::check::Offer::Rebind(k) => format!("rebind to {k}"),
                kim_hotkeys::check::Offer::Disable => "disable".into(),
                kim_hotkeys::check::Offer::Keep => "keep".into(),
                kim_hotkeys::check::Offer::Reset => "reset".into(),
            })
            .collect();
        if !offers.is_empty() {
            s.push_str(&format!("    offers: {}\n", offers.join(" · ")));
        }
        // The exact command, for the tool that owns the subject (a tool never edits another
        // tool's configuration, §9.6 6).
        if c.severity() != Severity::Info {
            let rebind = c.offers.iter().find_map(|o| match o {
                kim_hotkeys::check::Offer::Rebind(k) => Some(k.clone()),
                _ => None,
            });
            match rebind {
                Some(k) => s.push_str(&format!(
                    "    fix:    `{} hotkeys set {} {k}` or `{} hotkeys disable {}`\n",
                    c.tool, c.action, c.tool, c.action
                )),
                None if c.offers.contains(&kim_hotkeys::check::Offer::Disable) => s.push_str(
                    &format!("    fix:    `{} hotkeys disable {}`\n", c.tool, c.action),
                ),
                None if c.offers.contains(&kim_hotkeys::check::Offer::Reset) => s.push_str(
                    &format!("    fix:    `{} hotkeys reset {}`\n", c.tool, c.action),
                ),
                None => {}
            }
        }
    }
    s
}

fn show(paths: &Paths, a: &ShowArgs) -> Result<Outcome, KimError> {
    let w = world(paths, true, a.profile.as_deref());
    let analysis = analyze(&w.tools, &[]);
    let Some(data) = view::show_data(&w.tools[0], &analysis, &a.action, &registration) else {
        return Err(not_found(&a.action));
    };
    let v = &data["action"];
    let mut human = format!(
        "{} — {}\nKeys:     {}\nScope:    {}\nState:    {}\nDefaults: {}\n",
        a.action,
        v["title"].as_str().unwrap_or(""),
        keys_text(v),
        v["scope"].as_str().unwrap_or(""),
        source_text(v),
        v["defaults"]
            .as_array()
            .map(|d| d
                .iter()
                .filter_map(Value::as_str)
                .collect::<Vec<_>>()
                .join(", "))
            .filter(|s| !s.is_empty())
            .unwrap_or_else(|| "none".into()),
    );
    let involved: Vec<Conflict> = analysis
        .conflicts
        .iter()
        .filter(|c| c.involves(kh::TOOL, &a.action))
        .cloned()
        .collect();
    if involved.is_empty() {
        human.push_str("No conflicts.\n");
    } else {
        human.push_str("Conflicts:\n");
        human.push_str(&conflict_lines(&involved));
    }
    Ok(Outcome {
        data,
        human,
        warnings: w.warnings,
    })
}

fn not_found(action: &str) -> KimError {
    KimError::new(
        ErrorCode::ActionNotFound,
        format!("Kim Manager has no action `{action}`"),
    )
    .with_hint("run `kim hotkeys list` to see its actions")
}

/// Refuses writes while the file is broken or newer (§8.3): it is never overwritten implicitly.
fn writable(loaded: &Loaded) -> Result<(), KimError> {
    match loaded.state {
        ConfigState::Invalid => Err(KimError::new(
            ErrorCode::HotkeysInvalid,
            "hotkeys.json is invalid; nothing was changed",
        )
        .with_hint("fix the file, or run `kim hotkeys reset --all --yes` (keeps a backup)")
        .with_details(loaded.problems.clone())),
        ConfigState::NewerVersion => Err(KimError::new(
            ErrorCode::HotkeysFromNewerVersion,
            "hotkeys.json was written by a newer version of Kim and is read-only; nothing was changed",
        )
        .with_hint("use the newer Kim to change shortcuts")
        .with_details(loaded.problems.clone())),
        _ => Ok(()),
    }
}

fn refusal(r: Refusal) -> KimError {
    let code = match r.error {
        EditError::HotkeyInvalid => ErrorCode::HotkeyInvalid,
        EditError::HotkeyReserved => ErrorCode::HotkeyReserved,
        EditError::ActionNotFound => ErrorCode::ActionNotFound,
        EditError::HotkeyUnsupported => ErrorCode::HotkeyUnsupported,
        EditError::HotkeyConflict => ErrorCode::HotkeyConflict,
    };
    let mut e = KimError::new(code, r.message)
        .with_details(r.conflicts.iter().map(Conflict::detail).collect());
    if let Some(h) = r.hint {
        e = e.with_hint(h);
    }
    e
}

/// Writes (unless a dry run) and renders a change.
fn finish_change(
    w: &World,
    applied: &Applied,
    dry_run: bool,
    backup: bool,
    mut warnings: Vec<String>,
) -> Result<Outcome, KimError> {
    let path = &w.loaded.path;
    let written = !dry_run && applied.changed;
    if written {
        let layer = applied
            .layer
            .clone()
            .unwrap_or_else(|| UserLayer::new(kh::TOOL));
        let made = store::save(path, &layer, backup || w.loaded.migrated_from.is_some())
            .map_err(|e| KimError::io(path, &e))?;
        if made.is_some() {
            warnings.push("the previous file was kept as hotkeys.json.bak".into());
        }
    }
    let mut human = String::new();
    for c in applied.changes.iter().filter(|c| c.before != c.after) {
        let side = |s: &kim_hotkeys::check::LayerState| {
            if s.bindings.is_empty() {
                s.source.to_string()
            } else {
                format!("{} ({})", s.bindings.join(", "), s.source)
            }
        };
        human.push_str(&format!(
            "{}: {} → {}\n",
            c.action,
            side(&c.before),
            side(&c.after)
        ));
    }
    if !applied.changed {
        human.push_str("Nothing to change.\n");
    } else if applied.changes.iter().all(|c| c.before == c.after) {
        human.push_str("The user layer was replaced; the effective shortcuts stay the same.\n");
    }
    if !applied.conflicts.is_empty() {
        human.push_str("Conflicts:\n");
        human.push_str(&conflict_lines(&applied.conflicts));
    }
    if dry_run {
        human.push_str("Dry run: nothing was written.\n");
    } else if written {
        human.push_str(&format!(
            "Saved to {}. A running Kim Manager picks it up when its window is focused again.\n",
            path.display()
        ));
    }
    Ok(Outcome {
        data: json!({
            "tool": kh::TOOL,
            "dry_run": dry_run,
            "written": written,
            "path": w.path,
            "changes": applied.changes.iter().map(edit::Change::to_json).collect::<Vec<_>>(),
            "conflicts": applied.conflicts.iter().map(Conflict::to_json).collect::<Vec<_>>(),
        }),
        human,
        warnings,
    })
}

fn set(paths: &Paths, a: &SetArgs) -> Result<Outcome, KimError> {
    let w = world(paths, true, None);
    writable(&w.loaded)?;
    let mut bindings = Vec::new();
    for k in &a.keys {
        let seq = Sequence::parse_input(k).map_err(|e| {
            KimError::new(ErrorCode::HotkeyInvalid, e.to_string()).with_hint(
                "keys look like Ctrl+Shift+R, F5 or \"Ctrl+K Ctrl+R\" (docs/HOTKEY_CONTRACT.md §5)",
            )
        })?;
        let keys = seq.to_string();
        let options = a.scope.is_some() || a.trigger.is_some() || a.hold_ms.is_some() || a.repeat;
        bindings.push(if options {
            BindingSpec::Object(BindingObject {
                keys,
                scope: a.scope.map(|s| match s {
                    BindScope::Global => "global".to_string(),
                    BindScope::App => "app".to_string(),
                }),
                trigger: a.trigger.map(|t| match t {
                    TriggerOpt::Press => Trigger::Press,
                    TriggerOpt::Release => Trigger::Release,
                    TriggerOpt::Hold => Trigger::Hold,
                }),
                hold_ms: a.hold_ms,
                repeat: a.repeat.then_some(true),
                extra: serde_json::Map::new(),
            })
        } else {
            BindingSpec::Text(keys)
        });
    }
    let opts = Options {
        replace: a.replace,
        allow_conflict: a.allow_conflict,
        profile: a.profile.clone(),
    };
    let e = Edit::Set {
        action: a.action.clone(),
        bindings,
    };
    let applied = edit::apply(&w.tools, &[], 0, &e, &opts).map_err(refusal)?;
    finish_change(&w, &applied, a.dry_run, false, w.warnings.clone())
}

fn scope_family(s: &str) -> Result<String, KimError> {
    if matches!(s, "global" | "app" | "context") {
        Ok(s.to_string())
    } else {
        Err(KimError::new(
            ErrorCode::Usage,
            format!("`{s}` is not a scope family (global, app or context)"),
        ))
    }
}

fn disable(paths: &Paths, a: &DisableArgs) -> Result<Outcome, KimError> {
    let w = world(paths, true, None);
    writable(&w.loaded)?;
    let applied = if a.all {
        edit::apply_switch(&w.tools, &[], 0, &Switch::DisableAll)
    } else if let Some(s) = &a.scope {
        edit::apply_switch(&w.tools, &[], 0, &Switch::DisableScope(scope_family(s)?))
    } else {
        let action = a.action.clone().unwrap_or_default();
        let opts = Options {
            profile: a.profile.clone(),
            ..Options::default()
        };
        edit::apply(&w.tools, &[], 0, &Edit::Disable { action }, &opts).map_err(refusal)?
    };
    finish_change(&w, &applied, a.dry_run, false, w.warnings.clone())
}

fn enable(paths: &Paths, a: &EnableArgs) -> Result<Outcome, KimError> {
    let w = world(paths, true, None);
    writable(&w.loaded)?;
    let switch = match &a.scope {
        Some(s) => Switch::EnableScope(scope_family(s)?),
        None => Switch::EnableAll,
    };
    let applied = edit::apply_switch(&w.tools, &[], 0, &switch);
    finish_change(&w, &applied, a.dry_run, false, w.warnings.clone())
}

fn reset(paths: &Paths, a: &ResetArgs, ctx: Ctx) -> Result<Outcome, KimError> {
    let w = world(paths, true, None);
    let Some(action) = &a.action else {
        // `reset --all` may replace an invalid file (keeping it as the backup), never a newer one.
        if w.loaded.state == ConfigState::NewerVersion {
            writable(&w.loaded)?;
        }
        let applied = edit::apply_switch(&w.tools, &[], 0, &Switch::ResetAll);
        if !a.dry_run && applied.changed && !a.yes {
            let confirmed = crate::ops::confirm(
                ctx,
                "Discard all of Kim Manager's shortcut changes (a backup is kept)? [y/N]",
                "y",
            );
            match confirmed {
                Some(true) => {}
                Some(false) => {
                    return Err(KimError::new(
                        ErrorCode::Cancelled,
                        "reset cancelled; nothing was changed",
                    ));
                }
                None => {
                    return Err(KimError::new(
                        ErrorCode::ConfirmationRequired,
                        "resetting every shortcut needs confirmation",
                    )
                    .with_hint(
                        "re-run with --yes (or --dry-run to preview); nothing was changed",
                    ));
                }
            }
        }
        return finish_change(&w, &applied, a.dry_run, true, w.warnings.clone());
    };
    writable(&w.loaded)?;
    let opts = Options {
        profile: a.profile.clone(),
        ..Options::default()
    };
    let applied = edit::apply(
        &w.tools,
        &[],
        0,
        &Edit::Reset {
            action: action.clone(),
        },
        &opts,
    )
    .map_err(refusal)?;
    finish_change(&w, &applied, a.dry_run, false, w.warnings.clone())
}

fn check(paths: &Paths, a: &CheckArgs) -> Result<Outcome, KimError> {
    let w = world(paths, a.all_tools, a.profile.as_deref());
    let analysis = analyze(&w.tools, &[]);
    let min = match a.min_severity {
        MinSeverity::Error => Severity::Error,
        MinSeverity::Warning => Severity::Warning,
        MinSeverity::Info => Severity::Info,
    };
    let data = report_json(&w.entries, &analysis, min);
    let shown: Vec<Conflict> = analysis
        .conflicts
        .iter()
        .filter(|c| c.severity() <= min)
        .cloned()
        .collect();
    let names: Vec<String> = data["tools"]
        .as_array()
        .into_iter()
        .flatten()
        .map(|t| {
            format!(
                "{} {}",
                t["tool"].as_str().unwrap_or(""),
                t["version"].as_str().unwrap_or("")
            )
        })
        .collect();
    let mut human = format!("Checked: {}\n", names.join(", "));
    if shown.is_empty() {
        human.push_str("No conflicts.\n");
    } else {
        human.push_str(&conflict_lines(&shown));
    }
    human.push_str(&format!(
        "{} errors, {} warnings, {} infos. Nothing was changed.\n",
        data["summary"]["errors"], data["summary"]["warnings"], data["summary"]["infos"]
    ));
    if !a.all_tools {
        human.push_str("Add --all-tools to include the other installed Kim tools.\n");
    }
    Ok(Outcome {
        data,
        human,
        warnings: w.warnings,
    })
}

fn export(paths: &Paths) -> Result<Outcome, KimError> {
    let w = world(paths, false, None);
    writable(&w.loaded)?;
    let doc = serde_json::to_value(
        w.loaded
            .layer
            .clone()
            .unwrap_or_else(|| UserLayer::new(kh::TOOL)),
    )
    .map_err(|e| KimError::new(ErrorCode::Internal, e.to_string()))?;
    let human = format!(
        "{}\n",
        serde_json::to_string_pretty(&doc).unwrap_or_default()
    );
    Ok(Outcome {
        data: json!({"tool": kh::TOOL, "path": w.path, "document": doc}),
        human,
        warnings: w.warnings,
    })
}

fn import(paths: &Paths, a: &ImportArgs) -> Result<Outcome, KimError> {
    let w = world(paths, true, None);
    if w.loaded.state == ConfigState::NewerVersion {
        writable(&w.loaded)?;
    }
    let incoming = store::load(&a.file, &w.tools[0].manifest);
    match incoming.state {
        ConfigState::User => {}
        ConfigState::Default => {
            return Err(KimError::new(
                ErrorCode::Io,
                format!("{}: no such file", a.file.display()),
            ));
        }
        ConfigState::Invalid => {
            return Err(KimError::new(
                ErrorCode::HotkeysInvalid,
                format!(
                    "{} is not a valid hotkeys document for kim; nothing was changed",
                    a.file.display()
                ),
            )
            .with_details(incoming.problems));
        }
        ConfigState::NewerVersion => {
            return Err(KimError::new(
                ErrorCode::HotkeysFromNewerVersion,
                format!(
                    "{} was written by a newer version; nothing was changed",
                    a.file.display()
                ),
            ));
        }
    }
    let mut next = w.tools.clone();
    next[0].layer = incoming.layer;
    next[0].safe_mode = false;
    let names = w.tools[0]
        .manifest
        .actions
        .iter()
        .map(|d| d.id.clone())
        .collect::<Vec<_>>();
    let changes: Vec<edit::Change> = names
        .into_iter()
        .filter_map(|n| {
            let (b, a) = (
                edit::effective_state(&w.tools[0], &n),
                edit::effective_state(&next[0], &n),
            );
            (b != a && !(b.bindings.is_empty() && a.bindings.is_empty())).then_some(edit::Change {
                action: n,
                before: b,
                after: a,
            })
        })
        .collect();
    let analysis = analyze(&next, &[]);
    let applied = Applied {
        changed: next[0].layer != w.tools[0].layer || w.loaded.state != ConfigState::User,
        layer: next[0].layer.clone(),
        changes,
        conflicts: analysis
            .conflicts
            .into_iter()
            .filter(|c| c.tool == kh::TOOL)
            .collect(),
    };
    finish_change(&w, &applied, a.dry_run, true, w.warnings.clone())
}
