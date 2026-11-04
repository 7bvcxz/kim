//! Rule validation (docs/DDALGAK_RULE_MODEL.md §8, `DDALGAK-FR-006`, `DDALGAK-SEC-004`,
//! `DDALGAK-SEC-006`, `DDALGAK-SEC-009`).
//!
//! [`check`] produces one [`Report`] for a rules document, in three layers:
//!
//! 1. **Schema** ([`crate::schema`]): the document outside its rules (a violation there means the
//!    file is not loaded) and every rule on its own (a violation makes only that rule inactive).
//! 2. **The reference cross-references** ([`semantic_errors`]): the checks
//!    `xtask/src/ddalgak.rs` runs on the contract fixtures (unique IDs, mode and profile
//!    references, `output_is_trigger`, `duplicate_trigger`, `consent_stale`, `run_too_long`). The
//!    function is a line-by-line port and a test compares it with the xtask original on every
//!    fixture and on generated documents, so `schema-check` and the runtime cannot disagree.
//! 3. **Runtime extensions**: what only Ddalgak itself knows or what needs the whole profile —
//!    unknown fields and step types of later versions (`rule_unsupported`, `step_unsupported`),
//!    conditions (`condition_unsupported`), collisions with Ddalgak's own shortcuts
//!    (`duplicate_binding`), rules that can never fire because another rule takes their trigger in
//!    every mode they work in (`trigger_shadowed`) or in some modes (`trigger_overridden`),
//!    sequences hidden by a shorter trigger (`sequence_prefix`), output cycles between rules
//!    (`output_cycle`, a swap by construction, §7.4), the contract's chord warnings
//!    (`altgr_collision`, `ime_toggle`, `os_shortcut`) and, on request, missing program files and
//!    folders (`target_missing`).
//!
//! Every error and warning carries **offers** in the hotkey contract's style (HOTKEY_CONTRACT
//! §9.5): up to two free `rebind` suggestions, `disable`, `allow <permission>` for consent, and
//! `keep` for warnings. An offer is never applied by itself.

use crate::model::{Action, Document, Permission, Rule, RuleEntry, Trigger as RuleTrigger};
use crate::schema;
use crate::trigger::{TChord, TKey, Trigger};
use kim_hotkeys::keys::{Key, Side};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::collections::{BTreeMap, BTreeSet};

/// The longest total delay of one rule run (§5.12).
const MAX_RUN_DELAY_MS: u64 = 60_000;

/// How bad a problem is (§8).
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum Severity {
    Error,
    Warning,
    Info,
}

impl Severity {
    pub fn as_str(self) -> &'static str {
        match self {
            Severity::Error => "error",
            Severity::Warning => "warning",
            Severity::Info => "info",
        }
    }
}

/// A non-destructive resolution the user may choose (never applied automatically).
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Offer {
    /// Use these keys instead (free in the profile, not reserved, no warning).
    Rebind(String),
    Disable,
    /// Grant a permission with `--allow`.
    Allow(&'static str),
    /// Accept a warning.
    Keep,
}

impl Offer {
    pub fn to_json(&self) -> Value {
        match self {
            Offer::Rebind(k) => json!({ "kind": "rebind", "keys": k }),
            Offer::Disable => json!({ "kind": "disable" }),
            Offer::Allow(p) => json!({ "kind": "allow", "permission": p }),
            Offer::Keep => json!({ "kind": "keep" }),
        }
    }

    /// The CLI words that apply the offer to `rule` (hints only).
    pub fn hint(&self, rule: &str) -> String {
        match self {
            Offer::Rebind(k) => format!("ddalgak rule edit {rule} --trigger {k}"),
            Offer::Disable => format!("ddalgak rule disable {rule}"),
            Offer::Allow(p) => format!("ddalgak rule enable {rule} --allow {p}"),
            Offer::Keep => "keep it as it is".into(),
        }
    }
}

/// One finding.
#[derive(Clone, Debug, PartialEq)]
pub struct Problem {
    pub code: &'static str,
    pub severity: Severity,
    /// The profile it concerns (`None` for the document).
    pub profile: Option<String>,
    /// The rule it concerns (the subject), if any.
    pub rule: Option<String>,
    /// Where the rule is: (profile index, rule index).
    pub pos: Option<(usize, usize)>,
    /// JSON path in the document (`profiles[0].rules[3].trigger`).
    pub at: String,
    pub message: String,
    /// The other side: `{"rule": …}`, `{"action": …, "keys": …}` or `null`.
    pub with: Option<Value>,
    pub offers: Vec<Offer>,
}

impl Problem {
    pub fn to_json(&self) -> Value {
        json!({
            "code": self.code,
            "severity": self.severity.as_str(),
            "profile": self.profile,
            "rule": self.rule,
            "at": self.at,
            "message": self.message,
            "with": self.with,
            "offers": self.offers.iter().map(Offer::to_json).collect::<Vec<_>>(),
        })
    }

    /// Whether this problem keeps its rule from running (§8: errors and the `*_unsupported`
    /// infos).
    pub fn inactivates(&self) -> bool {
        self.pos.is_some()
            && (self.severity == Severity::Error || INACTIVE_INFOS.contains(&self.code))
    }
}

/// Infos that keep a rule inactive in this version (§8, §9).
pub const INACTIVE_INFOS: [&str; 3] = [
    "rule_unsupported",
    "condition_unsupported",
    "step_unsupported",
];

/// The order in which a rule names its first problem (its `inactive` code).
const CODE_ORDER: [&str; 21] = [
    "duplicate_rule_id",
    "trigger_reserved",
    "trigger_invalid",
    "output_reserved",
    "step_invalid",
    "recursion_risk",
    "consent_missing",
    "rule_invalid",
    "duplicate_profile_id",
    "unknown_mode",
    "output_is_trigger",
    "duplicate_trigger",
    "duplicate_binding",
    "consent_stale",
    "run_too_long",
    "rule_unsupported",
    "condition_unsupported",
    "step_unsupported",
    "trigger_shadowed",
    "sequence_prefix",
    "trigger_overridden",
];

/// What the checks know beyond the document.
#[derive(Clone, Debug, Default)]
pub struct Context {
    /// Ddalgak's own bound global shortcuts from `hotkeys.json`: (action, canonical keys).
    pub shortcuts: Vec<(String, String)>,
    /// Look for missing program files and folders (`target_missing`; the CLI does, the engine
    /// does not).
    pub check_targets: bool,
}

/// All findings for one document.
#[derive(Clone, Debug, Default)]
pub struct Report {
    /// Problems that keep the whole file from loading (`rules_invalid`).
    pub file: Vec<Problem>,
    /// Problems of profiles and rules (the file loads).
    pub problems: Vec<Problem>,
    /// Rules that fail the schema: (profile index, rule index).
    pub schema_invalid: BTreeSet<(usize, usize)>,
}

impl Report {
    pub fn usable(&self) -> bool {
        self.file.is_empty()
    }

    /// The problem that keeps the rule at `pos` inactive, if any.
    pub fn inactive(&self, pos: (usize, usize)) -> Option<&Problem> {
        self.problems
            .iter()
            .filter(|p| p.pos == Some(pos) && p.inactivates())
            .min_by_key(|p| CODE_ORDER.iter().position(|c| *c == p.code))
    }

    pub fn count(&self, s: Severity) -> usize {
        self.file
            .iter()
            .chain(&self.problems)
            .filter(|p| p.severity == s)
            .count()
    }

    pub fn all(&self) -> impl Iterator<Item = &Problem> {
        self.file.iter().chain(&self.problems)
    }
}

fn list(v: &Value) -> impl Iterator<Item = &Value> {
    v.as_array().into_iter().flatten()
}

fn profiles(doc: &Value) -> impl Iterator<Item = &Value> {
    list(&doc["profiles"])
}

/// One reference finding: a stable code and where it was found (as `xtask/src/ddalgak.rs`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Finding {
    pub code: &'static str,
    pub at: String,
}

/// The reference cross-reference errors of a rules document, in document order: a port of
/// `semantic_errors` in `xtask/src/ddalgak.rs` (a test keeps them equal). `skip(pi, ri)` leaves a
/// rule out of the duplicate-trigger accounting (rules that fail the schema never claim a
/// trigger); the reference is `|_, _| false`.
pub fn semantic_errors(doc: &Value, skip: &dyn Fn(usize, usize) -> bool) -> Vec<Finding> {
    let mut out = Vec::new();
    let mut push = |code, at: String| out.push(Finding { code, at });

    let mut profile_ids = BTreeSet::new();
    for (pi, p) in profiles(doc).enumerate() {
        if let Some(id) = p["id"].as_str()
            && !profile_ids.insert(id)
        {
            push("duplicate_profile_id", format!("profiles[{pi}]"));
        }
    }
    if let Some(active) = doc["active_profile"].as_str()
        && !profile_ids.contains(active)
    {
        push("unknown_profile", "active_profile".into());
    }

    let mut rule_ids = BTreeSet::new();
    for (pi, p) in profiles(doc).enumerate() {
        let at_p = format!("profiles[{pi}]");
        let mut modes = BTreeSet::new();
        for (mi, m) in list(&p["modes"]).enumerate() {
            if let Some(id) = m["id"].as_str()
                && !modes.insert(id)
            {
                push("duplicate_mode_id", format!("{at_p}.modes[{mi}]"));
            }
        }
        let known = |id: &Value| id.as_str().is_some_and(|id| modes.contains(id));
        if !known(&p["default_mode"]) {
            push("unknown_mode", format!("{at_p}.default_mode"));
        }

        let mut triggers: BTreeMap<String, usize> = BTreeMap::new();
        for (ri, r) in list(&p["rules"]).enumerate() {
            let at = format!("{at_p}.rules[{ri}]");
            if let Some(id) = r["id"].as_str()
                && !rule_ids.insert(id.to_string())
            {
                push("duplicate_rule_id", format!("{at}.id"));
            }
            for (i, m) in list(&r["modes"]).enumerate() {
                if !known(m) {
                    push("unknown_mode", format!("{at}.modes[{i}]"));
                }
            }
            let keys = r["trigger"]["keys"].as_str().unwrap_or_default();
            let mut delay_ms = 0;
            for (si, s) in list(&r["steps"]).enumerate() {
                let at_s = format!("{at}.steps[{si}]");
                if s["type"] == "mode" {
                    if !s["mode"].is_null() && !known(&s["mode"]) {
                        push("unknown_mode", format!("{at_s}.mode"));
                    }
                    for (i, m) in list(&s["modes"]).enumerate() {
                        if !known(m) {
                            push("unknown_mode", format!("{at_s}.modes[{i}]"));
                        }
                    }
                }
                let output = match s["type"].as_str() {
                    Some("remap") => s["to"].as_str(),
                    Some("send_keys") => s["keys"].as_str(),
                    _ => None,
                };
                if output == Some(keys) {
                    push("output_is_trigger", at_s.clone());
                }
                if s["type"] == "delay" {
                    delay_ms += s["ms"].as_u64().unwrap_or(0);
                }
            }
            if delay_ms > MAX_RUN_DELAY_MS {
                push("run_too_long", format!("{at}.steps"));
            }
            if let Some(granted) = r["consent"]["launch-programs"]["fingerprint"].as_str()
                && granted != launch_fingerprint(r)
            {
                push("consent_stale", format!("{at}.consent.launch-programs"));
            }
            if r["enabled"] == true && !skip(pi, ri) {
                let key = trigger_key(r);
                if triggers.insert(key, ri).is_some() {
                    push("duplicate_trigger", format!("{at}.trigger"));
                }
            }
        }
    }
    out
}

/// What makes two enabled rules of one profile claim the same input (the reference's key): the
/// trigger (keys, `on`, `hold_ms`), the modes (unordered; none = every mode), the conditions and
/// the priority.
fn trigger_key(rule: &Value) -> String {
    let t = &rule["trigger"];
    let modes: BTreeSet<&str> = list(&rule["modes"]).filter_map(Value::as_str).collect();
    let mut when = String::new();
    canonical_json(&rule["when"], &mut when);
    format!(
        "{}|{}|{}|{:?}|{}|{}",
        t["keys"].as_str().unwrap_or_default(),
        t["on"].as_str().unwrap_or("press"),
        t["hold_ms"].as_u64().unwrap_or(500),
        modes,
        when,
        rule["priority"].as_i64().unwrap_or(0)
    )
}

/// `sha256:` + hex SHA-256 of the canonical JSON array of the rule's `open_program` and
/// `open_url` steps, in order (§6.2).
pub fn launch_fingerprint(rule: &Value) -> String {
    let targets: Vec<Value> = list(&rule["steps"])
        .filter(|s| s["type"] == "open_program" || s["type"] == "open_url")
        .cloned()
        .collect();
    let mut text = String::new();
    canonical_json(&Value::Array(targets), &mut text);
    let digest = Sha256::digest(text.as_bytes());
    let hex: String = digest.iter().map(|b| format!("{b:02x}")).collect();
    format!("sha256:{hex}")
}

/// RFC 8785 (JSON Canonicalization Scheme) for the values rules contain: object keys sorted by
/// UTF-16 code units, no whitespace, strings escaped as serde_json does, integers as written.
pub fn canonical_json(v: &Value, out: &mut String) {
    match v {
        Value::Object(map) => {
            let mut entries: Vec<(&String, &Value)> = map.iter().collect();
            entries.sort_by(|a, b| a.0.encode_utf16().cmp(b.0.encode_utf16()));
            out.push('{');
            for (i, (k, val)) in entries.into_iter().enumerate() {
                if i > 0 {
                    out.push(',');
                }
                out.push_str(&Value::String(k.clone()).to_string());
                out.push(':');
                canonical_json(val, out);
            }
            out.push('}');
        }
        Value::Array(items) => {
            out.push('[');
            for (i, item) in items.iter().enumerate() {
                if i > 0 {
                    out.push(',');
                }
                canonical_json(item, out);
            }
            out.push(']');
        }
        other => out.push_str(&other.to_string()),
    }
}

/// Parses `profiles[1].rules[3]…` into (1, 3).
fn position(at: &str) -> Option<(usize, usize)> {
    let rest = at.strip_prefix("profiles[")?;
    let (p, rest) = rest.split_once(']')?;
    let rest = rest.strip_prefix(".rules[")?;
    let (r, _) = rest.split_once(']')?;
    Some((p.parse().ok()?, r.parse().ok()?))
}

fn profile_index(at: &str) -> Option<usize> {
    at.strip_prefix("profiles[")?
        .split_once(']')?
        .0
        .parse()
        .ok()
}

/// The Step that implements each step type this version does not run yet.
pub const LATER_STEPS: &[(&str, &str)] = &[
    ("remap", "Step 68"),
    ("block", "Step 68"),
    ("send_keys", "Step 68"),
    ("delay", "Step 68"),
    ("type_text", "Step 69"),
    ("open_program", "Step 70"),
    ("open_url", "Step 70"),
    ("mouse_move", "Step 71"),
    ("mouse_scroll", "Step 71"),
    ("mouse_click", "Step 71"),
    ("mode", "Step 72"),
];

fn message(code: &str) -> &'static str {
    match code {
        "duplicate_profile_id" => "another profile has the same ID; this one is not used",
        "unknown_profile" => "`active_profile` names no profile of the file",
        "duplicate_mode_id" => "the mode ID appears twice in the profile",
        "unknown_mode" => "the mode does not exist in this profile",
        "duplicate_rule_id" => "an earlier rule has the same ID; this one is inactive",
        "output_is_trigger" => {
            "the step sends the rule's own trigger (a recursive remap); the rule is inactive"
        }
        "run_too_long" => "the delays of one run add up to more than 60 seconds",
        "consent_stale" => {
            "the launch targets changed after consent was given; approve them again with --allow launch-programs"
        }
        "duplicate_trigger" => {
            "an earlier enabled rule has the same trigger, modes, conditions and priority; this one is inactive"
        }
        _ => "",
    }
}

/// A parsed, schema-valid rule with where it is.
struct Item<'a> {
    pi: usize,
    ri: usize,
    rule: &'a Rule,
    trigger: Option<Trigger>,
}

/// Validates a rules document (already parsed as JSON and known to be `ddalgak.rules` v1).
pub fn check(doc: &Value, ctx: &Context) -> Report {
    let mut r = Report::default();
    let file_problem = |message: String, at: &str| Problem {
        code: "rules_invalid",
        severity: Severity::Error,
        profile: None,
        rule: None,
        pos: None,
        at: at.to_string(),
        message,
        with: None,
        offers: Vec::new(),
    };
    for e in schema::file_errors(doc) {
        let at = e.split(':').next().unwrap_or_default().to_string();
        r.file.push(file_problem(e, &at));
    }
    // Rules that cannot be named cannot be listed as inactive: the file is not loaded.
    for (pi, p) in profiles(doc).enumerate() {
        for (ri, rule) in list(&p["rules"]).enumerate() {
            let at = format!("profiles[{pi}].rules[{ri}]");
            let id_ok = rule["id"].as_str().is_some_and(crate::model::is_rule_id);
            if !rule.is_object() || !id_ok || !rule["enabled"].is_boolean() {
                r.file.push(file_problem(
                    format!("{at}: a rule needs a valid `id` and a boolean `enabled`"),
                    &at,
                ));
            }
        }
    }
    if !r.file.is_empty() {
        return r;
    }
    let profile_id = |pi: usize| doc["profiles"][pi]["id"].as_str().map(str::to_string);
    let rule_id = |pi: usize, ri: usize| {
        doc["profiles"][pi]["rules"][ri]["id"]
            .as_str()
            .map(str::to_string)
    };
    // 1. Each rule against the schema.
    for (pi, p) in profiles(doc).enumerate() {
        for (ri, rule) in list(&p["rules"]).enumerate() {
            let at = format!("profiles[{pi}].rules[{ri}]");
            let found = schema::rule_problems(rule, &at);
            if !found.is_empty() {
                r.schema_invalid.insert((pi, ri));
            }
            for (code, msg) in found {
                let mut offers = Vec::new();
                if code == "consent_missing"
                    && let Ok(t) = serde_json::from_value::<Rule>(rule.clone())
                {
                    offers.extend(
                        t.permissions_needed()
                            .into_iter()
                            .filter(|p| t.grant(*p).is_none())
                            .map(|p| Offer::Allow(p.as_str())),
                    );
                }
                offers.push(Offer::Disable);
                r.problems.push(Problem {
                    code,
                    severity: Severity::Error,
                    profile: profile_id(pi),
                    rule: rule_id(pi, ri),
                    pos: Some((pi, ri)),
                    at: at.clone(),
                    message: msg,
                    with: None,
                    offers,
                });
            }
        }
    }
    // 2. The reference cross-references.
    let invalid = r.schema_invalid.clone();
    let mut duplicate_trigger = BTreeSet::new();
    for f in semantic_errors(doc, &|pi, ri| invalid.contains(&(pi, ri))) {
        let pos = position(&f.at);
        let pi = pos.map(|p| p.0).or_else(|| profile_index(&f.at));
        let active_profile = doc["active_profile"].as_str();
        let is_active = |pi: Option<usize>| {
            pi.and_then(profile_id).as_deref() == active_profile && active_profile.is_some()
        };
        // The engine needs an active profile with a default mode: otherwise nothing loads.
        if f.code == "unknown_profile"
            || (f.code == "unknown_mode" && f.at.ends_with(".default_mode") && is_active(pi))
        {
            r.file.push(file_problem(
                format!("{}: {}", f.at, message(f.code)),
                &f.at,
            ));
            continue;
        }
        if f.code == "duplicate_trigger"
            && let Some(p) = pos
        {
            duplicate_trigger.insert(p);
        }
        let mut offers = Vec::new();
        if f.code == "consent_stale" {
            offers.push(Offer::Allow(Permission::LaunchPrograms.as_str()));
        }
        if pos.is_some() {
            offers.push(Offer::Disable);
        }
        let with = (f.code == "duplicate_trigger")
            .then(|| pos.and_then(|(pi, ri)| earlier_same_trigger(doc, pi, ri, &invalid)))
            .flatten()
            .map(|id| json!({ "rule": id }));
        r.problems.push(Problem {
            code: f.code,
            severity: Severity::Error,
            profile: pi.and_then(profile_id),
            rule: pos.and_then(|(pi, ri)| rule_id(pi, ri)),
            pos,
            at: f.at.clone(),
            message: format!("{}: {}", f.at, message(f.code)),
            with,
            offers,
        });
    }
    if !r.file.is_empty() {
        return r;
    }
    // 3. Runtime extensions, on the rules that fit the schema and the types.
    let typed: Document = match serde_json::from_value(doc.clone()) {
        Ok(d) => d,
        Err(e) => {
            r.file.push(file_problem(
                format!("the document does not fit the rules types: {e}"),
                "",
            ));
            return r;
        }
    };
    extend(&typed, ctx, &invalid, &duplicate_trigger, &mut r);
    // Rebind offers come first, as in the hotkey contract.
    for p in &mut r.problems {
        p.offers.sort_by_key(|o| match o {
            Offer::Rebind(_) => 0,
            Offer::Allow(_) => 1,
            Offer::Disable => 2,
            Offer::Keep => 3,
        });
    }
    r
}

/// The ID of the earlier enabled rule a `duplicate_trigger` collides with.
fn earlier_same_trigger(
    doc: &Value,
    pi: usize,
    ri: usize,
    invalid: &BTreeSet<(usize, usize)>,
) -> Option<String> {
    let rules = doc["profiles"][pi]["rules"].as_array()?;
    let key = trigger_key(&rules[ri]);
    rules[..ri]
        .iter()
        .enumerate()
        .find(|(i, r)| {
            r["enabled"] == true && !invalid.contains(&(pi, *i)) && trigger_key(r) == key
        })
        .and_then(|(_, r)| r["id"].as_str().map(str::to_string))
}

fn canonical(v: Option<&Value>) -> String {
    let mut s = String::new();
    canonical_json(v.unwrap_or(&Value::Null), &mut s);
    s
}

#[allow(clippy::too_many_lines)]
fn extend(
    typed: &Document,
    ctx: &Context,
    invalid: &BTreeSet<(usize, usize)>,
    duplicate_trigger: &BTreeSet<(usize, usize)>,
    r: &mut Report,
) {
    for (pi, profile) in typed.profiles.iter().enumerate() {
        let items: Vec<Item> = profile
            .rules
            .iter()
            .enumerate()
            .filter(|(ri, _)| !invalid.contains(&(pi, *ri)))
            .filter_map(|(ri, e)| match e {
                RuleEntry::Typed(rule) => Some(Item {
                    pi,
                    ri,
                    rule,
                    trigger: Trigger::parse(&rule.trigger.keys).ok(),
                }),
                RuleEntry::Raw(_) => None,
            })
            .collect();
        let all_modes: BTreeSet<&str> = profile.modes.iter().map(|m| m.id.as_str()).collect();
        let used_keys: BTreeSet<&str> = items
            .iter()
            .filter(|i| i.rule.enabled)
            .map(|i| i.rule.trigger.keys.as_str())
            .collect();
        let suggestions = |item: &Item| rebinds(item, &used_keys, ctx);
        // The reference's duplicate triggers get the same rebind offers as the runtime's findings.
        for p in r
            .problems
            .iter_mut()
            .filter(|p| p.code == "duplicate_trigger")
        {
            if let Some(item) = items.iter().find(|i| p.pos == Some((i.pi, i.ri))) {
                let mut offers: Vec<Offer> =
                    suggestions(item).into_iter().map(Offer::Rebind).collect();
                offers.append(&mut p.offers);
                p.offers = offers;
            }
        }
        let mut push = |item: &Item,
                        code: &'static str,
                        severity: Severity,
                        at: String,
                        message: String,
                        with: Option<Value>,
                        offers: Vec<Offer>| {
            r.problems.push(Problem {
                code,
                severity,
                profile: Some(profile.id.clone()),
                rule: Some(item.rule.id.clone()),
                pos: Some((item.pi, item.ri)),
                at,
                message,
                with,
                offers,
            });
        };
        for item in &items {
            let rule = item.rule;
            let at = format!("profiles[{}].rules[{}]", item.pi, item.ri);
            // The crate's own trigger parser must agree with the schema (defense in depth).
            if let Err(e) = Trigger::parse(&rule.trigger.keys) {
                push(
                    item,
                    e.code(),
                    Severity::Error,
                    format!("{at}.trigger.keys"),
                    format!("{at}.trigger.keys: {e}"),
                    None,
                    vec![Offer::Disable],
                );
            }
            if !rule.enabled {
                continue;
            }
            // Never broader (§9): unknown fields, conditions and step types of later versions.
            let unknown = rule
                .extra
                .keys()
                .next()
                .cloned()
                .or_else(|| {
                    rule.trigger
                        .extra
                        .keys()
                        .next()
                        .map(|k| format!("trigger.{k}"))
                })
                .or_else(|| rule.when.as_ref().and_then(|w| w.has_unknown_fields()))
                .or_else(|| {
                    rule.steps
                        .iter()
                        .enumerate()
                        .find_map(|(i, s)| s.unknown_field().map(|f| format!("steps[{i}].{f}")))
                });
            if let Some(f) = unknown {
                push(
                    item,
                    "rule_unsupported",
                    Severity::Info,
                    format!("{at}.{f}"),
                    format!(
                        "unknown field `{f}` (a newer Ddalgak may restrict the rule with it); the rule stays inactive"
                    ),
                    None,
                    Vec::new(),
                );
            }
            if rule.when.is_some() {
                push(
                    item,
                    "condition_unsupported",
                    Severity::Info,
                    format!("{at}.when"),
                    "app and window conditions are evaluated from Step 77; until then the rule is inactive, never unconditional".into(),
                    None,
                    Vec::new(),
                );
            }
            if let Some((i, s)) = rule
                .steps
                .iter()
                .enumerate()
                .find(|(_, s)| s.kind() != "engine")
            {
                let why = match LATER_STEPS.iter().find(|(t, _)| *t == s.kind()) {
                    Some((t, step)) => {
                        format!("`{t}` steps are not implemented in this version yet ({step})")
                    }
                    None => format!(
                        "unknown step type `{}` (written by a newer Ddalgak?)",
                        s.kind()
                    ),
                };
                push(
                    item,
                    "step_unsupported",
                    Severity::Info,
                    format!("{at}.steps[{i}]"),
                    why,
                    None,
                    Vec::new(),
                );
            }
            // Ddalgak's own GUI shortcuts win over rules (§7.5).
            if let Some((action, keys)) =
                ctx.shortcuts.iter().find(|(_, k)| *k == rule.trigger.keys)
            {
                let mut offers: Vec<Offer> =
                    suggestions(item).into_iter().map(Offer::Rebind).collect();
                offers.push(Offer::Disable);
                push(
                    item,
                    "duplicate_binding",
                    Severity::Error,
                    format!("{at}.trigger"),
                    format!(
                        "`{keys}` is bound to Ddalgak's own shortcut `{action}` in hotkeys.json; the shortcut wins and the rule is inactive"
                    ),
                    Some(json!({ "action": action, "keys": keys })),
                    offers,
                );
            }
            if let Some(t) = &item.trigger
                && let Some((code, why)) = chord_warning(t)
            {
                let mut offers: Vec<Offer> =
                    suggestions(item).into_iter().map(Offer::Rebind).collect();
                offers.extend([Offer::Disable, Offer::Keep]);
                push(
                    item,
                    code,
                    Severity::Warning,
                    format!("{at}.trigger"),
                    why.into(),
                    None,
                    offers,
                );
            }
            if ctx.check_targets {
                for (i, s) in rule.steps.iter().enumerate() {
                    if let Action::OpenProgram(p) = s {
                        for (field, path) in [
                            ("path", p.path.as_deref()),
                            ("working_dir", p.working_dir.as_deref()),
                        ] {
                            if let Some(path) = path
                                && let Some(resolved) = resolve_known_folder(path)
                                && !resolved.exists()
                            {
                                push(
                                    item,
                                    "target_missing",
                                    Severity::Warning,
                                    format!("{at}.steps[{i}].{field}"),
                                    format!(
                                        "step {i}: the {} does not exist on this computer (yet); the run would end with program_not_found",
                                        if field == "path" {
                                            "program"
                                        } else {
                                            "working folder"
                                        }
                                    ),
                                    None,
                                    vec![Offer::Disable, Offer::Keep],
                                );
                            }
                        }
                    }
                }
            }
        }
        // Rules that take each other's trigger (§7.1): shadowed, overridden, hidden sequences.
        let enabled: Vec<&Item> = items
            .iter()
            .filter(|i| {
                i.rule.enabled && i.trigger.is_some() && !duplicate_trigger.contains(&(i.pi, i.ri))
            })
            .collect();
        // What the pairwise checks compare, computed once per rule.
        struct Pre<'a> {
            item: &'a Item<'a>,
            when: String,
            modes: BTreeSet<String>,
            first: String,
            rank: (bool, i64, std::cmp::Reverse<usize>),
        }
        let pre: Vec<Pre> = enabled
            .iter()
            .map(|i| Pre {
                item: i,
                when: canonical(
                    i.rule
                        .when
                        .as_ref()
                        .map(|w| serde_json::to_value(w).unwrap_or_default())
                        .as_ref(),
                ),
                modes: match &i.rule.modes {
                    Some(m) => m.iter().cloned().collect(),
                    None => all_modes.iter().map(|s| (*s).to_string()).collect(),
                },
                first: i
                    .rule
                    .trigger
                    .keys
                    .split(' ')
                    .next()
                    .unwrap_or_default()
                    .to_string(),
                rank: (
                    i.rule.modes.is_some(),
                    i.rule.priority(),
                    std::cmp::Reverse(i.ri),
                ),
            })
            .collect();
        // Only rules that share their first chord can take each other's trigger.
        let mut by_first: BTreeMap<&str, Vec<usize>> = BTreeMap::new();
        for (k, p) in pre.iter().enumerate() {
            by_first.entry(p.first.as_str()).or_default().push(k);
        }
        let mut reported: BTreeSet<(usize, &'static str)> = BTreeSet::new();
        for group in by_first.values() {
            for &x in group {
                for &y in group {
                    let (a, b) = (&pre[x], &pre[y]);
                    if x == y || a.when != b.when {
                        continue;
                    }
                    let overlap: BTreeSet<&String> = a.modes.intersection(&b.modes).collect();
                    if overlap.is_empty() {
                        continue;
                    }
                    let shared: Vec<&str> = overlap.iter().map(|s| s.as_str()).collect();
                    let (ra, rb) = (a.item.rule, b.item.rule);
                    // `b` loses against `a` on the same keys.
                    if ra.trigger.keys == rb.trigger.keys && a.rank > b.rank {
                        let all = b.modes.iter().all(|m| overlap.contains(m));
                        let code = if all {
                            "trigger_shadowed"
                        } else {
                            "trigger_overridden"
                        };
                        if reported.insert((b.item.ri, code)) {
                            let at =
                                format!("profiles[{}].rules[{}].trigger", b.item.pi, b.item.ri);
                            if all {
                                let mut offers: Vec<Offer> =
                                    suggestions(b.item).into_iter().map(Offer::Rebind).collect();
                                offers.extend([Offer::Disable, Offer::Keep]);
                                push(
                                    b.item,
                                    code,
                                    Severity::Warning,
                                    at,
                                    format!(
                                        "rule `{}` takes `{}` first in every mode this rule works in ({}), so this rule never fires",
                                        ra.id,
                                        rb.trigger.keys,
                                        shared.join(", ")
                                    ),
                                    Some(json!({ "rule": ra.id })),
                                    offers,
                                );
                            } else {
                                push(
                                    b.item,
                                    code,
                                    Severity::Info,
                                    at,
                                    format!(
                                        "in mode {} rule `{}` takes `{}` first (intended layering)",
                                        shared.join(", "),
                                        ra.id,
                                        rb.trigger.keys
                                    ),
                                    Some(json!({ "rule": ra.id })),
                                    Vec::new(),
                                );
                            }
                        }
                    }
                    // `b` is a sequence that starts with all of `a`'s chords.
                    let (ta, tb) = (
                        a.item.trigger.as_ref().expect("filtered"),
                        b.item.trigger.as_ref().expect("filtered"),
                    );
                    if tb.steps().len() > ta.steps().len()
                        && tb.steps()[..ta.steps().len()] == *ta.steps()
                        && reported.insert((b.item.ri, "sequence_prefix"))
                    {
                        push(
                            b.item,
                            "sequence_prefix",
                            Severity::Warning,
                            format!("profiles[{}].rules[{}].trigger", b.item.pi, b.item.ri),
                            format!(
                                "rule `{}` fires on `{}`, the start of this sequence, so this sequence never completes",
                                ra.id, ra.trigger.keys
                            ),
                            Some(json!({ "rule": ra.id })),
                            vec![Offer::Disable, Offer::Keep],
                        );
                    }
                }
            }
        }
        // Output cycles between rules: a swap by construction (own input is never matched).
        let mut by_keys: BTreeMap<&str, Vec<usize>> = BTreeMap::new();
        for (k, p) in pre.iter().enumerate() {
            by_keys
                .entry(p.item.rule.trigger.keys.as_str())
                .or_default()
                .push(k);
        }
        let edges: Vec<Vec<usize>> = pre
            .iter()
            .enumerate()
            .map(|(k, p)| {
                let mut out: Vec<usize> = p
                    .item
                    .rule
                    .steps
                    .iter()
                    .flat_map(Action::outputs)
                    .flat_map(|o| o.split(' '))
                    .filter_map(|o| by_keys.get(o))
                    .flatten()
                    .copied()
                    .filter(|t| *t != k)
                    .collect();
                out.sort_unstable();
                out.dedup();
                out
            })
            .collect();
        let mut in_cycle: BTreeSet<usize> = BTreeSet::new();
        for start in 0..pre.len() {
            if edges[start].is_empty() || in_cycle.contains(&start) {
                continue;
            }
            // Depth-first search for a path back to `start`.
            let mut stack = vec![(start, vec![start])];
            let mut seen = BTreeSet::new();
            while let Some((at, path)) = stack.pop() {
                for &next in &edges[at] {
                    if next == start && path.len() > 1 {
                        if path.iter().all(|p| !in_cycle.contains(p)) {
                            let names: Vec<String> = path
                                .iter()
                                .chain(std::iter::once(&start))
                                .map(|k| pre[*k].item.rule.id.clone())
                                .collect();
                            let subject = pre[*path
                                .iter()
                                .max_by_key(|k| pre[**k].item.ri)
                                .expect("non-empty")]
                            .item;
                            in_cycle.extend(path.iter().copied());
                            push(
                                subject,
                                "output_cycle",
                                Severity::Info,
                                format!("profiles[{}].rules[{}].steps", subject.pi, subject.ri),
                                format!(
                                    "the outputs of {} lead back to the first rule; Ddalgak never matches its own injected input, so this is a swap, not a loop",
                                    names.join(" → ")
                                ),
                                Some(json!({ "rules": names })),
                                Vec::new(),
                            );
                        }
                    } else if path.len() < 32 && seen.insert(next) {
                        let mut p = path.clone();
                        p.push(next);
                        stack.push((next, p));
                    }
                }
            }
        }
    }
}

/// Up to two free alternatives for a single-chord trigger: the chord with `Shift`, `Ctrl+Shift`
/// or `Alt+Shift` added (HOTKEY_CONTRACT §9.5), valid, not reserved, without a warning, not used
/// by an enabled rule of the profile and not one of Ddalgak's own shortcuts.
fn rebinds(item: &Item, used: &BTreeSet<&str>, ctx: &Context) -> Vec<String> {
    let Some(chord) = item.trigger.as_ref().and_then(Trigger::single).copied() else {
        return Vec::new();
    };
    if matches!(chord.key, TKey::Modifier(_)) {
        return Vec::new();
    }
    let mut out: Vec<String> = Vec::new();
    for (ctrl, alt) in [(false, false), (true, false), (false, true)] {
        if chord.mods.altgr && (ctrl || alt) {
            continue;
        }
        let mut c: TChord = chord;
        let add = |side: &mut Side, on: bool| {
            if on && !side.on() {
                *side = Side::Any;
            }
        };
        add(&mut c.mods.ctrl, ctrl);
        add(&mut c.mods.alt, alt);
        add(&mut c.mods.shift, true);
        let keys = c.to_string();
        if keys == item.rule.trigger.keys || out.contains(&keys) {
            continue;
        }
        let ok = Trigger::parse(&keys).is_ok_and(|t| chord_warning(&t).is_none())
            && schema::trigger_keys_ok(&keys)
            && !used.contains(keys.as_str())
            && !ctx.shortcuts.iter().any(|(_, k)| *k == keys);
        if ok {
            out.push(keys);
            if out.len() == 2 {
                break;
            }
        }
    }
    out
}

/// The hotkey contract's single-binding warnings (§3.1, HOTKEY_CONTRACT §9.3).
pub fn chord_warning(t: &Trigger) -> Option<(&'static str, &'static str)> {
    for (i, c) in t.steps().iter().enumerate() {
        let key = match c.key {
            TKey::Named(n) | TKey::Lock(n) => Key::Named(n),
            TKey::Code(n) => Key::Code(n),
            TKey::Modifier(_) => continue,
        };
        let m = &c.mods;
        if i == 0
            && t.steps().len() == 1
            && ((m.exactly(false, false, false) && key.is_named("PrintScreen"))
                || (m.exactly(false, true, false)
                    && (key.is_named("PrintScreen") || key.is_named("Space"))))
        {
            return Some((
                "os_shortcut",
                "a well-known Windows default (screenshot or the window menu): the rule replaces it while the engine runs",
            ));
        }
        if m.ctrl.on() && m.alt.on() && !m.altgr && !m.win.on() && key.is_printable() {
            return Some((
                "altgr_collision",
                "Ctrl+Alt with a character key may block an AltGr character on some keyboard layouts",
            ));
        }
        if (m.exactly(true, false, false) && key.is_named("Space"))
            || (m.exactly(false, false, true) && key.is_named("Space"))
            || (m.exactly(false, true, false) && matches!(key, Key::Code("Backquote")))
        {
            return Some((
                "ime_toggle",
                "the chord may block switching the input method",
            ));
        }
    }
    None
}

/// Expands the known-folder tokens of §5.5 (`%ProgramFiles%\…`) from the environment; `None`
/// when a token is unknown here (nothing is guessed).
pub fn resolve_known_folder(path: &str) -> Option<std::path::PathBuf> {
    if !cfg!(windows) {
        return None;
    }
    let Some(rest) = path.strip_prefix('%') else {
        return Some(std::path::PathBuf::from(path));
    };
    let (token, tail) = rest.split_once('%')?;
    let var = match token.to_ascii_lowercase().as_str() {
        "programfiles" => "ProgramFiles",
        "programfiles(x86)" => "ProgramFiles(x86)",
        "localappdata" => "LOCALAPPDATA",
        "appdata" => "APPDATA",
        "userprofile" => "USERPROFILE",
        "systemroot" => "SystemRoot",
        _ => return None,
    };
    let base = std::env::var_os(var)?;
    let mut p = std::path::PathBuf::from(base);
    let tail = tail.trim_start_matches('\\');
    if !tail.is_empty() {
        p.push(tail);
    }
    Some(p)
}

/// Which permissions an enabled rule still lacks, and whether its launch consent is stale.
pub fn missing_consent(rule: &Rule) -> Vec<Permission> {
    let v = rule.to_value();
    rule.permissions_needed()
        .into_iter()
        .filter(|p| match rule.grant(*p) {
            None => true,
            Some(g) => {
                *p == Permission::LaunchPrograms
                    && g.fingerprint.as_deref() != Some(launch_fingerprint(&v).as_str())
            }
        })
        .collect()
}

/// A trigger's display for messages: `RuleTrigger` keys.
pub fn keys_of(t: &RuleTrigger) -> &str {
    &t.keys
}

/// The reference checker of `cargo xtask schema-check`, compiled here for the agreement tests.
#[cfg(test)]
#[path = "../../../xtask/src/ddalgak.rs"]
#[allow(dead_code, unused_qualifications, clippy::all)]
mod reference;

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::Path;

    /// `reference` (xtask) uses `crate::jsonschema`; give it the runtime's.
    fn contracts() -> std::path::PathBuf {
        Path::new(env!("CARGO_MANIFEST_DIR")).join("../../docs/contracts")
    }

    fn read(p: &Path) -> Value {
        serde_json::from_slice(&std::fs::read(p).unwrap()).unwrap()
    }

    fn fixtures() -> Vec<(String, Value)> {
        let base = contracts().join("examples/ddalgak-rules-v1");
        let mut out = Vec::new();
        for sub in ["", "semantic-invalid", "invalid"] {
            for e in std::fs::read_dir(base.join(sub)).unwrap().flatten() {
                let p = e.path();
                if p.extension().is_some_and(|x| x == "json") {
                    out.push((
                        format!("{sub}/{}", p.file_name().unwrap().to_string_lossy()),
                        read(&p),
                    ));
                }
            }
        }
        out
    }

    fn reference_codes(doc: &Value) -> Vec<(String, String)> {
        reference::semantic_errors(doc)
            .into_iter()
            .map(|f| (f.code.to_string(), f.at))
            .collect()
    }

    fn mine(doc: &Value) -> Vec<(String, String)> {
        semantic_errors(doc, &|_, _| false)
            .into_iter()
            .map(|f| (f.code.to_string(), f.at))
            .collect()
    }

    #[test]
    fn the_port_agrees_with_the_xtask_reference_on_every_fixture() {
        let all = fixtures();
        assert!(all.len() >= 60, "{}", all.len());
        for (name, doc) in &all {
            assert_eq!(mine(doc), reference_codes(doc), "{name}");
            if !name.starts_with("invalid/") {
                assert_eq!(
                    reference::launch_fingerprint(&doc["profiles"][0]["rules"][0]),
                    launch_fingerprint(&doc["profiles"][0]["rules"][0]),
                    "{name}"
                );
            }
        }
    }

    #[test]
    fn the_port_agrees_with_the_reference_on_generated_documents() {
        // Mutations of every fixture that touch what the cross-references look at.
        let mut n = 0;
        for (name, doc) in fixtures() {
            for variant in 0..12 {
                let mut d = doc.clone();
                let rules = d["profiles"][0]["rules"]
                    .as_array()
                    .cloned()
                    .unwrap_or_default();
                if rules.is_empty() {
                    continue;
                }
                let first = rules[0].clone();
                let list = d["profiles"][0]["rules"].as_array_mut().unwrap();
                match variant {
                    0 => list.push(first.clone()),
                    1 => {
                        let mut c = first.clone();
                        c["id"] = json!("zz-copy");
                        list.push(c);
                    }
                    2 => {
                        let mut c = first.clone();
                        c["id"] = json!("zz-copy");
                        c["priority"] = json!(3);
                        list.push(c);
                    }
                    3 => list[0]["modes"] = json!(["nowhere"]),
                    4 => list[0]["enabled"] = json!(!first["enabled"].as_bool().unwrap_or(true)),
                    5 => {
                        list[0]["steps"] =
                            json!([{ "type": "remap", "to": first["trigger"]["keys"] }]);
                    }
                    6 => {
                        list[0]["steps"] =
                            Value::Array(vec![json!({ "type": "delay", "ms": 10000 }); 7]);
                    }
                    7 => {
                        list[0]["consent"] = json!({ "launch-programs": { "granted_at": "2026-11-04T00:00:00Z", "via": "cli", "fingerprint": format!("sha256:{}", "0".repeat(64)) } });
                    }
                    8 => d["active_profile"] = json!("missing"),
                    9 => {
                        let p = d["profiles"][0].clone();
                        d["profiles"].as_array_mut().unwrap().push(p);
                    }
                    10 => d["profiles"][0]["default_mode"] = json!("nowhere"),
                    _ => {
                        let mut c = first.clone();
                        c["id"] = json!("zz-copy");
                        c["when"] = json!({ "apps": [{ "exe": "Code.exe" }] });
                        list.push(c);
                    }
                }
                n += 1;
                assert_eq!(mine(&d), reference_codes(&d), "{name} variant {variant}");
            }
        }
        assert!(n > 300, "{n}");
    }

    #[test]
    fn semantic_invalid_fixtures_get_exactly_their_code_from_the_full_check() {
        let dir = contracts().join("examples/ddalgak-rules-v1/semantic-invalid");
        for e in std::fs::read_dir(dir).unwrap().flatten() {
            let p = e.path();
            let name = p.file_stem().unwrap().to_string_lossy().to_string();
            let (code, _) = name.split_once("--").unwrap();
            let report = check(&read(&p), &Context::default());
            let errors: BTreeSet<&str> = report
                .all()
                .filter(|p| p.severity == Severity::Error)
                .map(|p| {
                    if p.code == "rules_invalid" {
                        "unknown_profile"
                    } else {
                        p.code
                    }
                })
                .collect();
            assert_eq!(
                errors,
                BTreeSet::from([code]),
                "{name}: {:?}",
                report.all().collect::<Vec<_>>()
            );
        }
    }

    #[test]
    fn valid_fixtures_have_no_errors() {
        let dir = contracts().join("examples/ddalgak-rules-v1");
        for e in std::fs::read_dir(dir).unwrap().flatten() {
            let p = e.path();
            if p.extension().is_none_or(|x| x != "json") {
                continue;
            }
            let report = check(&read(&p), &Context::default());
            let errors: Vec<&Problem> = report
                .all()
                .filter(|p| p.severity == Severity::Error)
                .collect();
            assert!(errors.is_empty(), "{}: {errors:#?}", p.display());
        }
    }

    #[allow(clippy::needless_pass_by_value)]
    fn doc(rules: Value) -> Value {
        json!({
            "schema": "ddalgak.rules", "schema_version": 1, "hotkey_contract": 1,
            "active_profile": "p",
            "profiles": [{"id": "p", "title": "P", "default_mode": "base",
                          "modes": [{"id": "base", "title": "Base"}, {"id": "nav", "title": "Nav"}],
                          "rules": rules}]
        })
    }

    fn engine(id: &str, keys: &str) -> Value {
        json!({"id": id, "title": id, "enabled": true, "trigger": {"keys": keys},
               "steps": [{"type": "engine", "op": "pause"}]})
    }

    fn codes(r: &Report) -> Vec<(&'static str, Option<String>)> {
        r.all().map(|p| (p.code, p.rule.clone())).collect()
    }

    #[test]
    fn duplicates_come_with_free_rebind_offers_and_name_the_other_rule() {
        let r = check(
            &doc(json!([
                engine("ra", "F13"),
                engine("rb", "F13"),
                engine("rc", "Shift+F13")
            ])),
            &Context::default(),
        );
        let p = r
            .problems
            .iter()
            .find(|p| p.code == "duplicate_trigger")
            .unwrap();
        assert_eq!(p.rule.as_deref(), Some("rb"));
        assert_eq!(p.with, Some(json!({"rule": "ra"})));
        // Shift+F13 is used by `c`, so the free suggestions are Ctrl+Shift and Alt+Shift.
        assert_eq!(
            p.offers,
            [
                Offer::Rebind("Ctrl+Shift+F13".into()),
                Offer::Rebind("Alt+Shift+F13".into()),
                Offer::Disable
            ]
        );
        assert_eq!(r.inactive((0, 1)).unwrap().code, "duplicate_trigger");
        assert!(r.inactive((0, 0)).is_none());
    }

    #[test]
    fn shadowed_overridden_sequences_and_cycles() {
        let mut low = engine("low", "F14");
        low["priority"] = json!(-1);
        let mut nav = engine("nav-only", "F15");
        nav["modes"] = json!(["nav"]);
        let r = check(
            &doc(json!([
                engine("high", "F14"),
                low,
                engine("global", "F15"),
                nav,
                engine("prefix", "F16"),
                engine("sequence", "F16 F17"),
                {"id": "rx", "title": "x", "enabled": true, "trigger": {"keys": "F18"},
                 "steps": [{"type": "remap", "to": "F19"}],
                 "consent": {"input-injection": {"granted_at": "2026-11-04T00:00:00Z", "via": "cli"}}},
                {"id": "ry", "title": "y", "enabled": true, "trigger": {"keys": "F19"},
                 "steps": [{"type": "remap", "to": "F18"}],
                 "consent": {"input-injection": {"granted_at": "2026-11-04T00:00:00Z", "via": "cli"}}}
            ])),
            &Context::default(),
        );
        let c = codes(&r);
        assert!(
            c.contains(&("trigger_shadowed", Some("low".into()))),
            "{c:?}"
        );
        // The mode-scoped rule wins in nav; the global one still works in base.
        assert!(
            c.contains(&("trigger_overridden", Some("global".into()))),
            "{c:?}"
        );
        assert!(
            c.contains(&("sequence_prefix", Some("sequence".into()))),
            "{c:?}"
        );
        assert!(c.contains(&("output_cycle", Some("ry".into()))), "{c:?}");
        // None of these is an error: every rule above is valid and only the remaps are
        // inactive (step_unsupported until Step 68).
        assert!(r.all().all(|p| p.severity != Severity::Error), "{c:?}");
        let low = r
            .problems
            .iter()
            .find(|p| p.code == "trigger_shadowed")
            .unwrap();
        assert!(low.offers.contains(&Offer::Keep));
        assert!(
            matches!(low.offers[0], Offer::Rebind(_)),
            "{:?}",
            low.offers
        );
    }

    #[test]
    fn own_shortcuts_unknown_fields_conditions_and_warnings() {
        let mut cond = engine("cond", "F20");
        cond["when"] = json!({"apps": [{"exe": "Code.exe"}]});
        let mut future = engine("future", "Shift+F20");
        future["steps"] = json!([{"type": "teleport"}]);
        let mut odd = engine("odd", "Alt+F20");
        odd["trigger"]["double_tap"] = json!(true);
        let ctx = Context {
            shortcuts: vec![("engine.toggle".into(), "Ctrl+Alt+Shift+F13".into())],
            check_targets: false,
        };
        let r = check(
            &doc(json!([
                engine("mine", "Ctrl+Alt+Shift+F13"),
                cond,
                future,
                odd,
                engine("altgr", "Ctrl+Alt+Q"),
                engine("ime", "Ctrl+Space")
            ])),
            &ctx,
        );
        let c = codes(&r);
        for want in [
            ("duplicate_binding", "mine"),
            ("condition_unsupported", "cond"),
            ("step_unsupported", "future"),
            ("rule_unsupported", "odd"),
            ("altgr_collision", "altgr"),
            ("ime_toggle", "ime"),
        ] {
            assert!(
                c.contains(&(want.0, Some(want.1.into()))),
                "{want:?} in {c:?}"
            );
        }
        assert_eq!(r.inactive((0, 0)).unwrap().code, "duplicate_binding");
        assert!(
            r.inactive((0, 4)).is_none(),
            "a warning keeps the rule active"
        );
    }

    #[test]
    fn schema_errors_stay_with_their_rule_and_never_block_a_valid_twin() {
        let mut bad = engine("bad", "F13");
        bad["title"] = json!("");
        let r = check(
            &doc(json!([bad, engine("good", "F13")])),
            &Context::default(),
        );
        assert!(r.usable());
        assert_eq!(r.inactive((0, 0)).unwrap().code, "rule_invalid");
        assert!(
            r.inactive((0, 1)).is_none(),
            "a broken rule claims no trigger"
        );
        // An unnamed rule makes the whole file unusable.
        let r = check(&doc(json!([{"title": "no id"}])), &Context::default());
        assert!(!r.usable());
    }

    #[test]
    fn consent_helpers() {
        let mut rule: Rule = serde_json::from_value(json!({"id": "ra", "title": "A", "enabled": true,
            "trigger": {"keys": "F13"}, "steps": [{"type": "open_url", "url": "https://example.com/"}]})).unwrap();
        assert_eq!(missing_consent(&rule), [Permission::LaunchPrograms]);
        let fp = launch_fingerprint(&rule.to_value());
        rule.set_grant(
            Permission::LaunchPrograms,
            Some(crate::model::Grant::cli_now(Some(fp))),
        );
        assert!(missing_consent(&rule).is_empty());
        if let Action::OpenUrl(u) = &mut rule.steps[0] {
            u.url = "https://example.org/".into();
        }
        assert_eq!(
            missing_consent(&rule),
            [Permission::LaunchPrograms],
            "stale"
        );
    }
}
