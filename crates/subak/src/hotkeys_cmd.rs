//! `subak hotkeys …`: Subak's keyboard shortcuts from the command line (docs/HOTKEY_CONTRACT.md
//! §10, docs/SUBAK_CLI.md §3.12; `SUBAK-FR-034`). The same commands as `kim hotkeys` and
//! `noeul hotkeys`, on the shared `kim-hotkeys` library and the window's manifest
//! ([`crate::hotkeys`]).
//!
//! Headless by construction (`KIM-FR-012`, `KIM-FR-045`): these commands read and write
//! `%APPDATA%\mouv\subak\hotkeys.json` and read other installed tools' manifests and user
//! layers. They never contact or start a Host, never open a window, never register a hotkey and
//! never install a hook, so they work the same with or without a Host and on every target.
//! Subak has no global binding yet, so every `registration` is `not_applicable`; the window
//! (`subak-gui`) re-reads the file when it regains focus. A refused or invalid change writes
//! nothing, so working bindings never change by accident.

use crate::cli::{Args, Cmd};
use crate::hotkeys::{self as sh, Discovered};
use crate::output::{CliError, Code};
use kim_hotkeys::check::{Conflict, Offer, Severity, ToolEntry, ToolState, analyze, report_json};
use kim_hotkeys::edit::{self, Applied, Edit, EditError, Options, Refusal, Switch};
use kim_hotkeys::keys::Sequence;
use kim_hotkeys::layer::UserLayer;
use kim_hotkeys::manifest::{BindingObject, BindingSpec, Trigger};
use kim_hotkeys::store::{self, ConfigState, Loaded};
use kim_hotkeys::view;
use serde_json::{Value, json};
use std::path::{Path, PathBuf};

/// What a `hotkeys` command produced: the `--json` data, the human text and warnings.
#[derive(Debug)]
pub struct Outcome {
    pub data: Value,
    pub human: String,
    pub warnings: Vec<String>,
}

/// Subak's state and, with `all_tools`, every other installed tool's, for checks.
struct World {
    path: String,
    loaded: Loaded,
    tools: Vec<ToolState>,
    entries: Vec<ToolEntry>,
    warnings: Vec<String>,
}

fn world(dir: &Path, all_tools: bool, profile: Option<&str>) -> World {
    let (mut me, loaded) = sh::load(Some(dir));
    me.profile = profile.map(str::to_string);
    let mut warnings: Vec<String> = sh::safe_mode_warning(&loaded).into_iter().collect();
    let mut entries = vec![ToolEntry {
        tool: sh::TOOL.into(),
        version: Some(env!("CARGO_PKG_VERSION").into()),
        source: "self",
        enabled: me.enabled(),
    }];
    let mut tools = vec![me];
    if all_tools {
        match (sh::programs_dir(), sh::roaming_root()) {
            (Some(programs), Some(roaming)) => {
                let (found, w) = sh::installed(&programs, &roaming);
                warnings.extend(w);
                for Discovered { entry, state } in found {
                    entries.push(entry);
                    tools.push(state);
                }
            }
            _ => warnings.push(
                "Kim's install folder is unknown (%LOCALAPPDATA% is not set); other tools were not checked"
                    .into(),
            ),
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

/// Runs a `hotkeys` command.
pub fn run(cmd: Cmd, a: &Args) -> Result<Outcome, CliError> {
    let dir = sh::config_dir().ok_or_else(|| {
        CliError::new(
            Code::Io,
            if cfg!(windows) {
                "%APPDATA% is not set, so Subak's shortcut file has no place"
            } else {
                "$XDG_CONFIG_HOME is not set, so Subak's shortcut file has no place"
            },
        )
    })?;
    match cmd {
        Cmd::HotkeysList => list(&dir, a),
        Cmd::HotkeysShow => show(&dir, a),
        Cmd::HotkeysSet => set(&dir, a),
        Cmd::HotkeysDisable => disable(&dir, a),
        Cmd::HotkeysEnable => enable(&dir, a),
        Cmd::HotkeysReset => reset(&dir, a),
        Cmd::HotkeysCheck => check(&dir, a),
        Cmd::HotkeysExport => export(&dir),
        Cmd::HotkeysImport => import(&dir, a),
        _ => Err(CliError::new(Code::Internal, "not a hotkeys command")),
    }
}

/// Subak registers nothing system-wide yet (`gui.show` comes with Step 18).
fn registration(_action: &str, _keys: &str) -> &'static str {
    "not_applicable"
}

/// A plain text table: columns padded to their widest cell (wide characters count one; the
/// cells are ASCII action IDs, keys and English words).
fn table(rows: &[Vec<String>]) -> String {
    let cols = rows.iter().map(Vec::len).max().unwrap_or(0);
    let widths: Vec<usize> = (0..cols)
        .map(|c| {
            rows.iter()
                .filter_map(|r| r.get(c))
                .map(|s| s.chars().count())
                .max()
                .unwrap_or(0)
        })
        .collect();
    let mut out = String::new();
    for r in rows {
        let mut line = String::new();
        for (c, cell) in r.iter().enumerate() {
            if c + 1 == r.len() {
                line.push_str(cell);
            } else {
                line.push_str(cell);
                line.push_str(&" ".repeat(widths[c] - cell.chars().count() + 2));
            }
        }
        out.push_str(line.trim_end());
        out.push('\n');
    }
    out
}

fn source_text(v: &Value) -> String {
    let src = v["source"].as_str().unwrap_or("");
    let inactive = v["bindings"]
        .as_array()
        .is_some_and(|b| b.iter().any(|b| b["active"] == false));
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

fn list(dir: &Path, a: &Args) -> Result<Outcome, CliError> {
    let w = world(dir, false, a.profile.as_deref());
    let analysis = analyze(&w.tools, &[]);
    let mut data = view::list_data(
        &w.tools[0],
        &analysis,
        &w.path,
        w.loaded.state,
        &registration,
    );
    if let Some(want) = a.scope.as_deref() {
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
        "Subak shortcuts{}\n\n",
        if w.tools[0].enabled() {
            ""
        } else {
            " (all off: `subak hotkeys enable --all` turns them back on)"
        }
    );
    human.push_str(&table(&rows));
    human.push_str(&format!(
        "\nFile: {} ({})\nChange with `subak hotkeys set <action> <keys>`, `disable`, `reset`; `subak hotkeys check --all-tools` finds conflicts.\nThe prefix key (Ctrl+B) and its table arrive with Step 23; until then Ctrl+B goes to the program.\n",
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
                Offer::Rebind(k) => format!("rebind to {k}"),
                Offer::Disable => "disable".into(),
                Offer::Keep => "keep".into(),
                Offer::Reset => "reset".into(),
            })
            .collect();
        if !offers.is_empty() {
            s.push_str(&format!("    offers: {}\n", offers.join(" · ")));
        }
        // The exact command, for the tool that owns the subject (a tool never edits another
        // tool's configuration, §9.6 6).
        if c.severity() != Severity::Info {
            let rebind = c.offers.iter().find_map(|o| match o {
                Offer::Rebind(k) => Some(k.clone()),
                _ => None,
            });
            match rebind {
                Some(k) => s.push_str(&format!(
                    "    fix:    `{} hotkeys set {} {k}` or `{} hotkeys disable {}`\n",
                    c.tool, c.action, c.tool, c.action
                )),
                None if c.offers.contains(&Offer::Disable) => s.push_str(&format!(
                    "    fix:    `{} hotkeys disable {}`\n",
                    c.tool, c.action
                )),
                None if c.offers.contains(&Offer::Reset) => s.push_str(&format!(
                    "    fix:    `{} hotkeys reset {}`\n",
                    c.tool, c.action
                )),
                None => {}
            }
        }
    }
    s
}

fn show(dir: &Path, a: &Args) -> Result<Outcome, CliError> {
    let action = &a.positional[0];
    let w = world(dir, true, a.profile.as_deref());
    let analysis = analyze(&w.tools, &[]);
    let Some(data) = view::show_data(&w.tools[0], &analysis, action, &registration) else {
        return Err(not_found(action));
    };
    let v = &data["action"];
    let mut human = format!(
        "{action} — {}\nKeys:     {}\nScope:    {}\nState:    {}\nDefaults: {}\n",
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
        .filter(|c| c.involves(sh::TOOL, action))
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

fn not_found(action: &str) -> CliError {
    CliError::new(
        Code::ActionNotFound,
        format!("Subak has no action `{action}`"),
    )
    .hint("run `subak hotkeys list` to see its actions")
}

/// Refuses writes while the file is broken or newer (§8.3): it is never overwritten implicitly.
fn writable(loaded: &Loaded) -> Result<(), CliError> {
    let refused = |code: Code, message: &str, hint: &str| {
        let mut e = CliError::new(code, message).hint(hint);
        e.details.clone_from(&loaded.problems);
        Err(e)
    };
    match loaded.state {
        ConfigState::Invalid => refused(
            Code::HotkeysInvalid,
            "hotkeys.json is invalid; nothing was changed",
            "fix the file, or run `subak hotkeys reset --all --yes` (keeps a backup)",
        ),
        ConfigState::NewerVersion => refused(
            Code::HotkeysFromNewerVersion,
            "hotkeys.json was written by a newer version of Subak and is read-only; nothing was changed",
            "use the newer Subak to change shortcuts",
        ),
        _ => Ok(()),
    }
}

fn refusal(r: Refusal) -> CliError {
    let code = match r.error {
        EditError::HotkeyInvalid => Code::HotkeyInvalid,
        EditError::HotkeyReserved => Code::HotkeyReserved,
        EditError::ActionNotFound => Code::ActionNotFound,
        EditError::HotkeyUnsupported => Code::HotkeyUnsupported,
        EditError::HotkeyConflict => Code::HotkeyConflict,
    };
    let mut e = CliError::new(code, r.message);
    e.details = r.conflicts.iter().map(Conflict::detail).collect();
    if let Some(h) = r.hint {
        e = e.hint(h);
    }
    e
}

fn io_error(path: &Path, e: &std::io::Error) -> CliError {
    let code = if e.kind() == std::io::ErrorKind::PermissionDenied {
        Code::PermissionDenied
    } else {
        Code::Io
    };
    CliError::new(code, format!("{}: {e}", path.display()))
}

/// Writes (unless a dry run) and renders a change.
fn finish_change(
    w: &World,
    applied: &Applied,
    dry_run: bool,
    backup: bool,
    mut warnings: Vec<String>,
) -> Result<Outcome, CliError> {
    let path = &w.loaded.path;
    let written = !dry_run && applied.changed;
    if written {
        let layer = applied
            .layer
            .clone()
            .unwrap_or_else(|| UserLayer::new(sh::TOOL));
        let made = store::save(path, &layer, backup || w.loaded.migrated_from.is_some())
            .map_err(|e| io_error(path, &e))?;
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
            "Saved to {}. An open Subak window applies it when it regains focus.\n",
            path.display()
        ));
    }
    Ok(Outcome {
        data: json!({
            "tool": sh::TOOL,
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

fn set(dir: &Path, a: &Args) -> Result<Outcome, CliError> {
    let w = world(dir, true, None);
    writable(&w.loaded)?;
    let (action, keys) = a
        .positional
        .split_first()
        .expect("the parser requires an action and keys");
    let options = a.scope.is_some() || a.trigger.is_some() || a.hold_ms.is_some() || a.repeat;
    let mut bindings = Vec::new();
    for k in keys {
        let seq = Sequence::parse_input(k).map_err(|e| {
            CliError::new(Code::HotkeyInvalid, e.to_string()).hint(
                "keys look like Ctrl+Shift+R, F5 or \"Ctrl+K Ctrl+R\" (docs/HOTKEY_CONTRACT.md §5)",
            )
        })?;
        let keys = seq.to_string();
        bindings.push(if options {
            BindingSpec::Object(BindingObject {
                keys,
                scope: a.scope.clone(),
                trigger: a.trigger.as_deref().map(|t| match t {
                    "release" => Trigger::Release,
                    "hold" => Trigger::Hold,
                    _ => Trigger::Press,
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
        action: action.clone(),
        bindings,
    };
    let applied = edit::apply(&w.tools, &[], 0, &e, &opts).map_err(refusal)?;
    finish_change(&w, &applied, a.dry_run, false, w.warnings.clone())
}

fn disable(dir: &Path, a: &Args) -> Result<Outcome, CliError> {
    let w = world(dir, true, None);
    writable(&w.loaded)?;
    let applied = if a.all {
        edit::apply_switch(&w.tools, &[], 0, &Switch::DisableAll)
    } else if let Some(s) = &a.scope {
        edit::apply_switch(&w.tools, &[], 0, &Switch::DisableScope(s.clone()))
    } else {
        let opts = Options {
            profile: a.profile.clone(),
            ..Options::default()
        };
        let action = a.positional[0].clone();
        edit::apply(&w.tools, &[], 0, &Edit::Disable { action }, &opts).map_err(refusal)?
    };
    finish_change(&w, &applied, a.dry_run, false, w.warnings.clone())
}

fn enable(dir: &Path, a: &Args) -> Result<Outcome, CliError> {
    let w = world(dir, true, None);
    writable(&w.loaded)?;
    let switch = match &a.scope {
        Some(s) => Switch::EnableScope(s.clone()),
        None => Switch::EnableAll,
    };
    let applied = edit::apply_switch(&w.tools, &[], 0, &switch);
    finish_change(&w, &applied, a.dry_run, false, w.warnings.clone())
}

fn reset(dir: &Path, a: &Args) -> Result<Outcome, CliError> {
    let w = world(dir, true, None);
    let Some(action) = a.positional.first() else {
        // `reset --all` may replace an invalid file (keeping it as the backup), never a newer one.
        if w.loaded.state == ConfigState::NewerVersion {
            writable(&w.loaded)?;
        }
        let applied = edit::apply_switch(&w.tools, &[], 0, &Switch::ResetAll);
        if !a.dry_run && applied.changed {
            crate::cli::confirm(
                a,
                "Discard all of Subak's shortcut changes (a backup is kept)?",
                "resetting every shortcut needs confirmation",
                "re-run with --yes (or --dry-run to preview); nothing was changed",
            )?;
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

fn check(dir: &Path, a: &Args) -> Result<Outcome, CliError> {
    let w = world(dir, a.all_tools, a.profile.as_deref());
    let analysis = analyze(&w.tools, &[]);
    let min = a
        .min_severity
        .as_deref()
        .and_then(Severity::parse)
        .unwrap_or(Severity::Info);
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

fn export(dir: &Path) -> Result<Outcome, CliError> {
    let w = world(dir, false, None);
    writable(&w.loaded)?;
    let doc = serde_json::to_value(
        w.loaded
            .layer
            .clone()
            .unwrap_or_else(|| UserLayer::new(sh::TOOL)),
    )
    .map_err(|e| CliError::new(Code::Internal, e.to_string()))?;
    let human = format!(
        "{}\n",
        serde_json::to_string_pretty(&doc).unwrap_or_default()
    );
    Ok(Outcome {
        data: json!({"tool": sh::TOOL, "path": w.path, "document": doc}),
        human,
        warnings: w.warnings,
    })
}

fn import(dir: &Path, a: &Args) -> Result<Outcome, CliError> {
    let file = PathBuf::from(&a.positional[0]);
    let w = world(dir, true, None);
    if w.loaded.state == ConfigState::NewerVersion {
        writable(&w.loaded)?;
    }
    let incoming = store::load(&file, &w.tools[0].manifest);
    match incoming.state {
        ConfigState::User => {}
        ConfigState::Default => {
            return Err(CliError::new(
                Code::Io,
                format!("{}: no such file", file.display()),
            ));
        }
        ConfigState::Invalid => {
            let mut e = CliError::new(
                Code::HotkeysInvalid,
                format!(
                    "{} is not a valid hotkeys document for subak; nothing was changed",
                    file.display()
                ),
            );
            e.details = incoming.problems;
            return Err(e);
        }
        ConfigState::NewerVersion => {
            return Err(CliError::new(
                Code::HotkeysFromNewerVersion,
                format!(
                    "{} was written by a newer version; nothing was changed",
                    file.display()
                ),
            ));
        }
    }
    let mut next = w.tools.clone();
    next[0].layer = incoming.layer;
    next[0].safe_mode = false;
    let names: Vec<String> = w.tools[0]
        .manifest
        .actions
        .iter()
        .map(|d| d.id.clone())
        .collect();
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
            .filter(|c| c.tool == sh::TOOL)
            .collect(),
    };
    finish_change(&w, &applied, a.dry_run, true, w.warnings.clone())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tables_pad_every_column_but_the_last() {
        let t = table(&[
            vec!["A".into(), "KEYS".into(), "T".into()],
            vec!["terminal.new".into(), "Ctrl+T".into(), "New".into()],
        ]);
        assert_eq!(t, "A             KEYS    T\nterminal.new  Ctrl+T  New\n");
    }

    #[test]
    fn refusals_keep_the_contract_codes() {
        for (e, code) in [
            (EditError::HotkeyInvalid, "hotkey_invalid"),
            (EditError::HotkeyReserved, "hotkey_reserved"),
            (EditError::ActionNotFound, "action_not_found"),
            (EditError::HotkeyUnsupported, "hotkey_unsupported"),
            (EditError::HotkeyConflict, "hotkey_conflict"),
        ] {
            let err = refusal(Refusal {
                error: e,
                message: "m".into(),
                hint: Some("h".into()),
                conflicts: Vec::new(),
            });
            // The library's code and exit class are Subak's (CLI_CONTRACT §6).
            assert_eq!(err.code.as_str(), code);
            assert_eq!(err.code.as_str(), e.code());
            assert_eq!(err.code.exit_code(), e.exit_code());
            assert_eq!(err.hint.as_deref(), Some("h"));
        }
        assert_eq!(not_found("x.y").code, Code::ActionNotFound);
    }
}
