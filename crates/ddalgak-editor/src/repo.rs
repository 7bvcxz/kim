//! The editor's writes through the rule store (docs/DDALGAK_RULE_MODEL.md §9, §10; ADR 0028,
//! ADR 0049; `DDALGAK-FR-005…007`).
//!
//! Every change is one transaction, exactly like a `ddalgak rule …` command: take the store's
//! lock, **re-read** `rules.json`, apply one operation to that fresh document, run the full
//! validation before and after and refuse a change that introduces an error
//! ([`ddalgak::edit::review`]), then save the next revision atomically (backup, history snapshot
//! with origin `gui`). A change made by the CLI or another program between the editor's last read
//! and the save is therefore never overwritten: list operations apply to the current file, and a
//! form save refuses when the rule itself changed since the form opened (`rule_conflict`).
//!
//! **Undo** restores the document as it was before one of the editor's own changes, as a new
//! revision (like `rules restore`), and only while nothing else changed the file since
//! (`rule_conflict` otherwise); the history keeps every revision anyway.

use crate::form;
use ddalgak::edit::{self, Change, EditError, NewRule, Position};
use ddalgak::model::{Document, Permission, Rule, RuleEntry};
use ddalgak::paths::Paths;
use ddalgak::shortcuts::Shortcuts;
use ddalgak::store::{self, Files, Origin, StoreError};
use ddalgak::validate::{self, Context, Problem, Severity};
use serde_json::Value;

/// The rules store the editor works on.
#[derive(Clone, Debug)]
pub struct Repo {
    pub paths: Paths,
    pub files: Files,
}

/// A saved change.
#[derive(Clone, Debug)]
pub struct Committed {
    /// The revision now saved (unchanged when the operation changed nothing).
    pub revision: u64,
    pub changed: bool,
    pub change: Change,
    /// The document before the change (for undo).
    pub before: Document,
    /// Warnings on the rules the change touched (they never block).
    pub warnings: Vec<Problem>,
}

/// Why nothing was written.
#[derive(Debug)]
pub enum CommitError {
    Store(StoreError),
    Edit(EditError),
}

impl CommitError {
    /// The CLI error code (docs/CLI_CONTRACT.md §6).
    pub fn code(&self) -> &'static str {
        match self {
            CommitError::Store(e) => match e {
                StoreError::PermissionDenied(_) => "permission_denied",
                StoreError::Busy(_) => "busy",
                StoreError::Invalid(_) => "rules_invalid",
                StoreError::Newer(_) => "rules_from_newer_version",
                StoreError::Conflict { .. } => "rule_conflict",
                StoreError::NotFound(_) => "snapshot_not_found",
                StoreError::Io(_) => "io",
            },
            CommitError::Edit(e) => e.code(),
        }
    }

    pub fn message(&self) -> String {
        match self {
            CommitError::Store(e) => e.to_string(),
            CommitError::Edit(e) => e.message(),
        }
    }

    /// The problems behind a refused change (each with its code and message).
    pub fn problems(&self) -> Vec<Problem> {
        match self {
            CommitError::Edit(e) => e.problems().to_vec(),
            CommitError::Store(_) => Vec::new(),
        }
    }

    /// The permissions a `consent_required` refusal asks for.
    pub fn consent(&self) -> Option<(&str, &[Permission])> {
        match self {
            CommitError::Edit(EditError::ConsentRequired { rule, permissions }) => {
                Some((rule.as_str(), permissions.as_slice()))
            }
            _ => None,
        }
    }
}

impl From<StoreError> for CommitError {
    fn from(e: StoreError) -> Self {
        CommitError::Store(e)
    }
}

impl From<EditError> for CommitError {
    fn from(e: EditError) -> Self {
        CommitError::Edit(e)
    }
}

/// One of the editor's own changes that can be undone.
#[derive(Clone, Debug)]
pub struct UndoEntry {
    /// "Deleted 2 rules", "Edited CapsLock acts as Ctrl".
    pub label: String,
    pub before: Document,
    /// The revision the change saved; undo is possible only while the file is still at it.
    pub after: u64,
}

impl Repo {
    pub fn new(paths: Paths) -> Repo {
        Repo {
            files: Files::new(&paths),
            paths,
        }
    }

    /// What the checks know beyond the file: Ddalgak's own shortcuts and missing program files
    /// (the same context as the CLI).
    pub fn context(&self) -> Context {
        Context {
            shortcuts: Shortcuts::load(&self.paths, false)
                .list
                .iter()
                .map(|s| (s.action.clone(), s.keys.clone()))
                .collect(),
            check_targets: true,
        }
    }

    /// Reads and checks `rules.json` (reading never writes).
    pub fn read(&self) -> store::Loaded {
        isolated(|| store::read(&self.files, &self.context()))
    }

    /// One transaction: lock, re-read, `op`, review, save (origin `gui`, the history names
    /// `command`).
    pub fn commit(
        &self,
        command: &str,
        op: impl FnOnce(&mut Document) -> Result<Change, EditError> + Send,
    ) -> Result<Committed, CommitError> {
        self.transact(command, op, edit::review)
    }

    /// A mode change (Step 72): introduced mode problems are refused with `mode_invalid`.
    pub fn commit_modes(
        &self,
        command: &str,
        op: impl FnOnce(&mut Document) -> Result<Change, EditError> + Send,
    ) -> Result<Committed, CommitError> {
        self.transact(command, op, ddalgak::mode_edit::review)
    }

    /// Step 75: adds the planned profiles (never active, every rule off, no consent); problems of
    /// the imported rules themselves do not refuse it ([`ddalgak::profile::review_import`]).
    pub fn commit_profile_import(
        &self,
        plan: &ddalgak::profile::ImportPlan,
    ) -> Result<Committed, CommitError> {
        let ids = plan.rule_ids();
        let p = plan.clone();
        self.transact(
            "profile.import",
            move |d| ddalgak::profile::apply_import(d, &p),
            move |before, after| ddalgak::profile::review_import(before, after, &ids),
        )
    }

    /// Step 75: a save point of the saved rules for `profile` (`ddalgak profile save`).
    pub fn save_point(
        &self,
        profile: &str,
        label: Option<&str>,
    ) -> Result<Option<(std::path::PathBuf, u64)>, CommitError> {
        let lock = store::lock(&self.files, store::lock_timeout())?;
        Ok(store::save_point(&self.files, &lock, profile, label)?)
    }

    fn transact(
        &self,
        command: &str,
        op: impl FnOnce(&mut Document) -> Result<Change, EditError> + Send,
        review: impl Fn(&validate::Report, &validate::Report) -> Result<(), EditError> + Send + Sync,
    ) -> Result<Committed, CommitError> {
        isolated(|| self.transact_here(command, op, &review))
    }

    fn transact_here(
        &self,
        command: &str,
        op: impl FnOnce(&mut Document) -> Result<Change, EditError>,
        review: &dyn Fn(&validate::Report, &validate::Report) -> Result<(), EditError>,
    ) -> Result<Committed, CommitError> {
        let lock = store::lock(&self.files, store::lock_timeout())?;
        let ctx = self.context();
        let current = store::read(&self.files, &ctx);
        match &current.state {
            store::FileState::Invalid(why) => return Err(StoreError::Invalid(why.clone()).into()),
            store::FileState::NewerVersion(v) => return Err(StoreError::Newer(*v).into()),
            _ => {}
        }
        let mut doc = current.doc.clone().expect("a usable file has a document");
        let before_doc = doc.clone();
        let base = current.revision();
        let before = validate::check(&doc.to_value(), &ctx);
        let change = op(&mut doc)?;
        let after = validate::check(&doc.to_value(), &ctx);
        review(&before, &after)?;
        let warnings = after
            .all()
            .filter(|p| {
                p.severity == Severity::Warning
                    && p.rule.as_ref().is_some_and(|r| change.rules.contains(r))
            })
            .cloned()
            .collect();
        if !change.changed {
            return Ok(Committed {
                revision: base,
                changed: false,
                change,
                before: before_doc,
                warnings,
            });
        }
        let saved = store::save(
            &self.files,
            &lock,
            &mut doc,
            base,
            &Origin {
                origin: "gui",
                command: command.to_string(),
            },
        )?;
        Ok(Committed {
            revision: saved.revision,
            changed: true,
            change,
            before: before_doc,
            warnings,
        })
    }

    /// Undoes one of the editor's changes as a new revision, if nothing changed the file since.
    pub fn undo(&self, entry: &UndoEntry) -> Result<Committed, CommitError> {
        let label = entry.label.clone();
        let after = entry.after;
        let before = entry.before.clone();
        self.commit("editor.undo", move |doc| {
            let found = doc.revision();
            if found != after {
                return Err(EditError::Conflict {
                    message: format!(
                        "the rules changed since \"{label}\" (revision {after}, now {found}); undo would overwrite that change, so nothing was written (see the history instead)"
                    ),
                    problems: Vec::new(),
                });
            }
            let mut restored = before;
            restored.revision = doc.revision;
            *doc = restored;
            Ok(Change {
                changed: true,
                ..Change::default()
            })
        })
    }
}

/// Runs `f` on a short-lived thread and returns its result. The schema validator keeps its
/// compiled patterns and their search caches per thread; validating on a thread that ends frees
/// that memory again, so the editor's working set does not grow with the size of the rules file
/// it once read (ADR 0049, measured in DDALGAK_GUI_JOURNEYS §7).
pub fn isolated<T: Send>(f: impl FnOnce() -> T + Send) -> T {
    std::thread::scope(|s| {
        s.spawn(f)
            .join()
            .unwrap_or_else(|p| std::panic::resume_unwind(p))
    })
}

/// Adds a new rule at the end of `profile` (a fresh ID is chosen under the lock). Without the
/// consent it needs, an enabled rule is saved turned off (`consent_needed`).
pub fn add_rule(doc: &mut Document, profile: &str, mut rule: Rule) -> Result<Change, EditError> {
    rule.id = String::new();
    edit::add(
        doc,
        NewRule {
            id: None,
            profile: Some(profile.to_string()),
            position: Position::End,
            rule,
            allow: Vec::new(),
        },
    )
}

/// Replaces rule `id` with `rule`, if the rule on disk is still `base` (what the form opened);
/// otherwise `rule_conflict` and nothing changes.
pub fn replace_rule(
    doc: &mut Document,
    id: &str,
    mut rule: Rule,
    base: &Value,
) -> Result<Change, EditError> {
    let (pi, ri) = doc
        .find_rule(id)
        .ok_or_else(|| EditError::RuleNotFound(id.to_string()))?;
    let current = doc.profiles[pi].rules[ri].to_value();
    if &current != base {
        return Err(EditError::Conflict {
            message: format!(
                "rule `{id}` was changed outside the editor (by the CLI or another program) since you opened it; nothing was written. Load the newer rules and edit again."
            ),
            problems: Vec::new(),
        });
    }
    rule.id = id.to_string();
    let missing = validate::missing_consent(&rule);
    if rule.enabled && !missing.is_empty() {
        return Err(EditError::ConsentRequired {
            rule: id.to_string(),
            permissions: missing,
        });
    }
    let changed = rule.to_value() != current;
    if changed {
        rule.updated_at = Some(ddalgak::time::now_rfc3339());
    }
    doc.profiles[pi].rules[ri] = RuleEntry::Typed(Box::new(rule));
    Ok(Change {
        rules: vec![id.to_string()],
        changed,
        ..Change::default()
    })
}

/// Enables rules after recording the consent the user just gave in the editor's dialog
/// (`via: "gui"`); without `allow` a rule that needs consent is refused (`consent_required`).
pub fn enable_with_consent(
    doc: &mut Document,
    ids: &[String],
    on: bool,
    allow: &[Permission],
) -> Result<Change, EditError> {
    if on && !allow.is_empty() {
        for id in ids {
            if let Some((pi, ri)) = doc.find_rule(id)
                && let Some(r) = doc.profiles[pi].rules[ri].rule_mut()
            {
                form::grant_gui(r, allow);
            }
        }
    }
    edit::set_enabled(doc, ids, on, &[])
}

#[cfg(test)]
mod tests {
    use super::*;
    use ddalgak::model::{Action, EngineOp, EngineStep, Trigger};
    use serde_json::{Map, json};

    pub(crate) fn sandbox(tag: &str) -> Repo {
        let root = std::env::temp_dir().join(format!(
            "ddalgak-editor-repo-{tag}-{}-{:?}",
            std::process::id(),
            std::thread::current().id()
        ));
        let _ = std::fs::remove_dir_all(&root);
        Repo::new(Paths::under(root))
    }

    fn pause_rule(keys: &str) -> Rule {
        Rule::new(
            String::new(),
            format!("{keys} pauses"),
            Trigger::keys(keys),
            vec![Action::Engine(EngineStep {
                op: EngineOp::Pause,
                extra: Map::new(),
            })],
        )
    }

    #[test]
    fn a_commit_rereads_reviews_saves_with_origin_gui_and_undo_restores() {
        let repo = sandbox("commit");
        let c = repo
            .commit("rule.add", |d| add_rule(d, "default", pause_rule("F13")))
            .unwrap();
        assert!(c.changed);
        assert_eq!(c.revision, 1);
        let id = c.change.created.clone().unwrap();
        let l = repo.read();
        assert_eq!(l.revision(), 1);
        assert!(l.doc.as_ref().unwrap().find_rule(&id).is_some());
        // The history names the editor as the origin.
        let h = store::history(&repo.files);
        assert!(
            h.iter().any(|s| s.origin.as_deref() == Some("gui")),
            "{h:?}"
        );

        // A second rule with the same trigger would be a duplicate: refused, nothing written.
        let e = repo
            .commit("rule.add", |d| add_rule(d, "default", pause_rule("F13")))
            .unwrap_err();
        assert_eq!(e.code(), "rule_conflict");
        assert_eq!(repo.read().revision(), 1);

        // Undo of the add restores the empty profile as revision 2.
        let entry = UndoEntry {
            label: "Added".into(),
            before: c.before.clone(),
            after: c.revision,
        };
        let u = repo.undo(&entry).unwrap();
        assert_eq!(u.revision, 2);
        assert!(repo.read().doc.unwrap().find_rule(&id).is_none());
        // Undo again is refused: the file is no longer at the undone revision.
        assert_eq!(repo.undo(&entry).unwrap_err().code(), "rule_conflict");
        let _ = std::fs::remove_dir_all(&repo.paths.config);
    }

    #[test]
    fn a_form_save_never_overwrites_a_change_made_elsewhere() {
        let repo = sandbox("conflict");
        let c = repo
            .commit("rule.add", |d| add_rule(d, "default", pause_rule("F14")))
            .unwrap();
        let id = c.change.created.unwrap();
        let opened = repo.read().doc.unwrap();
        let (pi, ri) = opened.find_rule(&id).unwrap();
        let base = opened.profiles[pi].rules[ri].to_value();
        // The CLI renames it meanwhile (the same store code path).
        repo.commit("rule.edit", |d| {
            edit::edit(d, &id, &json!({ "title": "Renamed by the CLI" }), &[])
        })
        .unwrap();
        let mut mine = opened.profiles[pi].rules[ri].rule().unwrap().clone();
        mine.title = "Mine".into();
        let e = repo
            .commit("rule.edit", |d| replace_rule(d, &id, mine.clone(), &base))
            .unwrap_err();
        assert_eq!(e.code(), "rule_conflict");
        assert_eq!(
            repo.read().doc.unwrap().profiles[pi].rules[ri].title(),
            Some("Renamed by the CLI")
        );
        let _ = std::fs::remove_dir_all(&repo.paths.config);
    }
}
