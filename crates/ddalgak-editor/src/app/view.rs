//! Drawing the editor window: header, navigation rail, rule list, detail and form panel, status
//! line and dialogs. Every frame only reads the state and records [`Act`]s, applied afterwards.

use super::*;
use crate::theme;

/// The drag-and-drop payload of a rule row (reordering within its profile).
#[derive(Clone, Debug)]
struct DragRule {
    id: String,
    profile: String,
}

const ROW_H: f32 = 46.0;
const NARROW: f32 = 860.0;

fn small(text: impl Into<String>, color: egui::Color32) -> egui::RichText {
    egui::RichText::new(text.into())
        .size(Metrics::get().caption)
        .color(color)
}

fn badge_color(k: BadgeKind) -> egui::Color32 {
    let c = Colors::get();
    match k {
        BadgeKind::Conflict | BadgeKind::Problem => c.danger,
        BadgeKind::Warning | BadgeKind::Inactive => c.warning,
        BadgeKind::Review => c.info,
    }
}

/// A primary (accent) button.
fn primary(ui: &mut egui::Ui, text: &str) -> egui::Response {
    let c = Colors::get();
    ui.add(
        egui::Button::new(egui::RichText::new(text).color(c.text_on_accent).strong())
            .fill(c.accent),
    )
}

impl EditorApp {
    /// Draws one frame (also used by the headless tests).
    pub fn ui(&mut self, ui: &mut egui::Ui) {
        let ctx = ui.ctx().clone();
        if !self.themed {
            theme::apply(&ctx);
            self.themed = true;
            let c2 = ctx.clone();
            if let Ok(mut w) = self.wake.lock() {
                *w = Some(Box::new(move || c2.request_repaint()));
            }
        }
        self.poll();
        if ctx.input(|i| i.viewport().close_requested()) && !self.close_now && !self.request_close()
        {
            ctx.send_viewport_cmd(egui::ViewportCommand::CancelClose);
        }
        if self.close_now {
            ctx.send_viewport_cmd(egui::ViewportCommand::Close);
        }
        self.compute_visible();
        self.keys(&ctx);
        let narrow = ctx.content_rect().width() < NARROW;
        let c = Colors::get();
        egui::Panel::top("header")
            .frame(
                egui::Frame::new()
                    .fill(c.bg_raised)
                    .inner_margin(egui::Margin::symmetric(12, 8)),
            )
            .show(ui, |ui| self.header(ui, narrow));
        egui::Panel::bottom("status")
            .frame(
                egui::Frame::new()
                    .fill(c.bg_raised)
                    .inner_margin(egui::Margin::symmetric(12, 6)),
            )
            .show(ui, |ui| self.status_line(ui));
        if !narrow {
            egui::Panel::left("nav")
                .resizable(false)
                .exact_size(112.0)
                .frame(egui::Frame::new().fill(c.bg_raised).inner_margin(8))
                .show(ui, |ui| self.nav(ui));
        }
        let detail_open = self.form.is_some() || !self.selected.is_empty();
        if self.page == Page::Rules && !narrow && detail_open {
            egui::Panel::right("detail")
                .resizable(true)
                .default_size(420.0)
                .size_range(320.0..=720.0)
                .frame(
                    egui::Frame::new()
                        .fill(c.bg_base)
                        .inner_margin(egui::Margin::symmetric(14, 10)),
                )
                .show(ui, |ui| {
                    egui::ScrollArea::vertical()
                        .id_salt("detail-scroll")
                        .show(ui, |ui| self.detail(ui));
                });
        }
        if self.page == Page::Rules && narrow && self.form.is_none() && self.selected.len() == 1 {
            egui::Panel::bottom("detail-narrow")
                .resizable(true)
                .default_size(220.0)
                .frame(egui::Frame::new().fill(c.bg_base).inner_margin(10))
                .show(ui, |ui| {
                    egui::ScrollArea::vertical()
                        .id_salt("detail-scroll")
                        .show(ui, |ui| self.detail(ui));
                });
        }
        egui::CentralPanel::default()
            .frame(
                egui::Frame::new()
                    .fill(c.bg_base)
                    .inner_margin(egui::Margin::symmetric(14, 10)),
            )
            .show(ui, |ui| match self.page {
                Page::Modes => self.modes_page(ui),
                Page::Rules if narrow && self.form.is_some() => {
                    egui::ScrollArea::vertical()
                        .id_salt("detail-scroll")
                        .show(ui, |ui| self.detail(ui));
                }
                Page::Rules => self.list(ui),
            });
        if self.dialog.is_some() {
            self.dialog_ui(&ctx);
        }
        for a in std::mem::take(&mut self.acts) {
            self.apply(a);
        }
    }

    fn compute_visible(&mut self) {
        let shown: Vec<usize> = (0..self.rows.len())
            .filter(|i| self.filter.matches(&self.rows[*i]))
            .collect();
        let groups = listing::group(
            &self.rows,
            &shown,
            self.grouping,
            self.lang.t("group.every_mode"),
            self.lang.t("group.every_app"),
        );
        let mut seen = BTreeSet::new();
        self.visible = groups
            .iter()
            .flat_map(|g| g.rows.iter().copied())
            .filter(|i| seen.insert(*i))
            .collect();
    }

    // --- Header, navigation, status ----------------------------------------------------------

    fn header(&mut self, ui: &mut egui::Ui, narrow: bool) {
        let c = Colors::get();
        let m = Metrics::get();
        ui.horizontal_wrapped(|ui| {
            let (color, state) = match &self.engine {
                EngineView::Running { state, .. } => (
                    match state.as_str() {
                        "running" => c.success,
                        "paused" => c.warning,
                        _ => c.text_secondary,
                    },
                    Some(state.clone()),
                ),
                EngineView::Error { .. } => (c.danger, None),
                _ => (c.text_secondary, None),
            };
            ui.label(
                egui::RichText::new(self.t(self.engine.label_key()))
                    .color(color)
                    .size(m.emphasis)
                    .strong(),
            );
            match state.as_deref() {
                Some("off") => {
                    if ui.button(self.t("engine.turn_on")).clicked() {
                        self.acts.push(Act::Engine(Request::Start));
                    }
                }
                Some(s) => {
                    if ui.button(self.t("engine.turn_off")).clicked() {
                        self.acts.push(Act::Engine(Request::Stop));
                    }
                    let (label, req) = if s == "paused" {
                        (self.t("engine.resume"), Request::Resume)
                    } else {
                        (self.t("engine.pause"), Request::Pause)
                    };
                    if ui.button(label).clicked() {
                        self.acts.push(Act::Engine(req));
                    }
                }
                None => {
                    ui.label(small(self.t("engine.start_hint"), c.text_muted));
                }
            }
            ui.separator();
            let pid = self.active_profile_id();
            let ptitle = self
                .profile(&pid)
                .map_or_else(|| pid.clone(), |p| p.title.clone());
            ui.label(small(
                format!("{}: {ptitle}", self.t("header.profile")),
                c.text_secondary,
            ));
            let mode = match &self.engine {
                EngineView::Running {
                    mode: Some(m),
                    state,
                    ..
                } if state != "off" => Some(m.clone()),
                _ => None,
            };
            if let Some(mode) = mode {
                ui.label(small(
                    format!("{}: {mode}", self.t("header.mode")),
                    c.text_secondary,
                ));
            }
            if narrow {
                ui.separator();
                for (p, key) in [(Page::Rules, "nav.rules"), (Page::Modes, "nav.modes")] {
                    if ui.selectable_label(self.page == p, self.t(key)).clicked() {
                        self.acts.push(Act::Page(p));
                    }
                }
            }
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                if ui
                    .button("?")
                    .on_hover_text(self.t("help.f1"))
                    .accessible_name(self.t("dialog.help"))
                    .clicked()
                {
                    self.acts.push(Act::Help);
                }
                if ui.button(self.t("lang.switch")).clicked() {
                    self.acts.push(Act::SwitchLang);
                }
            });
        });
    }

    fn nav(&mut self, ui: &mut egui::Ui) {
        ui.vertical(|ui| {
            for (i, (p, key)) in [(Page::Rules, "nav.rules"), (Page::Modes, "nav.modes")]
                .into_iter()
                .enumerate()
            {
                let r = ui.add_sized(
                    [96.0, 28.0],
                    egui::Button::selectable(self.page == p, self.t(key)),
                );
                if i == 0 {
                    self.anchors[0] = Some(r.id);
                }
                if r.clicked() {
                    self.acts.push(Act::Page(p));
                }
            }
        });
    }

    fn status_line(&mut self, ui: &mut egui::Ui) {
        let c = Colors::get();
        ui.horizontal_wrapped(|ui| {
            let n = self.rows.len();
            let problems = self.rows.iter().filter(|r| r.has_problems()).count();
            let review = self.rows.iter().filter(|r| r.needs_review).count();
            let mut parts = vec![format!("{n} {}", self.t("status.rules"))];
            if problems > 0 {
                parts.push(format!("{problems} {}", self.t("status.problems")));
            }
            if review > 0 {
                parts.push(format!("{review} {}", self.t("status.review")));
            }
            if self.has_unsaved_changes() {
                parts.push(self.t("status.draft").to_string());
            }
            let rev = self.loaded.revision();
            let (key, r) = self.save.label().unwrap_or((
                if self.loaded.state.usable() {
                    "status.waiting"
                } else {
                    "status.kept_previous"
                },
                rev,
            ));
            let color = match &self.save {
                SaveState::Active { .. } => c.success,
                SaveState::KeptPrevious { .. } | SaveState::Unconfirmed { .. } => c.warning,
                _ => c.text_secondary,
            };
            let mut line = format!("{} ({} {r})", self.t(key), self.t("status.revision"));
            match &self.save {
                SaveState::KeptPrevious { reason, .. } => line.push_str(&format!(": {reason}")),
                SaveState::Unconfirmed { why, .. } => line.push_str(&format!(": {why}")),
                _ => {}
            }
            if !self.loaded.state.usable() {
                line = format!(
                    "rules.json is not usable ({}): {}",
                    self.loaded.state.as_str(),
                    self.loaded.state.problem().unwrap_or_default()
                );
            }
            ui.label(small(parts.join(" · "), c.text_secondary));
            ui.label(small("·", c.text_muted));
            ui.label(small(line, color))
                .on_hover_text(format!("{:?}", self.save));
            if let Some(t) = self.toast.clone() {
                ui.separator();
                ui.label(small(t.text, c.text_primary));
                if t.undo && ui.button(self.t("btn.undo")).clicked() {
                    self.acts.push(Act::Undo);
                    self.acts.push(Act::DismissToast);
                }
                if ui.button(self.t("btn.dismiss")).clicked() {
                    self.acts.push(Act::DismissToast);
                }
            }
        });
    }

    fn banner_ui(&mut self, ui: &mut egui::Ui) {
        let c = Colors::get();
        let Some(b) = self.banner.clone() else {
            return;
        };
        let (color, text, details) = match b {
            Banner::Error {
                code,
                message,
                details,
            } => (c.danger, format!("✕ {message} ({code})"), details),
            Banner::Info(m) => (c.info, format!("ⓘ {m}"), Vec::new()),
        };
        egui::Frame::new()
            .fill(c.bg_overlay)
            .stroke(egui::Stroke::new(1.0, color))
            .corner_radius(4)
            .inner_margin(8)
            .show(ui, |ui| {
                ui.horizontal_wrapped(|ui| {
                    ui.label(egui::RichText::new(text).color(color));
                    if ui.small_button(self.t("btn.dismiss")).clicked() {
                        self.banner = None;
                    }
                });
                for d in details {
                    ui.label(small(d, c.text_secondary));
                }
            });
    }

    // --- The rule list -------------------------------------------------------------------------

    fn list(&mut self, ui: &mut egui::Ui) {
        let c = Colors::get();
        let lang = self.lang;
        self.banner_ui(ui);
        ui.horizontal_wrapped(|ui| {
            let search = ui.add(
                egui::TextEdit::singleline(&mut self.filter.query)
                    .hint_text(lang.t("list.search"))
                    .desired_width(240.0),
            );
            let search = search.accessible_name(lang.t("list.search_label"));
            self.anchors[1] = Some(search.id);
            if std::mem::take(&mut self.focus_search) {
                search.request_focus();
            }
            if primary(ui, lang.t("list.new"))
                .on_hover_text(self.shortcuts.keys_of("rules.new").unwrap_or_default())
                .clicked()
            {
                self.acts.push(Act::New(None));
            }
            if ui.button(lang.t("btn.import")).clicked() {
                self.acts.push(Act::Import);
            }
            if ui.button(lang.t("btn.export")).clicked() {
                self.acts.push(Act::Export(self.selected.clone()));
            }
        });
        ui.horizontal_wrapped(|ui| {
            egui::ComboBox::from_id_salt("filter-state")
                .selected_text(format!(
                    "{}: {}",
                    lang.t("filter.state"),
                    lang.t(self.filter.state.key())
                ))
                .show_ui(ui, |ui| {
                    for s in StateFilter::ALL {
                        ui.selectable_value(&mut self.filter.state, s, lang.t(s.key()));
                    }
                })
                .response
                .accessible_name(lang.t("filter.state"));
            let (modes, apps) = self.doc().map(listing::filter_choices).unwrap_or_default();
            let profiles: Vec<(String, String)> = self
                .doc()
                .map(|d| {
                    d.profiles
                        .iter()
                        .map(|p| (p.id.clone(), p.title.clone()))
                        .collect()
                })
                .unwrap_or_default();
            let any = lang.t("filter.any");
            if profiles.len() > 1 {
                let cur = self
                    .filter
                    .profile
                    .as_ref()
                    .and_then(|p| profiles.iter().find(|x| &x.0 == p))
                    .map_or(any.to_string(), |x| x.1.clone());
                egui::ComboBox::from_id_salt("filter-profile")
                    .selected_text(format!("{}: {cur}", lang.t("filter.profile")))
                    .show_ui(ui, |ui| {
                        ui.selectable_value(&mut self.filter.profile, None, any);
                        for (id, t) in &profiles {
                            ui.selectable_value(&mut self.filter.profile, Some(id.clone()), t);
                        }
                    })
                    .response
                    .accessible_name(lang.t("filter.profile"));
            }
            if modes.len() > 1 {
                let cur = self
                    .filter
                    .mode
                    .as_ref()
                    .and_then(|m| modes.iter().find(|x| &x.0 == m))
                    .map_or(any.to_string(), |x| x.1.clone());
                egui::ComboBox::from_id_salt("filter-mode")
                    .selected_text(format!("{}: {cur}", lang.t("filter.mode")))
                    .show_ui(ui, |ui| {
                        ui.selectable_value(&mut self.filter.mode, None, any);
                        for (id, t) in &modes {
                            ui.selectable_value(&mut self.filter.mode, Some(id.clone()), t);
                        }
                    })
                    .response
                    .accessible_name(lang.t("filter.mode"));
            }
            if !apps.is_empty() {
                let cur = self.filter.app.clone().unwrap_or_else(|| any.to_string());
                egui::ComboBox::from_id_salt("filter-app")
                    .selected_text(format!("{}: {cur}", lang.t("filter.app")))
                    .show_ui(ui, |ui| {
                        ui.selectable_value(&mut self.filter.app, None, any);
                        for a in &apps {
                            ui.selectable_value(&mut self.filter.app, Some(a.clone()), a);
                        }
                    })
                    .response
                    .accessible_name(lang.t("filter.app"));
            }
            egui::ComboBox::from_id_salt("grouping")
                .selected_text(format!(
                    "{}: {}",
                    lang.t("group.label"),
                    lang.t(self.grouping.key())
                ))
                .show_ui(ui, |ui| {
                    for g in Grouping::ALL {
                        ui.selectable_value(&mut self.grouping, g, lang.t(g.key()));
                    }
                })
                .response
                .accessible_name(lang.t("group.label"));
            ui.checkbox(&mut self.filter.snippet_text, lang.t("list.snippet_search"));
        });
        if self.selected.len() > 1 {
            let sel = self.selected.clone();
            egui::Frame::new()
                .fill(c.accent_soft)
                .corner_radius(4)
                .inner_margin(6)
                .show(ui, |ui| {
                    ui.horizontal_wrapped(|ui| {
                        ui.label(format!("{} {}", sel.len(), lang.t("bulk.selected")));
                        if ui.button(lang.t("btn.enable")).clicked() {
                            self.acts.push(Act::SetEnabled(sel.clone(), true));
                        }
                        if ui.button(lang.t("btn.disable")).clicked() {
                            self.acts.push(Act::SetEnabled(sel.clone(), false));
                        }
                        if ui.button(lang.t("btn.duplicate")).clicked() {
                            self.acts.push(Act::Duplicate(sel.clone()));
                        }
                        if ui.button(lang.t("btn.delete")).clicked() {
                            self.acts.push(Act::AskDelete(sel.clone()));
                        }
                        if ui.button(lang.t("btn.export")).clicked() {
                            self.acts.push(Act::Export(sel.clone()));
                        }
                        if ui.button(lang.t("bulk.clear")).clicked() {
                            self.selected.clear();
                        }
                    });
                });
        }
        ui.add_space(4.0);
        if self.rows.is_empty() {
            self.empty_state(ui);
            return;
        }
        if self.visible.is_empty() {
            ui.label(small(lang.t("list.no_match"), c.text_muted));
            return;
        }
        let shown: Vec<usize> = (0..self.rows.len())
            .filter(|i| self.filter.matches(&self.rows[*i]))
            .collect();
        let groups = listing::group(
            &self.rows,
            &shown,
            self.grouping,
            lang.t("group.every_mode"),
            lang.t("group.every_app"),
        );
        if self.grouping == Grouping::None {
            let rows = groups
                .into_iter()
                .next()
                .map(|g| g.rows)
                .unwrap_or_default();
            egui::ScrollArea::vertical()
                .id_salt("rules")
                .auto_shrink([false, false])
                .show_rows(ui, ROW_H, rows.len(), |ui, range| {
                    for i in &rows[range] {
                        self.row_ui(ui, *i);
                    }
                });
        } else {
            egui::ScrollArea::vertical()
                .id_salt("rules")
                .auto_shrink([false, false])
                .show(ui, |ui| {
                    for (gi, g) in groups.iter().enumerate() {
                        ui.add_space(if gi == 0 { 0.0 } else { 6.0 });
                        ui.label(
                            egui::RichText::new(&g.heading)
                                .size(Metrics::get().emphasis)
                                .color(c.text_secondary)
                                .strong(),
                        );
                        for i in &g.rows {
                            self.row_ui(ui, *i);
                        }
                    }
                });
        }
    }

    fn empty_state(&mut self, ui: &mut egui::Ui) {
        let c = Colors::get();
        let lang = self.lang;
        ui.add_space(12.0);
        ui.label(egui::RichText::new(lang.t("list.empty")).size(Metrics::get().emphasis));
        ui.add_space(8.0);
        ui.label(lang.t("list.first_rule"));
        ui.horizontal_wrapped(|ui| {
            for (key, e) in [
                ("list.starter_remap", Effect::Remap),
                ("list.starter_text", Effect::TypeText),
                ("list.starter_program", Effect::OpenProgram),
            ] {
                if ui.button(lang.t(key)).clicked() {
                    self.acts.push(Act::New(Some(e)));
                }
            }
        });
        ui.add_space(8.0);
        ui.label(small(lang.t("header.emergency"), c.text_muted));
    }

    fn row_state(&self, r: &Row) -> (String, egui::Color32) {
        let c = Colors::get();
        let lang = self.lang;
        let active_now = self.save.is_active(self.loaded.revision());
        match r.state {
            "active" if active_now => (format!("● {}", lang.t("state.active")), c.success),
            "active" => (format!("○ {}", lang.t("state.ready")), c.text_muted),
            "disabled" => (lang.t("state.disabled").to_string(), c.text_muted),
            "other_mode" => (lang.t("state.other_mode").to_string(), c.text_muted),
            "other_profile" => (lang.t("state.other_profile").to_string(), c.text_muted),
            _ => (
                format!(
                    "{}: {}",
                    lang.t("badge.inactive"),
                    r.code.unwrap_or("rule_invalid")
                ),
                c.warning,
            ),
        }
    }

    fn row_ui(&mut self, ui: &mut egui::Ui, i: usize) {
        let c = Colors::get();
        let lang = self.lang;
        let r = self.rows[i].clone();
        let selected = self.selected.contains(&r.id);
        let at_cursor = self.cursor_row.as_deref() == Some(r.id.as_str());
        let (state, state_color) = self.row_state(&r);
        let frame = egui::Frame::new()
            .fill(if selected { c.bg_selected } else { c.bg_raised })
            .stroke(egui::Stroke::new(
                1.0,
                if at_cursor { c.focus } else { c.border_subtle },
            ))
            .corner_radius(4)
            .inner_margin(egui::Margin::symmetric(6, 3));
        let out = frame.show(ui, |ui| {
            ui.set_width(ui.available_width());
            ui.horizontal(|ui| {
                let handle = ui
                    .add(
                        egui::Button::new(small("≡", c.text_muted))
                            .frame(false)
                            .sense(egui::Sense::drag()),
                    )
                    .on_hover_text(lang.t("list.drag"))
                    .accessible_name(format!("{}: {}", lang.t("list.drag"), r.title));
                handle.dnd_set_drag_payload(DragRule {
                    id: r.id.clone(),
                    profile: r.profile.clone(),
                });
                let mut on = r.enabled;
                let cb = ui
                    .add(egui::Checkbox::without_text(&mut on))
                    .accessible_name(format!("{}: {}", lang.t("filter.enabled"), r.title));
                if cb.changed() {
                    self.acts.push(Act::SetEnabled(vec![r.id.clone()], on));
                }
                let text = format!("{}  →  {}", r.trigger, r.summary);
                let resp = ui.add(egui::Button::selectable(
                    selected,
                    egui::RichText::new(text).color(if r.enabled {
                        c.text_primary
                    } else {
                        c.text_secondary
                    }),
                ));
                if resp.clicked() {
                    let m = ui.input(|i| i.modifiers);
                    self.acts.push(Act::Select {
                        id: r.id.clone(),
                        ctrl: m.ctrl,
                        shift: m.shift,
                    });
                }
                if resp.double_clicked() && r.editable {
                    self.acts.push(Act::Edit(r.id.clone()));
                }
                for b in &r.badges {
                    ui.label(small(
                        format!("{} {}", b.kind.glyph(), lang.t(b.kind.key())),
                        badge_color(b.kind),
                    ))
                    .on_hover_text(format!("{} ({})", b.message, b.code));
                }
                ui.label(small(state, state_color));
            });
            let mut second = r.title.clone();
            if let Some(d) = &r.description {
                second.push_str(" — ");
                second.push_str(d);
            }
            ui.label(small(second, c.text_muted));
        });
        let rect = out.response.rect;
        let zone = ui.interact(
            rect,
            egui::Id::unique(("row-drop", &r.id)),
            egui::Sense::hover(),
        );
        if let Some(p) = zone.dnd_hover_payload::<DragRule>()
            && p.profile == r.profile
            && p.id != r.id
        {
            let y = ui
                .input(|i| i.pointer.hover_pos())
                .map_or(rect.center().y, |p| p.y);
            let line_y = if y < rect.center().y {
                rect.top()
            } else {
                rect.bottom()
            };
            ui.painter()
                .hline(rect.x_range(), line_y, egui::Stroke::new(2.0, c.accent));
        }
        if let Some(p) = zone.dnd_release_payload::<DragRule>()
            && p.profile == r.profile
            && p.id != r.id
        {
            let y = ui
                .input(|i| i.pointer.interact_pos())
                .map_or(rect.center().y, |p| p.y);
            let pos = if y < rect.center().y {
                Position::Before(r.id.clone())
            } else {
                Position::After(r.id.clone())
            };
            self.acts.push(Act::Move(p.id.clone(), pos));
        }
    }

    // --- Detail ----------------------------------------------------------------------------------

    fn detail(&mut self, ui: &mut egui::Ui) {
        let c = Colors::get();
        let lang = self.lang;
        if self.form.is_some() {
            self.form_ui(ui);
            return;
        }
        if self.selected.len() > 1 {
            ui.label(small(lang.t("bulk.hint"), c.text_muted));
            return;
        }
        let Some(id) = self.selected.first().cloned() else {
            ui.label(small(lang.t("detail.select_hint"), c.text_muted));
            return;
        };
        let Some(row) = self.rows.iter().find(|r| r.id == id).cloned() else {
            return;
        };
        let rule = self.doc().and_then(|d| {
            let (pi, ri) = d.find_rule(&id)?;
            d.profiles[pi].rules[ri].rule().cloned()
        });
        ui.label(
            egui::RichText::new(&row.title)
                .size(Metrics::get().title)
                .strong(),
        );
        if let Some(d) = &row.description {
            ui.label(small(d, c.text_secondary));
        }
        ui.add_space(6.0);
        egui::Grid::new("detail-grid")
            .num_columns(2)
            .spacing([12.0, 6.0])
            .show(ui, |ui| {
                ui.label(small(lang.t("form.when_press"), c.text_muted));
                ui.label(egui::RichText::new(&row.trigger).monospace());
                ui.end_row();
                ui.label(small(lang.t("form.do"), c.text_muted));
                ui.label(&row.summary);
                ui.end_row();
                ui.label(small(lang.t("filter.mode"), c.text_muted));
                ui.label(if row.mode_titles.is_empty() {
                    lang.t("group.every_mode").to_string()
                } else {
                    row.mode_titles.join(", ")
                });
                ui.end_row();
                if !row.apps.is_empty() {
                    ui.label(small(lang.t("filter.app"), c.text_muted));
                    ui.label(row.apps.join(", "));
                    ui.end_row();
                }
                let (state, color) = self.row_state(&row);
                ui.label(small(lang.t("detail.state"), c.text_muted));
                ui.label(egui::RichText::new(state).color(color));
                ui.end_row();
                if let Some(r) = &rule {
                    let needed = r.permissions_needed();
                    if !needed.is_empty() {
                        ui.label(small(lang.t("form.permission"), c.text_muted));
                        ui.horizontal_wrapped(|ui| {
                            let missing = ddalgak::validate::missing_consent(r);
                            for p in needed {
                                let short = lang.t(&format!("perm.short.{}", p.as_str()));
                                if missing.contains(&p) {
                                    ui.label(
                                        egui::RichText::new(format!(
                                            "? {short}: {}",
                                            lang.t("perm.missing")
                                        ))
                                        .color(c.info),
                                    );
                                } else {
                                    ui.label(
                                        egui::RichText::new(format!("✓ {short}")).color(c.success),
                                    );
                                }
                            }
                        });
                        ui.end_row();
                    }
                }
            });
        if let Some(reason) = &row.reason {
            ui.label(small(reason, c.warning));
        }
        for b in &row.badges {
            ui.label(
                egui::RichText::new(format!(
                    "{} {}: {} ({})",
                    b.kind.glyph(),
                    lang.t(b.kind.key()),
                    b.message,
                    b.code
                ))
                .color(badge_color(b.kind)),
            );
        }
        ui.add_space(8.0);
        ui.horizontal_wrapped(|ui| {
            let edit = ui.add_enabled(row.editable, egui::Button::new(lang.t("btn.edit")));
            self.anchors[2] = Some(edit.id);
            if edit.clicked() {
                self.acts.push(Act::Edit(id.clone()));
            }
            if ui
                .add_enabled(rule.is_some(), egui::Button::new(lang.t("btn.test")))
                .clicked()
            {
                self.acts.push(Act::Test(id.clone()));
            }
            if ui.button(lang.t("btn.duplicate")).clicked() {
                self.acts.push(Act::Duplicate(vec![id.clone()]));
            }
            let toggle = if row.enabled {
                "btn.disable"
            } else {
                "btn.enable"
            };
            if ui.button(lang.t(toggle)).clicked() {
                self.acts
                    .push(Act::SetEnabled(vec![id.clone()], !row.enabled));
            }
            if ui.button(lang.t("btn.delete")).clicked() {
                self.acts.push(Act::AskDelete(vec![id.clone()]));
            }
        });
        if !row.editable {
            let why = rule
                .as_ref()
                .and_then(|r| RuleForm::edit(&row.profile, r).unsupported)
                .unwrap_or_else(|| "it does not fit this version's rule format".into());
            ui.label(small(
                format!("{}: {why}", lang.t("form.not_editable")),
                c.text_muted,
            ));
        }
        if let Some((tid, sim)) = &self.test
            && *tid == id
        {
            ui.add_space(8.0);
            Self::simulation_ui(ui, lang, sim, &self.layout);
        }
    }

    fn simulation_ui(ui: &mut egui::Ui, lang: Lang, sim: &Simulation, layout: &Layout) {
        let c = Colors::get();
        egui::Frame::new()
            .fill(c.bg_overlay)
            .corner_radius(4)
            .inner_margin(8)
            .show(ui, |ui| {
                ui.label(small(lang.t("form.try_intro"), c.text_muted));
                for (i, l) in sim.lines.iter().enumerate() {
                    let color = if i == 0 {
                        if sim.fires { c.success } else { c.danger }
                    } else {
                        c.text_secondary
                    };
                    ui.label(egui::RichText::new(l).color(color));
                }
                if let Some(t) = &sim.typed {
                    let mut shown = t.clone();
                    ui.add(
                        egui::TextEdit::multiline(&mut shown)
                            .interactive(false)
                            .desired_rows(2)
                            .desired_width(f32::INFINITY),
                    )
                    .accessible_name(lang.t("form.as_typed"));
                }
                if let Some((from, to)) = sim.pointer {
                    screen_map(ui, layout, from, Some(to));
                }
            });
    }

    // --- The form --------------------------------------------------------------------------------

    fn form_ui(&mut self, ui: &mut egui::Ui) {
        let c = Colors::get();
        let lang = self.lang;
        let profile = {
            let pid = self
                .form
                .as_ref()
                .map(|f| f.profile.clone())
                .unwrap_or_default();
            self.loaded
                .doc
                .as_ref()
                .and_then(|d| d.profile(&pid))
                .cloned()
        };
        let Some(f) = self.form.as_mut() else {
            return;
        };
        let heading = if f.id.is_some() {
            "form.edit"
        } else {
            "form.new"
        };
        ui.horizontal_wrapped(|ui| {
            ui.label(
                egui::RichText::new(lang.t(heading))
                    .size(Metrics::get().title)
                    .strong(),
            );
            for s in Stage::ALL {
                if ui.selectable_label(f.stage == s, lang.t(s.key())).clicked() {
                    f.stage = s;
                }
            }
        });
        if let Some(why) = f.unsupported.clone() {
            ui.label(
                egui::RichText::new(format!("{}: {why}", lang.t("form.not_editable")))
                    .color(c.warning),
            );
            if ui.button(lang.t("btn.close")).clicked() {
                self.acts.push(Act::CloseForm);
            }
            return;
        }
        if self.external_change {
            egui::Frame::new()
                .stroke(egui::Stroke::new(1.0, c.warning))
                .inner_margin(6)
                .show(ui, |ui| {
                    ui.label(
                        egui::RichText::new(lang.t("form.changed_elsewhere")).color(c.warning),
                    );
                    if ui.button(lang.t("btn.reload")).clicked() {
                        self.acts.push(Act::ReloadFromDisk);
                    }
                });
        }
        let sentence = format!("{} — {}", f.trigger_text(), f.summary(profile.as_ref()));
        ui.label(small(sentence, c.text_secondary));
        ui.separator();
        let problems = f.problems(profile.as_ref());
        let stage = f.stage;
        let mut capture = std::mem::take(&mut self.capture);
        let mut target = self.capture_target;
        let mut acts = Vec::new();
        match stage {
            Stage::Keys => keys_stage(ui, lang, f, &mut capture, &mut target, &problems),
            Stage::Effect => effect_stage(ui, lang, f, profile.as_ref()),
            Stage::Details => details_stage(
                ui,
                lang,
                f,
                profile.as_ref(),
                &mut capture,
                &mut target,
                &problems,
                &self.layout,
                self.cursor,
                &mut self.new_mode_title,
                &mut acts,
                std::mem::take(&mut self.focus_title),
            ),
            Stage::Try => {}
        }
        self.capture = capture;
        self.capture_target = target;
        self.acts.extend(acts);
        if stage == Stage::Try {
            self.try_stage(ui, profile.as_ref(), &problems);
        }
        let Some(f) = self.form.as_mut() else {
            return;
        };
        ui.add_space(8.0);
        ui.separator();
        ui.horizontal_wrapped(|ui| {
            let idx = Stage::ALL.iter().position(|s| *s == f.stage).unwrap_or(0);
            if idx > 0 && ui.button(lang.t("btn.back")).clicked() {
                f.stage = Stage::ALL[idx - 1];
            }
            if idx + 1 < Stage::ALL.len() {
                let blocked = match f.stage {
                    Stage::Keys => problems.iter().any(|p| p.path.starts_with("trigger")),
                    Stage::Effect => f.fields.effect.is_none(),
                    _ => false,
                };
                if ui
                    .add_enabled(!blocked, egui::Button::new(lang.t("btn.next")))
                    .clicked()
                {
                    f.stage = Stage::ALL[idx + 1];
                }
            }
            if ui.button(lang.t("btn.cancel")).clicked() {
                self.acts.push(Act::CloseForm);
            }
        });
    }

    fn try_stage(
        &mut self,
        ui: &mut egui::Ui,
        profile: Option<&Profile>,
        problems: &[ddalgak::text::FieldProblem],
    ) {
        let c = Colors::get();
        let lang = self.lang;
        let doc_problems = self.candidate_problems(profile);
        let Some(f) = self.form.as_mut() else {
            return;
        };
        ui.label(small(lang.t("form.try_intro"), c.text_muted));
        let can = problems.is_empty();
        if ui
            .add_enabled(
                can,
                egui::Button::new(format!("▶ {}", lang.t("form.simulate"))),
            )
            .clicked()
        {
            let sim = f.simulate(profile, &self.layout, self.cursor);
            self.test = Some((f.id.clone().unwrap_or_default(), sim));
        }
        if let Some((_, sim)) = &self.test {
            Self::simulation_ui(ui, lang, sim, &self.layout);
        }
        for p in problems {
            ui.label(egui::RichText::new(format!("✕ {}", p.message)).color(c.danger));
        }
        for p in &doc_problems {
            let color = if p.severity == ddalgak::validate::Severity::Error {
                c.danger
            } else {
                c.warning
            };
            let glyph = if p.severity == ddalgak::validate::Severity::Error {
                "✕"
            } else {
                "⚠"
            };
            ui.label(
                egui::RichText::new(format!("{glyph} {} ({})", p.message, p.code)).color(color),
            );
            for o in &p.offers {
                if let ddalgak::validate::Offer::Rebind(k) = o
                    && ui
                        .button(format!("{} {k}", lang.t("form.try_keys")))
                        .clicked()
                {
                    f.fields.trigger.keys = k.clone();
                    f.stage = Stage::Keys;
                }
            }
        }
        let perms = f.permissions(profile);
        if !perms.is_empty() {
            let words: Vec<&str> = perms
                .iter()
                .map(|p| lang.t(&format!("perm.short.{}", p.as_str())))
                .collect();
            ui.label(small(
                format!("{}: {}", lang.t("form.permission"), words.join(", ")),
                c.text_secondary,
            ));
        }
        let blocked = !problems.is_empty()
            || doc_problems
                .iter()
                .any(|p| p.severity == ddalgak::validate::Severity::Error);
        ui.add_space(6.0);
        ui.horizontal_wrapped(|ui| {
            if f.id.is_some() {
                ui.checkbox(&mut f.fields.enabled, lang.t("form.enabled"));
                if ui
                    .add_enabled(!blocked && f.dirty(), egui::Button::new(lang.t("btn.save")))
                    .clicked()
                {
                    self.acts.push(Act::SaveForm);
                }
            } else {
                if ui
                    .add_enabled(!blocked, egui::Button::new(lang.t("btn.save_off")))
                    .clicked()
                {
                    f.fields.enabled = false;
                    self.acts.push(Act::SaveForm);
                }
                let c = Colors::get();
                let save = ui.add_enabled(
                    !blocked,
                    egui::Button::new(
                        egui::RichText::new(lang.t("btn.save_enable")).color(c.text_on_accent),
                    )
                    .fill(c.accent),
                );
                if save.clicked() {
                    f.fields.enabled = true;
                    self.acts.push(Act::SaveForm);
                }
            }
        });
    }

    /// The document checks for the form's rule as if it were saved now (duplicate triggers,
    /// collisions with Ddalgak's own shortcuts, warnings), for the Try step.
    fn candidate_problems(&self, profile: Option<&Profile>) -> Vec<ddalgak::validate::Problem> {
        let (Some(f), Some(doc)) = (&self.form, &self.loaded.doc) else {
            return Vec::new();
        };
        let id =
            f.id.clone()
                .unwrap_or_else(|| ddalgak::model::new_rule_id(|c| doc.has_rule_id(c)));
        let Ok(mut rule) = f.build(&id, profile) else {
            return Vec::new();
        };
        rule.id = id.clone();
        let mut d = doc.clone();
        match d.find_rule(&id) {
            Some((pi, ri)) => {
                d.profiles[pi].rules[ri] = ddalgak::model::RuleEntry::Typed(Box::new(rule));
            }
            None => {
                if let Some(pi) = d.profile_index(&f.profile) {
                    d.profiles[pi]
                        .rules
                        .push(ddalgak::model::RuleEntry::Typed(Box::new(rule)));
                }
            }
        }
        // Consent is asked when saving; a missing grant is not a problem of the form.
        let r = ddalgak::validate::check(&d.to_value(), &self.repo.context());
        r.all()
            .filter(|p| p.rule.as_deref() == Some(id.as_str()))
            .filter(|p| p.severity != ddalgak::validate::Severity::Info)
            .filter(|p| !p.code.starts_with("consent"))
            .filter(|p| p.code != "schema_violation" || !p.message.contains("consent"))
            .cloned()
            .collect()
    }

    // --- Modes page ------------------------------------------------------------------------------

    fn modes_page(&mut self, ui: &mut egui::Ui) {
        let c = Colors::get();
        let lang = self.lang;
        self.banner_ui(ui);
        let pid = self.active_profile_id();
        let Some(p) = self.profile(&pid).cloned() else {
            return;
        };
        ui.label(
            egui::RichText::new(format!("{} · {}", lang.t("nav.modes"), p.title))
                .size(Metrics::get().title)
                .strong(),
        );
        for m in &p.modes {
            let is_default = m.id == p.default_mode;
            ui.horizontal_wrapped(|ui| {
                if let Some(icon) = &m.icon {
                    ui.label(egui::RichText::new(format!("[{icon}]")).monospace());
                }
                ui.label(egui::RichText::new(&m.title).strong());
                let mut notes = Vec::new();
                if is_default {
                    notes.push(lang.t("modes.default").to_string());
                }
                if let Some(parent) = &m.parent {
                    let pt = p
                        .modes
                        .iter()
                        .find(|x| &x.id == parent)
                        .map_or(parent.as_str(), |x| x.title.as_str());
                    notes.push(format!("{} {pt}", lang.t("modes.layer_over")));
                }
                let on = m.enabled != Some(false);
                if !on {
                    notes.push(lang.t("state.disabled").to_string());
                }
                ui.label(small(notes.join(" · "), c.text_muted));
                let rules = ddalgak::mode_edit::rules_of(&p, &m.id).len();
                ui.label(small(
                    format!("{rules} {}", lang.t("status.rules")),
                    c.text_muted,
                ));
                if !is_default {
                    let label = if on { "btn.disable" } else { "btn.enable" };
                    if ui.small_button(lang.t(label)).clicked() {
                        self.acts.push(Act::ModeEnabled(m.id.clone(), !on));
                    }
                }
            });
        }
        ui.add_space(8.0);
        ui.horizontal(|ui| {
            ui.add(
                egui::TextEdit::singleline(&mut self.new_mode_title)
                    .hint_text(lang.t("field.new_mode"))
                    .desired_width(200.0),
            )
            .accessible_name(lang.t("field.new_mode"));
            if ui
                .add_enabled(
                    !self.new_mode_title.trim().is_empty(),
                    egui::Button::new(lang.t("modes.add")),
                )
                .clicked()
            {
                self.acts.push(Act::NewMode(self.new_mode_title.clone()));
            }
        });
        ui.label(small(lang.t("modes.note"), c.text_muted));
    }

    // --- Dialogs ---------------------------------------------------------------------------------

    fn dialog_ui(&mut self, ctx: &egui::Context) {
        let c = Colors::get();
        let lang = self.lang;
        let Some(dialog) = self.dialog.clone() else {
            return;
        };
        let focus = std::mem::take(&mut self.dialog_focus);
        let modal = egui::Modal::new(egui::Id::unique("editor-dialog")).show(ctx, |ui| {
            ui.set_max_width(520.0);
            match dialog {
                Dialog::Delete { ids, titles } => {
                    ui.label(
                        egui::RichText::new(lang.t("dialog.delete"))
                            .size(Metrics::get().title)
                            .strong(),
                    );
                    ui.label(format!(
                        "{} {}: {}?",
                        lang.t("btn.delete"),
                        ids.len(),
                        titles.join(", ")
                    ));
                    ui.horizontal(|ui| {
                        let cancel = ui.button(lang.t("btn.cancel"));
                        if focus {
                            cancel.request_focus();
                        }
                        if cancel.clicked() {
                            self.acts.push(Act::CloseDialog);
                        }
                        if ui
                            .add(egui::Button::new(
                                egui::RichText::new(format!(
                                    "{} ({})",
                                    lang.t("btn.delete"),
                                    ids.len()
                                ))
                                .color(c.danger),
                            ))
                            .clicked()
                        {
                            self.acts.push(Act::Delete(ids.clone()));
                        }
                    });
                }
                Dialog::Discard(then) => {
                    ui.label(
                        egui::RichText::new(lang.t("dialog.discard"))
                            .size(Metrics::get().title)
                            .strong(),
                    );
                    ui.label(lang.t("dialog.discard_text"));
                    ui.horizontal(|ui| {
                        let keep = ui.button(lang.t("btn.keep_editing"));
                        if focus {
                            keep.request_focus();
                        }
                        if keep.clicked() {
                            self.acts.push(Act::CloseDialog);
                        }
                        if ui.button(lang.t("btn.discard")).clicked() {
                            self.acts.push(Act::Discard(then.clone()));
                        }
                    });
                }
                Dialog::Consent { perms, targets, .. } => {
                    ui.label(
                        egui::RichText::new(lang.t("dialog.consent"))
                            .size(Metrics::get().title)
                            .strong(),
                    );
                    for p in &perms {
                        ui.label(lang.t(&form::permission_key(*p)));
                    }
                    for t in &targets {
                        ui.label(egui::RichText::new(t).monospace().color(c.text_primary));
                    }
                    ui.label(small(lang.t("dialog.consent_text"), c.text_muted));
                    ui.horizontal(|ui| {
                        let cancel = ui.button(lang.t("btn.cancel"));
                        if focus {
                            cancel.request_focus();
                        }
                        if cancel.clicked() {
                            self.acts.push(Act::CloseDialog);
                        }
                        if ui.button(lang.t("btn.allow")).clicked() {
                            self.acts.push(Act::Allow);
                        }
                    });
                }
                Dialog::Export {
                    ids,
                    mut path,
                    done,
                } => {
                    ui.label(
                        egui::RichText::new(lang.t("dialog.export"))
                            .size(Metrics::get().title)
                            .strong(),
                    );
                    ui.label(small(lang.t("dialog.export_text"), c.text_muted));
                    ui.label(format!("{} {}", ids.len(), lang.t("status.rules")));
                    let r = ui
                        .add(egui::TextEdit::singleline(&mut path).desired_width(480.0))
                        .accessible_name(lang.t("dialog.file"));
                    if focus {
                        r.request_focus();
                    }
                    let mut new_done = done.clone();
                    if let Some(d) = &done {
                        ui.label(egui::RichText::new(d).color(c.success));
                    }
                    ui.horizontal(|ui| {
                        if ui.button(lang.t("btn.close")).clicked() {
                            self.acts.push(Act::CloseDialog);
                        }
                        if done.is_none() && primary(ui, lang.t("btn.export")).clicked() {
                            new_done = Some(self.export(&ids, &path));
                        }
                    });
                    if let Some(Dialog::Export {
                        path: p, done: d, ..
                    }) = &mut self.dialog
                    {
                        *p = path;
                        *d = new_done;
                    }
                }
                Dialog::Import {
                    mut path,
                    plan,
                    error,
                } => {
                    ui.label(
                        egui::RichText::new(lang.t("dialog.import"))
                            .size(Metrics::get().title)
                            .strong(),
                    );
                    ui.label(small(lang.t("dialog.import_text"), c.text_muted));
                    let r = ui
                        .add(egui::TextEdit::singleline(&mut path).desired_width(480.0))
                        .accessible_name(lang.t("dialog.file"));
                    if focus {
                        r.request_focus();
                    }
                    let mut next_plan = plan.clone();
                    let mut next_error = error.clone();
                    if ui.button(lang.t("dialog.check")).clicked() {
                        match self.plan_import(&path) {
                            Ok(p) => {
                                next_plan = Some(Box::new(p));
                                next_error = None;
                            }
                            Err(e) => {
                                next_plan = None;
                                next_error = Some(e);
                            }
                        }
                    }
                    if let Some(e) = &error {
                        ui.label(egui::RichText::new(format!("✕ {e}")).color(c.danger));
                    }
                    if let Some(p) = &plan {
                        ui.label(format!(
                            "{} {} → {}",
                            p.rules.len(),
                            lang.t("dialog.import_adds"),
                            p.target_profile
                        ));
                        for r in &p.rules {
                            ui.label(format!("• {} — {}", r.title, r.summary));
                            for t in &r.launch_targets {
                                ui.label(small(
                                    format!("   {}: {t}", lang.t("dialog.import_opens")),
                                    c.warning,
                                ));
                            }
                            if r.injects {
                                ui.label(small(
                                    format!("   {}", lang.t("dialog.import_injects")),
                                    c.warning,
                                ));
                            }
                            if let Some(from) = &r.renamed_from {
                                ui.label(small(
                                    format!(
                                        "   {} {from} → {}",
                                        lang.t("dialog.import_renamed"),
                                        r.id
                                    ),
                                    c.text_muted,
                                ));
                            }
                        }
                        if !p.new_modes.is_empty() {
                            ui.label(small(
                                format!(
                                    "{}: {}",
                                    lang.t("dialog.import_modes"),
                                    p.new_modes.join(", ")
                                ),
                                c.text_muted,
                            ));
                        }
                    }
                    ui.horizontal(|ui| {
                        if ui.button(lang.t("btn.cancel")).clicked() {
                            self.acts.push(Act::CloseDialog);
                        }
                        if plan.as_ref().is_some_and(|p| !p.rules.is_empty())
                            && ui.button(lang.t("btn.add_disabled")).clicked()
                            && let Some(p) = plan.clone()
                        {
                            self.import(&p);
                        }
                    });
                    if let Some(Dialog::Import {
                        path: pp,
                        plan: pl,
                        error: er,
                    }) = &mut self.dialog
                    {
                        *pp = path;
                        *pl = next_plan;
                        *er = next_error;
                    }
                }
                Dialog::Help => {
                    ui.label(
                        egui::RichText::new(lang.t("dialog.help"))
                            .size(Metrics::get().title)
                            .strong(),
                    );
                    egui::Grid::new("help-grid")
                        .num_columns(2)
                        .spacing([16.0, 4.0])
                        .show(ui, |ui| {
                            let m = ddalgak::shortcuts::manifest();
                            for b in &self.shortcuts.bindings {
                                let title = m
                                    .actions
                                    .iter()
                                    .find(|a| a.id == b.action)
                                    .map_or(b.action.as_str(), |a| a.title.as_str());
                                ui.label(egui::RichText::new(&b.keys).monospace());
                                ui.label(title);
                                ui.end_row();
                            }
                            for (k, what) in shortcuts::FIXED {
                                ui.label(egui::RichText::new(*k).monospace());
                                ui.label(*what);
                                ui.end_row();
                            }
                        });
                    if self.shortcuts.off {
                        ui.label(small(lang.t("help.off"), c.warning));
                    }
                    ui.label(small(lang.t("help.rebind"), c.text_muted));
                    let close = ui.button(lang.t("btn.close"));
                    if focus {
                        close.request_focus();
                    }
                    if close.clicked() {
                        self.acts.push(Act::CloseDialog);
                    }
                }
            }
        });
        if modal.should_close() {
            self.acts.push(Act::CloseDialog);
        }
    }

    fn export(&mut self, ids: &[String], path: &str) -> String {
        let Some(doc) = self.loaded.doc.as_ref() else {
            return "nothing to export".into();
        };
        let out = transfer::export_rules(doc, ids, "Exported rules");
        match transfer::write_export(std::path::Path::new(path.trim()), &out) {
            Ok(()) => format!("✓ {} {path}", self.t("dialog.export_done")),
            Err(e) => format!("✕ {e}"),
        }
    }

    fn plan_import(&self, path: &str) -> Result<ImportPlan, String> {
        let bytes = std::fs::read(path.trim()).map_err(|e| format!("cannot read {path}: {e}"))?;
        let doc = self.loaded.doc.as_ref().ok_or("rules.json is not usable")?;
        let target = self
            .form
            .as_ref()
            .map_or_else(|| self.active_profile_id(), |f| f.profile.clone());
        transfer::plan_import(&bytes, doc, &target)
    }

    fn import(&mut self, plan: &ImportPlan) {
        let p = plan.clone();
        let n = p.rules.len();
        if let Some(c) = self.commit(format!("Imported {n} rule(s)"), "rule.import", move |d| {
            transfer::apply_import(d, &p)
        }) {
            self.selected = c.change.rules;
            self.dialog = None;
            self.filter = Filter::default();
        }
    }
}

/// The small screen map under the Mouse form and in the simulation: every monitor at its place,
/// the simulated pointer (ring) and the target (disc). Nothing moves.
fn screen_map(ui: &mut egui::Ui, layout: &Layout, from: Point, to: Option<Point>) {
    let c = Colors::get();
    let (w, h) = (220.0f32, 110.0f32);
    let map = mouse::screen_map(layout, from, to.map(|p| (p, false)), w as i32, h as i32);
    let (rect, _) = ui.allocate_exact_size(egui::vec2(w, h), egui::Sense::hover());
    let p = ui.painter_at(rect);
    p.rect_filled(rect, 4.0, c.bg_base);
    let at = |x: i32, y: i32| rect.min + egui::vec2(x as f32, y as f32);
    for m in &map.monitors {
        let r =
            egui::Rect::from_min_max(at(m.rect.left, m.rect.top), at(m.rect.right, m.rect.bottom));
        p.rect_stroke(
            r.shrink(1.0),
            2.0,
            egui::Stroke::new(
                1.0,
                if m.primary {
                    c.border_strong
                } else {
                    c.border_subtle
                },
            ),
            egui::StrokeKind::Inside,
        );
        p.text(
            r.left_top() + egui::vec2(3.0, 2.0),
            egui::Align2::LEFT_TOP,
            format!("{}%", m.scale_percent),
            egui::FontId::proportional(9.0),
            c.text_muted,
        );
    }
    p.circle_stroke(
        at(map.from.x, map.from.y),
        4.0,
        egui::Stroke::new(1.5, c.text_secondary),
    );
    if let Some(t) = map.to {
        p.circle_filled(at(t.x, t.y), 3.5, c.accent);
    }
}

fn problems_for(ui: &mut egui::Ui, problems: &[ddalgak::text::FieldProblem], prefix: &[&str]) {
    let c = Colors::get();
    for p in problems
        .iter()
        .filter(|p| prefix.iter().any(|x| p.path.starts_with(x)))
    {
        ui.label(egui::RichText::new(format!("✕ {}", p.message)).color(c.danger));
    }
}

/// A key field: the capture button and the "type instead" text.
#[allow(clippy::too_many_arguments)]
fn key_field(
    ui: &mut egui::Ui,
    lang: Lang,
    label: &str,
    value: &mut String,
    capture: &mut Capture,
    target: &mut CaptureTarget,
    me: CaptureTarget,
    heading: bool,
) {
    let c = Colors::get();
    if heading {
        ui.label(egui::RichText::new(label).strong());
    }
    ui.horizontal_wrapped(|ui| {
        let recording = capture.active && *target == me;
        let shown = if recording || value.trim().is_empty() {
            lang.t("form.capture").to_string()
        } else {
            value.clone()
        };
        let btn = ui.add(
            egui::Button::new(egui::RichText::new(shown).monospace())
                .min_size(egui::vec2(200.0, 28.0))
                .stroke(egui::Stroke::new(
                    if recording { 2.0 } else { 1.0 },
                    if recording { c.focus } else { c.border_strong },
                )),
        );
        let btn = btn.accessible_name(format!("{}: {label}", lang.t("form.capture_label")));
        if btn.clicked() {
            capture.active = true;
            *target = me;
            btn.request_focus();
        }
        if recording
            && !btn.has_focus()
            && !btn.clicked()
            && btn.ctx.memory(|m| m.focused().is_some())
        {
            capture.active = false;
        }
        ui.add(
            egui::TextEdit::singleline(value)
                .hint_text(lang.t("form.type_instead"))
                .desired_width(160.0),
        )
        .accessible_name(format!("{}: {label}", lang.t("form.type_instead")));
    });
    if !value.trim().is_empty() {
        ui.label(small(capture::announce(value), c.text_muted));
    }
}

fn keys_stage(
    ui: &mut egui::Ui,
    lang: Lang,
    f: &mut RuleForm,
    capture: &mut Capture,
    target: &mut CaptureTarget,
    problems: &[ddalgak::text::FieldProblem],
) {
    let c = Colors::get();
    let t = &mut f.fields.trigger;
    let radio = f.fields.effect.is_none_or(Effect::allows_hotstring);
    if radio {
        ui.horizontal(|ui| {
            ui.radio_value(&mut t.hotstring, false, lang.t("form.when_press"));
            ui.radio_value(&mut t.hotstring, true, lang.t("form.when_type"));
        });
    } else {
        t.hotstring = false;
    }
    if t.hotstring {
        let lbl = ui.label(egui::RichText::new(lang.t("form.abbreviation")).strong());
        ui.add(
            egui::TextEdit::singleline(&mut t.abbreviation)
                .hint_text("btw")
                .desired_width(160.0),
        )
        .labelled_by(lbl.id);
        problems_for(ui, problems, &["trigger"]);
        return;
    }
    key_field(
        ui,
        lang,
        lang.t("form.when_press"),
        &mut t.keys,
        capture,
        target,
        CaptureTarget::Trigger,
        !radio,
    );
    ui.checkbox(&mut capture.add_step, lang.t("form.add_step"));
    ui.label(small(lang.t("form.capture_hint"), c.text_muted));
    ui.label(small(lang.t("form.special_keys"), c.text_muted));
    ui.horizontal_wrapped(|ui| {
        for k in capture::SPECIAL_KEYS {
            if ui.small_button(k).clicked() {
                t.keys = k.to_string();
            }
        }
    });
    if capture.composing {
        ui.label(egui::RichText::new(lang.t("form.ime")).color(c.warning));
    }
    ui.horizontal_wrapped(|ui| {
        ui.radio_value(&mut t.on, TriggerOn::Press, lang.t("form.on_press"));
        ui.radio_value(&mut t.on, TriggerOn::Release, lang.t("form.on_release"));
        ui.radio_value(&mut t.on, TriggerOn::Hold, lang.t("form.on_hold"));
        if t.on == TriggerOn::Hold {
            ui.add(
                egui::DragValue::new(&mut t.hold_ms)
                    .range(200..=5000)
                    .suffix(" ms"),
            )
            .accessible_name(lang.t("form.hold_ms"));
        }
    });
    ui.checkbox(&mut t.pass_through, lang.t("form.pass_through"));
    problems_for(ui, problems, &["trigger"]);
}

fn effect_stage(ui: &mut egui::Ui, lang: Lang, f: &mut RuleForm, profile: Option<&Profile>) {
    let c = Colors::get();
    ui.label(egui::RichText::new(lang.t("form.do")).strong());
    egui::Grid::new("tiles")
        .num_columns(4)
        .spacing([8.0, 8.0])
        .show(ui, |ui| {
            for (i, e) in Effect::TILES.into_iter().enumerate() {
                let title = lang.t(&format!("tile.{}", e.key()));
                let example = lang.t(&format!("tile.{}.example", e.key()));
                let chosen = f.fields.effect == Some(e);
                let b = ui
                    .add(
                        egui::Button::selectable(
                            chosen,
                            egui::RichText::new(format!("{}  {title}\n{example}", e.icon())),
                        )
                        .min_size(egui::vec2(120.0, 44.0)),
                    )
                    .accessible_name(title);
                if b.clicked() {
                    f.fields.effect = Some(e);
                    f.refresh_title(profile);
                    f.stage = Stage::Details;
                }
                if i % 4 == 3 {
                    ui.end_row();
                }
            }
        });
    ui.horizontal(|ui| {
        ui.label(small(lang.t("form.more"), c.text_muted));
        let title = lang.t("tile.block");
        if ui
            .selectable_label(f.fields.effect == Some(Effect::Block), title)
            .accessible_name(title)
            .clicked()
        {
            f.fields.effect = Some(Effect::Block);
            f.refresh_title(profile);
            f.stage = Stage::Details;
        }
    });
}

#[allow(clippy::too_many_arguments)]
fn details_stage(
    ui: &mut egui::Ui,
    lang: Lang,
    f: &mut RuleForm,
    profile: Option<&Profile>,
    capture: &mut Capture,
    target: &mut CaptureTarget,
    problems: &[ddalgak::text::FieldProblem],
    layout: &Layout,
    cursor: Point,
    new_mode: &mut String,
    acts: &mut Vec<Act>,
    focus_title: bool,
) {
    let c = Colors::get();
    let Some(effect) = f.fields.effect else {
        ui.label(lang.t("form.do"));
        return;
    };
    ui.label(
        egui::RichText::new(format!(
            "{}  {}",
            effect.icon(),
            lang.t(&format!("tile.{}", effect.key()))
        ))
        .strong(),
    );
    let name = ui
        .add(egui::TextEdit::singleline(&mut f.fields.title).desired_width(320.0))
        .accessible_name(lang.t("form.title"));
    if focus_title {
        name.request_focus();
    }
    if name.changed() {
        f.fields.title_auto = false;
    }
    problems_for(ui, problems, &["title"]);
    ui.add_space(4.0);
    let fl = &mut f.fields;
    match effect {
        Effect::Remap => {
            key_field(
                ui,
                lang,
                lang.t("field.acts_as"),
                &mut fl.remap.to,
                capture,
                target,
                CaptureTarget::RemapTo,
                true,
            );
            key_field(
                ui,
                lang,
                lang.t("field.tap"),
                &mut fl.remap.tap,
                capture,
                target,
                CaptureTarget::RemapTap,
                true,
            );
            if !fl.remap.tap.trim().is_empty() {
                ui.add(
                    egui::DragValue::new(&mut fl.remap.tap_ms)
                        .range(100..=1000)
                        .suffix(" ms"),
                )
                .accessible_name(lang.t("field.tap_ms"));
            }
            let trig = fl.trigger.keys.clone();
            if ddalgak::remap::widened(&trig) && !fl.remap.to.trim().is_empty() {
                ui.label(small(
                    format!(
                        "{} Shift+{trig} → Shift+{}",
                        lang.t("form.held_modifiers"),
                        fl.remap.to.trim()
                    ),
                    c.text_muted,
                ));
            }
            problems_for(
                ui,
                problems,
                &["steps[0].to", "steps[0].tap", "trigger.pass_through"],
            );
        }
        Effect::Block => {
            ui.label(small(lang.t("tile.block.example"), c.text_muted));
        }
        Effect::SendKeys => {
            key_field(
                ui,
                lang,
                lang.t("field.shortcut"),
                &mut fl.keys.keys,
                capture,
                target,
                CaptureTarget::SendKeys,
                true,
            );
            ui.checkbox(&mut capture.add_step, lang.t("form.add_step"));
            ui.add(
                egui::DragValue::new(&mut fl.keys.step_delay_ms)
                    .range(0..=1000)
                    .suffix(" ms"),
            )
            .accessible_name(lang.t("field.step_delay"));
            problems_for(ui, problems, &["steps[0]"]);
        }
        Effect::TypeText => {
            let lbl = ui.label(egui::RichText::new(lang.t("field.text")).strong());
            ui.add(
                egui::TextEdit::multiline(&mut fl.text.text)
                    .desired_rows(4)
                    .desired_width(f32::INFINITY),
            )
            .labelled_by(lbl.id);
            ui.checkbox(&mut fl.text.variables, lang.t("field.variables"));
            ui.checkbox(&mut fl.text.paste, lang.t("field.paste"));
            ui.add(
                egui::DragValue::new(&mut fl.text.key_delay_ms)
                    .range(0..=200)
                    .suffix(" ms"),
            )
            .accessible_name(lang.t("field.key_delay"));
            let preview = f.text_draft().preview(&form::local_now());
            ui.label(small(lang.t("form.as_typed"), c.text_muted));
            let mut shown = preview.rendered;
            ui.add(
                egui::TextEdit::multiline(&mut shown)
                    .interactive(false)
                    .desired_rows(2)
                    .desired_width(f32::INFINITY),
            );
            ui.label(small(lang.t("form.plain_note"), c.text_muted));
            problems_for(ui, problems, &["steps[0]"]);
        }
        Effect::OpenProgram => {
            let lbl = ui.label(egui::RichText::new(lang.t("field.program")).strong());
            ui.add(
                egui::TextEdit::singleline(&mut fl.program.path)
                    .hint_text(r"C:\Windows\System32\notepad.exe")
                    .desired_width(f32::INFINITY),
            )
            .labelled_by(lbl.id);
            ui.add(
                egui::TextEdit::singleline(&mut fl.program.app_id)
                    .hint_text("Microsoft.WindowsCalculator_8wekyb3d8bbwe!App")
                    .desired_width(f32::INFINITY),
            )
            .accessible_name(lang.t("field.app_id"));
            ui.label(small(lang.t("field.args"), c.text_muted));
            ui.add(
                egui::TextEdit::multiline(&mut fl.program.args)
                    .desired_rows(2)
                    .desired_width(f32::INFINITY),
            )
            .accessible_name(lang.t("field.args"));
            ui.add(
                egui::TextEdit::singleline(&mut fl.program.working_dir)
                    .hint_text(lang.t("field.start_in"))
                    .desired_width(f32::INFINITY),
            )
            .accessible_name(lang.t("field.start_in"));
            ui.horizontal(|ui| {
                ui.label(lang.t("field.window"));
                for w in ["normal", "maximized", "minimized"] {
                    ui.radio_value(
                        &mut fl.program.window,
                        w.to_string(),
                        lang.t(&format!("field.window.{w}")),
                    );
                }
            });
            ui.checkbox(&mut fl.program.activate, lang.t("field.activate"));
            let d = f.program_draft();
            let pv = d.preview(&ddalgak::launch::process_var);
            if !pv.program.is_empty() {
                let (glyph, color, word) = if pv.found && pv.is_program {
                    ("✓", c.success, lang.t("form.found"))
                } else {
                    ("✕", c.warning, lang.t("form.not_found"))
                };
                ui.label(
                    egui::RichText::new(format!("{glyph} {} — {word}", pv.program)).color(color),
                );
            }
            if let Some(cl) = d.command_line(&ddalgak::launch::process_var) {
                ui.label(small(
                    format!("{}: {cl}", lang.t("form.command_line")),
                    c.text_muted,
                ));
            }
            ui.label(small(lang.t("form.program_note"), c.text_muted));
            problems_for(ui, problems, &["steps[0]"]);
        }
        Effect::OpenUrl => {
            let lbl = ui.label(egui::RichText::new(lang.t("field.url")).strong());
            ui.add(
                egui::TextEdit::singleline(&mut fl.url)
                    .hint_text("github.com")
                    .desired_width(f32::INFINITY),
            )
            .labelled_by(lbl.id);
            match f.url_draft().normalized() {
                Ok(u) if !f.fields.url.trim().is_empty() => {
                    ui.label(egui::RichText::new(format!("✓ {u}")).color(c.success));
                }
                _ => {}
            }
            ui.label(small(lang.t("form.program_note"), c.text_muted));
            problems_for(ui, problems, &["steps[0]"]);
        }
        Effect::Mouse => mouse_form(ui, lang, f, problems, layout, cursor),
        Effect::Mode => {
            let ops = [
                (ModeOp::Set, "switch to"),
                (ModeOp::Toggle, "toggle"),
                (ModeOp::Cycle, "cycle"),
                (ModeOp::Hold, "hold"),
                (ModeOp::Previous, "back"),
                (ModeOp::Default, "default"),
            ];
            ui.horizontal_wrapped(|ui| {
                ui.label(lang.t("field.switch"));
                for (op, word) in ops {
                    ui.radio_value(&mut fl.mode.op, op, word);
                }
            });
            let modes: Vec<(String, String)> = profile
                .map(|p| {
                    p.modes
                        .iter()
                        .map(|m| (m.id.clone(), m.title.clone()))
                        .collect()
                })
                .unwrap_or_default();
            match fl.mode.op {
                ModeOp::Set | ModeOp::Toggle | ModeOp::Hold => {
                    let cur = modes
                        .iter()
                        .find(|m| m.0 == fl.mode.mode)
                        .map_or("—".to_string(), |m| m.1.clone());
                    egui::ComboBox::from_id_salt("mode-target")
                        .selected_text(format!("{}: {cur}", lang.t("field.mode")))
                        .show_ui(ui, |ui| {
                            for (id, t) in &modes {
                                ui.selectable_value(&mut fl.mode.mode, id.clone(), t);
                            }
                        })
                        .response
                        .accessible_name(lang.t("field.mode"));
                }
                ModeOp::Cycle => {
                    ui.label(small(lang.t("field.cycle_modes"), c.text_muted));
                    for (id, t) in &modes {
                        let mut on = fl.mode.modes.contains(id);
                        if ui.checkbox(&mut on, t).changed() {
                            if on {
                                fl.mode.modes.push(id.clone());
                            } else {
                                fl.mode.modes.retain(|m| m != id);
                            }
                        }
                    }
                }
                ModeOp::Previous | ModeOp::Default => {}
            }
            ui.horizontal(|ui| {
                ui.add(
                    egui::TextEdit::singleline(new_mode)
                        .hint_text(lang.t("field.new_mode"))
                        .desired_width(160.0),
                )
                .accessible_name(lang.t("field.new_mode"));
                if ui
                    .add_enabled(
                        !new_mode.trim().is_empty(),
                        egui::Button::new(lang.t("modes.add")),
                    )
                    .clicked()
                {
                    acts.push(Act::NewMode(new_mode.clone()));
                }
            });
            problems_for(ui, problems, &["steps[0]"]);
        }
        Effect::Engine => {
            for (op, word) in [
                (EngineOp::Pause, "pause"),
                (EngineOp::Resume, "resume"),
                (EngineOp::TogglePause, "pause or resume"),
                (EngineOp::Disable, "turn off"),
            ] {
                ui.radio_value(&mut fl.engine, op, word);
            }
            ui.label(small(lang.t("field.engine_note"), c.text_muted));
        }
    }
    ui.add_space(4.0);
    ui.label(
        egui::RichText::new(format!(
            "{}: {}",
            lang.t("form.summary"),
            f.summary(profile)
        ))
        .color(c.text_secondary),
    );
    egui::CollapsingHeader::new(lang.t("form.advanced"))
        .id_salt("advanced")
        .show(ui, |ui| {
            let fl = &mut f.fields;
            ui.add(
                egui::TextEdit::singleline(&mut fl.description)
                    .hint_text(lang.t("form.description"))
                    .desired_width(f32::INFINITY),
            )
            .accessible_name(lang.t("form.description"));
            if let Some(p) = profile
                && p.modes.len() > 1
            {
                ui.label(small(lang.t("form.only_modes"), c.text_muted));
                ui.horizontal_wrapped(|ui| {
                    for m in &p.modes {
                        let mut on = fl.modes.contains(&m.id);
                        if ui.checkbox(&mut on, &m.title).changed() {
                            if on {
                                fl.modes.push(m.id.clone());
                            } else {
                                fl.modes.retain(|x| x != &m.id);
                            }
                        }
                    }
                });
            }
            ui.label(small(lang.t("form.only_apps"), c.text_muted));
            ui.add(
                egui::TextEdit::multiline(&mut fl.apps)
                    .desired_rows(2)
                    .hint_text("Code.exe")
                    .desired_width(f32::INFINITY),
            )
            .accessible_name(lang.t("form.only_apps"));
            if !fl.apps.trim().is_empty() {
                ui.label(small(lang.t("form.apps_note"), c.warning));
            }
            ui.horizontal(|ui| {
                ui.label(lang.t("form.priority"));
                ui.add(egui::DragValue::new(&mut fl.priority).range(-1000..=1000))
                    .accessible_name(lang.t("form.priority"));
            });
            problems_for(ui, problems, &["modes", "when", "priority", "description"]);
        });
}

fn mouse_form(
    ui: &mut egui::Ui,
    lang: Lang,
    f: &mut RuleForm,
    problems: &[ddalgak::text::FieldProblem],
    layout: &Layout,
    cursor: Point,
) {
    let c = Colors::get();
    let kind = form::mouse_kind(&f.fields.mouse);
    ui.horizontal(|ui| {
        for (i, key) in [
            "field.mouse.move_by",
            "field.mouse.move_to",
            "field.mouse.scroll",
            "field.mouse.click",
        ]
        .into_iter()
        .enumerate()
        {
            if ui.selectable_label(kind == i, lang.t(key)).clicked() && kind != i {
                f.fields.mouse = form::mouse_default(i);
            }
        }
    });
    let anchors = [
        (Anchor::VirtualScreen, "the desktop"),
        (Anchor::PrimaryMonitor, "the primary monitor"),
        (Anchor::ActiveWindow, "the active window"),
    ];
    match &mut f.fields.mouse {
        MouseAction::MoveBy { dx, dy, grid, hold } => {
            ui.horizontal(|ui| {
                ui.label(lang.t("field.right"));
                ui.add(egui::DragValue::new(dx).range(-10_000..=10_000))
                    .accessible_name(lang.t("field.right"));
                ui.label(lang.t("field.down"));
                ui.add(egui::DragValue::new(dy).range(-10_000..=10_000))
                    .accessible_name(lang.t("field.down"));
            });
            let mut cells = grid.is_some();
            if ui.checkbox(&mut cells, lang.t("field.unit_grid")).changed() {
                *grid = cells.then_some(Grid {
                    columns: 8,
                    rows: 8,
                });
                if cells && matches!(hold, HoldChoice::Continuous { .. }) {
                    *hold = HoldChoice::Once;
                }
            }
            if let Some(g) = grid {
                ui.horizontal(|ui| {
                    ui.label(lang.t("field.columns"));
                    ui.add(egui::DragValue::new(&mut g.columns).range(2..=64));
                    ui.label(lang.t("field.rows"));
                    ui.add(egui::DragValue::new(&mut g.rows).range(2..=64));
                });
            }
            ui.label(small(lang.t("field.while_held"), c.text_muted));
            ui.horizontal_wrapped(|ui| {
                let once = matches!(hold, HoldChoice::Once);
                if ui.radio(once, lang.t("field.hold.once")).clicked() {
                    *hold = HoldChoice::Once;
                }
                if grid.is_none() {
                    let glide = matches!(hold, HoldChoice::Continuous { .. });
                    if ui.radio(glide, lang.t("field.hold.glide")).clicked() && !glide {
                        *hold = HoldChoice::Continuous {
                            speed: 600,
                            acceleration: 0,
                        };
                    }
                }
                let rep = matches!(hold, HoldChoice::Repeat { .. });
                if ui.radio(rep, lang.t("field.hold.repeat")).clicked() && !rep {
                    *hold = HoldChoice::Repeat {
                        repeat_ms: 100,
                        delay_ms: mouse::DEFAULT_DELAY_MS,
                    };
                }
            });
            match hold {
                HoldChoice::Continuous { speed, .. } => {
                    ui.add(egui::DragValue::new(speed).range(50..=5000).suffix(" px/s"))
                        .accessible_name(lang.t("field.speed"));
                }
                HoldChoice::Repeat { repeat_ms, .. } => {
                    ui.add(
                        egui::DragValue::new(repeat_ms)
                            .range(20..=2000)
                            .suffix(" ms"),
                    )
                    .accessible_name(lang.t("field.repeat_ms"));
                }
                HoldChoice::Once => {}
            }
        }
        MouseAction::MoveTo(p) => {
            ui.horizontal(|ui| {
                ui.label(lang.t("field.x"));
                ui.add(egui::DragValue::new(&mut p.x).range(-32768..=32767));
                ui.label(lang.t("field.y"));
                ui.add(egui::DragValue::new(&mut p.y).range(-32768..=32767));
            });
            egui::ComboBox::from_id_salt("move-to-anchor")
                .selected_text(anchors.iter().find(|a| a.0 == p.anchor).map_or("", |a| a.1))
                .show_ui(ui, |ui| {
                    for (a, w) in anchors {
                        ui.selectable_value(&mut p.anchor, a, w);
                    }
                })
                .response
                .accessible_name(lang.t("field.relative_to"));
        }
        MouseAction::Scroll { dir, notches } => {
            ui.horizontal(|ui| {
                for d in [
                    ScrollDir::Up,
                    ScrollDir::Down,
                    ScrollDir::Left,
                    ScrollDir::Right,
                ] {
                    ui.radio_value(dir, d, d.as_str());
                }
                ui.add(egui::DragValue::new(notches).range(1..=50))
                    .accessible_name(lang.t("field.notches"));
            });
        }
        MouseAction::Click { button, op, at } => {
            ui.horizontal(|ui| {
                for b in ["left", "right", "middle", "x1", "x2"] {
                    if let Some(bt) = mouse::button(b) {
                        ui.radio_value(button, bt, b);
                    }
                }
            });
            ui.horizontal(|ui| {
                for o in ["click", "double_click", "hold"] {
                    if ui.radio(*op == o, o.replace('_', " ")).clicked() {
                        *op = o;
                    }
                }
            });
            let mut has_at = at.is_some();
            if ui.checkbox(&mut has_at, lang.t("field.at_point")).changed() {
                *at = has_at.then_some(mouse::PointSpec {
                    x: 0,
                    y: 0,
                    anchor: Anchor::PrimaryMonitor,
                });
            }
            if let Some(p) = at {
                ui.horizontal(|ui| {
                    ui.label(lang.t("field.x"));
                    ui.add(egui::DragValue::new(&mut p.x).range(-32768..=32767));
                    ui.label(lang.t("field.y"));
                    ui.add(egui::DragValue::new(&mut p.y).range(-32768..=32767));
                });
            }
        }
    }
    let map = f.mouse_draft().screen_map(layout, cursor, None, 220, 110);
    let to = map.to.map(|_| {
        // The draft's target in physical pixels, for the shared painter.
        let sim = f.simulate(None, layout, cursor);
        sim.pointer.map_or(cursor, |(_, t)| t)
    });
    ui.label(small(lang.t("form.screen_map"), c.text_muted));
    screen_map(ui, layout, cursor, to);
    if map.clamped {
        ui.label(small(lang.t("form.clamped"), c.warning));
    }
    problems_for(ui, problems, &["steps[0]"]);
}
