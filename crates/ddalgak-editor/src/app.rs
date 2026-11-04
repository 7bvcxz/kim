//! The editor window (docs/DDALGAK_GUI_JOURNEYS.md §2, J1–J4, J7; ADR 0049).
//!
//! Header (engine state in a glyph *and* a word, the engine buttons, profile and mode, the
//! language switch), navigation rail (Rules, Modes), the rule list with search, filters, grouping,
//! drag ordering, multi-select and badges, the detail or four-step form panel, and the status
//! line with the truthful save state. Every control is reachable from the keyboard; `F6` cycles
//! the regions; destructive actions confirm with the safe choice focused.
//!
//! Changes go through [`crate::repo`] (one locked, re-read, reviewed store transaction each) and
//! are confirmed by the running app through [`crate::sync`]. The window never performs an effect
//! of a rule: **Test** is the simulate mode of the playground ([`crate::form::RuleForm::simulate`]).

use crate::capture::{self, Capture, Captured};
use crate::form::{self, Effect, RuleForm, Simulation, Stage};
use crate::i18n::Lang;
use crate::listing::{self, BadgeKind, Filter, Grouping, Row, StateFilter};
use crate::repo::{self, CommitError, Committed, Repo, UndoEntry};
use crate::shortcuts::{self, Shortcuts};
use crate::sync::{Engine, EngineView, Job, Link, SaveState, Wake};
use crate::theme::{Colors, Metrics};
use crate::transfer::{self, ImportPlan};
use ddalgak::edit::{self, Change, EditError, Position};
use ddalgak::lifecycle::Request;
use ddalgak::model::{EngineOp, ModeOp, Permission, Profile, TriggerOn};
use ddalgak::mouse::{self, Anchor, Grid, HoldChoice, Layout, MouseAction, Point, ScrollDir};
use ddalgak::paths::Paths;
use ddalgak::store;
use eframe::egui;
use std::collections::BTreeSet;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

mod view;

/// How the editor starts.
pub struct Options {
    pub paths: Paths,
    pub engine: Arc<dyn Engine>,
    pub lang: Lang,
    /// `--no-hotkeys` / `KIM_NO_HOTKEYS`: the rule list's shortcuts are off (navigation keys stay).
    pub no_hotkeys: bool,
    /// Watch the rules folder for changes by the CLI or other programs (the real window).
    pub watch: bool,
    /// The monitor layout the mouse simulation draws on (reading it moves nothing).
    pub layout: Layout,
    /// Where the simulated pointer starts.
    pub cursor: Point,
}

impl std::fmt::Debug for Options {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Options")
            .field("paths", &self.paths)
            .field("lang", &self.lang)
            .finish_non_exhaustive()
    }
}

impl Options {
    /// Options for tests: an isolated rules folder, a single 1920×1080 monitor, no watcher.
    pub fn for_test(paths: Paths, engine: Arc<dyn Engine>) -> Options {
        Options {
            paths,
            engine,
            lang: Lang::En,
            no_hotkeys: false,
            watch: false,
            layout: Layout::single(1920, 1080),
            cursor: Point::new(960, 540),
        }
    }
}

/// A message under the header (errors keep their code).
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Banner {
    Error {
        code: String,
        message: String,
        details: Vec<String>,
    },
    Info(String),
}

/// The pages of the navigation rail.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub enum Page {
    #[default]
    Rules,
    Modes,
}

/// What a confirmed Discard leads to.
#[derive(Clone, Debug, PartialEq, Eq)]
enum AfterDiscard {
    New(Option<Effect>),
    Select(String),
    Edit(String),
    Close,
    CloseWindow,
    Reload,
}

/// What Allow in the consent dialog continues with.
#[derive(Clone, Debug, PartialEq, Eq)]
enum ConsentThen {
    SaveForm,
    Enable(Vec<String>),
}

/// The open dialog (one at a time; each has the safe choice focused).
#[derive(Clone, Debug)]
enum Dialog {
    Delete {
        ids: Vec<String>,
        titles: Vec<String>,
    },
    Discard(AfterDiscard),
    Consent {
        perms: Vec<Permission>,
        targets: Vec<String>,
        then: ConsentThen,
    },
    Export {
        ids: Vec<String>,
        path: String,
        done: Option<String>,
    },
    Import {
        path: String,
        plan: Option<Box<ImportPlan>>,
        error: Option<String>,
    },
    Help,
}

/// The undo toast after a delete (stays until dismissed, J7).
#[derive(Clone, Debug, PartialEq, Eq)]
struct Toast {
    text: String,
    undo: bool,
}

/// Which field the capture box writes.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum CaptureTarget {
    Trigger,
    RemapTo,
    RemapTap,
    SendKeys,
}

/// One user intent, applied after the frame is drawn.
#[derive(Clone, Debug)]
enum Act {
    New(Option<Effect>),
    Select { id: String, ctrl: bool, shift: bool },
    Edit(String),
    Duplicate(Vec<String>),
    AskDelete(Vec<String>),
    Delete(Vec<String>),
    SetEnabled(Vec<String>, bool),
    Move(String, Position),
    Test(String),
    Undo,
    DismissToast,
    SaveForm,
    CloseForm,
    Engine(Request),
    Export(Vec<String>),
    Import,
    Help,
    SwitchLang,
    Page(Page),
    Discard(AfterDiscard),
    Allow,
    CloseDialog,
    ReloadFromDisk,
    ModeEnabled(String, bool),
    NewMode(String),
}

/// The editor's state.
pub struct EditorApp {
    repo: Repo,
    pub lang: Lang,
    layout: Layout,
    cursor: Point,
    link: Link,
    wake: Wake,
    changed: Arc<AtomicBool>,
    #[cfg(windows)]
    watcher: Option<ddalgak::win::watch::FolderWatcher>,
    watch: bool,
    loaded: store::Loaded,
    rows: Vec<Row>,
    filter: Filter,
    grouping: Grouping,
    page: Page,
    selected: Vec<String>,
    cursor_row: Option<String>,
    form: Option<RuleForm>,
    capture: Capture,
    capture_target: CaptureTarget,
    test: Option<(String, Simulation)>,
    dialog: Option<Dialog>,
    dialog_focus: bool,
    toast: Option<Toast>,
    undo: Vec<UndoEntry>,
    engine: EngineView,
    save: SaveState,
    banner: Option<Banner>,
    shortcuts: Shortcuts,
    /// The file changed outside the editor while the form had unsaved changes.
    external_change: bool,
    acts: Vec<Act>,
    region: usize,
    anchors: [Option<egui::Id>; 3],
    focus_search: bool,
    focus_title: bool,
    new_mode_title: String,
    themed: bool,
    visible: Vec<usize>,
    /// Close the window on the next frame (a confirmed discard on close).
    close_now: bool,
}

impl std::fmt::Debug for EditorApp {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("EditorApp")
            .field("revision", &self.loaded.revision())
            .field("rules", &self.rows.len())
            .field("save", &self.save)
            .finish_non_exhaustive()
    }
}

impl EditorApp {
    pub fn new(opts: Options) -> EditorApp {
        let repo = Repo::new(opts.paths.clone());
        let wake: Wake = Arc::new(Mutex::new(None));
        let link = Link::new(Arc::clone(&opts.engine), Arc::clone(&wake));
        let loaded = repo.read();
        let rows = listing::rows(&loaded);
        let shortcuts = Shortcuts::load(&opts.paths, opts.no_hotkeys);
        let mut app = EditorApp {
            repo,
            lang: opts.lang,
            layout: opts.layout,
            cursor: opts.cursor,
            link,
            wake,
            changed: Arc::new(AtomicBool::new(false)),
            #[cfg(windows)]
            watcher: None,
            watch: opts.watch,
            loaded,
            rows,
            filter: Filter::default(),
            grouping: Grouping::None,
            page: Page::Rules,
            selected: Vec::new(),
            cursor_row: None,
            form: None,
            capture: Capture::default(),
            capture_target: CaptureTarget::Trigger,
            test: None,
            dialog: None,
            dialog_focus: false,
            toast: None,
            undo: Vec::new(),
            engine: EngineView::Unknown,
            save: SaveState::Idle,
            banner: None,
            shortcuts,
            external_change: false,
            acts: Vec::new(),
            region: 1,
            anchors: [None; 3],
            focus_search: false,
            focus_title: false,
            new_mode_title: String::new(),
            themed: false,
            visible: Vec::new(),
            close_now: false,
        };
        app.start_watch();
        app.link.send(Job::Status);
        if app.loaded.state.usable() {
            let r = app.loaded.revision();
            app.save = SaveState::Waiting { revision: r };
            app.link.send(Job::Reload { revision: r });
        }
        app
    }

    fn start_watch(&mut self) {
        #[cfg(windows)]
        if self.watch && self.watcher.is_none() && self.repo.paths.config.is_dir() {
            let flag = Arc::clone(&self.changed);
            let wake = Arc::clone(&self.wake);
            self.watcher = ddalgak::win::watch::FolderWatcher::start(
                &self.repo.paths.config,
                Box::new(move || {
                    flag.store(true, Ordering::SeqCst);
                    if let Ok(w) = wake.lock()
                        && let Some(w) = w.as_ref()
                    {
                        w();
                    }
                }),
            )
            .ok();
        }
    }

    // --- Accessors for tests and the binary ----------------------------------------------------

    pub fn rows(&self) -> &[Row] {
        &self.rows
    }

    pub fn save_state(&self) -> &SaveState {
        &self.save
    }

    pub fn engine_view(&self) -> &EngineView {
        &self.engine
    }

    pub fn banner(&self) -> Option<&Banner> {
        self.banner.as_ref()
    }

    pub fn form(&self) -> Option<&RuleForm> {
        self.form.as_ref()
    }

    pub fn form_mut(&mut self) -> Option<&mut RuleForm> {
        self.form.as_mut()
    }

    pub fn selected(&self) -> &[String] {
        &self.selected
    }

    pub fn revision(&self) -> u64 {
        self.loaded.revision()
    }

    pub fn simulation(&self) -> Option<&Simulation> {
        self.test.as_ref().map(|(_, s)| s)
    }

    pub fn dialog_open(&self) -> bool {
        self.dialog.is_some()
    }

    pub fn has_unsaved_changes(&self) -> bool {
        self.form.as_ref().is_some_and(RuleForm::dirty)
    }

    pub fn external_change(&self) -> bool {
        self.external_change
    }

    pub fn filter_mut(&mut self) -> &mut Filter {
        &mut self.filter
    }

    pub fn set_grouping(&mut self, g: Grouping) {
        self.grouping = g;
    }

    /// The ids of the rows the list shows, in display order.
    pub fn visible_ids(&self) -> Vec<String> {
        self.visible
            .iter()
            .map(|i| self.rows[*i].id.clone())
            .collect()
    }

    /// Notices a change of the rules folder now (what the watcher does in the real window).
    pub fn notice_change(&mut self) {
        self.changed.store(true, Ordering::SeqCst);
    }

    /// Waits for the app's answers and applies them (tests).
    pub fn wait_idle(&mut self, timeout: Duration) -> bool {
        let ok = self.link.wait_idle(timeout);
        self.poll();
        ok
    }

    /// Whether the window may close now; with unsaved changes it asks first.
    pub fn request_close(&mut self) -> bool {
        if self.has_unsaved_changes() {
            self.dialog = Some(Dialog::Discard(AfterDiscard::CloseWindow));
            self.dialog_focus = true;
            false
        } else {
            true
        }
    }

    // --- Model updates ---------------------------------------------------------------------------

    fn t(&self, key: &str) -> &'static str {
        self.lang.t(key)
    }

    fn doc(&self) -> Option<&ddalgak::model::Document> {
        self.loaded.doc.as_ref()
    }

    fn profile(&self, id: &str) -> Option<&Profile> {
        self.doc().and_then(|d| d.profile(id))
    }

    fn active_profile_id(&self) -> String {
        self.doc()
            .map_or_else(|| "default".into(), |d| d.active_profile.clone())
    }

    fn poll(&mut self) {
        for r in self.link.poll() {
            self.engine = EngineView::from_reply(&self.engine, &r.job, &r.result);
            match &r.job {
                Job::Reload { revision } => {
                    // A reply for an older save does not override the state of a newer one.
                    let current = match &self.save {
                        SaveState::Waiting { revision } => Some(*revision),
                        _ => None,
                    };
                    if current.is_none_or(|c| c == *revision) {
                        self.save = SaveState::after_reload(*revision, &r.result);
                    }
                }
                Job::Engine(_) => {
                    if let Err(e) = &r.result {
                        self.banner = Some(Banner::Error {
                            code: e.code.clone(),
                            message: e.message.clone(),
                            details: Vec::new(),
                        });
                    }
                    // The engine state changed: what is active changed with it.
                    if self.loaded.state.usable() {
                        let rev = self.loaded.revision();
                        self.save = SaveState::Waiting { revision: rev };
                        self.link.send(Job::Reload { revision: rev });
                    }
                }
                Job::Status => {}
            }
        }
        if self.changed.swap(false, Ordering::SeqCst) {
            self.reload_from_disk(true);
        }
    }

    /// Reads `rules.json` again. `external`: a change not made by this window (the CLI, another
    /// program): the app is asked to reload so the status line stays truthful.
    fn reload_from_disk(&mut self, external: bool) {
        let next = self.repo.read();
        if next.hash == self.loaded.hash && next.state == self.loaded.state {
            return;
        }
        self.loaded = next;
        self.rows = listing::rows(&self.loaded);
        let ids: BTreeSet<&str> = self.rows.iter().map(|r| r.id.as_str()).collect();
        self.selected.retain(|s| ids.contains(s.as_str()));
        if let Some(form) = &self.form
            && let (Some(id), Some(base)) = (&form.id, &form.base)
        {
            let now = self.doc().and_then(|d| {
                d.find_rule(id)
                    .map(|(pi, ri)| d.profiles[pi].rules[ri].to_value())
            });
            if now.as_ref() != Some(&base.to_value()) {
                self.external_change = true;
            }
        }
        if external && self.loaded.state.usable() {
            let rev = self.loaded.revision();
            self.save = SaveState::Waiting { revision: rev };
            self.link.send(Job::Reload { revision: rev });
        }
    }

    fn after_commit(&mut self, label: String, c: &Committed) {
        if c.changed {
            self.undo.push(UndoEntry {
                label,
                before: c.before.clone(),
                after: c.revision,
            });
            self.save = SaveState::Waiting {
                revision: c.revision,
            };
            self.link.send(Job::Reload {
                revision: c.revision,
            });
        }
        if !c.change.notes.is_empty() {
            self.banner = Some(Banner::Info(c.change.notes.join("; ")));
        } else if let Some(w) = c.warnings.first() {
            self.banner = Some(Banner::Info(format!("{} ({})", w.message, w.code)));
        }
        self.reload_from_disk(false);
        self.start_watch();
    }

    fn fail(&mut self, e: &CommitError) {
        self.banner = Some(Banner::Error {
            code: e.code().to_string(),
            message: e.message(),
            details: e
                .problems()
                .iter()
                .map(|p| format!("{} ({}): {}", p.code, p.severity.as_str(), p.message))
                .collect(),
        });
    }

    fn commit(
        &mut self,
        label: String,
        command: &str,
        op: impl FnOnce(&mut ddalgak::model::Document) -> Result<Change, EditError>,
    ) -> Option<Committed> {
        match self.repo.commit(command, op) {
            Ok(c) => {
                self.banner = None;
                self.after_commit(label, &c);
                Some(c)
            }
            Err(e) => {
                self.fail(&e);
                None
            }
        }
    }

    fn titles(&self, ids: &[String]) -> Vec<String> {
        ids.iter()
            .filter_map(|id| self.rows.iter().find(|r| &r.id == id))
            .map(|r| r.title.clone())
            .collect()
    }

    /// The permissions rules need before they can be enabled, and the exact launch targets.
    fn missing_consent(&self, ids: &[String]) -> (Vec<Permission>, Vec<String>) {
        let mut perms = BTreeSet::new();
        let mut targets = Vec::new();
        if let Some(d) = self.doc() {
            for id in ids {
                if let Some((pi, ri)) = d.find_rule(id)
                    && let Some(r) = d.profiles[pi].rules[ri].rule()
                {
                    let missing = ddalgak::validate::missing_consent(r);
                    if missing.contains(&Permission::LaunchPrograms) {
                        targets.extend(form::launch_targets(r));
                    }
                    perms.extend(missing);
                }
            }
        }
        (perms.into_iter().collect(), targets)
    }

    fn apply(&mut self, act: Act) {
        match act {
            Act::New(effect) => {
                if self.has_unsaved_changes() {
                    self.ask_discard(AfterDiscard::New(effect));
                    return;
                }
                let p = self
                    .selected
                    .first()
                    .and_then(|id| self.rows.iter().find(|r| &r.id == id))
                    .map_or_else(|| self.active_profile_id(), |r| r.profile.clone());
                self.form = Some(match effect {
                    Some(e) => RuleForm::starter(&p, e),
                    None => RuleForm::new(&p),
                });
                self.external_change = false;
                self.test = None;
                self.page = Page::Rules;
                self.capture = Capture {
                    active: effect.is_none(),
                    ..Capture::default()
                };
                self.capture_target = CaptureTarget::Trigger;
            }
            Act::Select { id, ctrl, shift } => {
                if self.has_unsaved_changes() && !ctrl && !shift {
                    self.ask_discard(AfterDiscard::Select(id));
                    return;
                }
                if !ctrl && !shift {
                    self.form = None;
                }
                self.select(&id, ctrl, shift);
            }
            Act::Edit(id) => {
                if self.has_unsaved_changes() {
                    self.ask_discard(AfterDiscard::Edit(id));
                    return;
                }
                let found = self.doc().and_then(|d| {
                    let (pi, ri) = d.find_rule(&id)?;
                    let r = d.profiles[pi].rules[ri].rule()?.clone();
                    Some((d.profiles[pi].id.clone(), r))
                });
                if let Some((p, r)) = found {
                    self.form = Some(RuleForm::edit(&p, &r));
                    self.external_change = false;
                    self.test = None;
                    self.selected = vec![id.clone()];
                    self.cursor_row = Some(id);
                    self.focus_title = true;
                }
            }
            Act::Duplicate(ids) => {
                let n = ids.len();
                let label = format!("Duplicated {n} rule(s)");
                let ids2 = ids;
                let c = self.commit(label, "rule.duplicate", move |d| {
                    let mut all = Change::default();
                    for id in &ids2 {
                        let c = edit::duplicate(d, id, None, None, None)?;
                        all.rules.extend(c.rules);
                        all.created = c.created;
                        all.changed |= c.changed;
                        all.notes = c.notes;
                    }
                    Ok(all)
                });
                if let Some(c) = c {
                    self.selected = c.change.rules.clone();
                    self.cursor_row = c.change.rules.last().cloned();
                }
            }
            Act::AskDelete(ids) => {
                if !ids.is_empty() {
                    let titles = self.titles(&ids);
                    self.dialog = Some(Dialog::Delete { ids, titles });
                    self.dialog_focus = true;
                }
            }
            Act::Delete(ids) => {
                let n = ids.len();
                let label = format!("{} {n} rule(s)", self.t("toast.deleted"));
                let titles = self.titles(&ids);
                let ids2 = ids.clone();
                if self
                    .commit(label.clone(), "rule.delete", move |d| {
                        edit::delete(d, &ids2)
                    })
                    .is_some()
                {
                    self.toast = Some(Toast {
                        text: format!("{label}: {}", titles.join(", ")),
                        undo: true,
                    });
                    self.selected.retain(|s| !ids.contains(s));
                    if self
                        .form
                        .as_ref()
                        .and_then(|f| f.id.as_ref())
                        .is_some_and(|i| ids.contains(i))
                    {
                        self.form = None;
                    }
                }
                self.dialog = None;
            }
            Act::SetEnabled(ids, on) => {
                if on {
                    let (perms, targets) = self.missing_consent(&ids);
                    if !perms.is_empty() {
                        self.dialog = Some(Dialog::Consent {
                            perms,
                            targets,
                            then: ConsentThen::Enable(ids),
                        });
                        self.dialog_focus = true;
                        return;
                    }
                }
                let label = format!(
                    "{} {} rule(s)",
                    if on { "Enabled" } else { "Disabled" },
                    ids.len()
                );
                self.commit(
                    label,
                    if on { "rule.enable" } else { "rule.disable" },
                    move |d| repo::enable_with_consent(d, &ids, on, &[]),
                );
            }
            Act::Move(id, pos) => {
                self.commit(format!("Moved {id}"), "rule.move", move |d| {
                    edit::move_rule(d, &id, &pos)
                });
            }
            Act::Test(id) => {
                let found = self.doc().and_then(|d| {
                    let (pi, ri) = d.find_rule(&id)?;
                    Some((
                        d.profiles[pi].clone(),
                        d.profiles[pi].rules[ri].rule()?.clone(),
                    ))
                });
                if let Some((p, r)) = found {
                    let f = RuleForm::edit(&p.id, &r);
                    let sim = f.simulate(Some(&p), &self.layout, self.cursor);
                    self.test = Some((id, sim));
                }
            }
            Act::Undo => {
                if let Some(entry) = self.undo.pop() {
                    match self.repo.undo(&entry) {
                        Ok(c) => {
                            self.save = SaveState::Waiting {
                                revision: c.revision,
                            };
                            self.link.send(Job::Reload {
                                revision: c.revision,
                            });
                            self.toast = Some(Toast {
                                text: format!("{}: {}", self.t("toast.undone"), entry.label),
                                undo: false,
                            });
                            self.banner = None;
                            self.reload_from_disk(false);
                        }
                        Err(e) => self.fail(&e),
                    }
                }
            }
            Act::DismissToast => self.toast = None,
            Act::SaveForm => self.save_form(&[]),
            Act::CloseForm => {
                if self.has_unsaved_changes() {
                    self.ask_discard(AfterDiscard::Close);
                } else {
                    self.form = None;
                    self.capture.active = false;
                }
            }
            Act::Engine(r) => self.link.send(Job::Engine(r)),
            Act::Export(ids) => {
                let ids = if ids.is_empty() {
                    self.rows.iter().map(|r| r.id.clone()).collect()
                } else {
                    ids
                };
                let dir = std::env::var_os("USERPROFILE")
                    .map(|h| std::path::PathBuf::from(h).join("Documents"))
                    .filter(|d| d.is_dir())
                    .unwrap_or_else(std::env::temp_dir);
                let path = dir
                    .join(format!("ddalgak-rules-{}.json", store::compact_time()))
                    .display()
                    .to_string();
                self.dialog = Some(Dialog::Export {
                    ids,
                    path,
                    done: None,
                });
                self.dialog_focus = true;
            }
            Act::Import => {
                self.dialog = Some(Dialog::Import {
                    path: String::new(),
                    plan: None,
                    error: None,
                });
                self.dialog_focus = true;
            }
            Act::Help => {
                self.dialog = Some(Dialog::Help);
                self.dialog_focus = true;
            }
            Act::SwitchLang => self.lang = self.lang.other(),
            Act::Page(p) => self.page = p,
            Act::Discard(then) => {
                self.dialog = None;
                self.form = None;
                self.capture.active = false;
                self.external_change = false;
                match then {
                    AfterDiscard::New(e) => self.apply(Act::New(e)),
                    AfterDiscard::Select(id) => self.select(&id, false, false),
                    AfterDiscard::Edit(id) => self.apply(Act::Edit(id)),
                    AfterDiscard::Close => {}
                    // The window closes on the next frame (see `ui`).
                    AfterDiscard::CloseWindow => self.close_now = true,
                    AfterDiscard::Reload => self.reload_from_disk(false),
                }
            }
            Act::Allow => {
                let Some(Dialog::Consent { perms, then, .. }) = self.dialog.take() else {
                    return;
                };
                match then {
                    ConsentThen::SaveForm => self.save_form(&perms),
                    ConsentThen::Enable(ids) => {
                        let label = format!("Enabled {} rule(s)", ids.len());
                        self.commit(label, "rule.enable", move |d| {
                            repo::enable_with_consent(d, &ids, true, &perms)
                        });
                    }
                }
            }
            Act::CloseDialog => self.dialog = None,
            Act::ReloadFromDisk => {
                if self.has_unsaved_changes() {
                    self.ask_discard(AfterDiscard::Reload);
                } else {
                    self.form = None;
                    self.external_change = false;
                    self.reload_from_disk(false);
                }
            }
            Act::ModeEnabled(id, on) => {
                let p = self.active_profile_id();
                let label = format!("{} mode {id}", if on { "Enabled" } else { "Disabled" });
                match self.repo.commit_modes("mode.enable", move |d| {
                    ddalgak::mode_edit::set_enabled(d, Some(&p), &id, on)
                }) {
                    Ok(c) => self.after_commit(label, &c),
                    Err(e) => self.fail(&e),
                }
            }
            Act::NewMode(title) => {
                let p = self
                    .form
                    .as_ref()
                    .map_or_else(|| self.active_profile_id(), |f| f.profile.clone());
                let fields = ddalgak::mode_edit::ModeFields {
                    title: Some(title.trim().to_string()),
                    ..Default::default()
                };
                match self.repo.commit_modes("mode.add", move |d| {
                    ddalgak::mode_edit::add(d, Some(&p), None, &fields)
                }) {
                    Ok(c) => {
                        let created = c.change.created_mode.clone();
                        self.after_commit("Added a mode".into(), &c);
                        self.new_mode_title.clear();
                        if let (Some(f), Some(m)) = (self.form.as_mut(), created) {
                            f.fields.mode.mode = m;
                        }
                    }
                    Err(e) => self.fail(&e),
                }
            }
        }
    }

    fn ask_discard(&mut self, then: AfterDiscard) {
        self.dialog = Some(Dialog::Discard(then));
        self.dialog_focus = true;
    }

    fn select(&mut self, id: &str, ctrl: bool, shift: bool) {
        self.test = None;
        if shift && let Some(anchor) = &self.cursor_row {
            let order: Vec<&str> = self
                .visible
                .iter()
                .map(|i| self.rows[*i].id.as_str())
                .collect();
            if let (Some(a), Some(b)) = (
                order.iter().position(|x| x == anchor),
                order.iter().position(|x| *x == id),
            ) {
                let (lo, hi) = (a.min(b), a.max(b));
                self.selected = order[lo..=hi].iter().map(|s| (*s).to_string()).collect();
                return;
            }
        }
        if ctrl {
            if let Some(i) = self.selected.iter().position(|s| s == id) {
                self.selected.remove(i);
            } else {
                self.selected.push(id.to_string());
            }
        } else {
            self.selected = vec![id.to_string()];
        }
        self.cursor_row = Some(id.to_string());
    }

    /// Saves the open form (J2 step 4). `allow`: consent the user just gave.
    fn save_form(&mut self, allow: &[Permission]) {
        let Some(f) = self.form.clone() else {
            return;
        };
        let profile = self.profile(&f.profile).cloned();
        let mut rule = match f.build("new-rule", profile.as_ref()) {
            Ok(r) => r,
            Err(p) => {
                self.banner = Some(Banner::Error {
                    code: "rule_invalid".into(),
                    message: "the rule is not complete yet; nothing was written".into(),
                    details: p
                        .iter()
                        .map(|x| format!("{}: {}", x.path, x.message))
                        .collect(),
                });
                return;
            }
        };
        if !allow.is_empty() {
            form::grant_gui(&mut rule, allow);
        }
        let missing = ddalgak::validate::missing_consent(&rule);
        if rule.enabled && !missing.is_empty() {
            let targets = if missing.contains(&Permission::LaunchPrograms) {
                form::launch_targets(&rule)
            } else {
                Vec::new()
            };
            self.dialog = Some(Dialog::Consent {
                perms: missing,
                targets,
                then: ConsentThen::SaveForm,
            });
            self.dialog_focus = true;
            return;
        }
        let pid = f.profile.clone();
        let result = match (&f.id, &f.base) {
            (Some(id), Some(base)) => {
                let id = id.clone();
                let base = base.to_value();
                self.commit(
                    format!("Edited \"{}\"", rule.title),
                    "rule.edit",
                    move |d| repo::replace_rule(d, &id, rule, &base),
                )
            }
            _ => self.commit(format!("Added \"{}\"", rule.title), "rule.add", move |d| {
                repo::add_rule(d, &pid, rule)
            }),
        };
        if let Some(c) = result {
            let id = c
                .change
                .created
                .clone()
                .or_else(|| c.change.rules.first().cloned());
            self.form = None;
            self.capture.active = false;
            self.external_change = false;
            if let Some(id) = id {
                self.selected = vec![id.clone()];
                self.cursor_row = Some(id);
            }
        }
    }

    // --- Keyboard ------------------------------------------------------------------------------

    fn keys(&mut self, ctx: &egui::Context) {
        let events = ctx.input(|i| i.events.clone());
        // The capture box gets every key while it records (Tab still leaves it).
        if self.capture.active && self.form.is_some() && self.dialog.is_none() {
            let current = self.capture_value();
            for c in self.capture.handle(&current, &events) {
                match c {
                    Captured::Keys(k) => {
                        self.set_capture_value(k);
                    }
                    Captured::Cancelled | Captured::Leave | Captured::Ignored => {}
                }
            }
            ctx.input_mut(|i| {
                i.events.retain(
                    |e| !matches!(e, egui::Event::Key { key, .. } if *key != egui::Key::Tab),
                );
            });
            return;
        }
        let text = ctx.text_edit_focused();
        for e in &events {
            let egui::Event::Key {
                key,
                pressed: true,
                modifiers: m,
                ..
            } = e
            else {
                continue;
            };
            let (key, m) = (*key, *m);
            let consume = |ctx: &egui::Context| ctx.input_mut(|i| i.consume_key(m, key));
            use egui::Key as K;
            match (key, m.ctrl, m.alt, m.shift) {
                (K::F1, false, false, false) => {
                    consume(ctx);
                    self.acts.push(Act::Help);
                }
                (K::F6, false, false, back) => {
                    consume(ctx);
                    self.region = if back {
                        (self.region + 2) % 3
                    } else {
                        (self.region + 1) % 3
                    };
                    if let Some(id) = self.anchors[self.region] {
                        ctx.memory_mut(|mem| mem.request_focus(id));
                    }
                }
                (K::Escape, false, false, false) if self.dialog.is_none() => {
                    if self.form.is_some() {
                        consume(ctx);
                        self.acts.push(Act::CloseForm);
                    } else if self.test.is_some() {
                        consume(ctx);
                        self.test = None;
                    }
                }
                _ if self.dialog.is_some() || self.page != Page::Rules => {}
                (K::ArrowUp | K::ArrowDown, false, true, false) if !text => {
                    consume(ctx);
                    if let Some(id) = self.selected.first().cloned() {
                        self.move_by(&id, key == K::ArrowUp);
                    }
                }
                (K::ArrowUp | K::ArrowDown, false, false, shift) if !text => {
                    consume(ctx);
                    self.step_cursor(key == K::ArrowDown, shift);
                }
                (K::Space, false, false, false) if !text => {
                    consume(ctx);
                    if let Some(id) = self.cursor_row.clone() {
                        self.acts.push(Act::Select {
                            id,
                            ctrl: true,
                            shift: false,
                        });
                    }
                }
                (K::Enter | K::F2, false, false, false) if !text && self.form.is_none() => {
                    consume(ctx);
                    if let Some(id) = self
                        .cursor_row
                        .clone()
                        .or_else(|| self.selected.first().cloned())
                    {
                        self.acts.push(Act::Edit(id));
                    }
                }
                (K::A, true, false, false) if !text => {
                    consume(ctx);
                    self.selected = self
                        .visible
                        .iter()
                        .map(|i| self.rows[*i].id.clone())
                        .collect();
                }
                (K::Z, true, false, false) if !text => {
                    consume(ctx);
                    self.acts.push(Act::Undo);
                }
                _ => {
                    let plain = !m.ctrl && !m.alt && !matches!(key, K::F5 | K::F2);
                    if text && plain {
                        continue;
                    }
                    let Some(action) = self.shortcuts.action_for(key, m).map(str::to_string) else {
                        continue;
                    };
                    consume(ctx);
                    let sel = self.selected.clone();
                    match action.as_str() {
                        "rules.new" => self.acts.push(Act::New(None)),
                        "rules.duplicate" if !sel.is_empty() => self.acts.push(Act::Duplicate(sel)),
                        "rules.delete" if !sel.is_empty() => self.acts.push(Act::AskDelete(sel)),
                        "rules.search" => self.focus_search = true,
                        "rules.test" => {
                            if let Some(id) = sel.first() {
                                self.acts.push(Act::Test(id.clone()));
                            }
                        }
                        "rules.toggle" if !sel.is_empty() => {
                            let on = !self
                                .rows
                                .iter()
                                .filter(|r| sel.contains(&r.id))
                                .all(|r| r.enabled);
                            self.acts.push(Act::SetEnabled(sel, on));
                        }
                        "rules.export" => self.acts.push(Act::Export(sel)),
                        _ => {}
                    }
                }
            }
        }
    }

    fn step_cursor(&mut self, down: bool, extend: bool) {
        if self.visible.is_empty() {
            return;
        }
        let order: Vec<String> = self
            .visible
            .iter()
            .map(|i| self.rows[*i].id.clone())
            .collect();
        let at = self
            .cursor_row
            .as_ref()
            .and_then(|c| order.iter().position(|x| x == c));
        let next = match at {
            None => 0,
            Some(i) if down => (i + 1).min(order.len() - 1),
            Some(i) => i.saturating_sub(1),
        };
        let id = order[next].clone();
        if extend {
            if !self.selected.contains(&id) {
                self.selected.push(id.clone());
            }
            self.cursor_row = Some(id);
        } else {
            self.acts.push(Act::Select {
                id,
                ctrl: false,
                shift: false,
            });
        }
    }

    fn move_by(&mut self, id: &str, up: bool) {
        let Some(r) = self.rows.iter().find(|r| r.id == id) else {
            return;
        };
        let pos = if up {
            if r.position == 0 {
                return;
            }
            Position::Index(r.position - 1)
        } else {
            Position::Index(r.position + 1)
        };
        self.acts.push(Act::Move(id.to_string(), pos));
    }

    fn capture_value(&self) -> String {
        let Some(f) = &self.form else {
            return String::new();
        };
        match self.capture_target {
            CaptureTarget::Trigger => f.fields.trigger.keys.clone(),
            CaptureTarget::RemapTo => f.fields.remap.to.clone(),
            CaptureTarget::RemapTap => f.fields.remap.tap.clone(),
            CaptureTarget::SendKeys => f.fields.keys.keys.clone(),
        }
    }

    fn set_capture_value(&mut self, v: String) {
        let target = self.capture_target;
        if let Some(f) = self.form.as_mut() {
            match target {
                CaptureTarget::Trigger => f.fields.trigger.keys = v,
                CaptureTarget::RemapTo => f.fields.remap.to = v,
                CaptureTarget::RemapTap => f.fields.remap.tap = v,
                CaptureTarget::SendKeys => f.fields.keys.keys = v,
            }
        }
    }
}

impl eframe::App for EditorApp {
    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        EditorApp::ui(self, ui);
    }
}
