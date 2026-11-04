//! The `ddalgak profile …` commands (Step 75, docs/DDALGAK_RULE_MODEL.md §10, §13, ADR 0052;
//! `DDALGAK-FR-030…032`, `DDALGAK-FR-060`, `DDALGAK-SEC-006`, `DDALGAK-SEC-011`,
//! `DDALGAK-SEC-013`).
//!
//! - `list|show|add|edit|rename|duplicate|delete|import` read and change `rules.json` through the
//!   rule store exactly like the `rule …` commands (lock, re-read, revision check, validation
//!   before and after, `.bak`, history snapshot, `--draft`, `--dry-run`, `--if-revision`) and work
//!   without the app.
//! - `apply` changes only `active_profile`; a running app then switches atomically (runs
//!   cancelled, everything held released, the new profile in its default mode) and its reload
//!   answer says which profile and revision it runs (`switched`). `--dry-run` is the preview: the
//!   side-by-side diff of what each trigger does before and after.
//! - `save` makes a labelled save point; `history` lists the profile's versions; `restore` and
//!   `rollback` put a version back as a new, undoable change, after a diff and a confirmation.
//! - `export` writes a portable file (no consent, settings or revision; environment values only
//!   with `--include-env-values`); `import` validates and adds profiles that are **never**
//!   activated, with every rule off and without consent. No output ever carries typed text,
//!   environment values or launch targets (`--show-targets` shows an import's targets).

use crate::cli::{Args, Cmd};
use crate::output::{CliError, Code};
use crate::rule_cmd::{self, Notify, s};
use ddalgak::edit::EditError;
use ddalgak::profile::{
    self, Align, ExportOptions, ImportError, ImportOptions, ProfileFields, Sort,
};
use ddalgak::store::{self, Files, Source};
use ddalgak::validate::Context;
use serde_json::{Value, json};
use std::cell::RefCell;
use std::collections::BTreeSet;
use std::path::PathBuf;

type Outcome = crate::Outcome;

fn not_found(id: &str) -> CliError {
    CliError::new(Code::ProfileNotFound, format!("no profile `{id}`"))
        .hint("see `ddalgak profile list`")
}

fn import_err(e: &ImportError) -> CliError {
    match e {
        ImportError::Invalid(_) => CliError::new(Code::ImportInvalid, e.to_string()).hint(
            "nothing was imported; export the profile again (`ddalgak profile export`) or check the file",
        ),
        ImportError::Newer(_) => CliError::new(Code::RulesFromNewerVersion, e.to_string())
            .hint("import it with the newer Ddalgak that wrote it"),
    }
}

fn import_edit_err(e: &ImportError) -> EditError {
    EditError::Invalid {
        code: match e {
            ImportError::Invalid(_) => "import_invalid",
            ImportError::Newer(_) => "rules_from_newer_version",
        },
        message: e.to_string(),
        problems: Vec::new(),
    }
}

/// `profile list [--search t] [--sort order|title|id|rules]`
fn list(a: &Args, files: &Files, ctx: &Context) -> Outcome {
    let (l, src) = rule_cmd::source(a, files, ctx)?;
    let doc = l.doc.as_ref().expect("usable");
    let sort = match a.get("--sort") {
        None => Sort::default(),
        Some(v) => Sort::parse(v).ok_or_else(|| {
            CliError::usage(format!("--sort takes order, title, id or rules, not `{v}`"))
        })?,
    };
    let query = a.get("--search").unwrap_or_default();
    let all = profile::rows(doc, Some(&l.report));
    let shown = profile::browse(&all, query, sort);
    Ok((
        json!({
            "source": src,
            "revision": l.revision(),
            "active": doc.active_profile,
            "search": a.get("--search"),
            "sort": sort.as_str(),
            "total": all.len(),
            "profiles": shown.iter().map(profile::Row::to_json).collect::<Vec<_>>(),
        }),
        Vec::new(),
    ))
}

/// `profile show <id>`: the profile, its modes and rules (titles, triggers and step kinds), its
/// versions and, when it is not active, what a switch to it would change.
fn show(a: &Args, files: &Files, ctx: &Context) -> Outcome {
    let (l, src) = rule_cmd::source(a, files, ctx)?;
    let doc = l.doc.as_ref().expect("usable");
    let id = &a.positional[0];
    let pi = doc.profile_index(id).ok_or_else(|| not_found(id))?;
    let p = &doc.profiles[pi];
    let row = profile::rows(doc, Some(&l.report))
        .into_iter()
        .find(|r| &r.id == id)
        .expect("listed");
    let modes: Vec<Value> = p
        .modes
        .iter()
        .map(|m| {
            json!({
                "id": m.id,
                "title": m.title,
                "parent": m.parent,
                "enabled": m.enabled != Some(false),
            })
        })
        .collect();
    let rules: Vec<Value> = p
        .rules
        .iter()
        .enumerate()
        .map(|(ri, e)| {
            let mut v = profile::RuleLine::of(e).to_json();
            v["inactive"] = json!(l.report.inactive((pi, ri)).map(|x| x.code));
            v
        })
        .collect();
    let versions = if src == "saved" {
        profile::versions(files, id, Some(p)).len()
    } else {
        0
    };
    let preview = (doc.active_profile != *id)
        .then(|| profile::diff_json(doc.profile(&doc.active_profile), Some(p), Align::Trigger));
    Ok((
        json!({
            "source": src,
            "revision": l.revision(),
            "profile": row.to_json(),
            "modes": modes,
            "rules": rules,
            "versions": versions,
            "switch_preview": preview,
        }),
        Vec::new(),
    ))
}

/// `profile apply <id>`: the switch, with its preview.
fn apply(cmd: Cmd, a: &Args, files: &Files, ctx: &Context, notify: Notify<'_>) -> Outcome {
    let id = a.positional[0].clone();
    let l = rule_cmd::saved(files, ctx)?;
    let doc = l.doc.as_ref().expect("usable");
    let target = doc.profile(&id).ok_or_else(|| not_found(&id))?;
    let from = doc.active_profile.clone();
    let preview = profile::diff_json(doc.profile(&from), Some(target), Align::Trigger);
    let default_mode = target.default_mode.clone();
    let (mut data, warnings) =
        rule_cmd::write(cmd, a, files, ctx, notify, |d| profile::apply(d, &id))?;
    // Whether the running app confirmed the switch: it now runs this profile and the saved file.
    let app = &data["app"];
    let switched = if app.is_null() || app["running"] == false {
        Value::Null
    } else {
        json!(app["in_sync"] == true && app["active_profile"] == id.as_str())
    };
    data["from"] = json!(from);
    data["to"] = json!(id);
    data["mode"] = json!(default_mode);
    data["switched"] = switched;
    data["preview"] = preview;
    Ok((data, warnings))
}

/// `profile save <id> [--label t]`: a save point.
fn save(a: &Args, files: &Files, ctx: &Context) -> Outcome {
    let id = &a.positional[0];
    let l = rule_cmd::saved(files, ctx)?;
    let doc = l.doc.as_ref().expect("usable");
    if doc.profile(id).is_none() {
        return Err(not_found(id));
    }
    let label = a.get("--label").map(str::trim).filter(|l| !l.is_empty());
    if let Some(t) = label
        && (t.chars().count() > 80 || t.chars().any(char::is_control))
    {
        return Err(CliError::usage(
            "a save point label has at most 80 characters and no control characters",
        ));
    }
    let mut data = json!({
        "dry_run": a.dry_run,
        "profile": id,
        "label": label,
        "revision": l.revision(),
        "save_point": null,
    });
    let mut warnings = Vec::new();
    if a.dry_run {
        return Ok((data, warnings));
    }
    let lock = store::lock(files, store::lock_timeout()).map_err(rule_cmd::store_err)?;
    match store::save_point(files, &lock, id, label).map_err(rule_cmd::store_err)? {
        Some((path, revision)) => {
            data["revision"] = json!(revision);
            data["save_point"] = json!(rule_cmd::file_name(&path));
        }
        None => warnings
            .push("nothing is saved yet (there is no rules.json): there is nothing to keep".into()),
    }
    Ok((data, warnings))
}

/// `profile history <id>`
fn history(a: &Args, files: &Files, ctx: &Context) -> Outcome {
    let id = &a.positional[0];
    let l = rule_cmd::saved(files, ctx)?;
    let doc = l.doc.as_ref().expect("usable");
    let current = doc.profile(id);
    let vs = profile::versions(files, id, current);
    if vs.is_empty() && current.is_none() {
        return Err(not_found(id));
    }
    let target = profile::rollback_target(&vs, current).map(|v| v.name.clone());
    Ok((
        json!({
            "profile": id,
            "exists": current.is_some(),
            "revision": l.revision(),
            "versions": vs.iter().map(profile::Version::to_json).collect::<Vec<_>>(),
            "rollback_to": target,
        }),
        Vec::new(),
    ))
}

/// `profile restore <id> <name>` and `profile rollback <id>`.
fn restore(cmd: Cmd, a: &Args, files: &Files, ctx: &Context, notify: Notify<'_>) -> Outcome {
    let id = a.positional[0].clone();
    let l = rule_cmd::saved(files, ctx)?;
    let doc = l.doc.as_ref().expect("usable");
    let current = doc.profile(&id).cloned();
    let (name, revision, version) = if cmd == Cmd::ProfileRestore {
        let name = a.positional[1].clone();
        let (_, snap) = store::restore_source(files, &Source::Snapshot(name.clone()))
            .map_err(rule_cmd::store_err)?;
        let p = snap.profile(&id).cloned().ok_or_else(|| {
            CliError::new(
                Code::SnapshotNotFound,
                format!("{name} has no profile `{id}`"),
            )
            .hint(format!("see `ddalgak profile history {id}`"))
        })?;
        (name, snap.revision(), p)
    } else {
        let vs = profile::versions(files, &id, current.as_ref());
        if vs.is_empty() && current.is_none() {
            return Err(not_found(&id));
        }
        let v = profile::rollback_target(&vs, current.as_ref()).ok_or_else(|| {
            CliError::new(
                Code::SnapshotNotFound,
                format!("profile `{id}` has no earlier working version to roll back to"),
            )
            .hint(format!(
                "see `ddalgak profile history {id}`; `ddalgak profile restore {id} <name>` restores any version"
            ))
        })?;
        (v.name.clone(), v.revision.unwrap_or(0), v.profile.clone())
    };
    let preview = profile::diff_json(current.as_ref(), Some(&version), Align::RuleId);
    rule_cmd::confirm(
        a,
        &format!("Replace profile `{id}` with its version from {name} (revision {revision})?"),
    )?;
    let (mut data, warnings) = rule_cmd::write(cmd, a, files, ctx, notify, |d| {
        profile::restore(d, &version)
    })?;
    data["source"] = json!(name);
    data["restored_revision"] = json!(revision);
    data["preview"] = preview;
    Ok((data, warnings))
}

/// `profile export <id>... | --all --output <file>`
fn export(a: &Args, files: &Files, ctx: &Context) -> Outcome {
    let l = rule_cmd::saved(files, ctx)?;
    let doc = l.doc.as_ref().expect("usable");
    let ids: Vec<String> = match (a.has("--all"), a.positional.is_empty()) {
        (true, true) => doc.profiles.iter().map(|p| p.id.clone()).collect(),
        (false, false) => a.positional.clone(),
        (true, false) => {
            return Err(CliError::usage("give profile IDs or --all, not both"));
        }
        (false, true) => {
            return Err(CliError::usage(
                "`profile export` needs profile IDs or --all, and --output <file>",
            ));
        }
    };
    let output = a
        .get("--output")
        .map(PathBuf::from)
        .ok_or_else(|| CliError::usage("`profile export` needs --output <file>"))?;
    let opts = ExportOptions {
        env_values: a.has("--include-env-values"),
    };
    let x = profile::export(doc, &ids, opts, &ddalgak::launch::process_var)
        .map_err(rule_cmd::edit_err)?;
    let text = x.doc.to_text();
    let mut warnings = Vec::new();
    if x.env_values_removed > 0 {
        warnings.push(format!(
            "{} environment value(s) were left out because they may be secrets (the names stay); --include-env-values keeps them",
            x.env_values_removed
        ));
    } else if opts.env_values {
        warnings.push(
            "the file contains the environment values of program rules; share it with care".into(),
        );
    }
    let data = json!({
        "dry_run": a.dry_run,
        "file": output.display().to_string(),
        "written": !a.dry_run,
        "profiles": x.profiles,
        "rules": x.rules,
        "bytes": text.len(),
        "portable_paths": x.portable_paths,
        "env_values_removed": x.env_values_removed,
        "consent_removed": x.consent_removed,
    });
    if a.dry_run {
        return Ok((data, warnings));
    }
    if output.exists() {
        rule_cmd::confirm(a, &format!("Replace {}?", output.display()))?;
    }
    store::write_atomic(&output, text.as_bytes()).map_err(rule_cmd::store_err)?;
    Ok((data, warnings))
}

/// `profile import <file> [--id <id>] [--title <name>]`: `--dry-run` is the validation preview.
fn import(cmd: Cmd, a: &Args, files: &Files, ctx: &Context, notify: Notify<'_>) -> Outcome {
    let path = PathBuf::from(&a.positional[0]);
    let bytes = std::fs::read(&path).map_err(|e| {
        let code = if e.kind() == std::io::ErrorKind::PermissionDenied {
            Code::PermissionDenied
        } else {
            Code::Io
        };
        CliError::new(code, format!("cannot read {}: {e}", path.display()))
    })?;
    let opts = ImportOptions {
        id: a.get("--id").map(str::to_string),
        title: a.get("--title").map(str::to_string),
    };
    // Checked first against the saved rules, so a bad file is refused before any lock.
    let l = rule_cmd::saved(files, ctx)?;
    profile::plan_import(&bytes, l.doc.as_ref().expect("usable"), ctx, &opts)
        .map_err(|e| import_err(&e))?;
    // The plan that is applied is made again inside the transaction, against the document read
    // under the lock (or the draft).
    let plan = RefCell::new(None);
    let ids = RefCell::new(BTreeSet::new());
    let (mut data, warnings) = rule_cmd::write_with(
        cmd,
        a,
        files,
        ctx,
        notify,
        |d| {
            let p = profile::plan_import(&bytes, d, ctx, &opts).map_err(|e| import_edit_err(&e))?;
            let change = profile::apply_import(d, &p)?;
            *ids.borrow_mut() = p.rule_ids();
            *plan.borrow_mut() = Some(p);
            Ok(change)
        },
        Some(&|before, after| profile::review_import(before, after, &ids.borrow())),
    )?;
    let plan = plan.into_inner().expect("planned");
    if let (Some(o), Value::Object(p)) =
        (data.as_object_mut(), plan.to_json(a.has("--show-targets")))
    {
        o.extend(p);
    }
    Ok((data, warnings))
}

/// Runs a profile command.
pub fn run(cmd: Cmd, a: &Args, paths: &ddalgak::paths::Paths, notify: Notify<'_>) -> Outcome {
    let files = Files::new(paths);
    let ctx = rule_cmd::context(paths);
    match cmd {
        Cmd::ProfileList => list(a, &files, &ctx),
        Cmd::ProfileShow => show(a, &files, &ctx),
        Cmd::ProfileAdd => {
            let title = a
                .get("--title")
                .ok_or_else(|| CliError::usage("`profile add` needs --title <name>"))?
                .to_string();
            let id = a.get("--id").map(str::to_string);
            let description = a.get("--description").map(str::to_string);
            rule_cmd::write(cmd, a, &files, &ctx, notify, |d| {
                profile::add(d, id.as_deref(), &title, description.as_deref())
            })
        }
        Cmd::ProfileEdit | Cmd::ProfileRename => {
            let id = a.positional[0].clone();
            let f = if cmd == Cmd::ProfileRename {
                ProfileFields {
                    title: Some(a.positional[1].clone()),
                    description: None,
                }
            } else {
                if a.has("--description") && a.has("--no-description") {
                    return Err(CliError::usage(
                        "--description and --no-description conflict",
                    ));
                }
                let f = ProfileFields {
                    title: a.get("--title").map(str::to_string),
                    description: if a.has("--no-description") {
                        Some(None)
                    } else {
                        a.get("--description").map(|d| Some(d.to_string()))
                    },
                };
                if f == ProfileFields::default() {
                    return Err(CliError::usage(
                        "nothing to change: give --title, --description or --no-description",
                    ));
                }
                f
            };
            rule_cmd::write(cmd, a, &files, &ctx, notify, |d| profile::edit(d, &id, &f))
        }
        Cmd::ProfileDuplicate => {
            let id = a.positional[0].clone();
            let new_id = a.get("--id").map(str::to_string);
            let title = a.get("--title").map(str::to_string);
            rule_cmd::write(cmd, a, &files, &ctx, notify, |d| {
                profile::duplicate(d, &id, new_id.as_deref(), title.as_deref())
            })
        }
        Cmd::ProfileDelete => {
            let id = a.positional[0].clone();
            {
                let l = rule_cmd::saved(&files, &ctx)?;
                let doc = l.doc.as_ref().expect("usable");
                let p = doc.profile(&id).ok_or_else(|| not_found(&id))?;
                rule_cmd::confirm(
                    a,
                    &format!(
                        "Delete profile `{id}` ({}) with its {} mode(s) and {} rule(s)?",
                        p.title,
                        p.modes.len(),
                        p.rules.len()
                    ),
                )?;
            }
            rule_cmd::write(cmd, a, &files, &ctx, notify, |d| profile::delete(d, &id))
        }
        Cmd::ProfileApply => apply(cmd, a, &files, &ctx, notify),
        Cmd::ProfileSave => save(a, &files, &ctx),
        Cmd::ProfileHistory => history(a, &files, &ctx),
        Cmd::ProfileRestore | Cmd::ProfileRollback => restore(cmd, a, &files, &ctx, notify),
        Cmd::ProfileExport => export(a, &files, &ctx),
        Cmd::ProfileImport => import(cmd, a, &files, &ctx, notify),
        _ => unreachable!("not a profile command"),
    }
}

fn diff_text(d: &Value) -> String {
    let mut o = String::new();
    let side = |v: &Value| {
        if v.is_null() {
            "(none)".to_string()
        } else {
            format!("{} ({})", s(&v["title"]), s(&v["id"]))
        }
    };
    o.push_str(&format!(
        "  {:<38} | {}\n",
        side(&d["left"]),
        side(&d["right"])
    ));
    for r in d["rows"].as_array().into_iter().flatten() {
        let cell = |v: &Value| {
            if v.is_null() {
                "-".to_string()
            } else {
                let steps: Vec<String> =
                    v["steps"].as_array().into_iter().flatten().map(s).collect();
                format!(
                    "{} → {}{}",
                    s(&v["title"]),
                    steps.join(", "),
                    if v["enabled"] == false { " (off)" } else { "" }
                )
            }
        };
        let mark = match r["status"].as_str() {
            Some("same") => "=",
            Some("changed") => "~",
            Some("only_left") => "-",
            Some("only_right") => "+",
            _ => "?",
        };
        o.push_str(&format!(
            "{mark} {:<22} {:<38} | {}\n",
            s(&r["key"]),
            cell(&r["left"]),
            cell(&r["right"])
        ));
    }
    let c = &d["counts"];
    o.push_str(&format!(
        "  {} same, {} changed, {} only before, {} only after\n",
        c["same"], c["changed"], c["only_left"], c["only_right"]
    ));
    o
}

fn write_line(d: &Value) -> String {
    if d["dry_run"] == true {
        return "dry run: nothing was written\n".into();
    }
    if d["changed"] == false {
        return "nothing changed\n".into();
    }
    let mut o = format!(
        "saved: revision {} → {} ({})\n",
        s(&d["revision"]["from"]),
        s(&d["revision"]["to"]),
        s(&d["target"])
    );
    o.push_str(&rule_cmd::app_line(&d["app"]));
    o
}

/// Human output of the profile commands (stdout).
pub fn human(cmd: Cmd, d: &Value) -> String {
    let mut o = String::new();
    match cmd {
        Cmd::ProfileList => {
            o.push_str(&format!(
                "{} profiles ({} shown), revision {}, active: {}\n",
                s(&d["total"]),
                d["profiles"].as_array().map_or(0, Vec::len),
                s(&d["revision"]),
                s(&d["active"])
            ));
            for p in d["profiles"].as_array().into_iter().flatten() {
                o.push_str(&format!(
                    "{} {:<24} {:<28} {:>3} rules ({} on), {} modes{}{}\n",
                    if p["active"] == true { "*" } else { " " },
                    s(&p["id"]),
                    s(&p["title"]),
                    s(&p["rules"]),
                    s(&p["rules_on"]),
                    s(&p["modes"]),
                    if p["enabled"] == false {
                        ", turned off"
                    } else {
                        ""
                    },
                    if p["errors"].as_u64().unwrap_or(0) > 0 {
                        format!(", {} error(s)", s(&p["errors"]))
                    } else {
                        String::new()
                    }
                ));
            }
        }
        Cmd::ProfileShow => {
            let p = &d["profile"];
            o.push_str(&format!(
                "{} ({}){}{}\n",
                s(&p["title"]),
                s(&p["id"]),
                if p["active"] == true { ", active" } else { "" },
                if p["enabled"] == false {
                    ", turned off"
                } else {
                    ""
                }
            ));
            if let Some(desc) = p["description"].as_str() {
                o.push_str(&format!("{desc}\n"));
            }
            let modes: Vec<String> = d["modes"]
                .as_array()
                .into_iter()
                .flatten()
                .map(|m| s(&m["id"]))
                .collect();
            o.push_str(&format!(
                "modes: {} (default {})\n",
                modes.join(", "),
                s(&p["default_mode"])
            ));
            for r in d["rules"].as_array().into_iter().flatten() {
                let steps: Vec<String> =
                    r["steps"].as_array().into_iter().flatten().map(s).collect();
                o.push_str(&format!(
                    "  {:<40} {:<22} {:<4} {} → {}{}\n",
                    s(&r["id"]),
                    s(&r["trigger"]),
                    if r["enabled"] == true { "on" } else { "off" },
                    s(&r["title"]),
                    steps.join(", "),
                    r["inactive"]
                        .as_str()
                        .map(|c| format!(" ({c})"))
                        .unwrap_or_default()
                ));
            }
            o.push_str(&format!(
                "{} version(s): ddalgak profile history {}\n",
                s(&d["versions"]),
                s(&p["id"])
            ));
            if !d["switch_preview"].is_null() {
                o.push_str("a switch to it would change:\n");
                o.push_str(&diff_text(&d["switch_preview"]));
            }
        }
        Cmd::ProfileApply => {
            if d["dry_run"] == true {
                o.push_str(&format!(
                    "preview, nothing changed: {} → {} (starts in mode {})\n",
                    s(&d["from"]),
                    s(&d["to"]),
                    s(&d["mode"])
                ));
                o.push_str(&diff_text(&d["preview"]));
            } else if d["changed"] == false {
                o.push_str(&format!("{} is already the active profile\n", s(&d["to"])));
            } else {
                o.push_str(&format!(
                    "active profile: {} → {} (mode {})\n",
                    s(&d["from"]),
                    s(&d["to"]),
                    s(&d["mode"])
                ));
                o.push_str(&write_line(d));
                match d["switched"].as_bool() {
                    Some(true) => o.push_str("the running app switched: the old profile's runs ended and every held key was released\n"),
                    Some(false) => o.push_str("the running app did not confirm the switch (see warnings)\n"),
                    None => {}
                }
            }
        }
        Cmd::ProfileSave => {
            if d["dry_run"] == true {
                o.push_str(&format!(
                    "dry run: would keep a save point of {} at revision {}\n",
                    s(&d["profile"]),
                    s(&d["revision"])
                ));
            } else if !d["save_point"].is_null() {
                o.push_str(&format!(
                    "save point {} of {} (revision {}){}\n",
                    s(&d["save_point"]),
                    s(&d["profile"]),
                    s(&d["revision"]),
                    d["label"]
                        .as_str()
                        .map(|l| format!(": {l}"))
                        .unwrap_or_default()
                ));
            }
        }
        Cmd::ProfileHistory => {
            o.push_str(&format!(
                "versions of {}{}, newest first:\n",
                s(&d["profile"]),
                if d["exists"] == false {
                    " (deleted)"
                } else {
                    ""
                }
            ));
            for v in d["versions"].as_array().into_iter().flatten() {
                o.push_str(&format!(
                    "{} {:<40} r{:<4} {:<10} {:>3} rules{}{}{}\n",
                    if v["current"] == true { "*" } else { " " },
                    s(&v["name"]),
                    s(&v["revision"]),
                    s(&v["kind"]),
                    s(&v["rules"]),
                    if v["working"] == false {
                        ", has errors"
                    } else {
                        ""
                    },
                    v["label"]
                        .as_str()
                        .map(|l| format!(", \"{l}\""))
                        .unwrap_or_default(),
                    v["command"]
                        .as_str()
                        .map(|c| format!(" ({c})"))
                        .unwrap_or_default()
                ));
            }
            match d["rollback_to"].as_str() {
                Some(n) => o.push_str(&format!(
                    "`ddalgak profile rollback {}` goes back to {n}\n",
                    s(&d["profile"])
                )),
                None => o.push_str("no earlier working version to roll back to\n"),
            }
        }
        Cmd::ProfileRestore | Cmd::ProfileRollback => {
            o.push_str(&format!(
                "{} {} from {} (revision {}):\n",
                if d["dry_run"] == true {
                    "would restore"
                } else {
                    "restored"
                },
                s(&d["profiles"][0]),
                s(&d["source"]),
                s(&d["restored_revision"])
            ));
            o.push_str(&diff_text(&d["preview"]));
            o.push_str(&write_line(d));
        }
        Cmd::ProfileExport => {
            o.push_str(&format!(
                "{} {} profile(s) with {} rule(s) to {}\n",
                if d["dry_run"] == true {
                    "would export"
                } else {
                    "exported"
                },
                d["profiles"].as_array().map_or(0, Vec::len),
                s(&d["rules"]),
                s(&d["file"])
            ));
            o.push_str(&format!(
                "every rule is off and without consent; {} path(s) made portable, {} environment value(s) left out\n",
                s(&d["portable_paths"]),
                s(&d["env_values_removed"])
            ));
        }
        Cmd::ProfileImport => {
            let c = &d["counts"];
            o.push_str(&format!(
                "{} {} profile(s) with {} rule(s){}\n",
                if d["dry_run"] == true {
                    "would import"
                } else {
                    "imported"
                },
                s(&c["profiles"]),
                s(&c["rules"]),
                d["file"]["by_version"]
                    .as_str()
                    .map(|v| format!(" (exported by Ddalgak {v})"))
                    .unwrap_or_default()
            ));
            for p in d["profiles"].as_array().into_iter().flatten() {
                o.push_str(&format!(
                    "profile {} ({}){}{} — not active\n",
                    s(&p["id"]),
                    s(&p["title"]),
                    p["renamed_from"]
                        .as_str()
                        .map(|r| format!(", renamed from {r}"))
                        .unwrap_or_default(),
                    p["restored_modes"]
                        .as_array()
                        .filter(|m| !m.is_empty())
                        .map(|m| format!(
                            ", restored modes: {}",
                            m.iter().map(s).collect::<Vec<_>>().join(", ")
                        ))
                        .unwrap_or_default()
                ));
                for r in p["rules"].as_array().into_iter().flatten() {
                    let mut notes = Vec::new();
                    if let Some(from) = r["renamed_from"].as_str() {
                        notes.push(format!("renamed from {from}"));
                    }
                    let perms: Vec<String> = r["permissions"]
                        .as_array()
                        .into_iter()
                        .flatten()
                        .map(s)
                        .collect();
                    if !perms.is_empty() {
                        notes.push(format!("needs consent: {}", perms.join(", ")));
                    }
                    if r["missing_programs"].as_u64().unwrap_or(0) > 0 {
                        notes.push("program not found on this computer".into());
                    }
                    for c in r["conflicts"].as_array().into_iter().flatten() {
                        notes.push(match c["rule"].as_str() {
                            Some(x) => format!("same trigger as {x}"),
                            None => format!("Ddalgak's own shortcut {}", s(&c["action"])),
                        });
                    }
                    if let Some(code) = r["inactive"].as_str() {
                        notes.push(format!("stays inactive: {code}"));
                    }
                    o.push_str(&format!(
                        "  {:<40} {:<22} off  {}{}\n",
                        s(&r["id"]),
                        s(&r["trigger"]),
                        s(&r["title"]),
                        if notes.is_empty() {
                            String::new()
                        } else {
                            format!(" [{}]", notes.join("; "))
                        }
                    ));
                }
            }
            o.push_str(&write_line(d));
        }
        _ => o.push_str(&write_line(d)),
    }
    if let Some(notes) = d["notes"].as_array() {
        for n in notes {
            o.push_str(&format!("note: {}\n", s(n)));
        }
    }
    if let Some(p) = d["problems"].as_array().filter(|p| !p.is_empty()) {
        o.push_str(&rule_cmd::problems_text(&json!(p)));
    }
    o
}
