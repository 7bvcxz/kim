//! Kim Manager GUI (egui + glow, ADR 0014). A thin, explicit front-end over the `kim` library:
//! every install / update / uninstall starts only from a button the user pressed, runs on a
//! worker thread with visible progress and a Cancel button, and never touches a running program.
//! The window can be closed at any time; nothing keeps running in the background afterwards.
//! Keyboard shortcuts (F5 refresh, F1 shortcut list, and any the user binds with
//! `kim hotkeys set`) are read from the window's own key events only ([`shortcuts`]).

pub mod shortcuts;
pub mod theme;

use eframe::egui;
use kim::catalog::Channel;
use kim::catalog::signature::TrustedKey;
use kim::catalog::status::{self, ToolStatus, ToolView};
use kim::catalog::store::{self, Loaded};
use kim::error::KimError;
use kim::fetch::{DefaultFetcher, Downloader};
use kim::install::{self, Event, Phase, PlanOptions};
use kim::paths::Paths;
use kim::platform::Platform;
use kim::settings::Settings;
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{Receiver, Sender, channel};

/// Everything the UI needs from the outside world; tests inject temp paths, test keys and a
/// loopback downloader.
#[derive(Clone)]
pub struct Backend {
    pub paths: Paths,
    pub keys: Vec<TrustedKey>,
    pub downloader: Arc<dyn Downloader + Send + Sync>,
    pub settings_path: Option<PathBuf>,
    pub platform: Platform,
    /// Source for the explicit "Refresh catalog" action.
    pub catalog_source: String,
}

impl std::fmt::Debug for Backend {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Backend")
            .field("paths", &self.paths)
            .finish_non_exhaustive()
    }
}

impl Backend {
    /// The real per-user environment.
    pub fn real() -> Result<Self, KimError> {
        Ok(Self {
            paths: Paths::from_env()?,
            keys: kim::catalog::signature::TRUSTED_KEYS.to_vec(),
            downloader: Arc::new(DefaultFetcher::default()),
            settings_path: Settings::path().ok(),
            platform: Platform::current(),
            catalog_source: store::DEFAULT_SOURCE.into(),
        })
    }
}

/// Messages from worker threads to the UI.
#[derive(Debug)]
enum Msg {
    Progress(Event),
    Finished(Result<String, KimError>),
    Refreshed(Result<String, KimError>),
}

#[derive(Debug)]
struct Running {
    tool: String,
    verb: &'static str,
    phase: Phase,
    done: u64,
    total: u64,
    cancel: Arc<AtomicBool>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Banner {
    Info(String),
    Success(String),
    Error { message: String, code: String },
}

#[derive(Debug, Default)]
struct ConfirmUninstall {
    tool: String,
    purge: bool,
    understood: bool,
}

pub struct KimManagerApp {
    backend: Backend,
    loaded: Option<Loaded>,
    views: Vec<ToolView>,
    load_error: Option<String>,
    selected: Option<String>,
    channel: Channel,
    settings: Settings,
    running: Option<Running>,
    refreshing: bool,
    confirm: Option<ConfirmUninstall>,
    banner: Option<Banner>,
    shortcuts: shortcuts::Shortcuts,
    show_shortcuts: bool,
    tx: Sender<Msg>,
    rx: Receiver<Msg>,
    ctx: Option<egui::Context>,
}

impl std::fmt::Debug for KimManagerApp {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("KimManagerApp")
            .field("selected", &self.selected)
            .field("banner", &self.banner)
            .finish_non_exhaustive()
    }
}

impl KimManagerApp {
    pub fn new(backend: Backend, ctx: Option<&egui::Context>) -> Self {
        let (tx, rx) = channel();
        let settings = backend
            .settings_path
            .as_deref()
            .map(|p| Settings::load_from(p).0)
            .unwrap_or_default();
        let shortcuts = shortcuts::Shortcuts::load(&backend.paths, kim_hotkeys::no_hotkeys_env());
        let mut app = Self {
            shortcuts,
            show_shortcuts: false,
            backend,
            loaded: None,
            views: Vec::new(),
            load_error: None,
            selected: None,
            channel: Channel::Stable,
            settings,
            running: None,
            refreshing: false,
            confirm: None,
            banner: None,
            tx,
            rx,
            ctx: ctx.cloned(),
        };
        if let Some(ctx) = ctx {
            theme::apply(ctx);
        }
        app.reload();
        // Launch-time check only if the user enabled it (off by default, KIM-SEC-002). It only
        // refreshes the catalog to *show* updates; it never downloads or installs a tool.
        if app.settings.check_updates_on_launch && !app.settings.offline_mode {
            app.refresh_catalog();
        }
        app
    }

    pub fn banner(&self) -> Option<&Banner> {
        self.banner.as_ref()
    }

    /// What the banner's Dismiss button does.
    pub fn dismiss_banner(&mut self) {
        self.banner = None;
    }

    /// Starts with every shortcut off (`--no-hotkeys`), without touching `hotkeys.json`.
    pub fn disable_hotkeys(&mut self) {
        self.shortcuts = shortcuts::Shortcuts::load(&self.backend.paths, true);
    }

    /// Whether the keyboard shortcut list is open (F1 by default).
    pub fn shortcuts_visible(&self) -> bool {
        self.show_shortcuts
    }

    /// Whether an uninstall confirmation is open.
    pub fn confirming(&self) -> Option<&str> {
        self.confirm.as_ref().map(|c| c.tool.as_str())
    }

    /// Runs one shortcut action exactly like its button or menu entry: the same checks and the
    /// same confirmation (a shortcut never skips consent, KIM-SEC-001).
    fn run_action(&mut self, action: &str) {
        let busy = self.is_busy();
        let view = self
            .selected
            .as_ref()
            .and_then(|id| self.views.iter().find(|v| &v.id == id))
            .filter(|v| v.id != "kim")
            .cloned();
        match action {
            "help.show" => self.show_shortcuts = !self.show_shortcuts,
            _ if self.show_shortcuts || busy => {}
            "catalog.refresh" => self.refresh_catalog(),
            "tool.install" | "tool.update" => {
                let want = if action == "tool.install" {
                    ToolStatus::Available
                } else {
                    ToolStatus::UpdateAvailable
                };
                if let Some(v) = view.filter(|v| v.status == want) {
                    self.start_install(&v.id);
                }
            }
            "tool.uninstall" => {
                if let Some(v) = view.filter(|v| {
                    matches!(
                        v.status,
                        ToolStatus::Installed | ToolStatus::UpdateAvailable | ToolStatus::Damaged
                    )
                }) {
                    self.confirm = Some(ConfirmUninstall {
                        tool: v.id,
                        ..Default::default()
                    });
                }
            }
            _ => {}
        }
    }

    fn handle_shortcuts(&mut self, ctx: &egui::Context) {
        let events = ctx.input(|i| i.events.clone());
        if events
            .iter()
            .any(|e| matches!(e, egui::Event::WindowFocused(true)))
        {
            // `kim hotkeys set` may have changed the file while the window was in the background.
            self.shortcuts.reload();
        }
        let actions = self.shortcuts.handle(&events);
        if self.confirm.is_some() || ctx.egui_wants_keyboard_input() {
            return;
        }
        for a in actions {
            self.run_action(&a);
        }
    }

    pub fn is_busy(&self) -> bool {
        self.running.is_some() || self.refreshing
    }

    pub fn views(&self) -> &[ToolView] {
        &self.views
    }

    fn reload(&mut self) {
        match store::load(
            &self.backend.paths,
            &self.backend.keys,
            kim::time::now_utc(),
        ) {
            Ok(l) => {
                self.views = status::evaluate(
                    &l.catalog,
                    &self.backend.paths,
                    &self.backend.platform,
                    self.channel,
                );
                if self.selected.is_none() {
                    self.selected = self
                        .views
                        .iter()
                        .find(|v| v.id != "kim")
                        .map(|v| v.id.clone());
                }
                self.load_error = None;
                self.loaded = Some(l);
            }
            Err(e) => self.load_error = Some(e.to_string()),
        }
    }

    fn refresh_catalog(&mut self) {
        if let Err(e) = self.settings.ensure_online() {
            self.banner = Some(Banner::Error {
                message: e.message,
                code: e.code.as_str().into(),
            });
            return;
        }
        self.refreshing = true;
        self.banner = Some(Banner::Info("Refreshing the catalog…".into()));
        let b = self.backend.clone();
        let tx = self.tx.clone();
        let ctx = self.ctx.clone();
        std::thread::spawn(move || {
            let r = store::refresh(
                &b.paths,
                &b.keys,
                kim::time::now_utc(),
                &b.catalog_source,
                &DefaultFetcher::default(),
            )
            .map(|o| {
                if o.updated {
                    format!("Catalog updated to v{}.", o.fetched_version)
                } else {
                    format!("Catalog is up to date (v{}).", o.fetched_version)
                }
            });
            let _ = tx.send(Msg::Refreshed(r));
            if let Some(c) = ctx {
                c.request_repaint();
            }
        });
    }

    /// Starts an explicit install or update of `tool` (the user pressed the button).
    pub fn start_install(&mut self, tool: &str) {
        let Some(loaded) = &self.loaded else { return };
        if let Err(e) = self.settings.ensure_online() {
            self.banner = Some(Banner::Error {
                message: e.message,
                code: e.code.as_str().into(),
            });
            return;
        }
        let opts = PlanOptions {
            channel: Some(self.channel),
            ..PlanOptions::default()
        };
        let plan = match install::plan_install(
            loaded,
            &self.backend.paths,
            &self.backend.platform,
            tool,
            &opts,
        ) {
            Ok(p) => p,
            Err(e) => {
                self.banner = Some(Banner::Error {
                    message: e.message,
                    code: e.code.as_str().into(),
                });
                return;
            }
        };
        let cancel = Arc::new(AtomicBool::new(false));
        let verb = if plan.from.is_some() {
            "Updating"
        } else {
            "Installing"
        };
        self.running = Some(Running {
            tool: tool.into(),
            verb,
            phase: Phase::Locking,
            done: 0,
            total: plan.asset.size,
            cancel: cancel.clone(),
        });
        self.banner = None;
        let b = self.backend.clone();
        let tx = self.tx.clone();
        let ctx = self.ctx.clone();
        std::thread::spawn(move || {
            let txp = tx.clone();
            let ctxp = ctx.clone();
            let result = install::execute(
                &plan,
                &b.paths,
                b.downloader.as_ref(),
                &mut |e| {
                    let _ = txp.send(Msg::Progress(e));
                    if let Some(c) = &ctxp {
                        c.request_repaint();
                    }
                },
                &cancel,
            )
            .map(|o| {
                let mut s = format!("{} {} is installed.", o.tool, o.to);
                if !o.running.is_empty() {
                    s.push_str(" The running copy was not stopped; the new version is used next time it starts.");
                }
                s
            });
            let _ = tx.send(Msg::Finished(result));
            if let Some(c) = ctx {
                c.request_repaint();
            }
        });
    }

    pub fn cancel(&mut self) {
        if let Some(r) = &self.running {
            r.cancel.store(true, Ordering::Relaxed);
        }
    }

    fn start_uninstall(&mut self, tool: &str, purge: bool) {
        let plan = match install::plan_uninstall(&self.backend.paths, tool, purge) {
            Ok(p) => p,
            Err(e) => {
                self.banner = Some(Banner::Error {
                    message: e.message,
                    code: e.code.as_str().into(),
                });
                return;
            }
        };
        self.running = Some(Running {
            tool: tool.into(),
            verb: "Uninstalling",
            phase: Phase::Activating,
            done: 0,
            total: 0,
            cancel: Arc::new(AtomicBool::new(false)),
        });
        let b = self.backend.clone();
        let tx = self.tx.clone();
        let ctx = self.ctx.clone();
        std::thread::spawn(move || {
            let result = install::execute_uninstall(&plan, &b.paths).map(|o| {
                if o.kept_data.is_empty() {
                    format!("{} was uninstalled.", o.tool)
                } else {
                    format!(
                        "{} was uninstalled. Your settings and data were kept.",
                        o.tool
                    )
                }
            });
            let _ = tx.send(Msg::Finished(result));
            if let Some(c) = ctx {
                c.request_repaint();
            }
        });
    }

    fn poll(&mut self) {
        while let Ok(msg) = self.rx.try_recv() {
            match msg {
                Msg::Progress(Event::Phase { phase }) => {
                    if let Some(r) = &mut self.running {
                        r.phase = phase;
                    }
                }
                Msg::Progress(Event::Progress { done, total }) => {
                    if let Some(r) = &mut self.running {
                        r.done = done;
                        r.total = total;
                    }
                }
                Msg::Finished(result) => {
                    self.running = None;
                    self.banner = Some(match result {
                        Ok(m) => Banner::Success(m),
                        Err(e) => Banner::Error {
                            message: e.message,
                            code: e.code.as_str().into(),
                        },
                    });
                    self.reload();
                }
                Msg::Refreshed(result) => {
                    self.refreshing = false;
                    self.banner = Some(match result {
                        Ok(m) => Banner::Info(m),
                        Err(e) => Banner::Error {
                            message: e.message,
                            code: e.code.as_str().into(),
                        },
                    });
                    self.reload();
                }
            }
        }
    }

    /// Blocks until background work finishes (tests).
    pub fn wait_idle(&mut self, timeout: std::time::Duration) -> bool {
        let start = std::time::Instant::now();
        while self.is_busy() {
            if start.elapsed() > timeout {
                return false;
            }
            std::thread::sleep(std::time::Duration::from_millis(10));
            self.poll();
        }
        true
    }

    fn save_settings(&mut self) {
        if let Some(p) = &self.backend.settings_path
            && let Err(e) = self.settings.save_to(p)
        {
            self.banner = Some(Banner::Error {
                message: e.message,
                code: e.code.as_str().into(),
            });
        }
    }

    // --- UI --------------------------------------------------------------------------------

    pub fn ui(&mut self, ui: &mut egui::Ui) {
        self.poll();
        self.handle_shortcuts(&ui.ctx().clone());
        let c = theme::Colors::get();
        egui::Panel::top("header")
            .frame(
                egui::Frame::new()
                    .fill(c.bg_raised)
                    .inner_margin(egui::Margin::symmetric(16, 10)),
            )
            .show(ui, |ui| self.header(ui));
        egui::Panel::left("tools")
            .resizable(false)
            .exact_size(320.0)
            .frame(egui::Frame::new().fill(c.bg_raised).inner_margin(8))
            .show(ui, |ui| self.tool_list(ui));
        egui::CentralPanel::default()
            .frame(
                egui::Frame::new()
                    .fill(c.bg_base)
                    .inner_margin(egui::Margin::symmetric(24, 16)),
            )
            .show(ui, |ui| self.details(ui));
        if self.confirm.is_some() {
            self.confirm_dialog(ui.ctx());
        }
        if self.show_shortcuts {
            self.shortcuts_dialog(ui.ctx());
        }
    }

    /// The shortcut list (§11.1): every action with its keys and state, fixed keys included.
    /// Changing shortcuts is done with `kim hotkeys …`; the in-window binding editor is a later
    /// step (docs/HOTKEY_CONTRACT.md §11.2).
    fn shortcuts_dialog(&mut self, ctx: &egui::Context) {
        let c = theme::Colors::get();
        let rows = self.shortcuts.rows();
        let mut close = false;
        let r = egui::Modal::new(egui::Id::unique("keyboard-shortcuts")).show(ctx, |ui| {
            ui.set_max_width(560.0);
            ui.label(
                egui::RichText::new("Keyboard shortcuts")
                    .size(15.0)
                    .strong(),
            );
            if self.shortcuts.is_off() {
                ui.colored_label(
                    c.warning,
                    "All shortcuts are off for this window (--no-hotkeys or KIM_NO_HOTKEYS).",
                );
            }
            if let Some(p) = &self.shortcuts.problem {
                ui.colored_label(c.warning, p);
            }
            egui::Grid::new("shortcut-rows")
                .num_columns(3)
                .spacing([24.0, 6.0])
                .show(ui, |ui| {
                    for h in ["Action", "Keys", "State"] {
                        ui.label(egui::RichText::new(h).size(11.0).color(c.text_muted));
                    }
                    ui.end_row();
                    for r in &rows {
                        ui.label(egui::RichText::new(&r.title).color(c.text_primary));
                        ui.label(egui::RichText::new(&r.keys).monospace());
                        ui.label(egui::RichText::new(&r.state).color(c.text_secondary));
                        ui.end_row();
                    }
                });
            ui.add_space(8.0);
            ui.label(
                egui::RichText::new(
                    "Change a shortcut with `kim hotkeys set <action> <keys>`, turn one off with \
                     `kim hotkeys disable <action>`, restore it with `kim hotkeys reset <action>`.",
                )
                .size(11.0)
                .color(c.text_muted),
            );
            if ui.button("Close").clicked() {
                close = true;
            }
        });
        if close || r.should_close() {
            self.show_shortcuts = false;
        }
    }

    fn header(&mut self, ui: &mut egui::Ui) {
        let c = theme::Colors::get();
        ui.horizontal(|ui| {
            ui.label(
                egui::RichText::new("Kim")
                    .size(15.0)
                    .strong()
                    .color(c.text_primary),
            );
            if let Some(l) = &self.loaded {
                let src = match l.source {
                    store::Source::Bundled => "bundled",
                    store::Source::Cache => "cached",
                };
                let stale = if l.stale { " · stale" } else { "" };
                ui.label(
                    egui::RichText::new(format!(
                        "Catalog v{} ({src}){stale}",
                        l.catalog.catalog_version
                    ))
                    .size(11.0)
                    .color(c.text_muted),
                );
            }
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                let busy = self.is_busy();
                if ui
                    .add_enabled(
                        !busy && !self.settings.offline_mode,
                        egui::Button::new("Refresh catalog"),
                    )
                    .on_hover_text("Explicitly fetch the signed catalog. Never installs anything.")
                    .clicked()
                {
                    self.refresh_catalog();
                }
                egui::ComboBox::from_label("Channel")
                    .selected_text(match self.channel {
                        Channel::Stable => "Stable",
                        Channel::Preview => "Preview",
                    })
                    .show_ui(ui, |ui| {
                        let before = self.channel;
                        ui.selectable_value(&mut self.channel, Channel::Stable, "Stable");
                        ui.selectable_value(&mut self.channel, Channel::Preview, "Preview");
                        if before != self.channel {
                            self.reload();
                        }
                    });
            });
        });
    }

    fn tool_list(&mut self, ui: &mut egui::Ui) {
        let c = theme::Colors::get();
        if let Some(e) = &self.load_error {
            ui.colored_label(c.danger, format!("Cannot load the catalog: {e}"));
            return;
        }
        let mut clicked = None;
        for v in &self.views {
            let selected = self.selected.as_deref() == Some(v.id.as_str());
            let label = status_label(v);
            let (rect, resp) = ui
                .allocate_exact_size(egui::vec2(ui.available_width(), 48.0), egui::Sense::click());
            let a11y = format!("{}, {label}", v.name);
            resp.widget_info(|| {
                egui::WidgetInfo::selected(egui::Role::Button, true, selected, a11y.clone())
            });
            if ui.is_rect_visible(rect) {
                let painter = ui.painter();
                let fill = if selected {
                    c.accent_soft
                } else if resp.hovered() {
                    c.bg_hover
                } else {
                    egui::Color32::TRANSPARENT
                };
                painter.rect_filled(rect, 6.0, fill);
                if selected {
                    let marker = egui::Rect::from_min_size(
                        rect.left_top() + egui::vec2(0.0, 10.0),
                        egui::vec2(2.0, rect.height() - 20.0),
                    );
                    painter.rect_filled(marker, 1.0, c.accent);
                }
                if resp.has_focus() {
                    painter.rect_stroke(
                        rect,
                        6.0,
                        egui::Stroke::new(2.0, c.focus),
                        egui::StrokeKind::Inside,
                    );
                }
                paint_status_shape(
                    painter,
                    rect.left_center() + egui::vec2(16.0, 0.0),
                    v.status,
                );
                let x = rect.left() + 32.0;
                painter.text(
                    egui::pos2(x, rect.top() + 8.0),
                    egui::Align2::LEFT_TOP,
                    &v.name,
                    egui::FontId::proportional(13.0),
                    c.text_primary,
                );
                painter.text(
                    egui::pos2(rect.right() - 8.0, rect.top() + 9.0),
                    egui::Align2::RIGHT_TOP,
                    &label,
                    egui::FontId::proportional(11.0),
                    status_color(v.status),
                );
                painter.text(
                    egui::pos2(x, rect.top() + 27.0),
                    egui::Align2::LEFT_TOP,
                    &v.summary,
                    egui::FontId::proportional(11.0),
                    c.text_muted,
                );
            }
            if resp.clicked() {
                clicked = Some(v.id.clone());
            }
        }
        if let Some(id) = clicked {
            self.selected = Some(id);
            self.banner = None;
        }
        ui.with_layout(egui::Layout::bottom_up(egui::Align::Min), |ui| {
            let mut changed = false;
            changed |= ui
                .checkbox(&mut self.settings.offline_mode, "Offline mode")
                .on_hover_text("Refuse every network operation.")
                .changed();
            changed |= ui
                .checkbox(
                    &mut self.settings.check_updates_on_launch,
                    "Check for updates when Kim starts",
                )
                .on_hover_text(
                    "Only shows available updates. Never downloads or installs anything.",
                )
                .changed();
            if changed {
                self.save_settings();
            }
            ui.label(
                egui::RichText::new("Settings")
                    .size(11.0)
                    .color(c.text_muted),
            );
        });
    }

    fn details(&mut self, ui: &mut egui::Ui) {
        let c = theme::Colors::get();
        if let Some(b) = self.banner.clone() {
            let (fill, text) = match &b {
                Banner::Info(m) => (c.bg_overlay, m.clone()),
                Banner::Success(m) => (c.bg_overlay, m.clone()),
                Banner::Error { message, code } => (c.bg_overlay, format!("{message}  ({code})")),
            };
            let color = match b {
                Banner::Info(_) => c.text_secondary,
                Banner::Success(_) => c.success,
                Banner::Error { .. } => c.danger,
            };
            egui::Frame::new()
                .fill(fill)
                .stroke(egui::Stroke::new(1.0, color))
                .corner_radius(6)
                .inner_margin(12)
                .show(ui, |ui| {
                    ui.horizontal(|ui| {
                        ui.colored_label(color, text);
                        if ui.small_button("Dismiss").clicked() {
                            self.banner = None;
                        }
                    });
                });
            ui.add_space(12.0);
        }
        let Some(id) = self.selected.clone() else {
            ui.label("Select a tool.");
            return;
        };
        let Some(view) = self.views.iter().find(|v| v.id == id).cloned() else {
            return;
        };
        let tool = self
            .loaded
            .as_ref()
            .and_then(|l| l.catalog.tool(&id))
            .cloned();
        ui.label(
            egui::RichText::new(&view.name)
                .size(15.0)
                .strong()
                .color(c.text_primary),
        );
        if let Some(t) = &tool {
            ui.label(egui::RichText::new(&t.description).color(c.text_secondary));
            ui.add_space(8.0);
            egui::Grid::new("meta")
                .num_columns(2)
                .spacing([24.0, 6.0])
                .show(ui, |ui| {
                    let row = |ui: &mut egui::Ui, k: &str, v: String| {
                        ui.label(egui::RichText::new(k).size(11.0).color(c.text_muted));
                        ui.label(egui::RichText::new(v).color(c.text_primary));
                        ui.end_row();
                    };
                    row(ui, "Status", status_label(&view));
                    row(
                        ui,
                        "Installed",
                        view.installed_version.clone().unwrap_or_else(|| "—".into()),
                    );
                    row(
                        ui,
                        "Newest",
                        view.latest_compatible
                            .clone()
                            .or(view.latest.clone())
                            .unwrap_or_else(|| "—".into()),
                    );
                    row(ui, "License", t.license.clone());
                });
            if let Some(reason) = &view.incompatible_reason {
                ui.colored_label(c.warning, format!("Not installable here: {reason}"));
            }
            if let Some(note) = &view.note {
                ui.colored_label(c.text_muted, note);
            }
        }
        ui.add_space(16.0);

        if let Some(r) = &self.running {
            ui.label(
                egui::RichText::new(format!("{} {} — {:?}", r.verb, r.tool, r.phase))
                    .color(c.text_secondary),
            );
            #[allow(clippy::cast_precision_loss)]
            let frac = if r.total == 0 {
                0.0
            } else {
                r.done as f32 / r.total as f32
            };
            ui.add(
                egui::ProgressBar::new(frac)
                    .desired_height(4.0)
                    .fill(c.accent),
            );
            if ui.button("Cancel").clicked() {
                self.cancel();
            }
            return;
        }
        if view.id == "kim" {
            ui.label(
                egui::RichText::new("This is Kim itself. It does not install or update itself.")
                    .color(c.text_muted),
            );
            return;
        }
        ui.horizontal(|ui| {
            let primary = match view.status {
                ToolStatus::Available => view
                    .latest_compatible
                    .as_ref()
                    .map(|v| format!("Install {v}")),
                ToolStatus::UpdateAvailable => view
                    .latest_compatible
                    .as_ref()
                    .map(|v| format!("Update to {v}")),
                ToolStatus::Damaged => Some("Repair".to_string()),
                _ => None,
            };
            if let Some(label) = primary {
                let button =
                    egui::Button::new(egui::RichText::new(label).color(c.text_on_accent).strong())
                        .fill(c.accent);
                if ui
                    .add_enabled(!self.settings.offline_mode, button)
                    .clicked()
                {
                    self.start_install(&view.id);
                }
            }
            if matches!(
                view.status,
                ToolStatus::Installed | ToolStatus::UpdateAvailable | ToolStatus::Damaged
            ) && ui
                .button(egui::RichText::new("Uninstall…").color(c.danger))
                .clicked()
            {
                self.confirm = Some(ConfirmUninstall {
                    tool: view.id.clone(),
                    ..Default::default()
                });
            }
        });
    }

    fn confirm_dialog(&mut self, ctx: &egui::Context) {
        let c = theme::Colors::get();
        let mut close = false;
        let mut go = None;
        if let Some(conf) = &mut self.confirm {
            egui::Modal::new(egui::Id::unique("confirm-uninstall")).show(ctx, |ui| {
                ui.set_max_width(440.0);
                ui.label(egui::RichText::new(format!("Uninstall {}?", conf.tool)).size(15.0).strong());
                ui.label("Program files are removed. Your settings and data are kept unless you choose otherwise.");
                ui.checkbox(&mut conf.purge, "Also delete this tool's settings and data");
                if conf.purge {
                    ui.colored_label(c.danger, "Deleting data is permanent.");
                    ui.checkbox(&mut conf.understood, "I understand that the data cannot be recovered");
                }
                ui.add_space(8.0);
                ui.horizontal(|ui| {
                    if ui.button("Keep installed").clicked() {
                        close = true;
                    }
                    let ok = !conf.purge || conf.understood;
                    let label = if conf.purge { "Uninstall and delete data" } else { "Uninstall" };
                    if ui.add_enabled(ok, egui::Button::new(egui::RichText::new(label).color(c.danger))).clicked() {
                        go = Some((conf.tool.clone(), conf.purge));
                    }
                });
            });
        }
        if close {
            self.confirm = None;
        }
        if let Some((tool, purge)) = go {
            self.confirm = None;
            self.start_uninstall(&tool, purge);
        }
    }
}

impl eframe::App for KimManagerApp {
    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        if self.ctx.is_none() {
            self.ctx = Some(ui.ctx().clone());
        }
        KimManagerApp::ui(self, ui);
    }
}

fn status_label(v: &ToolView) -> String {
    match v.status {
        ToolStatus::Installed => {
            format!("installed {}", v.installed_version.as_deref().unwrap_or(""))
        }
        ToolStatus::UpdateAvailable => {
            format!("update {}", v.latest_compatible.as_deref().unwrap_or(""))
        }
        ToolStatus::Available => "available".into(),
        ToolStatus::Incompatible => "incompatible".into(),
        ToolStatus::Unreleased => "not released".into(),
        ToolStatus::Damaged => "damaged".into(),
    }
}

/// Status marks are painted shapes, never font glyphs (DESIGN_SYSTEM §3.1): filled circle =
/// installed, ring with dot = update, ring = available, triangle = problem, small dot = none.
fn paint_status_shape(p: &egui::Painter, center: egui::Pos2, s: ToolStatus) {
    let col = status_color(s);
    let r = 4.0;
    match s {
        ToolStatus::Installed => {
            p.circle_filled(center, r, col);
        }
        ToolStatus::UpdateAvailable => {
            p.circle_stroke(center, r + 1.0, egui::Stroke::new(1.5, col));
            p.circle_filled(center, 2.0, col);
        }
        ToolStatus::Available => {
            p.circle_stroke(center, r, egui::Stroke::new(1.5, col));
        }
        ToolStatus::Incompatible | ToolStatus::Damaged => {
            let pts = vec![
                center + egui::vec2(0.0, -r - 1.0),
                center + egui::vec2(r + 1.0, r),
                center + egui::vec2(-r - 1.0, r),
            ];
            p.add(egui::Shape::convex_polygon(pts, col, egui::Stroke::NONE));
        }
        ToolStatus::Unreleased => {
            p.circle_filled(center, 2.0, col);
        }
    }
}

fn status_color(s: ToolStatus) -> egui::Color32 {
    let c = theme::Colors::get();
    match s {
        ToolStatus::Installed => c.success,
        ToolStatus::UpdateAvailable => c.accent,
        ToolStatus::Available => c.info,
        ToolStatus::Incompatible | ToolStatus::Damaged => c.warning,
        ToolStatus::Unreleased => c.text_muted,
    }
}
