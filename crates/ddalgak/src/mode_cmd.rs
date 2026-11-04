//! The `ddalgak mode …` commands (Step 72, docs/DDALGAK_RULE_MODEL.md §7.3, §13,
//! `DDALGAK-FR-016`, `DDALGAK-FR-060`).
//!
//! - `mode list|show|add|edit|rename|copy|delete` read and change the mode **definitions** in
//!   `rules.json` through the rule store, exactly like the `rule …` commands (lock, revision,
//!   validation before and after, `.bak`, history, `--draft`, `--dry-run`, `--if-revision`), and
//!   work without the app; after a saved change a running app reloads.
//! - `mode set|toggle|reset|status` read and change the **active** mode of the running engine
//!   over the identity-checked pipe (`main.rs`); the CLI never starts the app. The active mode is
//!   never written to `rules.json` (ADR 0043).

use crate::cli::{Args, Cmd};
use crate::output::{CliError, Code};
use crate::rule_cmd::{self, Notify};
use ddalgak::mode_edit::{self, ModeFields};
use ddalgak::modes::ModeSet;
use ddalgak::paths::Paths;
use ddalgak::store::Files;
use serde_json::{Value, json};

type Outcome = crate::Outcome;

/// What the running app says the active mode is (`None` without the app).
pub type Active<'a> = &'a dyn Fn() -> Option<(String, String)>;

fn fields(a: &Args) -> Result<ModeFields, CliError> {
    let opt = |name: &str, clear: &str| -> Result<Option<Option<String>>, CliError> {
        match (a.get(name), a.has(clear)) {
            (Some(_), true) => Err(CliError::usage(format!("{name} and {clear} conflict"))),
            (Some(v), false) => Ok(Some(Some(v.to_string()))),
            (None, true) => Ok(Some(None)),
            (None, false) => Ok(None),
        }
    };
    let one_shot = match (a.has("--one-shot"), a.has("--no-one-shot")) {
        (true, true) => return Err(CliError::usage("--one-shot and --no-one-shot conflict")),
        (true, false) => Some(true),
        (false, true) => Some(false),
        (false, false) => None,
    };
    let timeout = match (
        rule_cmd::num::<u32>(a, "--timeout-ms")?,
        a.has("--no-timeout"),
    ) {
        (Some(_), true) => {
            return Err(CliError::usage("--timeout-ms and --no-timeout conflict"));
        }
        (Some(t), false) => Some(Some(t)),
        (None, true) => Some(None),
        (None, false) => None,
    };
    Ok(ModeFields {
        title: a.get("--title").map(str::to_string),
        icon: opt("--icon", "--no-icon")?,
        color: opt("--color", "--no-color")?,
        parent: opt("--parent", "--no-parent")?,
        one_shot,
        timeout_ms: timeout,
        make_default: a.has("--default"),
    })
}

/// `mode list` / `mode show <id>`.
fn read(cmd: Cmd, a: &Args, files: &Files, paths: &Paths, active: Active<'_>) -> Outcome {
    let ctx = rule_cmd::context(paths);
    let (l, src) = rule_cmd::source(a, files, &ctx)?;
    let doc = l.doc.as_ref().expect("usable");
    let pid = a
        .get("--profile")
        .unwrap_or(&doc.active_profile)
        .to_string();
    let p = doc.profile(&pid).ok_or_else(|| {
        CliError::new(Code::ProfileNotFound, format!("no profile `{pid}`"))
            .hint("see `ddalgak rule list`")
    })?;
    let set = ModeSet::from_profile(p);
    // The running engine's mode, only for the profile it runs.
    let live = active().filter(|(prof, _)| *prof == p.id).map(|(_, m)| m);
    let mut warnings = Vec::new();
    if src == "draft" && live.is_some() {
        warnings.push("the active mode belongs to the saved rules, not to the draft".into());
    }
    if cmd == Cmd::ModeShow {
        let id = &a.positional[0];
        let i = p.modes.iter().position(|m| &m.id == id).ok_or_else(|| {
            CliError::new(
                Code::ModeNotFound,
                format!("no mode `{id}` in profile `{pid}`"),
            )
            .hint("see `ddalgak mode list`")
        })?;
        let mut m = mode_edit::row(p, &set, i, live.as_deref(), &l.report);
        m["rule_ids"] = json!(mode_edit::rules_of(p, id));
        return Ok((
            json!({
                "source": src,
                "revision": l.revision(),
                "profile": p.id,
                "mode": m,
            }),
            warnings,
        ));
    }
    let rows: Vec<Value> = (0..p.modes.len())
        .map(|i| mode_edit::row(p, &set, i, live.as_deref(), &l.report))
        .collect();
    Ok((
        json!({
            "source": src,
            "revision": l.revision(),
            "profile": p.id,
            "default": p.default_mode,
            "active": live,
            "indicator": doc
                .settings
                .as_ref()
                .and_then(|s| s.mode_indicator)
                .unwrap_or(mode_edit::INDICATOR_DEFAULT),
            "modes": rows,
        }),
        warnings,
    ))
}

/// Runs a mode command that reads or writes `rules.json`.
pub fn run(cmd: Cmd, a: &Args, paths: &Paths, notify: Notify<'_>, active: Active<'_>) -> Outcome {
    let files = Files::new(paths);
    let ctx = rule_cmd::context(paths);
    let profile = a.get("--profile").map(str::to_string);
    let profile = profile.as_deref();
    match cmd {
        Cmd::ModeList | Cmd::ModeShow => read(cmd, a, &files, paths, active),
        Cmd::ModeAdd => {
            let f = fields(a)?;
            if f.title.is_none() {
                return Err(CliError::usage("`mode add` needs --title <name>"));
            }
            let id = a.get("--id").map(str::to_string);
            rule_cmd::write(cmd, a, &files, &ctx, notify, |doc| {
                mode_edit::add(doc, profile, id, &f)
            })
        }
        Cmd::ModeEdit => {
            let f = fields(a)?;
            if f == ModeFields::default() {
                return Err(CliError::usage(
                    "nothing to change: give --title, --icon, --color, --parent, --one-shot, --timeout-ms, --default or a --no-… option",
                ));
            }
            let id = a.positional[0].clone();
            rule_cmd::write(cmd, a, &files, &ctx, notify, |doc| {
                mode_edit::edit(doc, profile, &id, &f)
            })
        }
        Cmd::ModeRename => {
            let id = a.positional[0].clone();
            let title = a.positional[1].clone();
            rule_cmd::write(cmd, a, &files, &ctx, notify, |doc| {
                mode_edit::rename(doc, profile, &id, &title)
            })
        }
        Cmd::ModeCopy => {
            let id = a.positional[0].clone();
            let new_id = a.get("--id").map(str::to_string);
            let title = a.get("--title").map(str::to_string);
            let with_rules = !a.has("--without-rules");
            rule_cmd::write(cmd, a, &files, &ctx, notify, |doc| {
                mode_edit::copy(doc, profile, &id, new_id, title, with_rules)
            })
        }
        Cmd::ModeDelete => {
            let id = a.positional[0].clone();
            let delete_rules = a.has("--delete-rules");
            {
                // Name what would go before asking.
                let l = rule_cmd::saved(&files, &ctx)?;
                let doc = l.doc.as_ref().expect("usable");
                let pid = profile.unwrap_or(&doc.active_profile);
                let p = doc.profile(pid).ok_or_else(|| {
                    CliError::new(Code::ProfileNotFound, format!("no profile `{pid}`"))
                })?;
                if !p.modes.iter().any(|m| m.id == id) {
                    return Err(CliError::new(
                        Code::ModeNotFound,
                        format!("no mode `{id}` in profile `{pid}`"),
                    )
                    .hint("see `ddalgak mode list`"));
                }
                let rules = mode_edit::rules_of(p, &id);
                let question = if delete_rules && !rules.is_empty() {
                    format!(
                        "Delete mode `{id}` and the rules limited to it alone ({} rule(s) use it)?",
                        rules.len()
                    )
                } else {
                    format!("Delete mode `{id}`?")
                };
                rule_cmd::confirm(a, &question)?;
            }
            rule_cmd::write(cmd, a, &files, &ctx, notify, |doc| {
                mode_edit::delete(doc, profile, &id, delete_rules)
            })
        }
        _ => unreachable!("not a mode file command"),
    }
}

fn s(v: &Value) -> String {
    match v {
        Value::String(s) => s.clone(),
        Value::Null => "-".into(),
        other => other.to_string(),
    }
}

/// The human line of a mode block (`status`, `mode status`, `mode set`).
pub fn mode_line(m: &Value) -> String {
    let mut line = format!("mode: {}", s(&m["title"]));
    if m["active"] != m["title"] {
        line.push_str(&format!(" ({})", s(&m["active"])));
    }
    if let Some(held) = m["held"].as_array().filter(|h| !h.is_empty()) {
        line.push_str(&format!(
            ", held over {} ({} layer{})",
            s(&m["latched"]),
            held.len(),
            if held.len() == 1 { "" } else { "s" }
        ));
    }
    if m["chain"].as_array().is_some_and(|c| c.len() > 1) {
        let chain: Vec<String> = m["chain"].as_array().into_iter().flatten().map(s).collect();
        line.push_str(&format!("; keys fall back to {}", chain[1..].join(" → ")));
    }
    if m["live"] == false {
        line.push_str(" — the engine starts in it");
    } else {
        line.push_str(&format!(
            " — since {}, by {}",
            s(&m["since"]),
            s(&m["origin"])
        ));
    }
    line.push('\n');
    line
}

/// Human output of the mode commands.
pub fn human(cmd: Cmd, d: &Value) -> String {
    let mut o = String::new();
    match cmd {
        Cmd::ModeList => {
            o.push_str(&format!(
                "profile {} (revision {}, {}): default mode {}{}\n",
                s(&d["profile"]),
                s(&d["revision"]),
                s(&d["source"]),
                s(&d["default"]),
                d["active"]
                    .as_str()
                    .map_or_else(String::new, |a| format!(", active now: {a}"))
            ));
            for m in d["modes"].as_array().into_iter().flatten() {
                let mut flags = Vec::new();
                if m["default"] == true {
                    flags.push("default".to_string());
                }
                if m["active"] == true {
                    flags.push("active".to_string());
                }
                if let Some(p) = m["parent"].as_str() {
                    flags.push(format!("layer over {p}"));
                }
                if m["one_shot"] == true {
                    flags.push("one key".into());
                }
                if let Some(t) = m["timeout_ms"].as_u64() {
                    flags.push(format!("back after {t} ms idle"));
                }
                o.push_str(&format!(
                    "  {} {:<14} {}{}  {} rule(s){}\n",
                    if m["active"] == true { "●" } else { " " },
                    s(&m["id"]),
                    m["icon"]
                        .as_str()
                        .map_or_else(String::new, |i| format!("{i} ")),
                    s(&m["title"]),
                    s(&m["rules"]),
                    if flags.is_empty() {
                        String::new()
                    } else {
                        format!("  [{}]", flags.join(", "))
                    }
                ));
                for p in m["problems"].as_array().into_iter().flatten() {
                    o.push_str(&format!(
                        "      {} ({}): {}\n",
                        s(&p["code"]),
                        s(&p["severity"]),
                        s(&p["message"])
                    ));
                }
            }
        }
        Cmd::ModeShow => {
            let m = &d["mode"];
            o.push_str(&format!("{}  {}\n", s(&m["id"]), s(&m["title"])));
            o.push_str(&format!(
                "  policy: {} (keys fall back along {})\n",
                s(&m["policy"]),
                m["chain"].as_array().map_or_else(String::new, |c| c
                    .iter()
                    .map(s)
                    .collect::<Vec<_>>()
                    .join(" → "))
            ));
            o.push_str(&format!(
                "  default: {}  one key: {}  timeout: {}  icon: {}  color: {}\n",
                s(&m["default"]),
                s(&m["one_shot"]),
                s(&m["timeout_ms"]),
                s(&m["icon"]),
                s(&m["color"])
            ));
            let list = |k: &str| {
                m[k].as_array()
                    .map(|a| a.iter().map(s).collect::<Vec<_>>().join(", "))
                    .filter(|t| !t.is_empty())
                    .unwrap_or_else(|| "-".into())
            };
            o.push_str(&format!("  rules limited to it: {}\n", list("rule_ids")));
            o.push_str(&format!(
                "  rules that switch to it: {}\n",
                list("switched_by")
            ));
            o.push_str(&format!("  layers over it: {}\n", list("layers")));
        }
        Cmd::ModeAdd | Cmd::ModeEdit | Cmd::ModeRename | Cmd::ModeCopy | Cmd::ModeDelete => {
            let verb = if d["dry_run"] == true {
                "would change"
            } else {
                "changed"
            };
            o.push_str(&format!(
                "{verb} mode(s) {} (revision {} → {})\n",
                d["modes"].as_array().map_or_else(String::new, |m| m
                    .iter()
                    .map(s)
                    .collect::<Vec<_>>()
                    .join(", ")),
                s(&d["revision"]["from"]),
                s(&d["revision"]["to"])
            ));
            for n in d["notes"].as_array().into_iter().flatten() {
                o.push_str(&format!("  {}\n", s(n)));
            }
            if d["app"]["running"] == true {
                o.push_str(&format!(
                    "the running app reloaded: {}\n",
                    s(&d["app"]["outcome"])
                ));
            }
        }
        Cmd::ModeSet | Cmd::ModeToggle | Cmd::ModeReset => {
            let c = &d["change"];
            if d["dry_run"] == true {
                o.push_str(&format!(
                    "would {} the mode (now {})\n",
                    s(&c["op"]),
                    s(&c["from"])
                ));
            } else if c["changed"] == true {
                o.push_str(&format!("mode {} → {}\n", s(&c["from"]), s(&c["to"])));
            } else {
                o.push_str(&format!("mode stays {}\n", s(&c["to"])));
            }
            o.push_str(&mode_line(&d["mode"]));
        }
        Cmd::ModeStatus => {
            o.push_str(&format!(
                "engine: {}  profile: {}\n",
                s(&d["engine"]),
                s(&d["profile"])
            ));
            o.push_str(&mode_line(&d["mode"]));
        }
        _ => {}
    }
    o
}
