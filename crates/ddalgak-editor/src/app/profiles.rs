//! The Profiles page and its dialogs (Step 75, docs/DDALGAK_GUI_JOURNEYS.md J6, ADR 0052;
//! `DDALGAK-FR-030…032`, `DDALGAK-SEC-013`).
//!
//! - **Browser**: every profile with its rules (on/total), modes and errors, the active one marked
//!   in words; search and sort; *Switch…*, *Duplicate*, *Rename…*, *History…*, *Export…*,
//!   *Delete…*; **+ New profile** and **Import profile…**.
//! - **Switch** opens a side-by-side diff of what each trigger does now and after the switch,
//!   with **Cancel** focused; only *Switch to this profile* changes `active_profile`, and the
//!   header names the new profile once the running app confirmed the reload.
//! - **History** lists the profile's versions (history snapshots and save points), compares the
//!   chosen one with now side by side, and restores it — or rolls back to the newest working
//!   version — as a new change (itself undoable).
//! - **Import** checks the file first and shows everything it would add: renamed IDs, restored
//!   modes, each rule's permissions and exact launch targets, missing programs, conflicts and
//!   codes that would keep a rule inactive. Imported profiles are never switched to, every rule
//!   arrives off and without consent, and nothing is opened or run.
//!
//! Like the rest of the window, a frame only reads the state and records [`Act`]s.

use super::*;
use ddalgak::profile::{self, Align, DiffRow, DiffStatus, ExportOptions, ImportOptions, Sort};

fn small(text: impl Into<String>, color: egui::Color32) -> egui::RichText {
    egui::RichText::new(text.into())
        .size(Metrics::get().caption)
        .color(color)
}

fn title(ui: &mut egui::Ui, text: &str) {
    ui.label(
        egui::RichText::new(text)
            .size(Metrics::get().title)
            .strong(),
    );
}

/// The safe choice of a dialog: focused when the dialog opens.
fn cancel_button(ui: &mut egui::Ui, label: &str, focus: bool) -> bool {
    let r = ui.button(label);
    if focus {
        r.request_focus();
    }
    r.clicked()
}

impl EditorApp {
    /// The profile rows the browser shows (search and sort applied).
    pub fn profile_rows(&self) -> Vec<profile::Row> {
        let Some(doc) = self.doc() else {
            return Vec::new();
        };
        let all = profile::rows(doc, Some(&self.loaded.report));
        profile::browse(&all, &self.profile_search, self.profile_sort)
    }

    /// The page shown now.
    pub fn page(&self) -> Page {
        self.page
    }

    pub(super) fn profiles_page(&mut self, ui: &mut egui::Ui) {
        let c = Colors::get();
        let lang = self.lang;
        self.banner_ui(ui);
        title(ui, lang.t("nav.profiles"));
        ui.label(small(lang.t("profiles.note"), c.text_muted));
        ui.add_space(4.0);
        ui.horizontal_wrapped(|ui| {
            ui.add(
                egui::TextEdit::singleline(&mut self.profile_search)
                    .hint_text(lang.t("profiles.search"))
                    .desired_width(220.0),
            )
            .accessible_name(lang.t("profiles.search"));
            let label = ui.label(small(lang.t("profiles.sort"), c.text_secondary));
            egui::ComboBox::from_id_salt("profile-sort")
                .selected_text(lang.t(&format!("sort.{}", self.profile_sort.as_str())))
                .show_ui(ui, |ui| {
                    for s in Sort::ALL {
                        ui.selectable_value(
                            &mut self.profile_sort,
                            s,
                            lang.t(&format!("sort.{}", s.as_str())),
                        );
                    }
                })
                .response
                .labelled_by(label.id);
        });
        ui.add_space(6.0);
        let rows = self.profile_rows();
        if rows.is_empty() {
            ui.label(small(lang.t("profiles.none_match"), c.text_muted));
        }
        egui::ScrollArea::vertical()
            .id_salt("profiles-scroll")
            .max_height(ui.available_height() - 90.0)
            .show(ui, |ui| {
                for r in &rows {
                    self.profile_row_ui(ui, r);
                    ui.separator();
                }
            });
        ui.add_space(6.0);
        ui.horizontal_wrapped(|ui| {
            ui.add(
                egui::TextEdit::singleline(&mut self.new_profile_title)
                    .hint_text(lang.t("profiles.new"))
                    .desired_width(200.0),
            )
            .accessible_name(lang.t("profiles.new"));
            if ui
                .add_enabled(
                    !self.new_profile_title.trim().is_empty(),
                    egui::Button::new(lang.t("profiles.add")),
                )
                .clicked()
            {
                self.acts
                    .push(Act::ProfileAdd(self.new_profile_title.trim().to_string()));
            }
            ui.separator();
            if ui.button(lang.t("profiles.import")).clicked() {
                self.acts.push(Act::OpenProfileImport);
            }
            if ui.button(lang.t("profiles.export_all")).clicked() {
                let ids = rows.iter().map(|r| r.id.clone()).collect();
                self.acts.push(Act::OpenProfileExport(ids));
            }
        });
    }

    fn profile_row_ui(&mut self, ui: &mut egui::Ui, r: &profile::Row) {
        let c = Colors::get();
        let lang = self.lang;
        ui.horizontal_wrapped(|ui| {
            if r.active {
                ui.label(
                    egui::RichText::new(lang.t("profiles.active"))
                        .color(c.success)
                        .strong(),
                );
            }
            ui.label(egui::RichText::new(&r.title).strong());
            ui.label(small(format!("({})", r.id), c.text_muted));
            let mut facts = vec![
                format!(
                    "{} {} ({} {})",
                    r.rules,
                    lang.t("status.rules"),
                    r.rules_on,
                    lang.t("profiles.on")
                ),
                format!("{} {}", r.modes, lang.t("profiles.modes")),
            ];
            if !r.enabled {
                facts.push(lang.t("state.disabled").to_string());
            }
            if r.errors > 0 {
                facts.push(format!("{} {}", r.errors, lang.t("profiles.errors")));
            }
            ui.label(small(facts.join(" · "), c.text_secondary));
        });
        if let Some(d) = &r.description {
            ui.label(small(d, c.text_muted));
        }
        ui.horizontal_wrapped(|ui| {
            if !r.active && ui.button(lang.t("btn.switch")).clicked() {
                self.acts.push(Act::OpenSwitch(r.id.clone()));
            }
            if ui.button(lang.t("btn.duplicate")).clicked() {
                self.acts.push(Act::DuplicateProfile(r.id.clone()));
            }
            if ui.button(lang.t("btn.rename")).clicked() {
                self.acts.push(Act::OpenEditProfile(r.id.clone()));
            }
            if ui.button(lang.t("btn.history")).clicked() {
                self.acts.push(Act::OpenHistory(r.id.clone()));
            }
            if ui.button(lang.t("btn.export")).clicked() {
                self.acts.push(Act::OpenProfileExport(vec![r.id.clone()]));
            }
            if ui
                .add_enabled(!r.active, egui::Button::new(lang.t("btn.delete_profile")))
                .on_disabled_hover_text(lang.t("profiles.active_cannot_delete"))
                .clicked()
            {
                self.acts.push(Act::AskDeleteProfile(r.id.clone()));
            }
        });
    }

    /// A side-by-side diff: what is on the left (now) and on the right, row by row. Titles,
    /// triggers and step kinds only.
    fn diff_ui(&self, ui: &mut egui::Ui, rows: &[DiffRow], left: &str, right: &str, salt: &str) {
        let c = Colors::get();
        let lang = self.lang;
        let side = |l: &Option<profile::RuleLine>| match l {
            None => "—".to_string(),
            Some(l) => {
                let mut s = format!("{} · {} → {}", l.title, l.trigger, l.steps.join(", "));
                if !l.enabled {
                    s.push_str(&format!(" ({})", lang.t("state.disabled")));
                }
                s
            }
        };
        egui::ScrollArea::vertical()
            .id_salt(salt)
            .max_height(300.0)
            .show(ui, |ui| {
                egui::Grid::new(salt)
                    .num_columns(3)
                    .striped(true)
                    .spacing([10.0, 4.0])
                    .show(ui, |ui| {
                        ui.label("");
                        ui.label(egui::RichText::new(left).strong());
                        ui.label(egui::RichText::new(right).strong());
                        ui.end_row();
                        for r in rows {
                            let (mark, color) = match r.status {
                                DiffStatus::Same => ("=", c.text_muted),
                                DiffStatus::Changed => ("≠", c.warning),
                                DiffStatus::OnlyLeft => ("−", c.danger),
                                DiffStatus::OnlyRight => ("+", c.success),
                            };
                            ui.label(
                                egui::RichText::new(format!(
                                    "{mark} {}",
                                    lang.t(&format!("diff.{}", r.status.as_str()))
                                ))
                                .color(color),
                            );
                            ui.label(side(&r.left));
                            ui.label(side(&r.right));
                            ui.end_row();
                        }
                    });
            });
        if rows.is_empty() {
            ui.label(small(lang.t("diff.empty"), c.text_muted));
        }
    }

    /// Draws one of the profile dialogs.
    pub(super) fn profile_dialog(&mut self, ui: &mut egui::Ui, dialog: Dialog, focus: bool) {
        let c = Colors::get();
        let lang = self.lang;
        match dialog {
            Dialog::SwitchProfile { to } => {
                let Some(doc) = self.doc().cloned() else {
                    return;
                };
                let from = doc.active_profile.clone();
                let (Some(now), Some(next)) = (doc.profile(&from), doc.profile(&to)) else {
                    self.acts.push(Act::CloseDialog);
                    return;
                };
                title(ui, lang.t("dialog.switch"));
                ui.label(small(lang.t("dialog.switch_text"), c.text_muted));
                ui.label(format!(
                    "{} → {} ({}: {})",
                    now.title,
                    next.title,
                    lang.t("dialog.starts_in"),
                    next.modes
                        .iter()
                        .find(|m| m.id == next.default_mode)
                        .map_or(next.default_mode.as_str(), |m| m.title.as_str())
                ));
                let rows = profile::side_by_side(Some(now), Some(next), Align::Trigger);
                self.diff_ui(
                    ui,
                    &rows,
                    &format!("{}: {}", lang.t("dialog.now"), now.title),
                    &format!("{}: {}", lang.t("dialog.after"), next.title),
                    "switch-diff",
                );
                ui.horizontal(|ui| {
                    if cancel_button(ui, lang.t("btn.cancel"), focus) {
                        self.acts.push(Act::CloseDialog);
                    }
                    if ui.button(lang.t("btn.switch_to")).clicked() {
                        self.acts.push(Act::ApplyProfile(to.clone()));
                    }
                });
            }
            Dialog::EditProfile {
                id,
                mut name,
                mut description,
            } => {
                title(ui, lang.t("dialog.edit_profile"));
                let n = ui
                    .add(egui::TextEdit::singleline(&mut name).desired_width(360.0))
                    .accessible_name(lang.t("field.profile_name"));
                if focus {
                    n.request_focus();
                }
                ui.add(
                    egui::TextEdit::multiline(&mut description)
                        .desired_width(360.0)
                        .desired_rows(2),
                )
                .accessible_name(lang.t("field.profile_description"));
                ui.horizontal(|ui| {
                    if ui.button(lang.t("btn.cancel")).clicked() {
                        self.acts.push(Act::CloseDialog);
                    }
                    if ui
                        .add_enabled(
                            !name.trim().is_empty(),
                            egui::Button::new(lang.t("btn.save")),
                        )
                        .clicked()
                    {
                        self.acts.push(Act::SaveProfileFields {
                            id: id.clone(),
                            name: name.clone(),
                            description: description.clone(),
                        });
                    }
                });
                if let Some(Dialog::EditProfile {
                    name: n,
                    description: d,
                    ..
                }) = &mut self.dialog
                {
                    *n = name;
                    *d = description;
                }
            }
            Dialog::DeleteProfile { id, name } => {
                title(ui, lang.t("dialog.delete_profile"));
                ui.label(format!("{} ({id})?", name));
                ui.label(small(lang.t("dialog.delete_profile_text"), c.text_muted));
                ui.horizontal(|ui| {
                    if cancel_button(ui, lang.t("btn.cancel"), focus) {
                        self.acts.push(Act::CloseDialog);
                    }
                    if ui
                        .add(egui::Button::new(
                            egui::RichText::new(lang.t("btn.delete_profile")).color(c.danger),
                        ))
                        .clicked()
                    {
                        self.acts.push(Act::DeleteProfile(id.clone()));
                    }
                });
            }
            Dialog::ExportProfile {
                ids,
                mut path,
                mut env_values,
                done,
            } => {
                title(ui, lang.t("dialog.export_profile"));
                ui.label(small(lang.t("dialog.export_profile_text"), c.text_muted));
                ui.label(ids.join(", "));
                let r = ui
                    .add(egui::TextEdit::singleline(&mut path).desired_width(480.0))
                    .accessible_name(lang.t("dialog.file"));
                if focus {
                    r.request_focus();
                }
                ui.checkbox(&mut env_values, lang.t("field.env_values"));
                let mut new_done = done.clone();
                if let Some(d) = &done {
                    ui.label(egui::RichText::new(d).color(c.success));
                }
                ui.horizontal(|ui| {
                    if ui.button(lang.t("btn.close")).clicked() {
                        self.acts.push(Act::CloseDialog);
                    }
                    if done.is_none() && ui.button(lang.t("btn.export")).clicked() {
                        new_done = Some(self.export_profiles(&ids, &path, env_values));
                    }
                });
                if let Some(Dialog::ExportProfile {
                    path: p,
                    env_values: e,
                    done: d,
                    ..
                }) = &mut self.dialog
                {
                    *p = path;
                    *e = env_values;
                    *d = new_done;
                }
            }
            Dialog::ImportProfile {
                mut path,
                plan,
                error,
            } => {
                title(ui, lang.t("dialog.import_profile"));
                ui.label(small(lang.t("dialog.import_profile_text"), c.text_muted));
                let r = ui
                    .add(egui::TextEdit::singleline(&mut path).desired_width(480.0))
                    .accessible_name(lang.t("dialog.file"));
                if focus {
                    r.request_focus();
                }
                let mut next = (plan.clone(), error.clone());
                if ui.button(lang.t("dialog.check")).clicked() {
                    next = match self.plan_profile_import(&path) {
                        Ok(p) => (Some(Box::new(p)), None),
                        Err(e) => (None, Some(e)),
                    };
                }
                if let Some(e) = &error {
                    ui.label(egui::RichText::new(format!("✕ {e}")).color(c.danger));
                }
                if let Some(p) = &plan {
                    egui::ScrollArea::vertical()
                        .id_salt("import-preview")
                        .max_height(320.0)
                        .show(ui, |ui| self.import_preview(ui, p));
                }
                ui.horizontal(|ui| {
                    if cancel_button(ui, lang.t("btn.cancel"), false) {
                        self.acts.push(Act::CloseDialog);
                    }
                    if let Some(p) = plan.as_ref().filter(|p| !p.profiles.is_empty())
                        && ui.button(lang.t("btn.import_profiles")).clicked()
                    {
                        self.acts.push(Act::ImportProfiles(p.clone()));
                    }
                });
                if let Some(Dialog::ImportProfile {
                    path: pp,
                    plan: pl,
                    error: er,
                }) = &mut self.dialog
                {
                    *pp = path;
                    *pl = next.0;
                    *er = next.1;
                }
            }
            Dialog::History {
                id,
                versions,
                selected,
                mut label,
            } => {
                let current = self.profile(&id).cloned();
                let name = current.as_ref().map_or(id.as_str(), |p| p.title.as_str());
                title(ui, &format!("{} · {name}", lang.t("dialog.history")));
                if versions.is_empty() {
                    ui.label(small(lang.t("history.none"), c.text_muted));
                }
                let mut pick = selected;
                egui::ScrollArea::vertical()
                    .id_salt("history-list")
                    .max_height(180.0)
                    .show(ui, |ui| {
                        for (i, v) in versions.iter().enumerate() {
                            let mut text = format!(
                                "r{} · {} · {} {}",
                                v.revision.map_or_else(|| "?".into(), |r| r.to_string()),
                                v.at.as_deref().unwrap_or("?"),
                                v.rules,
                                lang.t("status.rules")
                            );
                            if v.save_point {
                                text.push_str(&format!(" · {}", lang.t("history.save_point")));
                            }
                            if let Some(l) = &v.label {
                                text.push_str(&format!(" “{l}”"));
                            }
                            if let Some(cmd) = &v.command {
                                text.push_str(&format!(" · {cmd}"));
                            }
                            if v.current {
                                text.push_str(&format!(" · {}", lang.t("history.current")));
                            }
                            if !v.working() {
                                text.push_str(&format!(" · {}", lang.t("history.has_errors")));
                            }
                            let r = ui.selectable_label(pick == Some(i), text);
                            if focus && i == 0 {
                                r.request_focus();
                            }
                            if r.clicked() {
                                pick = Some(i);
                            }
                        }
                    });
                if let Some(v) = pick.and_then(|i| versions.get(i)) {
                    ui.label(small(lang.t("history.compare"), c.text_secondary));
                    let rows =
                        profile::side_by_side(current.as_ref(), Some(&v.profile), Align::RuleId);
                    self.diff_ui(
                        ui,
                        &rows,
                        lang.t("dialog.now"),
                        &format!("r{}", v.revision.unwrap_or(0)),
                        "history-diff",
                    );
                }
                ui.horizontal_wrapped(|ui| {
                    ui.add(
                        egui::TextEdit::singleline(&mut label)
                            .hint_text(lang.t("field.save_label"))
                            .desired_width(180.0),
                    )
                    .accessible_name(lang.t("field.save_label"));
                    if current.is_some() && ui.button(lang.t("btn.save_point")).clicked() {
                        self.acts
                            .push(Act::SaveProfileVersion(id.clone(), label.clone()));
                    }
                });
                ui.horizontal_wrapped(|ui| {
                    if ui.button(lang.t("btn.close")).clicked() {
                        self.acts.push(Act::CloseDialog);
                    }
                    let target = profile::rollback_target(&versions, current.as_ref());
                    if let Some(t) = target
                        && ui.button(lang.t("btn.rollback")).clicked()
                    {
                        self.acts.push(Act::RestoreProfile(
                            Box::new(t.profile.clone()),
                            t.name.clone(),
                        ));
                    }
                    if let Some(v) = pick.and_then(|i| versions.get(i))
                        && !v.current
                        && ui.button(lang.t("btn.restore_version")).clicked()
                    {
                        self.acts.push(Act::RestoreProfile(
                            Box::new(v.profile.clone()),
                            v.name.clone(),
                        ));
                    }
                });
                if let Some(Dialog::History {
                    selected: s,
                    label: l,
                    ..
                }) = &mut self.dialog
                {
                    *s = pick;
                    *l = label;
                }
            }
            _ => {}
        }
    }

    fn import_preview(&self, ui: &mut egui::Ui, p: &profile::ImportPlan) {
        let c = Colors::get();
        let lang = self.lang;
        for ip in &p.profiles {
            let mut head = format!("{} ({})", ip.title, ip.id);
            if let Some(from) = &ip.renamed_from {
                head.push_str(&format!(" · {} {from}", lang.t("dialog.import_renamed")));
            }
            ui.label(egui::RichText::new(head).strong());
            ui.label(small(lang.t("import.not_active"), c.text_secondary));
            if !ip.restored_modes.is_empty() {
                ui.label(small(
                    format!(
                        "{}: {}",
                        lang.t("import.restored"),
                        ip.restored_modes.join(", ")
                    ),
                    c.warning,
                ));
            }
            for r in &ip.rules {
                ui.label(format!(
                    "• {} · {} → {}",
                    r.line.title,
                    r.line.trigger,
                    r.line.steps.join(", ")
                ));
                if !r.permissions.is_empty() {
                    let perms: Vec<&str> = r
                        .permissions
                        .iter()
                        .map(|x| lang.t(&format!("perm.short.{}", x.as_str())))
                        .collect();
                    ui.label(small(
                        format!("   {}: {}", lang.t("import.consent"), perms.join(", ")),
                        c.warning,
                    ));
                }
                for t in &r.targets {
                    ui.label(small(
                        format!("   {}: {t}", lang.t("dialog.import_opens")),
                        c.warning,
                    ));
                }
                if r.missing > 0 {
                    ui.label(small(format!("   {}", lang.t("import.missing")), c.warning));
                }
                for k in &r.conflicts {
                    let what = match k {
                        profile::Conflict::Rule(other) => {
                            format!("{} {other}", lang.t("import.conflict"))
                        }
                        profile::Conflict::Shortcut(a) => {
                            format!("{} {a}", lang.t("import.shortcut"))
                        }
                    };
                    ui.label(small(format!("   {what}"), c.danger));
                }
                if let Some(code) = &r.inactive {
                    ui.label(small(
                        format!("   {}: {code}", lang.t("import.inactive")),
                        c.danger,
                    ));
                }
                if let Some(from) = &r.renamed_from {
                    ui.label(small(
                        format!("   {} {from} → {}", lang.t("dialog.import_renamed"), r.id),
                        c.text_muted,
                    ));
                }
            }
        }
    }

    fn export_profiles(&mut self, ids: &[String], path: &str, env_values: bool) -> String {
        let Some(doc) = self.loaded.doc.as_ref() else {
            return "rules.json is not usable".into();
        };
        match profile::export(
            doc,
            ids,
            ExportOptions { env_values },
            &ddalgak::launch::process_var,
        ) {
            Ok(x) => match transfer::write_export(std::path::Path::new(path.trim()), &x.doc) {
                Ok(()) => format!(
                    "✓ {} {path} ({} {}, {} {})",
                    self.t("dialog.export_done"),
                    x.rules,
                    self.t("status.rules"),
                    x.env_values_removed,
                    self.t("export.env_removed")
                ),
                Err(e) => format!("✕ {e}"),
            },
            Err(e) => format!("✕ {}", e.message()),
        }
    }

    fn plan_profile_import(&self, path: &str) -> Result<profile::ImportPlan, String> {
        let bytes = std::fs::read(path.trim()).map_err(|e| format!("cannot read {path}: {e}"))?;
        let doc = self.loaded.doc.as_ref().ok_or("rules.json is not usable")?;
        profile::plan_import(&bytes, doc, &self.repo.context(), &ImportOptions::default())
            .map_err(|e| e.to_string())
    }

    /// Applies one of the profile acts.
    pub(super) fn apply_profile_act(&mut self, act: Act) {
        match act {
            Act::ProfileAdd(name) => {
                let label = format!("Added profile {name}");
                if self
                    .commit(label, "profile.add", move |d| {
                        profile::add(d, None, &name, None)
                    })
                    .is_some()
                {
                    self.new_profile_title.clear();
                }
            }
            Act::OpenSwitch(to) => {
                self.dialog = Some(Dialog::SwitchProfile { to });
                self.dialog_focus = true;
            }
            Act::ApplyProfile(to) => {
                let label = format!("Switched to profile {to}");
                let id = to.clone();
                if self
                    .commit(label, "profile.apply", move |d| profile::apply(d, &id))
                    .is_some()
                {
                    // The header names the new profile once the app confirmed the reload.
                    self.switching = Some(to);
                    self.dialog = None;
                }
            }
            Act::DuplicateProfile(id) => {
                self.commit(
                    format!("Duplicated profile {id}"),
                    "profile.duplicate",
                    move |d| profile::duplicate(d, &id, None, None),
                );
            }
            Act::OpenEditProfile(id) => {
                if let Some(p) = self.profile(&id) {
                    self.dialog = Some(Dialog::EditProfile {
                        name: p.title.clone(),
                        description: p.description.clone().unwrap_or_default(),
                        id,
                    });
                    self.dialog_focus = true;
                }
            }
            Act::SaveProfileFields {
                id,
                name,
                description,
            } => {
                let f = profile::ProfileFields {
                    title: Some(name),
                    description: Some(Some(description).filter(|d| !d.trim().is_empty())),
                };
                if self
                    .commit(format!("Renamed profile {id}"), "profile.edit", move |d| {
                        profile::edit(d, &id, &f)
                    })
                    .is_some()
                {
                    self.dialog = None;
                }
            }
            Act::AskDeleteProfile(id) => {
                if let Some(p) = self.profile(&id) {
                    self.dialog = Some(Dialog::DeleteProfile {
                        name: p.title.clone(),
                        id,
                    });
                    self.dialog_focus = true;
                }
            }
            Act::DeleteProfile(id) => {
                let label = format!("{} profile {id}", self.t("toast.deleted"));
                if self
                    .commit(label.clone(), "profile.delete", move |d| {
                        profile::delete(d, &id)
                    })
                    .is_some()
                {
                    self.toast = Some(Toast {
                        text: label,
                        undo: true,
                    });
                }
                self.dialog = None;
            }
            Act::OpenHistory(id) => {
                let current = self.profile(&id).cloned();
                let versions = profile::versions(&self.repo.files, &id, current.as_ref());
                self.dialog = Some(Dialog::History {
                    id,
                    versions,
                    selected: None,
                    label: String::new(),
                });
                self.dialog_focus = true;
            }
            Act::SaveProfileVersion(id, label) => {
                let label = label.trim().to_string();
                match self
                    .repo
                    .save_point(&id, Some(label.as_str()).filter(|l| !l.is_empty()))
                {
                    Ok(_) => {
                        self.banner = Some(Banner::Info(self.t("history.saved").into()));
                        self.apply_profile_act(Act::OpenHistory(id));
                    }
                    Err(e) => self.fail(&e),
                }
            }
            Act::RestoreProfile(version, name) => {
                let label = format!("Restored profile {} from {name}", version.id);
                let v = *version;
                if self
                    .commit(label, "profile.restore", move |d| profile::restore(d, &v))
                    .is_some()
                {
                    self.dialog = None;
                }
            }
            Act::OpenProfileExport(ids) => {
                let dir = std::env::var_os("USERPROFILE")
                    .map(|h| std::path::PathBuf::from(h).join("Documents"))
                    .filter(|d| d.is_dir())
                    .unwrap_or_else(std::env::temp_dir);
                let path = dir
                    .join(format!("ddalgak-profiles-{}.json", store::compact_time()))
                    .display()
                    .to_string();
                self.dialog = Some(Dialog::ExportProfile {
                    ids,
                    path,
                    env_values: false,
                    done: None,
                });
                self.dialog_focus = true;
            }
            Act::OpenProfileImport => {
                self.dialog = Some(Dialog::ImportProfile {
                    path: String::new(),
                    plan: None,
                    error: None,
                });
                self.dialog_focus = true;
            }
            Act::ImportProfiles(plan) => match self.repo.commit_profile_import(&plan) {
                Ok(c) => {
                    self.banner = None;
                    self.after_commit(format!("Imported {} profile(s)", plan.profiles.len()), &c);
                    self.dialog = None;
                }
                Err(e) => self.fail(&e),
            },
            _ => {}
        }
    }

    /// After a reload answer: a switch is confirmed when the app runs the new profile.
    pub(super) fn confirm_switch(
        &mut self,
        result: &Result<serde_json::Value, crate::sync::CallError>,
    ) {
        let Some(to) = self.switching.clone() else {
            return;
        };
        let name = self
            .profile(&to)
            .map_or_else(|| to.clone(), |p| p.title.clone());
        match result {
            Ok(v) if v["active_set"]["profile"] == to.as_str() && v["in_sync"] == true => {
                self.switching = None;
                self.banner = Some(Banner::Info(format!(
                    "{}: {name}",
                    self.t("profiles.switched")
                )));
            }
            Err(e) if e.code == "engine_not_running" => {
                self.switching = None;
                self.banner = Some(Banner::Info(format!(
                    "{}: {name}",
                    self.t("profiles.switch_saved")
                )));
            }
            _ => {}
        }
    }

    /// The header's profile: the one the app confirmed, or the saved one while it switches.
    pub fn switching_to(&self) -> Option<&str> {
        self.switching.as_deref()
    }
}
