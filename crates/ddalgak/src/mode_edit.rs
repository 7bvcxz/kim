//! Mode CRUD on a rules document and the editor's Modes page (Step 72; docs/DDALGAK_RULE_MODEL.md
//! §7.3, §13; DDALGAK_GUI_JOURNEYS J6; `DDALGAK-FR-016`).
//!
//! Every operation is a pure change of an in-memory [`Document`], like the rule operations of
//! [`crate::edit`]: callers (the CLI, the Step 74 editor) load the document under the store's
//! lock, apply one operation and then call [`review`], which refuses a change that introduces an
//! error (a parent cycle, a default mode with a parent or an exit, an unknown mode) with
//! `mode_invalid` and nothing written. Mode IDs never change after creation (rules, mode steps and
//! layers refer to them); the title is what users rename.
//!
//! - **Delete** refuses (`mode_in_use`) the default mode, a mode other modes are layers over, and
//!   a mode that a rule's mode step switches to; rules limited to the mode are listed and kept
//!   unless the caller asks to delete them (`delete_rules`), in which case rules limited to it
//!   alone are deleted and the mode is taken out of the others' lists. Deleting the mode the
//!   running engine is in is allowed: the engine falls back to the default mode on the reload.
//! - **Copy** duplicates the definition (a fresh ID, "… (copy)") and, unless asked not to, the
//!   rules limited to the mode alone, each with a fresh ID, limited to the copy, with the same
//!   enabled state and consent (the targets are unchanged, RULE_MODEL §6.3).

use crate::edit::{Change, EditError};
use crate::model::{
    Action, Document, Mode, ModeIndicator, Profile, RuleEntry, is_short_id, new_rule_id,
};
use crate::modes::{self, ModeSet};
use crate::remap::Field;
use crate::text::FieldProblem;
use crate::validate::{Problem, Report, Severity};
use serde_json::{Value, json};

/// Field changes of `mode add` / `mode edit` (`Some(None)` clears an optional field).
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ModeFields {
    pub title: Option<String>,
    pub icon: Option<Option<String>>,
    pub color: Option<Option<String>>,
    pub parent: Option<Option<String>>,
    pub one_shot: Option<bool>,
    pub timeout_ms: Option<Option<u32>>,
    /// Make it the profile's default mode.
    pub make_default: bool,
}

fn profile_index(doc: &Document, id: Option<&str>) -> Result<usize, EditError> {
    let id = id.unwrap_or(&doc.active_profile);
    doc.profile_index(id)
        .ok_or_else(|| EditError::ProfileNotFound(id.to_string()))
}

fn mode_index(p: &Profile, id: &str) -> Result<usize, EditError> {
    p.modes
        .iter()
        .position(|m| m.id == id)
        .ok_or_else(|| EditError::ModeNotFound(id.to_string()))
}

/// A mode ID from a title: lowercase ASCII letters, digits and `-`, starting with a letter, at
/// most 32 characters, unique in the profile (`-2`, `-3`, … when taken).
pub fn id_from_title(p: &Profile, title: &str) -> String {
    let mut base: String = title
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() {
                c.to_ascii_lowercase()
            } else {
                '-'
            }
        })
        .collect();
    while base.contains("--") {
        base = base.replace("--", "-");
    }
    let mut base = base.trim_matches('-').to_string();
    if !base.starts_with(|c: char| c.is_ascii_lowercase()) {
        base = format!("mode-{base}").trim_end_matches('-').to_string();
    }
    base.truncate(28);
    let base = base.trim_end_matches('-').to_string();
    let taken = |id: &str| p.modes.iter().any(|m| m.id == id);
    if !taken(&base) {
        return base;
    }
    (2..)
        .map(|n| format!("{base}-{n}"))
        .find(|id| !taken(id))
        .expect("a free ID")
}

fn check_fields(f: &ModeFields) -> Result<(), EditError> {
    let bad = |m: String| Err(EditError::Usage(m));
    if let Some(t) = &f.title
        && !(1..=80).contains(&t.chars().count())
    {
        return bad("a mode title has 1–80 characters".into());
    }
    if let Some(Some(i)) = &f.icon
        && (i.is_empty()
            || i.chars().count() > 8
            || i.chars().any(|c| c.is_whitespace() || c.is_control()))
    {
        return bad(
            "an icon is 1–8 characters without spaces (a letter, a syllable, a symbol or an emoji)"
                .into(),
        );
    }
    if let Some(Some(c)) = &f.color
        && !(c.len() == 7 && c.starts_with('#') && c[1..].chars().all(|x| x.is_ascii_hexdigit()))
    {
        return bad(format!("`{c}` is not a color like #B7A6F5"));
    }
    if let Some(Some(p)) = &f.parent
        && !is_short_id(p)
    {
        return bad(format!("`{p}` is not a mode ID"));
    }
    if let Some(Some(t)) = f.timeout_ms
        && !(1000..=3_600_000).contains(&t)
    {
        return bad("a timeout is 1000–3600000 ms (1 s to 1 h)".into());
    }
    Ok(())
}

fn apply_fields(m: &mut Mode, f: &ModeFields) {
    if let Some(t) = &f.title {
        m.title = t.clone();
    }
    if let Some(i) = &f.icon {
        m.icon = i.clone();
    }
    if let Some(c) = &f.color {
        m.color = c.as_ref().map(|c| c.to_ascii_uppercase());
    }
    if let Some(p) = &f.parent {
        m.parent = p.clone();
    }
    if let Some(o) = f.one_shot {
        m.one_shot = o.then_some(true);
    }
    if let Some(t) = f.timeout_ms {
        m.timeout_ms = t;
    }
}

fn changed(mut c: Change, mode: &str) -> Change {
    c.changed = true;
    c.modes.push(mode.to_string());
    c
}

/// `mode add`: a new mode with a title, an optional ID (else one from the title) and fields.
pub fn add(
    doc: &mut Document,
    profile: Option<&str>,
    id: Option<String>,
    fields: &ModeFields,
) -> Result<Change, EditError> {
    check_fields(fields)?;
    let pi = profile_index(doc, profile)?;
    let p = &mut doc.profiles[pi];
    let title = fields
        .title
        .clone()
        .ok_or_else(|| EditError::Usage("a new mode needs --title".into()))?;
    if p.modes.len() >= modes::MAX_MODES {
        return Err(EditError::Usage(format!(
            "profile `{}` already has {} modes, the most a profile can have",
            p.id,
            modes::MAX_MODES
        )));
    }
    let id = match id {
        Some(id) if !is_short_id(&id) => {
            return Err(EditError::Usage(format!(
                "`{id}` is not a mode ID: 1–32 lowercase letters, digits and `-`, starting with a letter"
            )));
        }
        Some(id) if p.modes.iter().any(|m| m.id == id) => {
            return Err(EditError::Conflict {
                message: format!("profile `{}` already has a mode `{id}`", p.id),
                problems: Vec::new(),
            });
        }
        Some(id) => id,
        None => id_from_title(p, &title),
    };
    let mut m = Mode::new(id.clone(), title);
    apply_fields(&mut m, fields);
    p.modes.push(m);
    if fields.make_default {
        p.default_mode = id.clone();
    }
    let mut c = changed(Change::default(), &id);
    c.created_mode = Some(id);
    Ok(c)
}

/// `mode edit`: changes fields of a mode (the ID never changes).
pub fn edit(
    doc: &mut Document,
    profile: Option<&str>,
    id: &str,
    fields: &ModeFields,
) -> Result<Change, EditError> {
    check_fields(fields)?;
    let pi = profile_index(doc, profile)?;
    let p = &mut doc.profiles[pi];
    let mi = mode_index(p, id)?;
    let before = p.modes[mi].clone();
    let was_default = p.default_mode == id;
    apply_fields(&mut p.modes[mi], fields);
    if fields.make_default {
        p.default_mode = id.to_string();
    }
    let mut c = Change::default();
    if p.modes[mi] != before || (fields.make_default && !was_default) {
        c = changed(c, id);
    } else {
        c.modes.push(id.to_string());
        c.notes.push("nothing changed".into());
    }
    Ok(c)
}

/// `mode rename`: a new title (the ID stays: rules refer to it).
pub fn rename(
    doc: &mut Document,
    profile: Option<&str>,
    id: &str,
    title: &str,
) -> Result<Change, EditError> {
    edit(
        doc,
        profile,
        id,
        &ModeFields {
            title: Some(title.to_string()),
            ..ModeFields::default()
        },
    )
}

/// The rules of profile `pi` limited to mode `id` alone, and those that list it among others.
fn scoped(p: &Profile, id: &str) -> (Vec<String>, Vec<String>) {
    let mut alone = Vec::new();
    let mut shared = Vec::new();
    for e in &p.rules {
        let Some(r) = e.rule() else { continue };
        if let Some(m) = &r.modes
            && m.iter().any(|x| x == id)
        {
            if m.iter().all(|x| x == id) {
                alone.push(r.id.clone());
            } else {
                shared.push(r.id.clone());
            }
        }
    }
    (alone, shared)
}

/// The rules whose mode steps switch to, toggle, hold or cycle through mode `id`.
fn switching(p: &Profile, id: &str) -> Vec<String> {
    p.rules
        .iter()
        .filter_map(RuleEntry::rule)
        .filter(|r| {
            r.steps.iter().any(|s| match s {
                Action::Mode(m) => {
                    m.mode.as_deref() == Some(id)
                        || m.modes.as_ref().is_some_and(|l| l.iter().any(|x| x == id))
                }
                _ => false,
            })
        })
        .map(|r| r.id.clone())
        .collect()
}

/// `mode copy`: a copy of the mode (fresh ID, "… (copy)" unless a title is given) and, with
/// `with_rules`, of the rules limited to it alone.
pub fn copy(
    doc: &mut Document,
    profile: Option<&str>,
    id: &str,
    new_id: Option<String>,
    title: Option<String>,
    with_rules: bool,
) -> Result<Change, EditError> {
    let pi = profile_index(doc, profile)?;
    let mi = mode_index(&doc.profiles[pi], id)?;
    let src = doc.profiles[pi].modes[mi].clone();
    let title = title.unwrap_or_else(|| {
        let t = format!("{} (copy)", src.title);
        t.chars().take(80).collect()
    });
    let fields = ModeFields {
        title: Some(title),
        icon: Some(src.icon.clone()),
        color: Some(src.color.clone()),
        parent: Some(src.parent.clone()),
        one_shot: Some(src.one_shot == Some(true)),
        timeout_ms: Some(src.timeout_ms),
        make_default: false,
    };
    let mut c = add(doc, profile, new_id, &fields)?;
    let copy_id = c.created_mode.clone().expect("add creates a mode");
    // Rule IDs are unique in the whole document.
    let mut taken: std::collections::BTreeSet<String> = doc
        .profiles
        .iter()
        .flat_map(|p| p.rules.iter().filter_map(|r| r.id().map(str::to_string)))
        .collect();
    // Unknown fields of the source mode come along (a newer Ddalgak may have written them).
    let p = &mut doc.profiles[pi];
    if let Some(m) = p.modes.iter_mut().find(|m| m.id == copy_id) {
        m.extra = src.extra;
    }
    if with_rules {
        let (alone, _) = scoped(p, id);
        let now = crate::time::now_rfc3339();
        let mut copies = Vec::new();
        for rid in &alone {
            let Some(r) = p
                .rules
                .iter()
                .find_map(|e| e.rule().filter(|r| &r.id == rid))
            else {
                continue;
            };
            let mut r = r.clone();
            r.id = new_rule_id(|cand| taken.contains(cand));
            taken.insert(r.id.clone());
            r.modes = Some(vec![copy_id.clone()]);
            r.created_at = Some(now.clone());
            r.updated_at = Some(now.clone());
            copies.push(r);
        }
        for r in copies {
            c.rules.push(r.id.clone());
            p.rules.push(RuleEntry::Typed(Box::new(r)));
        }
        if !alone.is_empty() {
            c.notes.push(format!(
                "copied {} rule(s) limited to `{id}`; each copy works only in `{copy_id}`",
                alone.len()
            ));
        }
    }
    Ok(c)
}

/// `mode delete`: removes a mode nothing else depends on (see the module documentation).
pub fn delete(
    doc: &mut Document,
    profile: Option<&str>,
    id: &str,
    delete_rules: bool,
) -> Result<Change, EditError> {
    let pi = profile_index(doc, profile)?;
    // Rule IDs are unique in the whole document: other profiles cannot refer to this mode.
    let p = &doc.profiles[pi];
    let mi = mode_index(p, id)?;
    if p.default_mode == id {
        return Err(EditError::ModeInUse {
            message: format!(
                "`{id}` is the default mode of profile `{}`; make another mode the default first (`ddalgak mode edit <id> --default`)",
                p.id
            ),
            users: Vec::new(),
        });
    }
    let layers: Vec<String> = p
        .modes
        .iter()
        .filter(|m| m.parent.as_deref() == Some(id))
        .map(|m| m.id.clone())
        .collect();
    if !layers.is_empty() {
        return Err(EditError::ModeInUse {
            message: format!(
                "mode {} {} a layer over `{id}`; give {} another parent or none first",
                layers
                    .iter()
                    .map(|l| format!("`{l}`"))
                    .collect::<Vec<_>>()
                    .join(", "),
                if layers.len() == 1 { "is" } else { "are" },
                if layers.len() == 1 { "it" } else { "them" }
            ),
            users: layers,
        });
    }
    let switches = switching(p, id);
    if !switches.is_empty() {
        return Err(EditError::ModeInUse {
            message: format!(
                "rule {} switch{} to `{id}`; edit or delete {} first",
                switches
                    .iter()
                    .map(|r| format!("`{r}`"))
                    .collect::<Vec<_>>()
                    .join(", "),
                if switches.len() == 1 { "es" } else { "" },
                if switches.len() == 1 { "it" } else { "them" }
            ),
            users: switches,
        });
    }
    let (alone, shared) = scoped(p, id);
    if (!alone.is_empty() || !shared.is_empty()) && !delete_rules {
        let mut users = alone;
        users.extend(shared.iter().cloned());
        return Err(EditError::ModeInUse {
            message: format!(
                "{} rule(s) are limited to `{id}`: {}; move them to another mode or pass --delete-rules (rules limited to `{id}` alone are deleted, the others lose it from their list)",
                users.len(),
                users
                    .iter()
                    .map(|r| format!("`{r}`"))
                    .collect::<Vec<_>>()
                    .join(", ")
            ),
            users,
        });
    }
    let p = &mut doc.profiles[pi];
    p.modes.remove(mi);
    let mut c = changed(Change::default(), id);
    p.rules
        .retain(|e| !e.id().is_some_and(|rid| alone.iter().any(|a| a == rid)));
    let now = crate::time::now_rfc3339();
    for e in &mut p.rules {
        if let Some(r) = e.rule_mut()
            && let Some(m) = &mut r.modes
            && m.iter().any(|x| x == id)
        {
            m.retain(|x| x != id);
            r.updated_at = Some(now.clone());
        }
    }
    c.rules.extend(alone.iter().cloned());
    c.rules.extend(shared.iter().cloned());
    if !alone.is_empty() {
        c.notes
            .push(format!("deleted {} rule(s) limited to `{id}`", alone.len()));
    }
    if !shared.is_empty() {
        c.notes.push(format!(
            "took `{id}` out of the modes of {} rule(s)",
            shared.len()
        ));
    }
    Ok(c)
}

/// Refuses a change that introduces errors: `mode_invalid` for problems of modes (a parent cycle,
/// an unknown parent, a default mode with a parent or an exit), the rule codes of
/// [`crate::edit::review`] for problems of rules.
pub fn review(before: &Report, after: &Report) -> Result<(), EditError> {
    let new = crate::edit::introduced(before, after);
    let Some(first) = new.first() else {
        return Ok(());
    };
    if first.rule.is_some() {
        return crate::edit::review(before, after);
    }
    Err(EditError::Invalid {
        code: "mode_invalid",
        message: format!("{}; nothing was written", first.message),
        problems: new,
    })
}

/// The problems of profile `profile` that concern modes (no rule), for `mode list`/`show`.
pub fn mode_problems<'a>(r: &'a Report, profile: &str) -> Vec<&'a Problem> {
    r.all()
        .filter(|p| {
            p.rule.is_none() && p.profile.as_deref() == Some(profile) && p.at.contains(".modes")
        })
        .collect()
}

/// One row of `mode list` and of the editor's Modes page.
pub fn row(p: &Profile, set: &ModeSet, i: usize, active: Option<&str>, r: &Report) -> Value {
    let m = &p.modes[i];
    let (alone, shared) = scoped(p, &m.id);
    let def = set.get(u8::try_from(i).unwrap_or(u8::MAX));
    let at = |pi: &Problem| pi.at.contains(&format!(".modes[{i}]"));
    json!({
        "id": m.id,
        "title": m.title,
        "icon": m.icon,
        "color": m.color,
        "parent": m.parent,
        "policy": def.map_or("exclusive", modes::ModeDef::policy),
        "chain": def.map_or_else(Vec::new, |_| set
            .chain(u8::try_from(i).unwrap_or(u8::MAX))
            .iter()
            .map(|c| set.id(*c).to_string())
            .collect::<Vec<_>>()),
        "one_shot": m.one_shot == Some(true),
        "timeout_ms": m.timeout_ms,
        "default": p.default_mode == m.id,
        "active": active.map(|a| a == m.id),
        "rules": alone.len() + shared.len(),
        "switched_by": switching(p, &m.id),
        "layers": p.modes.iter().filter(|x| x.parent.as_deref() == Some(m.id.as_str())).map(|x| x.id.clone()).collect::<Vec<_>>(),
        "problems": mode_problems(r, &p.id).into_iter().filter(|x| at(x)).map(Problem::to_json).collect::<Vec<_>>(),
    })
}

/// The rules limited to mode `id` (alone or with other modes), for `mode show`.
pub fn rules_of(p: &Profile, id: &str) -> Vec<String> {
    let (mut alone, shared) = scoped(p, id);
    alone.extend(shared);
    alone
}

/// The "Modes" page form (GUI_JOURNEYS J6, Step 74), in display order. Every field is reachable
/// and editable from the keyboard alone; the color is never the only cue (the title is always
/// shown with it).
pub const MODE_FIELDS: &[Field] = &[
    Field {
        path: "title",
        input: "text",
        required: true,
        default: None,
        label: "Name",
        help: "What the header, the tray and the rule list show (1–80 characters). F2 renames it.",
    },
    Field {
        path: "icon",
        input: "text",
        required: false,
        default: None,
        label: "Badge",
        help: "Optional: one to eight characters shown before the name (N, 가, ⌨). Never instead of the name.",
    },
    Field {
        path: "color",
        input: "color",
        required: false,
        default: None,
        label: "Color",
        help: "Optional chip next to the name, like #B7A6F5. The name is always shown with it.",
    },
    Field {
        path: "parent",
        input: "choice",
        required: false,
        default: None,
        label: "Layer over",
        help: "None: only this mode's rules and the rules for every mode apply (exclusive). A mode: keys this mode does not change keep that mode's rules (a layer).",
    },
    Field {
        path: "one_shot",
        input: "toggle",
        required: false,
        default: Some("false"),
        label: "Only for the next key",
        help: "After switching to it, the mode ends after the next key that is not a modifier, and the mode before returns. Useful instead of holding a key.",
    },
    Field {
        path: "timeout_ms",
        input: "number",
        required: false,
        default: None,
        label: "Return to the default mode after (ms)",
        help: "Optional, 1000–3600000: with no key pressed for this long, the default mode returns, so a mode is never left on by accident.",
    },
];

/// The "Switch mode" action form (GUI_JOURNEYS J4, Step 74). Any permitted trigger works: a key,
/// a chord, a sequence, a lock key, a mouse button, or a bare modifier on release.
pub const SWITCH_FIELDS: &[Field] = &[
    Field {
        path: "steps[0].op",
        input: "segmented",
        required: true,
        default: Some("set"),
        label: "Switch",
        help: "Switch to a mode, toggle it, cycle through modes, hold for a temporary layer, go back to the previous mode, or to the default mode.",
    },
    Field {
        path: "steps[0].mode",
        input: "choice",
        required: false,
        default: None,
        label: "Mode",
        help: "For switch to, toggle and hold. + New mode creates one here.",
    },
    Field {
        path: "steps[0].modes",
        input: "list",
        required: false,
        default: None,
        label: "Modes, in order",
        help: "For cycle: two to 32 modes; each press goes to the next one.",
    },
];

/// The editor's draft of one mode (the Modes page, Step 74 draws it). Nothing is saved by itself:
/// Save writes through the rule store only when the user presses it and the draft is valid.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ModeDraft {
    /// Empty for a new mode (an ID is made from the title on save).
    pub id: String,
    pub title: String,
    pub icon: String,
    pub color: String,
    pub parent: String,
    pub one_shot: bool,
    /// Empty: no timeout.
    pub timeout_ms: String,
    /// The draft as last saved (`None` for a new mode).
    pub saved: Option<Box<ModeDraft>>,
}

impl ModeDraft {
    /// The draft of an existing mode.
    pub fn of(m: &Mode) -> ModeDraft {
        let mut d = ModeDraft {
            id: m.id.clone(),
            title: m.title.clone(),
            icon: m.icon.clone().unwrap_or_default(),
            color: m.color.clone().unwrap_or_default(),
            parent: m.parent.clone().unwrap_or_default(),
            one_shot: m.one_shot == Some(true),
            timeout_ms: m.timeout_ms.map(|t| t.to_string()).unwrap_or_default(),
            saved: None,
        };
        d.saved = Some(Box::new(d.clone()));
        d
    }

    fn fields(&self) -> Result<ModeFields, Vec<FieldProblem>> {
        let opt = |s: &str| (!s.trim().is_empty()).then(|| s.trim().to_string());
        let timeout = match opt(&self.timeout_ms) {
            None => None,
            Some(t) => match t.parse::<u32>() {
                Ok(n) => Some(n),
                Err(_) => {
                    return Err(vec![FieldProblem {
                        path: "timeout_ms",
                        message: "a whole number of milliseconds, 1000–3600000".into(),
                    }]);
                }
            },
        };
        Ok(ModeFields {
            title: Some(self.title.trim().to_string()),
            icon: Some(opt(&self.icon)),
            color: Some(opt(&self.color)),
            parent: Some(opt(&self.parent)),
            one_shot: Some(self.one_shot),
            timeout_ms: Some(timeout),
            make_default: false,
        })
    }

    /// The field problems against `profile` (the same rules as the schema and validation).
    pub fn problems(&self, profile: &Profile) -> Vec<FieldProblem> {
        let f = match self.fields() {
            Ok(f) => f,
            Err(p) => return p,
        };
        let mut out = Vec::new();
        let path = |m: &str| -> &'static str {
            match m {
                m if m.contains("title") => "title",
                m if m.contains("icon") => "icon",
                m if m.contains("color") => "color",
                m if m.contains("timeout") => "timeout_ms",
                _ => "parent",
            }
        };
        if let Err(EditError::Usage(m)) = check_fields(&f) {
            out.push(FieldProblem {
                path: path(&m),
                message: m,
            });
        }
        // Try the change on a copy and keep only what it would introduce.
        let mut doc = Document::empty();
        doc.profiles = vec![profile.clone()];
        doc.active_profile = profile.id.clone();
        let before = crate::validate::check(&doc.to_value(), &crate::validate::Context::default());
        let r = if self.id.is_empty() {
            add(&mut doc, None, None, &f)
        } else {
            edit(&mut doc, None, &self.id, &f)
        };
        if let Err(e) = r {
            out.push(FieldProblem {
                path: "title",
                message: e.message(),
            });
            return out;
        }
        let after = crate::validate::check(&doc.to_value(), &crate::validate::Context::default());
        for p in crate::edit::introduced(&before, &after) {
            if p.severity == Severity::Error && p.rule.is_none() {
                out.push(FieldProblem {
                    path: if p.code == "mode_invalid" {
                        "one_shot"
                    } else {
                        "parent"
                    },
                    message: p.message,
                });
            }
        }
        out
    }

    fn dirty(&self) -> bool {
        self.saved.as_ref().is_none_or(|s| {
            let mut a = (**s).clone();
            a.saved = None;
            let mut b = self.clone();
            b.saved = None;
            a != b
        })
    }

    /// Save is enabled for a changed, valid draft; otherwise the problems say why not.
    pub fn can_save(&self, profile: &Profile) -> Result<(), Vec<FieldProblem>> {
        let p = self.problems(profile);
        if !p.is_empty() {
            return Err(p);
        }
        if !self.dirty() {
            return Err(vec![FieldProblem {
                path: "",
                message: "nothing changed since the last save".into(),
            }]);
        }
        Ok(())
    }

    /// The one-line summary under the form: "Navigation: a layer over Base; ends after 5 s
    /// without a key".
    pub fn summary(&self, profile: &Profile) -> String {
        let title = |id: &str| {
            profile
                .modes
                .iter()
                .find(|m| m.id == id)
                .map_or_else(|| id.to_string(), |m| m.title.clone())
        };
        let mut s = format!(
            "{}: {}",
            if self.title.trim().is_empty() {
                "New mode"
            } else {
                self.title.trim()
            },
            if self.parent.trim().is_empty() {
                "only its own rules and the rules for every mode apply".to_string()
            } else {
                format!(
                    "a layer over {}: keys it does not change keep their rules there",
                    title(self.parent.trim())
                )
            }
        );
        if self.one_shot {
            s.push_str("; ends after the next key");
        }
        if let Ok(t) = self.timeout_ms.trim().parse::<u32>() {
            s.push_str(&format!(
                "; {} returns after {} s without a key",
                title(&profile.default_mode),
                t.div_ceil(1000)
            ));
        }
        s
    }
}

/// The default mode-indicator setting when `settings.mode_indicator` is absent.
pub const INDICATOR_DEFAULT: ModeIndicator = ModeIndicator::Auto;

#[cfg(test)]
mod tests;
