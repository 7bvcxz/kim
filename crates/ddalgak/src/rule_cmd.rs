//! The `ddalgak rule …` and `ddalgak rules …` commands (Step 67, docs/DDALGAK_RULE_MODEL.md §13,
//! `DDALGAK-FR-005…007`, `DDALGAK-FR-009`, `DDALGAK-FR-060`, `DDALGAK-SEC-006`).
//!
//! They edit `rules.json` directly, under the store's lock, and work without the app. After a
//! saved change the CLI asks a running app to reload (`rules.reload` over the identity-checked
//! pipe) and reports whether the active rules now equal the saved ones; it never starts the app.
//! `--draft` edits `rules.draft.json` instead (saved and active rules stay as they are);
//! `rules draft apply` saves the draft. Every change runs the full validation before and after
//! and is refused, with nothing written, when it would introduce an error.

use crate::cli::{Args, Cmd};
use crate::output::{CliError, Code};
use ddalgak::edit::{self, EditError, NewRule, Position, StepInputs};
use ddalgak::model::{
    Action, Document, InjectedPolicy, OnError, Permission, Rule, RuleEntry, TriggerOn,
};
use ddalgak::paths::Paths;
use ddalgak::rules;
use ddalgak::shortcuts::Shortcuts;
use ddalgak::store::{self, Draft, FileState, Files, Origin, Source, StoreError};
use ddalgak::trigger::TriggerError;
use ddalgak::validate::{self, Context, Problem, Severity};
use serde_json::{Map, Value, json};
use std::collections::BTreeSet;
use std::io::IsTerminal;

/// Asks a running app to reload (`dry_run` passes through); `engine_not_running` without one.
pub type Notify<'a> = &'a dyn Fn(bool) -> Result<Value, CliError>;

type Outcome = crate::Outcome;

/// What the checks know beyond the file: Ddalgak's own shortcuts, and missing program files.
fn context(paths: &Paths) -> Context {
    Context {
        shortcuts: Shortcuts::load(paths, false)
            .list
            .iter()
            .map(|s| (s.action.clone(), s.keys.clone()))
            .collect(),
        check_targets: true,
    }
}

#[allow(clippy::needless_pass_by_value)] // a `map_err` adapter
fn store_err(e: StoreError) -> CliError {
    let msg = e.to_string();
    match e {
        StoreError::PermissionDenied(_) => CliError::new(Code::PermissionDenied, msg)
            .hint("Ddalgak never elevates; make the folder and rules.json writable for your account"),
        StoreError::Busy(_) => CliError::new(Code::Busy, msg)
            .hint("another `ddalgak rule …` command or the editor is saving; try again"),
        StoreError::Invalid(_) => CliError::new(Code::RulesInvalid, format!("rules.json is not usable: {msg}"))
            .hint("it is never rewritten automatically: fix it by hand, or restore the backup with `ddalgak rules restore --backup --yes` (the file is kept as rules.corrupt-<time>.json)"),
        StoreError::Newer(_) => CliError::new(Code::RulesFromNewerVersion, msg)
            .hint("use the newer Ddalgak that wrote it"),
        StoreError::Conflict { .. } => CliError::new(Code::RuleConflict, msg)
            .hint("read the rules again (`ddalgak rule list`) and repeat the change"),
        StoreError::NotFound(_) => CliError::new(Code::SnapshotNotFound, msg)
            .hint("see `ddalgak rules history`"),
        StoreError::Io(_) => CliError::new(Code::Io, msg),
    }
}

fn problem_line(p: &Problem) -> String {
    let who = p
        .rule
        .as_deref()
        .map_or_else(String::new, |r| format!("rule `{r}`: "));
    let mut s = format!("{who}{} ({}): {}", p.code, p.severity.as_str(), p.message);
    if let Some(rule) = &p.rule {
        let offers: Vec<String> = p
            .offers
            .iter()
            .filter(|o| !matches!(o, validate::Offer::Keep))
            .map(|o| o.hint(rule))
            .collect();
        if !offers.is_empty() {
            s.push_str(&format!(" — you may: {}", offers.join("; or ")));
        }
    }
    s
}

#[allow(clippy::needless_pass_by_value)] // a `map_err` adapter
fn edit_err(e: EditError) -> CliError {
    let code = Code::parse(e.code()).unwrap_or(Code::Internal);
    let details: Vec<String> = e.problems().iter().map(problem_line).collect();
    let mut err = CliError::new(code, e.message()).details(details);
    err.hint = Some(match &e {
        EditError::Conflict { .. } => {
            "nothing was written; pick an offer above (they are never applied automatically)".into()
        }
        EditError::ConsentRequired { rule, permissions } => format!(
            "approve it explicitly: ddalgak rule enable {rule} {}",
            permissions
                .iter()
                .map(|p| format!("--allow {}", p.as_str()))
                .collect::<Vec<_>>()
                .join(" ")
        ),
        EditError::RuleNotFound(_) => "see `ddalgak rule list`".into(),
        EditError::ModeNotFound(_) | EditError::ProfileNotFound(_) => {
            "modes and profiles are created in the editor (Steps 72 and 75) or in rules.json".into()
        }
        _ => "see `ddalgak --help`".into(),
    });
    err
}

fn trigger_err(e: &TriggerError) -> CliError {
    match e {
        TriggerError::Reserved(_) => CliError::new(Code::HotkeyReserved, format!("the trigger is reserved: {e}"))
            .hint("reserved chords (Windows key chords, Alt+Tab, Ctrl+Alt+Delete, F12, the emergency chord) are never rule triggers"),
        TriggerError::Invalid(_) => CliError::new(Code::HotkeyInvalid, format!("not a trigger: {e}"))
            .hint("keys like Ctrl+Shift+J, CapsLock, RAlt or a sequence `Ctrl+K Ctrl+U` (docs/HOTKEY_CONTRACT.md §5)"),
    }
}

/// The saved file, which must be usable.
fn saved(files: &Files, ctx: &Context) -> Result<store::Loaded, CliError> {
    let l = store::read(files, ctx);
    match &l.state {
        FileState::Invalid(why) => Err(store_err(StoreError::Invalid(why.clone()))),
        FileState::NewerVersion(v) => Err(store_err(StoreError::Newer(*v))),
        _ => Ok(l),
    }
}

/// A document checked as if it were the file (drafts, documents being edited).
fn analyze(doc: &Document, ctx: &Context) -> store::Loaded {
    store::interpret(doc.to_text().as_bytes(), ctx, store::MIGRATIONS)
}

/// What a read command looks at: the saved file, or the draft with `--draft`.
fn source(
    a: &Args,
    files: &Files,
    ctx: &Context,
) -> Result<(store::Loaded, &'static str), CliError> {
    if a.has("--draft") {
        let d = store::read_draft(files)
            .map_err(store_err)?
            .ok_or_else(no_draft)?;
        Ok((analyze(&d.doc, ctx), "draft"))
    } else {
        Ok((saved(files, ctx)?, "saved"))
    }
}

fn no_draft() -> CliError {
    CliError::new(
        Code::SnapshotNotFound,
        "there is no draft (rules.draft.json)",
    )
    .hint("start one with any `ddalgak rule …` change and --draft")
}

fn steps_of(e: &RuleEntry) -> Vec<String> {
    match e {
        RuleEntry::Typed(r) => r.steps.iter().map(|s| s.kind().to_string()).collect(),
        RuleEntry::Raw(v) => v["steps"]
            .as_array()
            .into_iter()
            .flatten()
            .map(|s| s["type"].as_str().unwrap_or("?").to_string())
            .collect(),
    }
}

fn counts(r: &validate::Report) -> Value {
    json!({
        "errors": r.count(Severity::Error),
        "warnings": r.count(Severity::Warning),
        "infos": r.count(Severity::Info),
    })
}

/// `rule list`
fn list(a: &Args, files: &Files, ctx: &Context) -> Outcome {
    let (l, src) = source(a, files, ctx)?;
    let doc = l.doc.as_ref().expect("usable");
    if let Some(p) = a.get("--profile")
        && doc.profile(p).is_none()
    {
        return Err(CliError::new(
            Code::ProfileNotFound,
            format!("no profile `{p}`"),
        ));
    }
    let states = rules::rule_states(&l);
    let mut out = Vec::new();
    for s in &states {
        if a.get("--profile").is_some_and(|p| p != s.profile) {
            continue;
        }
        let p = doc.profile(&s.profile).expect("listed");
        let e = &p.rules[s.position];
        let r = e.rule();
        if let Some(m) = a.get("--mode") {
            let modes = r.and_then(|r| r.modes.as_ref());
            if modes.is_some_and(|ms| !ms.iter().any(|x| x == m)) {
                continue;
            }
        }
        let problems = l
            .report
            .problems
            .iter()
            .filter(|x| {
                x.profile.as_deref() == Some(s.profile.as_str())
                    && x.pos.map(|q| q.1) == Some(s.position)
            })
            .filter(|x| x.severity != Severity::Info)
            .count();
        out.push(json!({
            "id": s.id,
            "title": e.title(),
            "profile": s.profile,
            "position": s.position,
            "enabled": e.enabled().unwrap_or(false),
            "trigger": r.map(|r| r.trigger.keys.clone()),
            "on": r.map(|r| r.trigger.on().as_str()),
            "modes": r.and_then(|r| r.modes.clone()),
            "priority": r.map_or(0, Rule::priority),
            "steps": steps_of(e),
            "state": s.state,
            "code": s.code,
            "problems": problems,
        }));
    }
    let data = json!({
        "source": src,
        "file": l.state.as_str(),
        "revision": l.revision(),
        "active_profile": doc.active_profile,
        "profiles": doc.profiles.iter().map(|p| json!({
            "id": p.id,
            "title": p.title,
            "default_mode": p.default_mode,
            "modes": p.modes.iter().map(|m| m.id.clone()).collect::<Vec<_>>(),
            "rules": p.rules.len(),
        })).collect::<Vec<_>>(),
        "rules": out,
        "summary": counts(&l.report),
    });
    Ok((data, Vec::new()))
}

fn rule_problems<'a>(l: &'a store::Loaded, ids: &[String]) -> Vec<&'a Problem> {
    l.report
        .all()
        .filter(|p| p.rule.as_ref().is_some_and(|r| ids.contains(r)))
        .collect()
}

/// `rule show <id>`
fn show(a: &Args, files: &Files, ctx: &Context) -> Outcome {
    let (l, src) = source(a, files, ctx)?;
    let id = &a.positional[0];
    let doc = l.doc.as_ref().expect("usable");
    let Some((pi, ri)) = doc.find_rule(id) else {
        return Err(CliError::new(Code::RuleNotFound, format!("no rule `{id}`"))
            .hint("see `ddalgak rule list`"));
    };
    let state = rules::rule_states(&l)
        .into_iter()
        .find(|s| s.profile == doc.profiles[pi].id && s.position == ri)
        .expect("listed");
    let data = json!({
        "source": src,
        "revision": l.revision(),
        "profile": doc.profiles[pi].id,
        "position": ri,
        "state": state.state,
        "code": state.code,
        "rule": doc.profiles[pi].rules[ri].to_value(),
        "problems": rule_problems(&l, std::slice::from_ref(id)).iter().map(|p| p.to_json()).collect::<Vec<_>>(),
    });
    Ok((data, Vec::new()))
}

/// `rule check`
fn check(a: &Args, files: &Files, ctx: &Context) -> Outcome {
    let (l, src) = if a.has("--draft") {
        let d = store::read_draft(files)
            .map_err(store_err)?
            .ok_or_else(no_draft)?;
        (analyze(&d.doc, ctx), "draft")
    } else {
        (store::read(files, ctx), "saved")
    };
    let mut problems: Vec<Value> = l.report.all().map(Problem::to_json).collect();
    if problems.is_empty() && !l.state.usable() {
        problems.push(json!({
            "code": if matches!(l.state, FileState::NewerVersion(_)) { "rules_from_newer_version" } else { "rules_invalid" },
            "severity": "error", "profile": null, "rule": null, "at": "",
            "message": l.state.problem().unwrap_or_default(), "with": null, "offers": [],
        }));
    }
    let errors = problems.iter().filter(|p| p["severity"] == "error").count();
    let data = json!({
        "source": src,
        "file": l.state.as_str(),
        "revision": l.revision,
        "ok": errors == 0,
        "summary": {
            "errors": errors,
            "warnings": problems.iter().filter(|p| p["severity"] == "warning").count(),
            "infos": problems.iter().filter(|p| p["severity"] == "info").count(),
        },
        "problems": problems,
    });
    Ok((data, Vec::new()))
}

fn num<T: std::str::FromStr>(a: &Args, name: &str) -> Result<Option<T>, CliError> {
    a.get(name)
        .map(|v| {
            v.parse()
                .map_err(|_| CliError::usage(format!("{name} takes a number, not `{v}`")))
        })
        .transpose()
}

fn inputs(a: &Args) -> Result<StepInputs, CliError> {
    Ok(StepInputs {
        to: a.get("--to").map(str::to_string),
        tap: a.get("--tap").map(str::to_string),
        tap_ms: num(a, "--tap-ms")?,
        keys: a.get("--keys").map(str::to_string),
        step_delay_ms: num(a, "--step-delay-ms")?,
        text: a.get("--text").map(str::to_string),
        method: a.get("--method").map(str::to_string),
        key_delay_ms: num(a, "--key-delay-ms")?,
        path: a.get("--path").map(str::to_string),
        app_id: a.get("--app-id").map(str::to_string),
        args: a.all("--arg").to_vec(),
        working_dir: a.get("--working-dir").map(str::to_string),
        window: a.get("--window").map(str::to_string),
        if_running: a.get("--if-running").map(str::to_string),
        url: a.get("--url").map(str::to_string),
        op: a.get("--op").map(str::to_string),
        target_mode: a
            .all("--target-mode")
            .first()
            .cloned()
            .filter(|_| a.all("--target-mode").len() == 1),
        target_modes: if a.all("--target-mode").len() > 1 {
            a.all("--target-mode").to_vec()
        } else {
            Vec::new()
        },
        ms: num(a, "--ms")?,
    })
}

/// Steps from `--action` (with its flags) or `--step` JSON, if any were given.
fn steps(a: &Args) -> Result<Option<Vec<Action>>, CliError> {
    match (a.get("--action"), a.all("--step")) {
        (Some(_), s) if !s.is_empty() => Err(CliError::usage("use --action or --step, not both")),
        (Some(kind), _) => Ok(Some(vec![
            edit::build_step(kind, &inputs(a)?).map_err(CliError::usage)?,
        ])),
        (None, s) if !s.is_empty() => s
            .iter()
            .map(|j| edit::parse_step(j).map_err(CliError::usage))
            .collect::<Result<Vec<_>, _>>()
            .map(Some),
        _ => Ok(None),
    }
}

fn permissions(a: &Args) -> Result<Vec<Permission>, CliError> {
    a.all("--allow")
        .iter()
        .map(|p| {
            Permission::parse(p).ok_or_else(|| {
                CliError::usage(format!(
                    "unknown permission `{p}` (input-injection, launch-programs or clipboard)"
                ))
            })
        })
        .collect()
}

fn enum_flag<T>(a: &Args, name: &str, values: &[(&str, T)]) -> Result<Option<T>, CliError>
where
    T: Copy,
{
    a.get(name)
        .map(|v| {
            values
                .iter()
                .find(|(n, _)| *n == v)
                .map(|(_, t)| *t)
                .ok_or_else(|| {
                    CliError::usage(format!(
                        "{name} takes {}",
                        values
                            .iter()
                            .map(|(n, _)| *n)
                            .collect::<Vec<_>>()
                            .join(", ")
                    ))
                })
        })
        .transpose()
}

const ON: &[(&str, TriggerOn)] = &[
    ("press", TriggerOn::Press),
    ("release", TriggerOn::Release),
    ("hold", TriggerOn::Hold),
];
const ON_ERROR: &[(&str, OnError)] = &[("stop", OnError::Stop), ("continue", OnError::Continue)];

fn canonical(keys: &str) -> Result<String, CliError> {
    edit::canonical_trigger(keys).map_err(|e| trigger_err(&e))
}

/// A short default title: `F13: engine pause`.
fn default_title(keys: &str, steps: &[Action]) -> String {
    let what = match steps.first() {
        Some(Action::Engine(e)) => format!("engine {}", e.op.as_str()),
        Some(Action::Remap(r)) => format!("acts as {}", r.to),
        Some(Action::SendKeys(s)) => format!("sends {}", s.keys),
        Some(s) => s.kind().replace('_', " "),
        None => "rule".into(),
    };
    let t = format!("{keys}: {what}");
    t.chars().take(80).collect()
}

/// The `NewRule` of `rule add`.
fn new_rule(a: &Args) -> Result<NewRule, CliError> {
    let mut base = json!({});
    if let Some(r) = a.get("--rule") {
        let v: Value = serde_json::from_str(r)
            .map_err(|e| CliError::usage(format!("--rule is not JSON: {e}")))?;
        if !v.is_object() {
            return Err(CliError::usage("--rule takes a JSON object (a rule)"));
        }
        base = v;
    }
    let keys = match a.get("--trigger") {
        Some(k) => canonical(k)?,
        None => base["trigger"]["keys"]
            .as_str()
            .map(canonical)
            .transpose()?
            .ok_or_else(|| CliError::usage("`rule add` needs --trigger <keys>"))?,
    };
    let steps = match steps(a)? {
        Some(s) => s,
        None => match base.get("steps") {
            Some(s) => serde_json::from_value(s.clone())
                .map_err(|e| CliError::usage(format!("--rule has unusable steps: {e}")))?,
            None => {
                return Err(CliError::usage(
                    "`rule add` needs an action: --action <type> with its options, or --step '<json>'",
                ));
            }
        },
    };
    let mut v = base;
    v["id"] = json!("placeholder");
    if v.get("title").is_none() {
        v["title"] = json!(default_title(&keys, &steps));
    }
    if v.get("enabled").is_none() {
        v["enabled"] = json!(true);
    }
    if v["trigger"].is_object() {
        v["trigger"]["keys"] = json!(keys);
    } else {
        v["trigger"] = json!({ "keys": keys });
    }
    v["steps"] = serde_json::to_value(&steps).unwrap_or_default();
    let mut rule: Rule = serde_json::from_value(v)
        .map_err(|e| CliError::new(Code::RuleInvalid, format!("not a rule: {e}")))?;
    apply_fields(a, &mut rule)?;
    if a.has("--disabled") {
        rule.enabled = false;
    }
    let position = match (a.get("--before"), a.get("--after")) {
        (Some(_), Some(_)) => return Err(CliError::usage("use --before or --after, not both")),
        (Some(b), None) => Position::Before(b.to_string()),
        (None, Some(x)) => Position::After(x.to_string()),
        (None, None) => Position::End,
    };
    Ok(NewRule {
        id: a.get("--id").map(str::to_string),
        profile: a.get("--profile").map(str::to_string),
        position,
        rule,
        allow: permissions(a)?,
    })
}

/// The rule fields given as flags (`--title`, `--on`, `--mode`, …).
fn apply_fields(a: &Args, rule: &mut Rule) -> Result<(), CliError> {
    if let Some(t) = a.get("--title") {
        rule.title = t.to_string();
    }
    if let Some(d) = a.get("--description") {
        rule.description = (!d.is_empty()).then(|| d.to_string());
    }
    if let Some(on) = enum_flag(a, "--on", ON)? {
        rule.trigger.on = Some(on);
    }
    if let Some(ms) = num(a, "--hold-ms")? {
        rule.trigger.hold_ms = Some(ms);
    }
    if a.has("--repeat") {
        rule.trigger.repeat = Some(true);
    }
    if a.has("--pass-through") {
        rule.trigger.pass_through = Some(true);
    }
    if a.has("--accept-injected") {
        rule.trigger.injected = Some(InjectedPolicy::Accept);
    }
    if !a.all("--mode").is_empty() {
        rule.modes = Some(a.all("--mode").to_vec());
    }
    if let Some(p) = num(a, "--priority")? {
        rule.priority = Some(p);
    }
    if let Some(e) = enum_flag(a, "--on-error", ON_ERROR)? {
        rule.on_error = Some(e);
    }
    Ok(())
}

/// The merge patch of `rule edit` from its flags and `--patch`.
fn edit_patch(a: &Args) -> Result<Value, CliError> {
    let mut patch = Map::new();
    let mut trigger = Map::new();
    if let Some(t) = a.get("--title") {
        patch.insert("title".into(), json!(t));
    }
    if let Some(d) = a.get("--description") {
        patch.insert(
            "description".into(),
            if d.is_empty() { Value::Null } else { json!(d) },
        );
    }
    if let Some(k) = a.get("--trigger") {
        trigger.insert("keys".into(), json!(canonical(k)?));
    }
    if let Some(on) = enum_flag(a, "--on", ON)? {
        trigger.insert("on".into(), json!(on.as_str()));
    }
    if let Some(ms) = num::<u32>(a, "--hold-ms")? {
        trigger.insert("hold_ms".into(), json!(ms));
    }
    if a.has("--repeat") {
        trigger.insert("repeat".into(), json!(true));
    }
    if a.has("--pass-through") {
        trigger.insert("pass_through".into(), json!(true));
    }
    if a.has("--accept-injected") {
        trigger.insert("injected".into(), json!("accept"));
    }
    if !trigger.is_empty() {
        patch.insert("trigger".into(), Value::Object(trigger));
    }
    if a.has("--all-modes") && !a.all("--mode").is_empty() {
        return Err(CliError::usage("use --mode or --all-modes, not both"));
    }
    if a.has("--all-modes") {
        patch.insert("modes".into(), Value::Null);
    }
    if !a.all("--mode").is_empty() {
        patch.insert("modes".into(), json!(a.all("--mode")));
    }
    if let Some(p) = num::<i64>(a, "--priority")? {
        patch.insert("priority".into(), json!(p));
    }
    if let Some(e) = a.get("--on-error") {
        enum_flag(a, "--on-error", ON_ERROR)?;
        patch.insert("on_error".into(), json!(e));
    }
    if let Some(s) = steps(a)? {
        patch.insert("steps".into(), serde_json::to_value(&s).unwrap_or_default());
    }
    let mut patch = Value::Object(patch);
    if let Some(p) = a.get("--patch") {
        let v: Value = serde_json::from_str(p)
            .map_err(|e| CliError::usage(format!("--patch is not JSON: {e}")))?;
        if !v.is_object() {
            return Err(CliError::usage(
                "--patch takes a JSON object (RFC 7396 merge patch)",
            ));
        }
        let mut v = v;
        if let Some(k) = v["trigger"]["keys"].as_str() {
            v["trigger"]["keys"] = json!(canonical(k)?);
        }
        combine(&mut patch, &v);
    }
    Ok(patch)
}

/// Adds the fields of patch `extra` to patch `base`; a `null` stays `null` (it removes the field
/// when the combined patch is applied).
fn combine(base: &mut Value, extra: &Value) {
    if let (Some(b), Some(e)) = (base.as_object_mut(), extra.as_object()) {
        for (k, v) in e {
            match b.get_mut(k) {
                Some(slot) if slot.is_object() && v.is_object() => combine(slot, v),
                _ => {
                    b.insert(k.clone(), v.clone());
                }
            }
        }
    }
}

fn position(a: &Args) -> Result<Position, CliError> {
    match (a.get("--before"), a.get("--after"), a.get("--to")) {
        (Some(b), None, None) => Ok(Position::Before(b.to_string())),
        (None, Some(x), None) => Ok(Position::After(x.to_string())),
        (None, None, Some("start")) => Ok(Position::Start),
        (None, None, Some("end")) => Ok(Position::End),
        (None, None, Some(n)) => n
            .parse()
            .map(Position::Index)
            .map_err(|_| CliError::usage("--to takes start, end or a position (0 is the first)")),
        (None, None, None) => Err(CliError::usage(
            "`rule move` needs --before <id>, --after <id> or --to <position>",
        )),
        _ => Err(CliError::usage("give one of --before, --after and --to")),
    }
}

fn no_input_env() -> bool {
    std::env::var("KIM_NO_INPUT")
        .ok()
        .is_some_and(|v| !v.is_empty() && v != "0" && !v.eq_ignore_ascii_case("false"))
}

/// Confirms a destructive change (CLI_CONTRACT §4): `--yes`, or a prompt when one is allowed.
fn confirm(a: &Args, question: &str) -> Result<(), CliError> {
    if a.yes || a.dry_run {
        return Ok(());
    }
    let can_ask = !a.json
        && !a.no_input
        && !no_input_env()
        && std::io::stdin().is_terminal()
        && std::io::stderr().is_terminal();
    if !can_ask {
        return Err(CliError::new(
            Code::ConfirmationRequired,
            format!("{question} This needs --yes when ddalgak cannot ask."),
        )
        .hint("re-run with --yes (or --dry-run to see what would happen)"));
    }
    eprint!("{question} [y/N] ");
    let mut line = String::new();
    let _ = std::io::stdin().read_line(&mut line);
    if matches!(line.trim().to_ascii_lowercase().as_str(), "y" | "yes") {
        Ok(())
    } else {
        Err(CliError::new(
            Code::Cancelled,
            "cancelled; nothing was changed",
        ))
    }
}

/// What a running app said about a reload, for the `app` field.
fn app_json(r: Result<Value, CliError>, warnings: &mut Vec<String>) -> Value {
    match r {
        Ok(v) => {
            if v["outcome"] == "kept_previous" {
                warnings.push(format!(
                    "the running app kept its previous rules: {}",
                    v["reason"].as_str().unwrap_or("the file is not usable")
                ));
            }
            json!({
                "running": true,
                "outcome": v["outcome"],
                "in_sync": v["in_sync"],
                "active_revision": v["active_set"]["revision"],
            })
        }
        Err(e) if e.code == Code::EngineNotRunning => json!({ "running": false }),
        Err(e) => {
            warnings.push(format!(
                "the change is saved, but the running app did not confirm the reload ({}: {}); it reloads on its own when it notices the file",
                e.code.as_str(),
                e.message
            ));
            json!({ "running": true, "outcome": null, "in_sync": null, "active_revision": null, "error": e.code.as_str() })
        }
    }
}

fn file_name(p: &std::path::Path) -> String {
    p.file_name()
        .map(|n| n.to_string_lossy().to_string())
        .unwrap_or_default()
}

/// One rule change: lock, read (saved or draft), change, review, save, ask the app to reload.
fn write(
    cmd: Cmd,
    a: &Args,
    files: &Files,
    ctx: &Context,
    notify: Notify<'_>,
    op: impl FnOnce(&mut Document) -> Result<edit::Change, EditError>,
) -> Outcome {
    let draft_mode = a.has("--draft");
    // A dry run takes no lock: it creates no file at all.
    let lock = if a.dry_run {
        None
    } else {
        Some(store::lock(files, store::lock_timeout()).map_err(store_err)?)
    };
    let current = saved(files, ctx)?;
    let (mut doc, base) = match (draft_mode, store::read_draft(files).map_err(store_err)?) {
        (true, Some(d)) => (d.doc, d.base),
        _ => (current.doc.clone().expect("usable"), current.revision()),
    };
    if let Some(expected) = num::<u64>(a, "--if-revision")?
        && expected != base
    {
        return Err(store_err(StoreError::Conflict {
            expected,
            found: base,
        }));
    }
    let before = validate::check(&doc.to_value(), ctx);
    let change = op(&mut doc).map_err(edit_err)?;
    let after = validate::check(&doc.to_value(), ctx);
    edit::review(&before, &after).map_err(edit_err)?;
    let touched: Vec<Value> = after
        .all()
        .filter(|p| p.rule.as_ref().is_some_and(|r| change.rules.contains(r)))
        .map(Problem::to_json)
        .collect();
    let mut warnings: Vec<String> = after
        .all()
        .filter(|p| {
            p.severity == Severity::Warning
                && p.rule.as_ref().is_some_and(|r| change.rules.contains(r))
        })
        .map(problem_line)
        .collect();
    let elsewhere = after.count(Severity::Error);
    if elsewhere > 0 {
        warnings.push(format!(
            "{elsewhere} existing error(s) elsewhere in the rules keep those rules inactive; see `ddalgak rule check`"
        ));
    }
    let mut data = json!({
        "dry_run": a.dry_run,
        "target": if draft_mode { "draft" } else { "saved" },
        "changed": change.changed,
        "revision": { "from": base, "to": null },
        "rules": change.rules,
        "created": change.created,
        "notes": change.notes,
        "problems": touched,
        "backup": null,
        "snapshot": null,
        "app": null,
    });
    if !a.dry_run && change.changed {
        let lock = lock.as_ref().expect("locked");
        if draft_mode {
            store::write_draft(files, lock, &Draft { doc, base }).map_err(store_err)?;
            data["revision"]["to"] = json!(base);
        } else {
            let s = store::save(
                files,
                lock,
                &mut doc,
                base,
                &Origin {
                    origin: "cli",
                    command: cmd.name(),
                },
            )
            .map_err(store_err)?;
            data["revision"]["to"] = json!(s.revision);
            data["backup"] = json!(s.backup.as_deref().map(file_name));
            data["snapshot"] = json!(s.snapshot.as_deref().map(file_name));
        }
    }
    drop(lock);
    if !draft_mode && !a.dry_run && change.changed {
        data["app"] = app_json(notify(false), &mut warnings);
    }
    if !change.consent_needed.is_empty() {
        let id = change.created.clone().unwrap_or_default();
        let flags: Vec<String> = change
            .consent_needed
            .iter()
            .map(|p| format!("--allow {}", p.as_str()))
            .collect();
        return Err(CliError::new(
            Code::ConsentRequired,
            format!(
                "rule `{id}` {} disabled: it needs consent for {}",
                if a.dry_run {
                    "would be saved"
                } else {
                    "was saved"
                },
                change
                    .consent_needed
                    .iter()
                    .map(|p| format!("`{}`", p.as_str()))
                    .collect::<Vec<_>>()
                    .join(" and ")
            ),
        )
        .details(vec![
            format!("rule: {id}"),
            format!("saved: {}", !a.dry_run),
            format!("revision: {}", data["revision"]["to"]),
        ])
        .hint(format!(
            "approve it explicitly: ddalgak rule enable {id} {}",
            flags.join(" ")
        )));
    }
    Ok((data, warnings))
}

/// `rules history`
fn history(files: &Files, ctx: &Context) -> Outcome {
    let l = store::read(files, ctx);
    let bak = std::fs::read(files.backup())
        .ok()
        .map(|b| store::interpret(&b, &Context::default(), store::MIGRATIONS));
    let snaps: Vec<Value> = store::history(files)
        .iter()
        .map(|s| json!({ "name": s.name, "revision": s.revision, "at": s.at, "origin": s.origin, "command": s.command }))
        .collect();
    Ok((
        json!({
            "file": l.state.as_str(),
            "revision": l.revision,
            "backup": { "exists": bak.is_some(), "revision": bak.as_ref().and_then(|b| b.revision), "usable": bak.as_ref().is_some_and(|b| b.state.usable()) },
            "snapshots": snaps,
        }),
        Vec::new(),
    ))
}

/// `rules backup`
fn backup(a: &Args, files: &Files, ctx: &Context) -> Outcome {
    let current = saved(files, ctx)?;
    if a.dry_run {
        return Ok((
            json!({ "dry_run": true, "revision": current.revision, "snapshot": null }),
            Vec::new(),
        ));
    }
    let lock = store::lock(files, store::lock_timeout()).map_err(store_err)?;
    let made = store::backup_now(files, &lock).map_err(store_err)?;
    let mut warnings = Vec::new();
    if made.is_none() {
        warnings.push("nothing saved yet: there is no rules.json to back up".into());
    }
    Ok((
        json!({
            "dry_run": false,
            "revision": made.as_ref().map(|(_, r)| *r),
            "snapshot": made.as_ref().map(|(p, _)| file_name(p)),
        }),
        warnings,
    ))
}

/// `rules restore (--backup | <snapshot>)`
fn restore(a: &Args, files: &Files, ctx: &Context, notify: Notify<'_>) -> Outcome {
    let src = match (a.has("--backup"), a.positional.first()) {
        (true, None) => Source::Backup,
        (false, Some(name)) => Source::Snapshot(name.clone()),
        _ => {
            return Err(CliError::usage(
                "`rules restore` takes --backup or one snapshot name from `ddalgak rules history`",
            ));
        }
    };
    let (path, mut doc) = store::restore_source(files, &src).map_err(store_err)?;
    let current = store::read(files, ctx);
    if let FileState::NewerVersion(v) = current.state {
        return Err(store_err(StoreError::Newer(v)));
    }
    let from = current.revision();
    let label = match &src {
        Source::Backup => "the backup (rules.json.bak)".to_string(),
        Source::Snapshot(n) => format!("snapshot {n}"),
    };
    confirm(
        a,
        &format!(
            "Replace the saved rules (revision {from}) with {label} (revision {})?",
            doc.revision()
        ),
    )?;
    let mut data = json!({
        "dry_run": a.dry_run,
        "source": file_name(&path),
        "restored_revision": doc.revision(),
        "revision": { "from": from, "to": null },
        "kept": null,
        "snapshot": null,
        "app": null,
    });
    let mut warnings = Vec::new();
    if !current.state.usable() {
        warnings.push(format!(
            "the current rules.json is not usable ({}); it is kept as rules.corrupt-<time>.json",
            current.state.problem().unwrap_or_default()
        ));
    }
    if a.dry_run {
        return Ok((data, warnings));
    }
    let lock = store::lock(files, store::lock_timeout()).map_err(store_err)?;
    let expected = if current.state.usable() {
        from
    } else {
        doc.revision()
    };
    let s = store::save(
        files,
        &lock,
        &mut doc,
        expected,
        &Origin {
            origin: "restore",
            command: "rules.restore".into(),
        },
    )
    .map_err(store_err)?;
    drop(lock);
    data["revision"]["to"] = json!(s.revision);
    data["kept"] = json!(s.backup.as_deref().map(file_name));
    data["snapshot"] = json!(s.snapshot.as_deref().map(file_name));
    data["app"] = app_json(notify(false), &mut warnings);
    Ok((data, warnings))
}

/// The differences between two documents by rule ID.
fn diff(old: &Document, new: &Document) -> Value {
    let rules = |d: &Document| -> Vec<(String, Value)> {
        d.profiles
            .iter()
            .flat_map(|p| {
                p.rules
                    .iter()
                    .map(|r| (r.id().unwrap_or_default().to_string(), r.to_value()))
            })
            .collect()
    };
    let (o, n) = (rules(old), rules(new));
    let ids = |v: &[(String, Value)]| v.iter().map(|(i, _)| i.clone()).collect::<BTreeSet<_>>();
    let (oi, ni) = (ids(&o), ids(&n));
    let changed: Vec<&String> = n
        .iter()
        .filter(|(i, v)| o.iter().any(|(oi2, ov)| oi2 == i && ov != v))
        .map(|(i, _)| i)
        .collect();
    json!({
        "added": ni.difference(&oi).collect::<Vec<_>>(),
        "removed": oi.difference(&ni).collect::<Vec<_>>(),
        "changed": changed,
    })
}

/// The draft as `status` shows it.
pub fn draft_json(paths: &Paths) -> Value {
    let files = Files::new(paths);
    match store::read_draft(&files) {
        Ok(Some(d)) => {
            let saved = store::read(&files, &Context::default());
            json!({ "base_revision": d.base, "stale": !saved.state.usable() || saved.revision() != d.base })
        }
        Ok(None) => Value::Null,
        Err(e) => json!({ "base_revision": null, "stale": true, "problem": e.to_string() }),
    }
}

/// `rules draft [show]`
fn draft_show(files: &Files, ctx: &Context) -> Outcome {
    let current = store::read(files, ctx);
    let d = store::read_draft(files).map_err(store_err)?;
    let data = match &d {
        None => {
            json!({ "exists": false, "base_revision": null, "saved_revision": current.revision, "stale": false, "changes": null, "summary": null })
        }
        Some(d) => {
            let l = analyze(&d.doc, ctx);
            json!({
                "exists": true,
                "base_revision": d.base,
                "saved_revision": current.revision,
                "stale": !current.state.usable() || current.revision() != d.base,
                "changes": current.doc.as_ref().map(|s| diff(s, &d.doc)),
                "summary": counts(&l.report),
            })
        }
    };
    Ok((data, Vec::new()))
}

/// `rules draft apply`
fn draft_apply(a: &Args, files: &Files, ctx: &Context, notify: Notify<'_>) -> Outcome {
    let lock = if a.dry_run {
        None
    } else {
        Some(store::lock(files, store::lock_timeout()).map_err(store_err)?)
    };
    let d = store::read_draft(files)
        .map_err(store_err)?
        .ok_or_else(no_draft)?;
    let current = saved(files, ctx)?;
    if current.revision() != d.base {
        return Err(CliError::new(
            Code::RuleConflict,
            format!(
                "the saved rules changed since the draft was made (draft based on revision {}, saved is {}); nothing was written",
                d.base,
                current.revision()
            ),
        )
        .hint("discard the draft (`ddalgak rules draft discard --yes`) or make the change again"));
    }
    let before = current.report.clone();
    let after = validate::check(&d.doc.to_value(), ctx);
    edit::review(&before, &after).map_err(edit_err)?;
    let changes = diff(current.doc.as_ref().expect("usable"), &d.doc);
    let mut data = json!({
        "dry_run": a.dry_run,
        "target": "saved",
        "changed": true,
        "revision": { "from": d.base, "to": null },
        "changes": changes,
        "snapshot": null,
        "app": null,
    });
    let mut warnings = Vec::new();
    if a.dry_run {
        return Ok((data, warnings));
    }
    let lock = lock.expect("locked");
    let mut doc = d.doc;
    let s = store::save(
        files,
        &lock,
        &mut doc,
        d.base,
        &Origin {
            origin: "cli",
            command: "rules.draft.apply".into(),
        },
    )
    .map_err(store_err)?;
    store::remove_draft(files, &lock).map_err(store_err)?;
    drop(lock);
    data["revision"]["to"] = json!(s.revision);
    data["snapshot"] = json!(s.snapshot.as_deref().map(file_name));
    data["app"] = app_json(notify(false), &mut warnings);
    Ok((data, warnings))
}

/// `rules draft discard`
fn draft_discard(a: &Args, files: &Files) -> Outcome {
    let d = store::read_draft(files).map_err(store_err)?;
    let Some(d) = d else {
        return Ok((
            json!({ "dry_run": a.dry_run, "discarded": false, "base_revision": null }),
            vec!["there is no draft".into()],
        ));
    };
    confirm(a, "Discard the unsaved draft?")?;
    if !a.dry_run {
        let lock = store::lock(files, store::lock_timeout()).map_err(store_err)?;
        store::remove_draft(files, &lock).map_err(store_err)?;
    }
    Ok((
        json!({ "dry_run": a.dry_run, "discarded": !a.dry_run, "base_revision": d.base }),
        Vec::new(),
    ))
}

/// Runs a `rule` or `rules` command.
pub fn run(cmd: Cmd, a: &Args, paths: &Paths, notify: Notify<'_>) -> Outcome {
    let files = Files::new(paths);
    let ctx = context(paths);
    match cmd {
        Cmd::RuleList => list(a, &files, &ctx),
        Cmd::RuleShow => show(a, &files, &ctx),
        Cmd::RuleCheck => check(a, &files, &ctx),
        Cmd::RuleAdd => {
            let n = new_rule(a)?;
            write(cmd, a, &files, &ctx, notify, |doc| edit::add(doc, n))
        }
        Cmd::RuleEdit => {
            let patch = edit_patch(a)?;
            let allow = permissions(a)?;
            if patch.as_object().is_some_and(Map::is_empty) && allow.is_empty() {
                return Err(CliError::usage(
                    "nothing to change: give a field option or --patch",
                ));
            }
            let id = a.positional[0].clone();
            write(cmd, a, &files, &ctx, notify, |doc| {
                edit::edit(doc, &id, &patch, &allow)
            })
        }
        Cmd::RuleDelete => {
            let ids = a.positional.clone();
            let n = ids.len();
            {
                // Name what would go before asking (a new draft starts from the saved rules).
                let doc = match (
                    a.has("--draft"),
                    store::read_draft(&files).map_err(store_err)?,
                ) {
                    (true, Some(d)) => d.doc,
                    _ => saved(&files, &ctx)?.doc.expect("usable"),
                };
                if let Some(missing) = ids.iter().find(|i| !doc.has_rule_id(i)) {
                    return Err(CliError::new(
                        Code::RuleNotFound,
                        format!("no rule `{missing}`"),
                    ));
                }
            }
            confirm(
                a,
                &format!("Delete {n} rule{}?", if n == 1 { "" } else { "s" }),
            )?;
            write(cmd, a, &files, &ctx, notify, |doc| edit::delete(doc, &ids))
        }
        Cmd::RuleDuplicate => {
            let id = a.positional[0].clone();
            let trigger = a.get("--trigger").map(canonical).transpose()?;
            let new_id = a.get("--id").map(str::to_string);
            let title = a.get("--title").map(str::to_string);
            write(cmd, a, &files, &ctx, notify, |doc| {
                edit::duplicate(doc, &id, new_id, title, trigger)
            })
        }
        Cmd::RuleEnable | Cmd::RuleDisable => {
            let ids = a.positional.clone();
            let allow = permissions(a)?;
            let on = cmd == Cmd::RuleEnable;
            write(cmd, a, &files, &ctx, notify, |doc| {
                edit::set_enabled(doc, &ids, on, &allow)
            })
        }
        Cmd::RuleMove => {
            let id = a.positional[0].clone();
            let pos = position(a)?;
            write(cmd, a, &files, &ctx, notify, |doc| {
                edit::move_rule(doc, &id, &pos)
            })
        }
        Cmd::RuleRevoke => {
            let id = a.positional[0].clone();
            let p = Permission::parse(&a.positional[1]).ok_or_else(|| {
                CliError::usage(format!(
                    "unknown permission `{}` (input-injection, launch-programs or clipboard)",
                    a.positional[1]
                ))
            })?;
            write(cmd, a, &files, &ctx, notify, |doc| {
                edit::revoke(doc, &id, p)
            })
        }
        Cmd::RulesHistory => history(&files, &ctx),
        Cmd::RulesBackup => backup(a, &files, &ctx),
        Cmd::RulesRestore => restore(a, &files, &ctx, notify),
        Cmd::RulesReload => {
            let v = notify(a.dry_run)?;
            Ok((v, Vec::new()))
        }
        Cmd::RulesDraft => draft_show(&files, &ctx),
        Cmd::RulesDraftApply => draft_apply(a, &files, &ctx, notify),
        Cmd::RulesDraftDiscard => draft_discard(a, &files),
        _ => unreachable!("not a rule command"),
    }
}

fn s(v: &Value) -> String {
    match v {
        Value::String(s) => s.clone(),
        Value::Null => "-".into(),
        other => other.to_string(),
    }
}

fn app_line(app: &Value) -> String {
    if app.is_null() {
        return String::new();
    }
    if app["running"] == false {
        return "the app is not running; it loads the rules when it starts\n".into();
    }
    match app["outcome"].as_str() {
        Some("kept_previous") => "the running app kept its previous rules (see warnings)\n".into(),
        Some(_) if app["in_sync"] == true => format!(
            "the running app now uses revision {}\n",
            s(&app["active_revision"])
        ),
        _ => "the running app did not confirm the reload\n".into(),
    }
}

fn problems_text(v: &Value) -> String {
    let mut o = String::new();
    for p in v.as_array().into_iter().flatten() {
        o.push_str(&format!(
            "  {} {}{}: {}\n",
            s(&p["severity"]),
            p["rule"]
                .as_str()
                .map(|r| format!("{r} "))
                .unwrap_or_default(),
            s(&p["code"]),
            s(&p["message"])
        ));
        let offers: Vec<String> = p["offers"]
            .as_array()
            .into_iter()
            .flatten()
            .map(|o| match o["kind"].as_str() {
                Some("rebind") => format!("use {}", s(&o["keys"])),
                Some("allow") => format!("--allow {}", s(&o["permission"])),
                Some(k) => k.to_string(),
                None => String::new(),
            })
            .collect();
        if !offers.is_empty() {
            o.push_str(&format!(
                "      offers (never applied by themselves): {}\n",
                offers.join(", ")
            ));
        }
    }
    o
}

/// Human output for the rule commands (stdout).
pub fn human(cmd: Cmd, d: &Value) -> String {
    let mut o = String::new();
    match cmd {
        Cmd::RuleList => {
            o.push_str(&format!(
                "{} rules, revision {} (active profile {})\n",
                s(&d["source"]),
                s(&d["revision"]),
                s(&d["active_profile"])
            ));
            for r in d["rules"].as_array().into_iter().flatten() {
                o.push_str(&format!(
                    "{:<40} {:<22} {:<14} {}{}\n",
                    s(&r["id"]),
                    s(&r["trigger"]),
                    s(&r["state"]),
                    s(&r["title"]),
                    r["code"]
                        .as_str()
                        .map(|c| format!(" ({c})"))
                        .unwrap_or_default()
                ));
            }
            let sm = &d["summary"];
            o.push_str(&format!(
                "{} errors, {} warnings, {} infos (details: ddalgak rule check)\n",
                sm["errors"], sm["warnings"], sm["infos"]
            ));
        }
        Cmd::RuleShow => {
            o.push_str(&format!(
                "{} in profile {} (position {}), {}{}\n",
                s(&d["rule"]["id"]),
                s(&d["profile"]),
                s(&d["position"]),
                s(&d["state"]),
                d["code"]
                    .as_str()
                    .map(|c| format!(" ({c})"))
                    .unwrap_or_default()
            ));
            o.push_str(&serde_json::to_string_pretty(&d["rule"]).unwrap_or_default());
            o.push('\n');
            o.push_str(&problems_text(&d["problems"]));
        }
        Cmd::RuleCheck => {
            let sm = &d["summary"];
            o.push_str(&format!(
                "{} rules ({}), revision {}: {} errors, {} warnings, {} infos\n",
                s(&d["source"]),
                s(&d["file"]),
                s(&d["revision"]),
                sm["errors"],
                sm["warnings"],
                sm["infos"]
            ));
            o.push_str(&problems_text(&d["problems"]));
            o.push_str("Nothing was changed.\n");
        }
        Cmd::RulesHistory => {
            o.push_str(&format!(
                "rules.json: {} (revision {}); backup: {}\n",
                s(&d["file"]),
                s(&d["revision"]),
                if d["backup"]["exists"] == true {
                    format!("revision {}", s(&d["backup"]["revision"]))
                } else {
                    "none".into()
                }
            ));
            for sn in d["snapshots"].as_array().into_iter().flatten() {
                o.push_str(&format!(
                    "{}  revision {}  {}  {}\n",
                    s(&sn["name"]),
                    s(&sn["revision"]),
                    s(&sn["origin"]),
                    s(&sn["command"])
                ));
            }
        }
        Cmd::RulesBackup => {
            o.push_str(&match d["snapshot"].as_str() {
                Some(n) => format!("backed up revision {} as history/{n}\n", s(&d["revision"])),
                None if d["dry_run"] == true => "dry run: would back up the saved rules\n".into(),
                None => "nothing to back up\n".into(),
            });
        }
        Cmd::RulesReload => {
            o.push_str(&format!(
                "{}{}: saved revision {}, active revision {}{}\n",
                if d["dry_run"] == true {
                    "dry run, "
                } else {
                    ""
                },
                s(&d["outcome"]),
                s(&d["saved"]["revision"]),
                s(&d["active_set"]["revision"]),
                d["reason"]
                    .as_str()
                    .map(|r| format!(" ({r})"))
                    .unwrap_or_default()
            ));
        }
        Cmd::RulesDraft => {
            if d["exists"] == true {
                o.push_str(&format!(
                    "draft based on revision {} (saved: {}){}\n",
                    s(&d["base_revision"]),
                    s(&d["saved_revision"]),
                    if d["stale"] == true {
                        ", stale: the saved rules changed since"
                    } else {
                        ""
                    }
                ));
                for k in ["added", "removed", "changed"] {
                    let ids: Vec<String> = d["changes"][k]
                        .as_array()
                        .into_iter()
                        .flatten()
                        .map(s)
                        .collect();
                    if !ids.is_empty() {
                        o.push_str(&format!("  {k}: {}\n", ids.join(", ")));
                    }
                }
            } else {
                o.push_str("no draft\n");
            }
        }
        Cmd::RulesDraftDiscard => {
            o.push_str(if d["discarded"] == true {
                "the draft was discarded\n"
            } else if d["dry_run"] == true {
                "dry run: would discard the draft\n"
            } else {
                "no draft to discard\n"
            });
        }
        _ => {
            // Writes: rule add/edit/delete/duplicate/enable/disable/move/revoke, restore, draft apply.
            let prefix = if d["dry_run"] == true {
                "dry run, nothing written: "
            } else {
                ""
            };
            let target = if d["target"] == "draft" {
                " (draft)"
            } else {
                ""
            };
            let what = match cmd {
                Cmd::RuleAdd => format!("added {}", s(&d["created"])),
                Cmd::RuleDuplicate => format!("copied as {}", s(&d["created"])),
                Cmd::RulesRestore => format!(
                    "restored {} (revision {})",
                    s(&d["source"]),
                    s(&d["restored_revision"])
                ),
                Cmd::RulesDraftApply => "saved the draft".into(),
                _ => {
                    let ids: Vec<String> =
                        d["rules"].as_array().into_iter().flatten().map(s).collect();
                    format!(
                        "{} {}",
                        cmd.name().trim_start_matches("rule."),
                        ids.join(", ")
                    )
                }
            };
            if d["changed"] == false {
                o.push_str(&format!("{prefix}nothing to change{target}\n"));
            } else {
                let rev = if d["revision"]["to"].is_null() {
                    format!("revision {}", s(&d["revision"]["from"]))
                } else {
                    format!(
                        "revision {} → {}",
                        s(&d["revision"]["from"]),
                        s(&d["revision"]["to"])
                    )
                };
                o.push_str(&format!("{prefix}{what}{target}, {rev}\n"));
            }
            for n in d["notes"].as_array().into_iter().flatten() {
                o.push_str(&format!("note: {}\n", s(n)));
            }
            o.push_str(&problems_text(&d["problems"]));
            o.push_str(&app_line(&d["app"]));
        }
    }
    o
}
