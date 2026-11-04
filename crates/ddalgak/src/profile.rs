//! Profiles (Step 75, docs/DDALGAK_RULE_MODEL.md §10, ADR 0052; `DDALGAK-FR-030`,
//! `DDALGAK-FR-031`, `DDALGAK-FR-032`, `DDALGAK-SEC-006`, `DDALGAK-SEC-013`, `DDALGAK-NFR-005`,
//! `DDALGAK-NFR-008`).
//!
//! A profile is a complete, switchable set of modes and rules ("Work", "Gaming",
//! "Development"). Like [`crate::edit`] and [`crate::mode_edit`], every operation here is a pure
//! change of an in-memory [`Document`]; callers (the CLI, the editor) run it inside one store
//! transaction (lock, re-read, review, atomic save, history snapshot) and then ask a running app
//! to reload.
//!
//! - **CRUD**: [`add`], [`edit`] (title and description), [`duplicate`] (modes and rules, every
//!   copied rule under a new ID), [`delete`] (never the active or the only profile:
//!   `profile_in_use`), with [`rows`] for search and sort.
//! - **Switch**: [`apply`] only changes `active_profile`. The running app turns that reload into
//!   an atomic switch ([`crate::core::Core::switch_table`]): runs are cancelled, everything held
//!   is released, keys whose down was consumed keep their up consumed, and the new profile starts
//!   in its default mode.
//! - **Versions**: every applied change is a history snapshot and `ddalgak profile save` adds a
//!   labelled save point ([`crate::store::save_point`]). [`versions`] lists the distinct versions
//!   of one profile across both, [`rollback_target`] picks the newest earlier version that worked
//!   (no error in that profile), and [`restore`] puts a version back as a new change.
//! - **Transfer**: [`export`] writes portable `ddalgak.rules` documents (an `exported` block, no
//!   consent, settings or revision, every rule off, program paths under known folders as tokens,
//!   environment values removed unless asked for). [`plan_import`] validates and migrates a file
//!   and plans what would be added — **never** activating a profile, granting consent or running
//!   anything — and [`apply_import`] adds it.
//!
//! [`side_by_side`] aligns two profiles for the editor's diff and the CLI's preview: rule titles,
//! triggers and step types only, never typed text, paths, arguments or URLs (`DDALGAK-SEC-011`).

use crate::edit::{self, Change, EditError};
use crate::model::{
    Action, Document, Mode, Permission, Profile, RuleEntry, is_rule_id, is_short_id,
};
use crate::store::{self, FileState, Files, Migration};
use crate::validate::{self, Context, Problem, Report, Severity};
use serde_json::{Map, Value, json};
use std::collections::BTreeSet;
use std::ffi::OsString;

/// Profiles a rules document holds at most (schema `profiles.maxItems`).
pub const MAX_PROFILES: usize = 64;
/// Modes a profile holds at most (schema `modes.maxItems`).
pub const MAX_MODES: usize = 32;
const TITLE_MAX: usize = 80;
const DESCRIPTION_MAX: usize = 500;

// --- IDs, titles -------------------------------------------------------------------------------

/// A profile ID from a title: ASCII letters and digits in lowercase, everything else as one `-`;
/// `profile` when nothing is left (a Korean title).
pub fn id_from_title(title: &str) -> String {
    let mut out = String::new();
    for c in title.chars() {
        if c.is_ascii_alphanumeric() {
            out.push(c.to_ascii_lowercase());
        } else if !out.is_empty() && !out.ends_with('-') {
            out.push('-');
        }
    }
    let mut id: String = out.trim_matches('-').chars().take(32).collect();
    while id.ends_with('-') {
        id.pop();
    }
    if id.is_empty() {
        return "profile".into();
    }
    if !id.starts_with(|c: char| c.is_ascii_lowercase()) {
        id = format!("p-{id}").chars().take(32).collect();
        while id.ends_with('-') {
            id.pop();
        }
    }
    if is_short_id(&id) {
        id
    } else {
        "profile".into()
    }
}

/// `base` when it is free, else `base-2`, `base-3`, … (at most 32 characters).
fn free_id(taken: &dyn Fn(&str) -> bool, base: &str, max: usize) -> String {
    if !taken(base) {
        return base.to_string();
    }
    for n in 2.. {
        let suffix = format!("-{n}");
        let stem: String = base.chars().take(max - suffix.len()).collect();
        let id = format!("{}{suffix}", stem.trim_end_matches('-'));
        if !taken(&id) {
            return id;
        }
    }
    unreachable!("an unbounded search finds a free ID")
}

/// The first free profile ID based on `base`.
pub fn free_profile_id(doc: &Document, base: &str) -> String {
    free_id(&|id| doc.profile(id).is_some(), base, 32)
}

fn free_rule_id(taken: &BTreeSet<String>, base: &str) -> String {
    let base = if is_rule_id(base) { base } else { "r-imported" };
    free_id(&|id| taken.contains(id), base, 48)
}

fn check_title(t: &str) -> Result<String, EditError> {
    let t = t.trim();
    let n = t.chars().count();
    if n == 0 || n > TITLE_MAX || t.chars().any(char::is_control) {
        return Err(EditError::Usage(format!(
            "a profile name has 1–{TITLE_MAX} characters and no control characters"
        )));
    }
    Ok(t.to_string())
}

fn check_description(d: Option<&str>) -> Result<Option<String>, EditError> {
    match d.map(str::trim) {
        None | Some("") => Ok(None),
        Some(d) if d.chars().count() > DESCRIPTION_MAX => Err(EditError::Usage(format!(
            "a profile description has at most {DESCRIPTION_MAX} characters"
        ))),
        Some(d) => Ok(Some(d.to_string())),
    }
}

fn check_new_id(doc: &Document, id: &str) -> Result<(), EditError> {
    if !is_short_id(id) {
        return Err(EditError::Usage(format!(
            "`{id}` is not a profile ID (a lowercase letter, then lowercase letters, digits or `-`, at most 32)"
        )));
    }
    if doc.profile(id).is_some() {
        return Err(EditError::Conflict {
            message: format!(
                "a profile `{id}` exists already; choose another ID (nothing was written)"
            ),
            problems: Vec::new(),
        });
    }
    Ok(())
}

fn room(doc: &Document, adding: usize) -> Result<(), EditError> {
    if doc.profiles.len() + adding > MAX_PROFILES {
        return Err(EditError::Usage(format!(
            "a rules file holds at most {MAX_PROFILES} profiles; delete one first"
        )));
    }
    Ok(())
}

fn index(doc: &Document, id: &str) -> Result<usize, EditError> {
    doc.profile_index(id)
        .ok_or_else(|| EditError::ProfileNotFound(id.to_string()))
}

fn rule_ids(doc: &Document) -> BTreeSet<String> {
    doc.profiles
        .iter()
        .flat_map(|p| p.rules.iter().filter_map(|r| r.id().map(str::to_string)))
        .collect()
}

/// Gives a rule a new ID in either shape (a raw rule keeps everything else as written).
fn set_rule_id(e: &mut RuleEntry, id: &str) {
    match e {
        RuleEntry::Typed(r) => r.id = id.to_string(),
        RuleEntry::Raw(Value::Object(m)) => {
            m.insert("id".into(), Value::String(id.to_string()));
        }
        RuleEntry::Raw(_) => {}
    }
}

// --- CRUD and switching ------------------------------------------------------------------------

/// `profile add`: an empty profile with one mode `base`, not active.
pub fn add(
    doc: &mut Document,
    id: Option<&str>,
    title: &str,
    description: Option<&str>,
) -> Result<Change, EditError> {
    let title = check_title(title)?;
    let description = check_description(description)?;
    room(doc, 1)?;
    let id = match id {
        Some(id) => {
            check_new_id(doc, id)?;
            id.to_string()
        }
        None => free_profile_id(doc, &id_from_title(&title)),
    };
    doc.profiles.push(Profile {
        id: id.clone(),
        title,
        description,
        default_mode: "base".into(),
        enabled: None,
        modes: vec![Mode::new("base", "Base")],
        rules: Vec::new(),
        extra: Map::new(),
    });
    Ok(Change {
        changed: true,
        profiles: vec![id.clone()],
        created_profile: Some(id.clone()),
        notes: vec![format!(
            "profile `{id}` is not active; `ddalgak profile apply {id}` switches to it"
        )],
        ..Change::default()
    })
}

/// What `profile edit` changes; `None` leaves a field as it is.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ProfileFields {
    pub title: Option<String>,
    /// `Some(None)` removes the description.
    pub description: Option<Option<String>>,
}

/// `profile edit` / `profile rename`: the friendly name and the description.
pub fn edit(doc: &mut Document, id: &str, f: &ProfileFields) -> Result<Change, EditError> {
    let pi = index(doc, id)?;
    let title = f.title.as_deref().map(check_title).transpose()?;
    let description = f
        .description
        .as_ref()
        .map(|d| check_description(d.as_deref()))
        .transpose()?;
    let p = &mut doc.profiles[pi];
    let before = (p.title.clone(), p.description.clone());
    if let Some(t) = title {
        p.title = t;
    }
    if let Some(d) = description {
        p.description = d;
    }
    Ok(Change {
        changed: (p.title.clone(), p.description.clone()) != before,
        profiles: vec![id.to_string()],
        ..Change::default()
    })
}

/// `profile duplicate`: a copy with every mode and rule, under a new profile ID; each copied rule
/// gets a new rule ID (rule IDs are unique in the whole document). The copy keeps the rules'
/// `enabled` and consent: its targets are unchanged and it does not run before it is applied.
pub fn duplicate(
    doc: &mut Document,
    id: &str,
    new_id: Option<&str>,
    title: Option<&str>,
) -> Result<Change, EditError> {
    let pi = index(doc, id)?;
    room(doc, 1)?;
    let src = doc.profiles[pi].clone();
    let title = match title {
        Some(t) => check_title(t)?,
        None => {
            let t = format!("{} (copy)", src.title);
            if t.chars().count() > TITLE_MAX {
                src.title.clone()
            } else {
                t
            }
        }
    };
    let new_id = match new_id {
        Some(n) => {
            check_new_id(doc, n)?;
            n.to_string()
        }
        None => free_profile_id(doc, id),
    };
    let mut taken = rule_ids(doc);
    let mut copy = src;
    copy.id = new_id.clone();
    copy.title = title;
    let mut rules = Vec::new();
    for e in &mut copy.rules {
        let old = e.id().unwrap_or("r-copy").to_string();
        let fresh = free_rule_id(&taken, &old);
        taken.insert(fresh.clone());
        set_rule_id(e, &fresh);
        rules.push(fresh);
    }
    doc.profiles.push(copy);
    Ok(Change {
        changed: true,
        rules,
        profiles: vec![new_id.clone()],
        created_profile: Some(new_id.clone()),
        notes: vec![
            "the copy's rules have new rule IDs (`<id>-2`, …); their triggers, steps, enabled state and consent are unchanged".into(),
            format!("profile `{new_id}` is not active; `ddalgak profile apply {new_id}` switches to it"),
        ],
        ..Change::default()
    })
}

/// `profile delete`: never the active profile or the only one (`profile_in_use`).
pub fn delete(doc: &mut Document, id: &str) -> Result<Change, EditError> {
    let pi = index(doc, id)?;
    if doc.profiles.len() == 1 {
        return Err(EditError::ProfileInUse(format!(
            "`{id}` is the only profile; a rules file always has one (nothing was written)"
        )));
    }
    if doc.active_profile == id {
        return Err(EditError::ProfileInUse(format!(
            "`{id}` is the active profile; switch to another one first (`ddalgak profile apply <id>`); nothing was written"
        )));
    }
    let p = doc.profiles.remove(pi);
    Ok(Change {
        changed: true,
        rules: p
            .rules
            .iter()
            .filter_map(|r| r.id().map(str::to_string))
            .collect(),
        profiles: vec![id.to_string()],
        notes: vec![
            "the history keeps every earlier version; `ddalgak profile history` and `profile restore` bring it back".into(),
        ],
        ..Change::default()
    })
}

/// `profile apply`: makes `id` the active profile (only `active_profile` changes; the running
/// app switches atomically when it reloads).
pub fn apply(doc: &mut Document, id: &str) -> Result<Change, EditError> {
    let pi = index(doc, id)?;
    let from = doc.active_profile.clone();
    let mut change = Change {
        profiles: vec![from.clone(), id.to_string()],
        ..Change::default()
    };
    if from == id {
        change.profiles = vec![id.to_string()];
        change
            .notes
            .push(format!("`{id}` is already the active profile"));
        return Ok(change);
    }
    doc.active_profile = id.to_string();
    change.changed = true;
    if !doc.profiles[pi].is_enabled() {
        change.notes.push(format!(
            "profile `{id}` is turned off: none of its rules run until `ddalgak enable --profile {id}`"
        ));
    }
    Ok(change)
}

/// `profile restore` / `profile rollback`: puts `version` (a profile as a snapshot or save point
/// holds it) back as a new change. It replaces the profile with the same ID in its place, or
/// comes back at the end when that profile was deleted. The active profile stays what it is. A
/// rule ID another profile uses meanwhile is renamed (`<id>-2`).
pub fn restore(doc: &mut Document, version: &Profile) -> Result<Change, EditError> {
    let mut v = version.clone();
    let others: BTreeSet<String> = doc
        .profiles
        .iter()
        .filter(|p| p.id != v.id)
        .flat_map(|p| p.rules.iter().filter_map(|r| r.id().map(str::to_string)))
        .collect();
    let mut taken = others.clone();
    taken.extend(v.rules.iter().filter_map(|r| r.id().map(str::to_string)));
    let mut notes = Vec::new();
    for e in &mut v.rules {
        let Some(old) = e.id().map(str::to_string) else {
            continue;
        };
        if others.contains(&old) {
            let fresh = free_rule_id(&taken, &old);
            taken.insert(fresh.clone());
            set_rule_id(e, &fresh);
            notes.push(format!(
                "rule `{old}` is used by another profile now; the restored copy is `{fresh}`"
            ));
        }
    }
    let rules = v
        .rules
        .iter()
        .filter_map(|r| r.id().map(str::to_string))
        .collect();
    let id = v.id.clone();
    let changed = match doc.profile_index(&id) {
        Some(pi) => {
            let changed = doc.profiles[pi] != v;
            doc.profiles[pi] = v;
            changed
        }
        None => {
            room(doc, 1)?;
            doc.profiles.push(v);
            notes.push(format!(
                "profile `{id}` had been deleted; it is back (not active)"
            ));
            true
        }
    };
    Ok(Change {
        changed,
        rules,
        profiles: vec![id],
        notes,
        ..Change::default()
    })
}

// --- Browsing: rows, search, sort --------------------------------------------------------------

/// One profile as the profile browser and `profile list` show it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Row {
    pub id: String,
    pub title: String,
    pub description: Option<String>,
    pub active: bool,
    pub enabled: bool,
    pub default_mode: String,
    pub modes: usize,
    pub rules: usize,
    /// Rules turned on.
    pub rules_on: usize,
    /// Errors that keep rules of the profile inactive.
    pub errors: usize,
    /// The position in the file (the order the tray menu uses).
    pub order: usize,
}

impl Row {
    pub fn to_json(&self) -> Value {
        json!({
            "id": self.id,
            "title": self.title,
            "description": self.description,
            "active": self.active,
            "enabled": self.enabled,
            "default_mode": self.default_mode,
            "modes": self.modes,
            "rules": self.rules,
            "rules_on": self.rules_on,
            "errors": self.errors,
        })
    }

    /// Whether `query` (case-insensitive) is part of the ID, the name or the description.
    pub fn matches(&self, query: &str) -> bool {
        let q = query.trim().to_lowercase();
        q.is_empty()
            || self.id.to_lowercase().contains(&q)
            || self.title.to_lowercase().contains(&q)
            || self
                .description
                .as_deref()
                .is_some_and(|d| d.to_lowercase().contains(&q))
    }
}

/// How the browser sorts profiles.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Sort {
    /// The order in the file.
    #[default]
    Order,
    /// By name (case-insensitive), then ID.
    Title,
    Id,
    /// Most rules first.
    Rules,
}

impl Sort {
    pub const ALL: [Sort; 4] = [Sort::Order, Sort::Title, Sort::Id, Sort::Rules];

    pub fn as_str(self) -> &'static str {
        match self {
            Sort::Order => "order",
            Sort::Title => "title",
            Sort::Id => "id",
            Sort::Rules => "rules",
        }
    }

    pub fn parse(s: &str) -> Option<Sort> {
        Sort::ALL.into_iter().find(|x| x.as_str() == s)
    }
}

/// The rows of every profile, in file order (`report` adds the error counts).
pub fn rows(doc: &Document, report: Option<&Report>) -> Vec<Row> {
    doc.profiles
        .iter()
        .enumerate()
        .map(|(i, p)| Row {
            id: p.id.clone(),
            title: p.title.clone(),
            description: p.description.clone(),
            active: doc.active_profile == p.id,
            enabled: p.is_enabled(),
            default_mode: p.default_mode.clone(),
            modes: p.modes.len(),
            rules: p.rules.len(),
            rules_on: p.rules.iter().filter(|r| r.enabled() == Some(true)).count(),
            errors: report.map_or(0, |r| {
                r.problems
                    .iter()
                    .filter(|x| {
                        x.severity == Severity::Error && x.profile.as_deref() == Some(&p.id)
                    })
                    .count()
            }),
            order: i,
        })
        .collect()
}

/// Search, then sort.
pub fn browse(rows: &[Row], query: &str, sort: Sort) -> Vec<Row> {
    let mut out: Vec<Row> = rows.iter().filter(|r| r.matches(query)).cloned().collect();
    match sort {
        Sort::Order => out.sort_by_key(|r| r.order),
        Sort::Title => out.sort_by(|a, b| {
            a.title
                .to_lowercase()
                .cmp(&b.title.to_lowercase())
                .then_with(|| a.id.cmp(&b.id))
        }),
        Sort::Id => out.sort_by(|a, b| a.id.cmp(&b.id)),
        Sort::Rules => {
            out.sort_by(|a, b| b.rules.cmp(&a.rules).then_with(|| a.order.cmp(&b.order)));
        }
    }
    out
}

// --- The side-by-side diff ---------------------------------------------------------------------

/// One rule as the diff shows it: what it is called, what triggers it and which kinds of steps
/// it has — never typed text, paths, arguments or URLs.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RuleLine {
    pub id: String,
    pub title: String,
    pub trigger: String,
    pub steps: Vec<String>,
    pub enabled: bool,
    pub modes: Vec<String>,
}

impl RuleLine {
    pub fn of(e: &RuleEntry) -> RuleLine {
        match e {
            RuleEntry::Typed(r) => RuleLine {
                id: r.id.clone(),
                title: r.title.clone(),
                trigger: r.trigger.display(),
                steps: r.steps.iter().map(|s| s.kind().to_string()).collect(),
                enabled: r.enabled,
                modes: r.modes.clone().unwrap_or_default(),
            },
            RuleEntry::Raw(v) => RuleLine {
                id: e.id().unwrap_or_default().to_string(),
                title: e.title().unwrap_or_default().to_string(),
                trigger: v["trigger"]["keys"]
                    .as_str()
                    .map(str::to_string)
                    .or_else(|| {
                        v["trigger"]["hotstring"]["abbreviation"]
                            .as_str()
                            .map(|a| format!("\"{a}\" (hotstring)"))
                    })
                    .unwrap_or_default(),
                steps: v["steps"]
                    .as_array()
                    .into_iter()
                    .flatten()
                    .map(|s| s["type"].as_str().unwrap_or("?").to_string())
                    .collect(),
                enabled: e.enabled() == Some(true),
                modes: v["modes"]
                    .as_array()
                    .into_iter()
                    .flatten()
                    .filter_map(|m| m.as_str().map(str::to_string))
                    .collect(),
            },
        }
    }

    pub fn to_json(&self) -> Value {
        json!({
            "id": self.id,
            "title": self.title,
            "trigger": self.trigger,
            "steps": self.steps,
            "enabled": self.enabled,
            "modes": self.modes,
        })
    }

    /// The trigger with the modes it is limited to (`CapsLock [nav]`).
    fn trigger_key(&self) -> String {
        if self.modes.is_empty() {
            self.trigger.clone()
        } else {
            let mut m = self.modes.clone();
            m.sort();
            format!("{} [{}]", self.trigger, m.join(", "))
        }
    }
}

/// How a diff row compares.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DiffStatus {
    /// Both sides have it and it does the same.
    Same,
    /// Both sides have it, differently.
    Changed,
    /// Only the left side (the current profile, or the current version).
    OnlyLeft,
    /// Only the right side.
    OnlyRight,
}

impl DiffStatus {
    pub fn as_str(self) -> &'static str {
        match self {
            DiffStatus::Same => "same",
            DiffStatus::Changed => "changed",
            DiffStatus::OnlyLeft => "only_left",
            DiffStatus::OnlyRight => "only_right",
        }
    }
}

/// How two profiles are lined up.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Align {
    /// By trigger and modes: what a key does before and after a switch.
    Trigger,
    /// By rule ID: two versions of one profile.
    RuleId,
}

/// One line of a side-by-side diff.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DiffRow {
    pub key: String,
    pub left: Option<RuleLine>,
    pub right: Option<RuleLine>,
    pub status: DiffStatus,
}

impl DiffRow {
    pub fn to_json(&self) -> Value {
        json!({
            "key": self.key,
            "status": self.status.as_str(),
            "left": self.left.as_ref().map(RuleLine::to_json),
            "right": self.right.as_ref().map(RuleLine::to_json),
        })
    }
}

/// What a rule does, without its identity and bookkeeping (to compare across profiles).
fn effect(e: &RuleEntry, keep_id: bool) -> Value {
    let mut v = e.to_value();
    if let Some(o) = v.as_object_mut() {
        if !keep_id {
            o.remove("id");
        }
        o.remove("created_at");
        o.remove("updated_at");
    }
    v
}

/// The two profiles side by side. Rows follow the left profile's order, then what only the
/// right one has.
pub fn side_by_side(left: Option<&Profile>, right: Option<&Profile>, align: Align) -> Vec<DiffRow> {
    let key = |e: &RuleEntry| {
        let line = RuleLine::of(e);
        match align {
            Align::Trigger => line.trigger_key(),
            Align::RuleId => line.id,
        }
    };
    let l: Vec<&RuleEntry> = left.map(|p| p.rules.iter().collect()).unwrap_or_default();
    let r: Vec<&RuleEntry> = right.map(|p| p.rules.iter().collect()).unwrap_or_default();
    let mut used = vec![false; r.len()];
    let mut out = Vec::new();
    for le in &l {
        let k = key(le);
        let found = r
            .iter()
            .enumerate()
            .find(|(i, re)| !used[*i] && key(re) == k)
            .map(|(i, _)| i);
        let row = match found {
            Some(i) => {
                used[i] = true;
                let keep_id = align == Align::RuleId;
                let same = effect(le, keep_id) == effect(r[i], keep_id);
                DiffRow {
                    key: k,
                    left: Some(RuleLine::of(le)),
                    right: Some(RuleLine::of(r[i])),
                    status: if same {
                        DiffStatus::Same
                    } else {
                        DiffStatus::Changed
                    },
                }
            }
            None => DiffRow {
                key: k,
                left: Some(RuleLine::of(le)),
                right: None,
                status: DiffStatus::OnlyLeft,
            },
        };
        out.push(row);
    }
    for (i, re) in r.iter().enumerate() {
        if !used[i] {
            out.push(DiffRow {
                key: key(re),
                left: None,
                right: Some(RuleLine::of(re)),
                status: DiffStatus::OnlyRight,
            });
        }
    }
    out
}

/// The diff as JSON: the two sides (ID, name, default mode, modes), the rows and their counts.
pub fn diff_json(left: Option<&Profile>, right: Option<&Profile>, align: Align) -> Value {
    let rows = side_by_side(left, right, align);
    let count = |s: DiffStatus| rows.iter().filter(|r| r.status == s).count();
    let side = |p: Option<&Profile>| {
        p.map(|p| {
            json!({
                "id": p.id,
                "title": p.title,
                "default_mode": p.default_mode,
                "modes": p.modes.iter().map(|m| m.id.clone()).collect::<Vec<_>>(),
            })
        })
    };
    json!({
        "align": match align { Align::Trigger => "trigger", Align::RuleId => "rule_id" },
        "left": side(left),
        "right": side(right),
        "counts": {
            "same": count(DiffStatus::Same),
            "changed": count(DiffStatus::Changed),
            "only_left": count(DiffStatus::OnlyLeft),
            "only_right": count(DiffStatus::OnlyRight),
        },
        "rows": rows.iter().map(DiffRow::to_json).collect::<Vec<_>>(),
    })
}

// --- Versions: history snapshots and save points -----------------------------------------------

/// One distinct version of a profile.
#[derive(Clone, Debug)]
pub struct Version {
    /// The snapshot or save point file name (`ddalgak profile restore <id> <name>`).
    pub name: String,
    pub save_point: bool,
    pub revision: Option<u64>,
    pub at: Option<String>,
    pub origin: Option<String>,
    pub command: Option<String>,
    pub label: Option<String>,
    pub rules: usize,
    pub modes: usize,
    /// Errors that kept rules of this profile inactive in that version.
    pub errors: usize,
    /// It is the profile as it is saved now.
    pub current: bool,
    pub profile: Profile,
}

impl Version {
    /// A version that worked: the document was usable and the profile had no error.
    pub fn working(&self) -> bool {
        self.errors == 0
    }

    pub fn to_json(&self) -> Value {
        json!({
            "name": self.name,
            "kind": if self.save_point { "save_point" } else { "snapshot" },
            "revision": self.revision,
            "at": self.at,
            "origin": self.origin,
            "command": self.command,
            "label": self.label,
            "rules": self.rules,
            "modes": self.modes,
            "errors": self.errors,
            "working": self.working(),
            "current": self.current,
        })
    }
}

/// Reads one snapshot or save point and the version of `id` it holds (`None` when the file is
/// not usable or has no such profile).
fn version_of(snap: &store::Snapshot, save_point: bool, id: &str) -> Option<Version> {
    let mut v: Value = serde_json::from_slice(&std::fs::read(&snap.path).ok()?).ok()?;
    v.as_object_mut()?.remove("snapshot");
    let text = serde_json::to_vec(&v).ok()?;
    let l = store::interpret(&text, &Context::default(), store::MIGRATIONS);
    if l.state != FileState::Loaded {
        return None;
    }
    let doc = l.doc?;
    let p = doc.profile(id)?.clone();
    let errors = l
        .report
        .problems
        .iter()
        .filter(|x| x.severity == Severity::Error && x.profile.as_deref() == Some(id))
        .count();
    Some(Version {
        name: snap.name.clone(),
        save_point,
        revision: snap.revision,
        at: snap.at.clone(),
        origin: snap.origin.clone(),
        command: snap.command.clone(),
        label: snap.label.clone(),
        rules: p.rules.len(),
        modes: p.modes.len(),
        errors,
        current: false,
        profile: p,
    })
}

/// The versions of profile `id`, newest first: every save point, and each history snapshot in
/// which the profile differs from the next older one. `current` marks the versions equal to the
/// profile as saved now.
pub fn versions(files: &Files, id: &str, current: Option<&Profile>) -> Vec<Version> {
    let mut all: Vec<Version> = store::history(files)
        .iter()
        .filter_map(|s| version_of(s, false, id))
        .chain(
            store::save_points(files)
                .iter()
                .filter_map(|s| version_of(s, true, id)),
        )
        .collect();
    // Oldest first, by time and then revision (save points of one revision after its snapshot).
    all.sort_by(|a, b| {
        (a.at.as_deref(), a.revision, a.save_point).cmp(&(
            b.at.as_deref(),
            b.revision,
            b.save_point,
        ))
    });
    let mut out: Vec<Version> = Vec::new();
    let mut last: Option<Profile> = None;
    for mut v in all {
        let distinct = last.as_ref() != Some(&v.profile);
        if v.save_point || distinct {
            v.current = current == Some(&v.profile);
            if !v.save_point {
                last = Some(v.profile.clone());
            }
            out.push(v);
        }
    }
    out.reverse();
    out
}

/// The version a rollback goes back to: the newest one that differs from the current profile and
/// worked (no error in that profile).
pub fn rollback_target<'a>(
    versions: &'a [Version],
    current: Option<&Profile>,
) -> Option<&'a Version> {
    versions
        .iter()
        .find(|v| Some(&v.profile) != current && v.working())
}

// --- Export ------------------------------------------------------------------------------------

/// What `profile export` includes.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct ExportOptions {
    /// Keep the values of `open_program` environment overrides (they may be secrets; off by
    /// default, the names stay).
    pub env_values: bool,
}

/// An export and what was changed to make it portable.
#[derive(Clone, Debug)]
pub struct Exported {
    pub doc: Document,
    pub profiles: Vec<String>,
    pub rules: usize,
    /// Program and folder paths rewritten under a known-folder token (`%LocalAppData%\…`).
    pub portable_paths: usize,
    /// Environment values left out.
    pub env_values_removed: usize,
    /// Rules whose consent was left out.
    pub consent_removed: usize,
}

/// Makes the rules of an exported profile portable and inert (every rule off, no consent).
fn export_rule(
    e: &mut RuleEntry,
    opts: ExportOptions,
    var: &dyn Fn(&str) -> Option<OsString>,
    out: &mut Exported,
) {
    e.set_enabled(false);
    match e {
        RuleEntry::Typed(r) => {
            if r.consent.take().is_some() {
                out.consent_removed += 1;
            }
            r.created_at = None;
            r.updated_at = None;
            for s in &mut r.steps {
                if let Action::OpenProgram(p) = s {
                    for path in [&mut p.path, &mut p.working_dir].into_iter().flatten() {
                        if let Some(t) = crate::launch::tokenize(path, var)
                            && t != *path
                        {
                            *path = t;
                            out.portable_paths += 1;
                        }
                    }
                    if !opts.env_values {
                        for v in p.env.iter_mut().flatten() {
                            if !v.value.is_empty() {
                                v.value.clear();
                                out.env_values_removed += 1;
                            }
                        }
                    }
                }
            }
        }
        RuleEntry::Raw(Value::Object(m)) => {
            if m.remove("consent").is_some() {
                out.consent_removed += 1;
            }
            if !opts.env_values {
                for s in m
                    .get_mut("steps")
                    .and_then(Value::as_array_mut)
                    .into_iter()
                    .flatten()
                {
                    for v in s
                        .get_mut("env")
                        .and_then(Value::as_array_mut)
                        .into_iter()
                        .flatten()
                    {
                        if v["value"].as_str().is_some_and(|x| !x.is_empty()) {
                            v["value"] = json!("");
                            out.env_values_removed += 1;
                        }
                    }
                }
            }
        }
        RuleEntry::Raw(_) => {}
    }
}

/// `profile export`: the profiles `ids` as a portable `ddalgak.rules` document (§10): an
/// `exported` block, **without** consent, settings or revision, every rule off, paths under known
/// folders as tokens, environment values removed unless `opts.env_values`. `var` reads the
/// environment for the tokens ([`crate::launch::process_var`] in the CLI).
pub fn export(
    doc: &Document,
    ids: &[String],
    opts: ExportOptions,
    var: &dyn Fn(&str) -> Option<OsString>,
) -> Result<Exported, EditError> {
    if ids.is_empty() {
        return Err(EditError::Usage(
            "name at least one profile to export".into(),
        ));
    }
    let mut out = Exported {
        doc: Document::empty(),
        profiles: Vec::new(),
        rules: 0,
        portable_paths: 0,
        env_values_removed: 0,
        consent_removed: 0,
    };
    let mut profiles = Vec::new();
    for id in ids {
        let mut p = doc
            .profile(id)
            .ok_or_else(|| EditError::ProfileNotFound(id.clone()))?
            .clone();
        if profiles.iter().any(|x: &Profile| x.id == p.id) {
            continue;
        }
        for e in &mut p.rules {
            export_rule(e, opts, var, &mut out);
        }
        out.rules += p.rules.len();
        out.profiles.push(p.id.clone());
        profiles.push(p);
    }
    let mut exported = json!({
        "exported_at": crate::time::now_rfc3339(),
        "by_version": crate::VERSION,
    });
    if out.env_values_removed > 0 {
        exported["withheld"] = json!(["env_values"]);
    }
    out.doc.revision = None;
    out.doc.settings = None;
    out.doc.exported = Some(exported);
    out.doc.active_profile = profiles[0].id.clone();
    out.doc.profiles = profiles;
    Ok(out)
}

// --- Import ------------------------------------------------------------------------------------

/// Why a file cannot be imported. Nothing was changed.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ImportError {
    /// Not JSON, not a `ddalgak.rules` document, or it fails the schema outside its rules
    /// (`import_invalid`).
    Invalid(String),
    /// Written by a newer Ddalgak (`rules_from_newer_version`).
    Newer(u64),
}

impl std::fmt::Display for ImportError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ImportError::Invalid(m) => write!(f, "the file cannot be imported: {m}"),
            ImportError::Newer(v) => write!(
                f,
                "the file has schema_version {v}, written by a newer Ddalgak; this version cannot import it"
            ),
        }
    }
}

/// What the user chose for an import.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ImportOptions {
    /// The ID for the imported profile (a file with one profile only).
    pub id: Option<String>,
    /// The name for the imported profile (a file with one profile only).
    pub title: Option<String>,
}

/// Another rule (or one of Ddalgak's own shortcuts) an imported rule would collide with once
/// both are turned on.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Conflict {
    /// An earlier imported rule of the same profile with the same trigger, modes, conditions and
    /// priority (`duplicate_trigger`).
    Rule(String),
    /// Ddalgak's own shortcut in `hotkeys.json` (`duplicate_binding`).
    Shortcut(String),
}

/// One rule an import would add.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct IncomingRule {
    pub id: String,
    pub renamed_from: Option<String>,
    pub line: RuleLine,
    /// What it needs consent for before it can be turned on (never imported).
    pub permissions: Vec<Permission>,
    /// The exact programs, apps and websites it would open once approved (shown only on request:
    /// `--show-targets`, and in the editor's preview).
    pub targets: Vec<String>,
    /// Program files or working folders that do not exist on this computer.
    pub missing: usize,
    pub conflicts: Vec<Conflict>,
    /// The code that keeps it inactive even when turned on (a newer step type, a reserved
    /// trigger, …), if any.
    pub inactive: Option<String>,
}

impl IncomingRule {
    pub fn to_json(&self, show_targets: bool) -> Value {
        json!({
            "id": self.id,
            "renamed_from": self.renamed_from,
            "title": self.line.title,
            "trigger": self.line.trigger,
            "steps": self.line.steps,
            "permissions": self.permissions.iter().map(|p| p.as_str()).collect::<Vec<_>>(),
            "targets": if show_targets { json!(self.targets) } else { json!({ "redacted": true, "count": self.targets.len() }) },
            "missing_programs": self.missing,
            "conflicts": self.conflicts.iter().map(|c| match c {
                Conflict::Rule(r) => json!({ "rule": r }),
                Conflict::Shortcut(a) => json!({ "action": a }),
            }).collect::<Vec<_>>(),
            "inactive": self.inactive,
        })
    }
}

/// One profile an import would add.
#[derive(Clone, Debug)]
pub struct IncomingProfile {
    pub id: String,
    pub renamed_from: Option<String>,
    pub title: String,
    pub description: Option<String>,
    pub modes: Vec<String>,
    /// Modes the file's rules (or its default mode) referred to but did not define (a deleted
    /// mode): added empty, so the rules keep their limits.
    pub restored_modes: Vec<String>,
    pub rules: Vec<IncomingRule>,
    /// The profile as it would be added.
    pub profile: Profile,
}

impl IncomingProfile {
    pub fn to_json(&self, show_targets: bool) -> Value {
        json!({
            "id": self.id,
            "renamed_from": self.renamed_from,
            "title": self.title,
            "description": self.description,
            "modes": self.modes,
            "restored_modes": self.restored_modes,
            "rules": self.rules.iter().map(|r| r.to_json(show_targets)).collect::<Vec<_>>(),
        })
    }
}

/// What an import would do (shown before anything is written).
#[derive(Clone, Debug)]
pub struct ImportPlan {
    pub exported_at: Option<String>,
    pub by_version: Option<String>,
    /// The file's `schema_version` when it was migrated.
    pub migrated_from: Option<u64>,
    pub profiles: Vec<IncomingProfile>,
}

impl ImportPlan {
    /// Every rule ID the import adds.
    pub fn rule_ids(&self) -> BTreeSet<String> {
        self.profiles
            .iter()
            .flat_map(|p| p.rules.iter().map(|r| r.id.clone()))
            .collect()
    }

    pub fn counts(&self) -> Value {
        let rules: Vec<&IncomingRule> = self.profiles.iter().flat_map(|p| &p.rules).collect();
        json!({
            "profiles": self.profiles.len(),
            "rules": rules.len(),
            "need_consent": rules.iter().filter(|r| !r.permissions.is_empty()).count(),
            "missing_programs": rules.iter().filter(|r| r.missing > 0).count(),
            "conflicts": rules.iter().filter(|r| !r.conflicts.is_empty()).count(),
            "inactive": rules.iter().filter(|r| r.inactive.is_some()).count(),
            "renamed_rules": rules.iter().filter(|r| r.renamed_from.is_some()).count(),
            "restored_modes": self.profiles.iter().map(|p| p.restored_modes.len()).sum::<usize>(),
        })
    }

    pub fn to_json(&self, show_targets: bool) -> Value {
        json!({
            "file": {
                "exported_at": self.exported_at,
                "by_version": self.by_version,
                "migrated_from": self.migrated_from,
            },
            "counts": self.counts(),
            "profiles": self.profiles.iter().map(|p| p.to_json(show_targets)).collect::<Vec<_>>(),
        })
    }
}

/// The mode IDs a profile's raw JSON refers to: its default mode, the modes' parents, the rules'
/// `modes` and the `mode` steps' targets.
fn referenced_modes(p: &Value) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    let mut add = |m: &Value| {
        if let Some(id) = m.as_str()
            && is_short_id(id)
            && !out.iter().any(|x| x == id)
        {
            out.push(id.to_string());
        }
    };
    add(&p["default_mode"]);
    for m in p["modes"].as_array().into_iter().flatten() {
        add(&m["parent"]);
    }
    for r in p["rules"].as_array().into_iter().flatten() {
        for m in r["modes"].as_array().into_iter().flatten() {
            add(m);
        }
        for s in r["steps"].as_array().into_iter().flatten() {
            if s["type"] == "mode" {
                add(&s["mode"]);
                for m in s["modes"].as_array().into_iter().flatten() {
                    add(m);
                }
            }
        }
    }
    out
}

/// Adds the modes a profile refers to but does not define (a mode deleted in the exporting
/// Ddalgak, a hand edit), each empty with its ID as its name; returns their IDs per profile.
fn restore_missing_modes(v: &mut Value) -> Vec<Vec<String>> {
    let mut restored = Vec::new();
    for p in v
        .get_mut("profiles")
        .and_then(Value::as_array_mut)
        .into_iter()
        .flatten()
    {
        let mut added = Vec::new();
        let wanted = referenced_modes(p);
        if let Some(modes) = p.get_mut("modes").and_then(Value::as_array_mut) {
            for id in wanted {
                if modes.len() >= MAX_MODES {
                    break;
                }
                if !modes.iter().any(|m| m["id"] == id.as_str()) {
                    modes.push(json!({ "id": id, "title": id }));
                    added.push(id);
                }
            }
        }
        restored.push(added);
    }
    restored
}

/// The exact targets of a rule's launch steps (paths, app IDs, URLs) — for the review only.
fn targets(e: &RuleEntry) -> Vec<String> {
    let Some(r) = e.rule() else {
        return Vec::new();
    };
    r.steps
        .iter()
        .filter_map(|s| match s {
            Action::OpenProgram(p) => p.path.clone().or_else(|| p.app_id.clone()),
            Action::OpenUrl(u) => Some(u.url.clone()),
            _ => None,
        })
        .collect()
}

/// Program files and working folders of a rule that do not exist here (unknown tokens count).
fn missing_programs(e: &RuleEntry) -> usize {
    let Some(r) = e.rule() else {
        return 0;
    };
    r.steps
        .iter()
        .filter_map(|s| match s {
            Action::OpenProgram(p) => Some([p.path.as_deref(), p.working_dir.as_deref()]),
            _ => None,
        })
        .flatten()
        .flatten()
        .filter(|path| validate::resolve_known_folder(path).is_none_or(|p| !p.exists()))
        .count()
}

/// What makes two turned-on rules of one profile claim the same input (DDALGAK_RULE_MODEL §8
/// `duplicate_trigger`): the trigger claim, the modes, the conditions and the priority.
fn claim(e: &RuleEntry) -> Option<String> {
    let r = e.rule()?;
    let mut modes = r.modes.clone().unwrap_or_default();
    modes.sort();
    Some(format!(
        "{:?}|{modes:?}|{}|{}",
        r.trigger.claim(),
        r.priority(),
        r.when
            .as_ref()
            .map(|w| serde_json::to_string(w).unwrap_or_default())
            .unwrap_or_default()
    ))
}

/// Reads an import file and plans adding its profiles to `current` (nothing is written).
pub fn plan_import(
    bytes: &[u8],
    current: &Document,
    ctx: &Context,
    opts: &ImportOptions,
) -> Result<ImportPlan, ImportError> {
    plan_import_with(bytes, current, ctx, opts, store::MIGRATIONS)
}

/// [`plan_import`] with a migration chain (the tests use a test chain; the shipped one is empty).
pub fn plan_import_with(
    bytes: &[u8],
    current: &Document,
    ctx: &Context,
    opts: &ImportOptions,
    chain: &[Migration],
) -> Result<ImportPlan, ImportError> {
    let mut v: Value = serde_json::from_slice(bytes)
        .map_err(|e| ImportError::Invalid(format!("not valid JSON: {e}")))?;
    if !v.is_object() || v["schema"] != crate::model::SCHEMA {
        return Err(ImportError::Invalid(format!(
            "not a `{}` document",
            crate::model::SCHEMA
        )));
    }
    if let Some(n) = v["schema_version"].as_u64()
        && n > crate::model::SCHEMA_VERSION
    {
        return Err(ImportError::Newer(n));
    }
    let restored = restore_missing_modes(&mut v);
    let text = serde_json::to_vec(&v).unwrap_or_default();
    let l = store::interpret(&text, &Context::default(), chain);
    let doc = match (&l.state, l.doc) {
        (FileState::Loaded, Some(d)) => d,
        (FileState::NewerVersion(n), _) => return Err(ImportError::Newer(*n)),
        (state, _) => {
            return Err(ImportError::Invalid(
                state
                    .problem()
                    .unwrap_or_else(|| "not a usable rules document".into()),
            ));
        }
    };
    if (opts.id.is_some() || opts.title.is_some()) && doc.profiles.len() != 1 {
        return Err(ImportError::Invalid(format!(
            "the file has {} profiles; a new ID or name can be given only for a file with one",
            doc.profiles.len()
        )));
    }
    if let Some(t) = &opts.title {
        check_title(t).map_err(|e| ImportError::Invalid(e.message()))?;
    }
    if let Some(id) = &opts.id
        && !is_short_id(id)
    {
        return Err(ImportError::Invalid(format!("`{id}` is not a profile ID")));
    }
    let mut taken_profiles: BTreeSet<String> =
        current.profiles.iter().map(|p| p.id.clone()).collect();
    let current_rules = rule_ids(current);
    let mut taken_rules: BTreeSet<String> = current_rules.clone();
    taken_rules.extend(
        doc.profiles
            .iter()
            .flat_map(|p| p.rules.iter().filter_map(|r| r.id().map(str::to_string))),
    );
    let mut plan = ImportPlan {
        exported_at: doc
            .exported
            .as_ref()
            .and_then(|e| e["exported_at"].as_str())
            .map(str::to_string),
        by_version: doc
            .exported
            .as_ref()
            .and_then(|e| e["by_version"].as_str())
            .map(str::to_string),
        migrated_from: l.migrated_from,
        profiles: Vec::new(),
    };
    for (pi, src) in doc.profiles.iter().enumerate() {
        let mut p = src.clone();
        let wanted = opts.id.clone().unwrap_or_else(|| p.id.clone());
        let id = free_id(&|x| taken_profiles.contains(x), &wanted, 32);
        taken_profiles.insert(id.clone());
        let renamed_from = (id != src.id).then(|| src.id.clone());
        p.id = id.clone();
        if let Some(t) = &opts.title {
            p.title = check_title(t).map_err(|e| ImportError::Invalid(e.message()))?;
        }
        let mut seen: Vec<(String, String)> = Vec::new();
        let mut rules = Vec::new();
        for e in &mut p.rules {
            let original = e.id().unwrap_or("r-imported").to_string();
            let mut new_id = original.clone();
            if current_rules.contains(&original) {
                new_id = free_rule_id(&taken_rules, &original);
                taken_rules.insert(new_id.clone());
                set_rule_id(e, &new_id);
            }
            // Every imported rule arrives off and without consent (DDALGAK-SEC-013).
            e.set_enabled(false);
            match e {
                RuleEntry::Typed(r) => {
                    r.consent = None;
                    r.created_at = None;
                    r.updated_at = None;
                }
                RuleEntry::Raw(Value::Object(m)) => {
                    m.remove("consent");
                }
                RuleEntry::Raw(_) => {}
            }
            let mut conflicts = Vec::new();
            if let Some(c) = claim(e) {
                if let Some((other, _)) = seen.iter().find(|(_, k)| *k == c) {
                    conflicts.push(Conflict::Rule(other.clone()));
                }
                seen.push((new_id.clone(), c));
            }
            if let Some(keys) = e.rule().and_then(|r| r.trigger.keys.clone()) {
                for (action, k) in &ctx.shortcuts {
                    if *k == keys {
                        conflicts.push(Conflict::Shortcut(action.clone()));
                    }
                }
            }
            rules.push(IncomingRule {
                id: new_id.clone(),
                renamed_from: (new_id != original).then_some(original),
                line: RuleLine::of(e),
                permissions: e.rule().map(|r| r.permissions_needed()).unwrap_or_default(),
                targets: targets(e),
                missing: missing_programs(e),
                conflicts,
                inactive: None,
            });
        }
        plan.profiles.push(IncomingProfile {
            id,
            renamed_from,
            title: p.title.clone(),
            description: p.description.clone(),
            modes: p.modes.iter().map(|m| m.id.clone()).collect(),
            restored_modes: restored.get(pi).cloned().unwrap_or_default(),
            rules,
            profile: p,
        });
    }
    // What would keep imported rules inactive once turned on (a newer step type, a reserved
    // trigger, a self-triggering output, …), checked as if they were on in the document they
    // would join. Consent and conflicts are listed on their own above.
    let mut merged = current.clone();
    for ip in &plan.profiles {
        let mut on = ip.profile.clone();
        for e in &mut on.rules {
            e.set_enabled(true);
        }
        merged.profiles.push(on);
    }
    let report = validate::check(&merged.to_value(), ctx);
    for ip in &mut plan.profiles {
        for r in &mut ip.rules {
            r.inactive = report
                .problems
                .iter()
                .find(|x| {
                    x.inactivates()
                        && x.rule.as_deref() == Some(&r.id)
                        && !matches!(
                            x.code,
                            "consent_missing" | "duplicate_trigger" | "duplicate_binding"
                        )
                })
                .map(|x| x.code.to_string());
        }
    }
    Ok(plan)
}

/// Adds the planned profiles. It never changes `active_profile` or `settings`, and every rule is
/// off and without consent. Refused when an ID the plan uses was taken meanwhile.
pub fn apply_import(doc: &mut Document, plan: &ImportPlan) -> Result<Change, EditError> {
    room(doc, plan.profiles.len())?;
    let taken = rule_ids(doc);
    for p in &plan.profiles {
        if doc.profile(&p.id).is_some() {
            return Err(EditError::Conflict {
                message: format!(
                    "a profile `{}` appeared meanwhile; import again (nothing was written)",
                    p.id
                ),
                problems: Vec::new(),
            });
        }
        if let Some(r) = p.rules.iter().find(|r| taken.contains(&r.id)) {
            return Err(EditError::Conflict {
                message: format!(
                    "a rule `{}` appeared meanwhile; import again (nothing was written)",
                    r.id
                ),
                problems: Vec::new(),
            });
        }
    }
    let mut change = Change {
        changed: !plan.profiles.is_empty(),
        ..Change::default()
    };
    for p in &plan.profiles {
        doc.profiles.push(p.profile.clone());
        change.profiles.push(p.id.clone());
        change.rules.extend(p.rules.iter().map(|r| r.id.clone()));
    }
    change.created_profile = plan.profiles.first().map(|p| p.id.clone());
    change.notes.push(
        "imported profiles are not active and every imported rule is off: review each one, then turn it on (programs, websites and typing ask for consent first)".into(),
    );
    Ok(change)
}

/// The review of an import: like [`edit::review`], except that problems of the imported rules
/// themselves do not refuse it (they arrive off and stay inactive until fixed, and the preview
/// names them); anything else the import would break refuses it.
pub fn review_import(
    before: &Report,
    after: &Report,
    imported: &BTreeSet<String>,
) -> Result<(), EditError> {
    let rest: Vec<Problem> = edit::introduced(before, after)
        .into_iter()
        .filter(|p| !p.rule.as_ref().is_some_and(|r| imported.contains(r)))
        .collect();
    match rest.first() {
        None => Ok(()),
        Some(first) => Err(EditError::Invalid {
            code: "rule_invalid",
            message: format!("{}; nothing was written", first.message),
            problems: rest,
        }),
    }
}

#[cfg(test)]
mod tests;
