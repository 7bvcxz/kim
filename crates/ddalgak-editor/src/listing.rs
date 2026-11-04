//! The rule list (docs/DDALGAK_GUI_JOURNEYS.md §2 and J7, `DDALGAK-FR-051`): one row per rule
//! with its sentence, state and badges, search, filters and grouping.
//!
//! Rows show only summaries of personal content: a snippet's counts, a program's file name, a
//! website's host. Search matches titles, keys and effect names, and a snippet's text only when
//! the user ticks "Also search snippet text".

use crate::form::{Effect, RuleForm};
use ddalgak::model::{Document, Rule};
use ddalgak::rules::rule_states;
use ddalgak::store;
use ddalgak::validate::{self, Problem, Severity};
use std::collections::BTreeMap;

/// What a badge says (always in words; the color only repeats it).
#[derive(Clone, Copy, PartialEq, Eq, Debug, PartialOrd, Ord)]
pub enum BadgeKind {
    /// Another rule or one of Ddalgak's own shortcuts takes the same trigger.
    Conflict,
    /// An error that keeps the rule from running.
    Problem,
    Warning,
    /// Disabled and without the consent it needs (an imported rule, a rule saved turned off).
    Review,
    /// Enabled but not running in this version (an `*_unsupported` info, a disabled mode).
    Inactive,
}

impl BadgeKind {
    pub fn key(self) -> &'static str {
        match self {
            BadgeKind::Conflict => "badge.conflict",
            BadgeKind::Problem => "badge.problem",
            BadgeKind::Warning => "badge.warning",
            BadgeKind::Review => "badge.review",
            BadgeKind::Inactive => "badge.inactive",
        }
    }

    pub fn glyph(self) -> &'static str {
        match self {
            BadgeKind::Conflict | BadgeKind::Problem => "✕",
            BadgeKind::Warning | BadgeKind::Inactive => "⚠",
            BadgeKind::Review => "?",
        }
    }
}

/// One badge with the code and message behind it (shown on hover and in the detail view).
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct Badge {
    pub kind: BadgeKind,
    pub code: String,
    pub message: String,
}

/// One row of the list.
#[derive(Clone, PartialEq, Debug)]
pub struct Row {
    pub profile: String,
    pub profile_title: String,
    /// The rule's index in its profile's list (the list order is the last precedence step).
    pub position: usize,
    pub id: String,
    pub title: String,
    pub description: Option<String>,
    pub trigger: String,
    pub effect: Option<Effect>,
    pub summary: String,
    pub modes: Vec<String>,
    pub mode_titles: Vec<String>,
    pub apps: Vec<String>,
    pub enabled: bool,
    /// `active`, `disabled`, `inactive`, `other_mode`, `other_profile` (`rules::rule_states`).
    pub state: &'static str,
    pub code: Option<&'static str>,
    pub reason: Option<String>,
    pub badges: Vec<Badge>,
    pub needs_review: bool,
    /// Whether the form can edit it (one step of a known kind, a rule that fits the types).
    pub editable: bool,
    /// Lower-case words search matches: title, keys, effect name, mode titles, apps.
    search: String,
    /// A snippet's text, lower-case (searched only on request).
    snippet: Option<String>,
}

impl Row {
    pub fn has_problems(&self) -> bool {
        self.badges
            .iter()
            .any(|b| matches!(b.kind, BadgeKind::Conflict | BadgeKind::Problem))
    }
}

fn badges(problems: &[&Problem], needs_review: bool) -> Vec<Badge> {
    let mut out: Vec<Badge> = problems
        .iter()
        .filter(|p| p.severity != Severity::Info || validate::INACTIVE_INFOS.contains(&p.code))
        .map(|p| Badge {
            kind: match (p.severity, p.code) {
                (
                    Severity::Error,
                    "duplicate_trigger" | "duplicate_binding" | "output_is_trigger"
                    | "duplicate_rule_id",
                ) => BadgeKind::Conflict,
                (Severity::Error, _) => BadgeKind::Problem,
                (Severity::Warning, _) => BadgeKind::Warning,
                _ => BadgeKind::Inactive,
            },
            code: p.code.to_string(),
            message: p.message.clone(),
        })
        .collect();
    if needs_review {
        out.push(Badge {
            kind: BadgeKind::Review,
            code: "consent_required".into(),
            message: "turned off until you approve what it may do".into(),
        });
    }
    out.sort_by(|a, b| a.kind.cmp(&b.kind).then_with(|| a.code.cmp(&b.code)));
    out.dedup_by(|a, b| a.kind == b.kind && a.code == b.code);
    out
}

/// The rows of a read rules file, in profile and list order.
pub fn rows(l: &store::Loaded) -> Vec<Row> {
    let Some(doc) = &l.doc else {
        return Vec::new();
    };
    let states = rule_states(l);
    let mut out = Vec::with_capacity(states.len());
    for (pi, p) in doc.profiles.iter().enumerate() {
        for (ri, e) in p.rules.iter().enumerate() {
            let st = states
                .iter()
                .find(|s| s.profile == p.id && s.position == ri);
            let problems: Vec<&Problem> = l
                .report
                .problems
                .iter()
                .filter(|x| x.pos == Some((pi, ri)))
                .collect();
            let rule = e.rule();
            let needs_review =
                rule.is_some_and(|r| !r.enabled && !validate::missing_consent(r).is_empty());
            let (summary, effect, editable, snippet) = match rule {
                Some(r) => {
                    let form = RuleForm::edit(&p.id, r);
                    (
                        form.summary(Some(p)),
                        form.fields.effect,
                        form.unsupported.is_none(),
                        match r.steps.as_slice() {
                            [ddalgak::model::Action::TypeText(t)] => Some(t.text.to_lowercase()),
                            _ => None,
                        },
                    )
                }
                None => (
                    "does not fit this version's rule format; kept as written".into(),
                    None,
                    false,
                    None,
                ),
            };
            let modes = rule.and_then(|r| r.modes.clone()).unwrap_or_default();
            let mode_titles: Vec<String> = modes
                .iter()
                .map(|m| {
                    p.modes
                        .iter()
                        .find(|x| &x.id == m)
                        .map_or_else(|| m.clone(), |x| x.title.clone())
                })
                .collect();
            let apps: Vec<String> = rule
                .and_then(|r| r.when.as_ref())
                .and_then(|w| w.apps.as_ref())
                .map(|a| {
                    a.iter()
                        .filter_map(|m| m.exe.clone().or_else(|| m.path.clone()))
                        .collect()
                })
                .unwrap_or_default();
            let title = e.title().unwrap_or_default().to_string();
            let trigger = rule.map(|r| r.trigger.display()).unwrap_or_default();
            let search = format!(
                "{} {} {} {} {} {}",
                title,
                trigger,
                effect.map_or("", Effect::key).replace('_', " "),
                summary,
                mode_titles.join(" "),
                apps.join(" ")
            )
            .to_lowercase();
            out.push(Row {
                profile: p.id.clone(),
                profile_title: p.title.clone(),
                position: ri,
                id: e.id().unwrap_or_default().to_string(),
                description: rule.and_then(|r| r.description.clone()),
                title,
                trigger,
                effect,
                summary,
                modes,
                mode_titles,
                apps,
                enabled: e.enabled().unwrap_or(false),
                state: st.map_or("inactive", |s| s.state),
                code: st.and_then(|s| s.code),
                reason: st.and_then(|s| s.reason.clone()),
                badges: badges(&problems, needs_review),
                needs_review,
                editable,
                search,
                snippet,
            });
        }
    }
    out
}

/// Which rules the list shows.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub enum StateFilter {
    #[default]
    All,
    Enabled,
    Disabled,
    Review,
    Problems,
}

impl StateFilter {
    pub const ALL: [StateFilter; 5] = [
        StateFilter::All,
        StateFilter::Enabled,
        StateFilter::Disabled,
        StateFilter::Review,
        StateFilter::Problems,
    ];

    pub fn key(self) -> &'static str {
        match self {
            StateFilter::All => "filter.all",
            StateFilter::Enabled => "filter.enabled",
            StateFilter::Disabled => "filter.disabled",
            StateFilter::Review => "filter.review",
            StateFilter::Problems => "filter.problems",
        }
    }
}

/// The search box and the filters (J7: profile, mode, app, enabled, needs review, problems).
#[derive(Clone, PartialEq, Eq, Debug, Default)]
pub struct Filter {
    pub query: String,
    pub snippet_text: bool,
    pub state: StateFilter,
    pub profile: Option<String>,
    pub mode: Option<String>,
    pub app: Option<String>,
}

impl Filter {
    pub fn is_default(&self) -> bool {
        *self == Filter::default()
    }

    /// Whether `row` passes the search and every filter. A mode filter shows the rules that
    /// work in that mode: its own and those for every mode.
    pub fn matches(&self, row: &Row) -> bool {
        let q = self.query.trim().to_lowercase();
        if !q.is_empty() {
            let words = q.split_whitespace().all(|w| {
                row.search.contains(w)
                    || (self.snippet_text && row.snippet.as_ref().is_some_and(|s| s.contains(w)))
            });
            if !words {
                return false;
            }
        }
        let state = match self.state {
            StateFilter::All => true,
            StateFilter::Enabled => row.enabled,
            StateFilter::Disabled => !row.enabled,
            StateFilter::Review => row.needs_review,
            StateFilter::Problems => {
                !row.badges.is_empty() && row.badges.iter().any(|b| b.kind != BadgeKind::Review)
            }
        };
        state
            && self.profile.as_ref().is_none_or(|p| &row.profile == p)
            && self
                .mode
                .as_ref()
                .is_none_or(|m| row.modes.is_empty() || row.modes.contains(m))
            && self
                .app
                .as_ref()
                .is_none_or(|a| row.apps.iter().any(|x| x.eq_ignore_ascii_case(a)))
    }
}

/// How the list is grouped (J7 and the issue: profile, mode, app).
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub enum Grouping {
    #[default]
    None,
    Profile,
    Mode,
    App,
}

impl Grouping {
    pub const ALL: [Grouping; 4] = [
        Grouping::None,
        Grouping::Profile,
        Grouping::Mode,
        Grouping::App,
    ];

    pub fn key(self) -> &'static str {
        match self {
            Grouping::None => "group.none",
            Grouping::Profile => "group.profile",
            Grouping::Mode => "group.mode",
            Grouping::App => "group.app",
        }
    }
}

/// One group: its heading and the indices (into the rows) it shows, in list order. A rule in
/// several modes or apps appears under each.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct Group {
    pub heading: String,
    pub rows: Vec<usize>,
}

/// Groups the visible rows. `every_mode` / `every_app` head the rules without modes or apps.
pub fn group(
    rows: &[Row],
    visible: &[usize],
    g: Grouping,
    every_mode: &str,
    every_app: &str,
) -> Vec<Group> {
    if g == Grouping::None {
        return vec![Group {
            heading: String::new(),
            rows: visible.to_vec(),
        }];
    }
    let mut map: BTreeMap<(u8, String), Vec<usize>> = BTreeMap::new();
    for &i in visible {
        let r = &rows[i];
        let keys: Vec<(u8, String)> = match g {
            Grouping::Profile => vec![(1, r.profile_title.clone())],
            Grouping::Mode if r.mode_titles.is_empty() => vec![(0, every_mode.to_string())],
            Grouping::Mode => r.mode_titles.iter().map(|t| (1, t.clone())).collect(),
            Grouping::App if r.apps.is_empty() => vec![(0, every_app.to_string())],
            Grouping::App => r.apps.iter().map(|a| (1, a.clone())).collect(),
            Grouping::None => unreachable!(),
        };
        for k in keys {
            map.entry(k).or_default().push(i);
        }
    }
    map.into_iter()
        .map(|((_, heading), rows)| Group { heading, rows })
        .collect()
}

/// The modes and apps the filters offer: every mode of every profile and every app any rule
/// names, sorted.
pub fn filter_choices(doc: &Document) -> (Vec<(String, String)>, Vec<String>) {
    let mut modes = BTreeMap::new();
    let mut apps = std::collections::BTreeSet::new();
    for p in &doc.profiles {
        for m in &p.modes {
            modes.entry(m.id.clone()).or_insert_with(|| m.title.clone());
        }
        for e in &p.rules {
            for a in e
                .rule()
                .and_then(|r: &Rule| r.when.as_ref())
                .and_then(|w| w.apps.as_ref())
                .into_iter()
                .flatten()
            {
                if let Some(x) = a.exe.clone().or_else(|| a.path.clone()) {
                    apps.insert(x);
                }
            }
        }
    }
    (modes.into_iter().collect(), apps.into_iter().collect())
}

#[cfg(test)]
mod tests {
    use super::*;
    use ddalgak::validate::Context;
    use serde_json::json;

    fn loaded(rules: serde_json::Value) -> store::Loaded {
        let doc = json!({
            "schema": "ddalgak.rules", "schema_version": 1, "hotkey_contract": 1, "revision": 4,
            "active_profile": "work",
            "profiles": [
                { "id": "work", "title": "Work", "default_mode": "base",
                  "modes": [ { "id": "base", "title": "Base" }, { "id": "nav", "title": "Navigation" } ],
                  "rules": rules },
                { "id": "game", "title": "Gaming", "default_mode": "base",
                  "modes": [ { "id": "base", "title": "Base" } ],
                  "rules": [ { "id": "g1", "title": "F9 pauses", "enabled": true,
                      "trigger": { "keys": "F9" }, "steps": [ { "type": "engine", "op": "pause" } ] } ] }
            ]
        });
        store::interpret(
            doc.to_string().as_bytes(),
            &Context::default(),
            store::MIGRATIONS,
        )
    }

    fn sample() -> store::Loaded {
        loaded(json!([
            { "id": "caps", "title": "CapsLock acts as Ctrl", "enabled": true, "description": "Ctrl where it belongs",
              "trigger": { "keys": "CapsLock" }, "steps": [ { "type": "remap", "to": "LCtrl", "tap": "Escape" } ],
              "consent": { "input-injection": { "granted_at": "2026-11-04T09:00:00Z", "via": "gui" } } },
            { "id": "thanks", "title": "Thanks", "enabled": true,
              "trigger": { "keys": "Ctrl+Shift+F2" }, "steps": [ { "type": "type_text", "text": "감사합니다 secret" } ],
              "consent": { "input-injection": { "granted_at": "2026-11-04T09:00:00Z", "via": "gui" } } },
            { "id": "dup", "title": "Also Ctrl+Shift+F2", "enabled": true,
              "trigger": { "keys": "Ctrl+Shift+F2" }, "steps": [ { "type": "engine", "op": "pause" } ] },
            { "id": "nav-left", "title": "H moves left", "enabled": true, "modes": ["nav"],
              "trigger": { "keys": "H" }, "steps": [ { "type": "remap", "to": "Left" } ],
              "consent": { "input-injection": { "granted_at": "2026-11-04T09:00:00Z", "via": "gui" } } },
            { "id": "imported", "title": "Open tool", "enabled": false,
              "trigger": { "keys": "Ctrl+Alt+Shift+F8" }, "steps": [ { "type": "open_url", "url": "https://example.com/" } ] },
            { "id": "code-only", "title": "In Code", "enabled": true, "when": { "apps": [ { "exe": "Code.exe" } ] },
              "trigger": { "keys": "Ctrl+Alt+Shift+F9" }, "steps": [ { "type": "engine", "op": "pause" } ] }
        ]))
    }

    #[test]
    fn rows_carry_sentences_states_and_badges_with_words() {
        let l = sample();
        let rows = rows(&l);
        assert_eq!(rows.len(), 7);
        let caps = &rows[0];
        assert_eq!(
            caps.summary,
            "CapsLock acts as Left Ctrl; a quick tap is Escape"
        );
        assert_eq!(caps.state, "active");
        assert_eq!(caps.description.as_deref(), Some("Ctrl where it belongs"));
        assert!(caps.badges.is_empty());
        // The snippet's text never appears in the row.
        let thanks = &rows[1];
        assert_eq!(thanks.summary, "Types 12 characters on 1 line");
        assert!(!thanks.summary.contains("감사"));
        // The later of two same triggers has a conflict badge and is inactive.
        let dup = &rows[2];
        assert!(
            dup.badges
                .iter()
                .any(|b| b.kind == BadgeKind::Conflict && b.code == "duplicate_trigger")
        );
        assert_eq!(dup.state, "inactive");
        assert!(dup.has_problems());
        assert_eq!(rows[3].state, "other_mode");
        assert_eq!(rows[3].mode_titles, ["Navigation"]);
        let imp = &rows[4];
        assert!(imp.needs_review && !imp.enabled);
        assert!(imp.badges.iter().any(|b| b.kind == BadgeKind::Review));
        let code = &rows[5];
        assert!(
            code.badges
                .iter()
                .any(|b| b.code == "condition_unsupported" && b.kind == BadgeKind::Inactive)
        );
        assert_eq!(rows[6].state, "other_profile");
    }

    #[test]
    fn search_filters_and_groups() {
        let rows = rows(&sample());
        let all: Vec<usize> = (0..rows.len()).collect();
        let show = |f: &Filter| -> Vec<String> {
            all.iter()
                .filter(|i| f.matches(&rows[**i]))
                .map(|i| rows[*i].id.clone())
                .collect()
        };
        // Titles, keys and effect names.
        assert_eq!(
            show(&Filter {
                query: "capslock".into(),
                ..Filter::default()
            }),
            ["caps"]
        );
        assert_eq!(
            show(&Filter {
                query: "ctrl+shift+f2".into(),
                ..Filter::default()
            }),
            ["thanks", "dup"]
        );
        assert_eq!(
            show(&Filter {
                query: "open url".into(),
                ..Filter::default()
            }),
            ["imported"]
        );
        // Snippet text only on request.
        assert!(
            show(&Filter {
                query: "secret".into(),
                ..Filter::default()
            })
            .is_empty()
        );
        assert_eq!(
            show(&Filter {
                query: "secret".into(),
                snippet_text: true,
                ..Filter::default()
            }),
            ["thanks"]
        );
        assert_eq!(
            show(&Filter {
                state: StateFilter::Review,
                ..Filter::default()
            }),
            ["imported"]
        );
        assert_eq!(
            show(&Filter {
                state: StateFilter::Disabled,
                ..Filter::default()
            }),
            ["imported"]
        );
        assert_eq!(
            show(&Filter {
                state: StateFilter::Problems,
                ..Filter::default()
            }),
            ["dup", "code-only"]
        );
        assert_eq!(
            show(&Filter {
                profile: Some("game".into()),
                ..Filter::default()
            }),
            ["g1"]
        );
        // A mode filter shows the mode's rules and the rules for every mode.
        let nav = show(&Filter {
            mode: Some("nav".into()),
            profile: Some("work".into()),
            ..Filter::default()
        });
        assert!(nav.contains(&"nav-left".to_string()) && nav.contains(&"caps".to_string()));
        assert_eq!(
            show(&Filter {
                app: Some("code.exe".into()),
                ..Filter::default()
            }),
            ["code-only"]
        );

        let by_profile = group(&rows, &all, Grouping::Profile, "Every mode", "Every app");
        assert_eq!(
            by_profile
                .iter()
                .map(|g| g.heading.as_str())
                .collect::<Vec<_>>(),
            ["Gaming", "Work"]
        );
        let by_mode = group(&rows, &all, Grouping::Mode, "Every mode", "Every app");
        assert_eq!(by_mode[0].heading, "Every mode");
        assert_eq!(by_mode[1].heading, "Navigation");
        assert_eq!(by_mode[1].rows, [3]);
        let by_app = group(&rows, &all, Grouping::App, "Every mode", "Every app");
        assert_eq!(by_app.last().unwrap().heading, "Code.exe");
        let (modes, apps) = filter_choices(l_doc(&sample()));
        assert_eq!(modes.len(), 2);
        assert_eq!(apps, ["Code.exe"]);
    }

    fn l_doc(l: &store::Loaded) -> &Document {
        // The sample is leaked for the test's lifetime only.
        Box::leak(Box::new(l.doc.clone().unwrap()))
    }

    /// Many rules and modes: building, filtering and grouping 1000 rules (a profile's limit) over
    /// 32 modes stays fast
    /// enough for every frame of a search (the list is rebuilt only when the file changes).
    #[test]
    fn many_rules_and_modes_stay_fast() {
        let modes: Vec<serde_json::Value> = (0..32)
            .map(|i| json!({ "id": format!("m{i}"), "title": format!("Mode {i}") }))
            .collect();
        let rules: Vec<serde_json::Value> = (0..1000)
            .map(|i| {
                json!({ "id": format!("r{i}"), "title": format!("Rule {i}"), "enabled": i % 3 != 0,
                "modes": [format!("m{}", i % 32)],
                "trigger": { "keys": format!("Ctrl+Alt+Shift+F{}", 13 + i % 8) },
                "steps": [ { "type": "engine", "op": "pause" } ] })
            })
            .collect();
        let doc = json!({ "schema": "ddalgak.rules", "schema_version": 1, "hotkey_contract": 1,
            "active_profile": "p", "profiles": [ { "id": "p", "title": "P", "default_mode": "m0",
            "modes": modes, "rules": rules } ] });
        let t = std::time::Instant::now();
        let l = store::interpret(
            doc.to_string().as_bytes(),
            &Context::default(),
            store::MIGRATIONS,
        );
        let rows = rows(&l);
        let built = t.elapsed();
        assert_eq!(rows.len(), 1000, "{:?}", l.state);
        let t = std::time::Instant::now();
        let all: Vec<usize> = (0..rows.len()).collect();
        let f = Filter {
            query: "rule 19".into(),
            mode: Some("m7".into()),
            ..Filter::default()
        };
        let visible: Vec<usize> = all
            .iter()
            .copied()
            .filter(|i| f.matches(&rows[*i]))
            .collect();
        let groups = group(&rows, &visible, Grouping::Mode, "Every mode", "Every app");
        let filtered = t.elapsed();
        assert!(!visible.is_empty() && !groups.is_empty());
        // Debug build budgets (release is several times faster).
        assert!(built.as_secs_f64() < 20.0, "building took {built:?}");
        assert!(
            filtered.as_millis() < 50,
            "a search frame took {filtered:?}"
        );
    }
}
