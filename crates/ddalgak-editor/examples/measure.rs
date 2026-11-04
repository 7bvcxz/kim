//! Developer measurement of the editor window (Step 74, ADR 0049): startup to the first frame,
//! frames and CPU time while idle, and a screenshot for visual review, without disturbing the
//! desktop: the window is off-screen, not activated and has no taskbar entry. Sample rules live
//! in a temp folder; the real `%APPDATA%` and a running Ddalgak are never touched (no engine
//! connection: the header says "not running").
//!
//!     cargo run --release -p ddalgak-editor --example measure -- --rules 100 --idle 30 \
//!         --out m.json --screenshot shot.bmp [--lang ko] [--select] [--starter text]
//!
//! `--rules 0` shows the first start (J1).
//!
//! Memory is read from outside while it idles (working set and private bytes of the process).
//! Not shipped.

use ddalgak::paths::Paths;
use ddalgak_editor::i18n::Lang;
use ddalgak_editor::sync::NoEngine;
use ddalgak_editor::{EditorApp, Options};
use eframe::egui;
use serde_json::json;
use std::sync::Arc;
use std::time::{Duration, Instant};

fn sample(root: &std::path::Path, n: usize) -> Paths {
    let paths = Paths::under(root);
    let modes: Vec<serde_json::Value> = (0..4)
        .map(|i| {
            let title = ["Base", "Navigation", "Symbols", "Gaming"][i];
            json!({ "id": format!("m{i}"), "title": title })
        })
        .collect();
    if n == 0 {
        // The first start (J1): no rules file at all.
        std::fs::create_dir_all(&paths.config).expect("dir");
        return paths;
    }
    let mut rules = vec![
        json!({ "id": "caps", "title": "CapsLock acts as Ctrl", "enabled": true, "description": "Ctrl where the thumb rests",
            "trigger": { "keys": "CapsLock" }, "steps": [ { "type": "remap", "to": "LCtrl", "tap": "Escape" } ],
            "consent": { "input-injection": { "granted_at": "2026-11-04T09:00:00Z", "via": "gui" } } }),
        json!({ "id": "thanks", "title": "감사 인사", "enabled": true,
            "trigger": { "keys": "Ctrl+Shift+F2" }, "steps": [ { "type": "type_text", "text": "감사합니다.\n김민수 드림" } ],
            "consent": { "input-injection": { "granted_at": "2026-11-04T09:00:00Z", "via": "gui" } } }),
        json!({ "id": "tool", "title": "Open tool.exe", "enabled": false,
            "trigger": { "keys": "Ctrl+Shift+F8" }, "steps": [ { "type": "open_program", "path": "C:\\Tools\\tool.exe" } ] }),
        json!({ "id": "nav-left", "title": "H moves left", "enabled": true, "modes": ["m1"],
            "trigger": { "keys": "H" }, "steps": [ { "type": "remap", "to": "Left" } ],
            "consent": { "input-injection": { "granted_at": "2026-11-04T09:00:00Z", "via": "gui" } } }),
        json!({ "id": "pause", "title": "F14 pauses", "enabled": true,
            "trigger": { "keys": "Ctrl+Alt+Shift+F14" }, "steps": [ { "type": "engine", "op": "toggle_pause" } ] }),
    ];
    for i in rules.len()..n {
        rules.push(
            json!({ "id": format!("r{i}"), "title": format!("Sample rule {i}"), "enabled": false,
            "modes": [format!("m{}", i % 4)],
            "trigger": { "keys": format!("Ctrl+Alt+Shift+F{}", 13 + i % 8) },
            "steps": [ { "type": "engine", "op": "pause" } ] }),
        );
    }
    let doc = json!({ "schema": "ddalgak.rules", "schema_version": 1, "hotkey_contract": 1, "revision": 7,
        "active_profile": "work", "profiles": [ { "id": "work", "title": "Work", "default_mode": "m0",
        "modes": modes, "rules": rules } ] });
    std::fs::create_dir_all(&paths.config).expect("dir");
    std::fs::write(
        paths.rules(),
        serde_json::to_vec_pretty(&doc).expect("json"),
    )
    .expect("write");
    paths
}

struct Measure {
    app: EditorApp,
    started: Instant,
    first_frame: Option<Duration>,
    frames: u64,
    settled: Option<(u64, u64)>,
    idle: Duration,
    out: Option<std::path::PathBuf>,
    shot: Option<std::path::PathBuf>,
    select: bool,
    starter: Option<ddalgak_editor::form::Effect>,
    done: bool,
}

impl eframe::App for Measure {
    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        self.frames += 1;
        if self.first_frame.is_none() {
            self.first_frame = Some(self.started.elapsed());
        }
        if self.frames == 2
            && let Some(e) = self.starter.take()
        {
            self.app.start_new(Some(e));
        }
        if self.select && self.frames == 2 {
            ui.input_mut(|i| {
                i.events.push(egui::Event::Key {
                    key: egui::Key::ArrowDown,
                    physical_key: None,
                    pressed: true,
                    repeat: false,
                    modifiers: egui::Modifiers::NONE,
                });
            });
        }
        self.app.ui(ui);
        if self.frames == 4
            && let Some(path) = self.shot.take()
        {
            ui.ctx().request_screenshot(move |img| {
                write_bmp(&path, img.size[0], img.size[1], &img.pixels);
            });
        }
        if self.frames < 6 {
            ui.ctx().request_repaint();
        }
        let now = self.started.elapsed();
        if self.settled.is_none() && now > Duration::from_secs(3) {
            self.settled = Some((ddalgak::win::process_cpu_us(), self.frames));
        }
        if !self.done && now > self.idle + Duration::from_secs(3) {
            self.done = true;
            let (cpu0, f0) = self.settled.unwrap_or((0, 0));
            let r = json!({
                "first_frame_ms": self.first_frame.map(|d| d.as_millis()),
                "idle_s": self.idle.as_secs(),
                "cpu_ms_while_idle": (ddalgak::win::process_cpu_us().saturating_sub(cpu0)) / 1000,
                "frames_while_idle": self.frames.saturating_sub(f0 + 1),
                "rules": self.app.rows().len(),
            });
            if let Some(o) = &self.out {
                std::fs::write(o, r.to_string()).expect("write results");
            }
            // A starter's form is an unsaved draft, so the editor would ask before closing (that
            // is its draft safety); the measurement ends its own process instead.
            if self.app.has_unsaved_changes() {
                std::process::exit(0);
            }
            ui.ctx().send_viewport_cmd(egui::ViewportCommand::Close);
        }
    }
}

fn main() -> eframe::Result {
    let started = Instant::now();
    let args: Vec<String> = std::env::args().collect();
    let flag = |f: &str| {
        args.iter()
            .position(|a| a == f)
            .and_then(|i| args.get(i + 1))
            .cloned()
    };
    let n: usize = flag("--rules").and_then(|s| s.parse().ok()).unwrap_or(100);
    let idle = Duration::from_secs(flag("--idle").and_then(|s| s.parse().ok()).unwrap_or(10));
    let out = flag("--out").map(std::path::PathBuf::from);
    let shot = flag("--screenshot").map(std::path::PathBuf::from);
    let lang = flag("--lang")
        .and_then(|l| Lang::parse(&l))
        .unwrap_or(Lang::En);
    let select = args.iter().any(|a| a == "--select");
    // `--starter remap|text|program`: the form of a J1 starter suggestion, at its details step.
    let starter = flag("--starter").and_then(|s| match s.as_str() {
        "remap" => Some(ddalgak_editor::form::Effect::Remap),
        "text" => Some(ddalgak_editor::form::Effect::TypeText),
        "program" => Some(ddalgak_editor::form::Effect::OpenProgram),
        _ => None,
    });
    let root = std::env::temp_dir().join(format!("ddalgak-editor-measure-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    let paths = sample(&root, n);
    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_title("Ddalgak rules (measure)")
            .with_inner_size([1060.0, 680.0])
            .with_position([-6000.0, -6000.0])
            .with_active(false)
            .with_taskbar(false),
        renderer: eframe::Renderer::Glow,
        ..Default::default()
    };
    let result = eframe::run_native(
        "ddalgak-editor-measure",
        options,
        Box::new(move |cc| {
            // A guard: the window closes even if the frame loop stalls.
            let ctx = cc.egui_ctx.clone();
            std::thread::spawn(move || {
                std::thread::sleep(idle + Duration::from_secs(8));
                ctx.send_viewport_cmd(egui::ViewportCommand::Close);
                ctx.request_repaint();
            });
            let ctx = cc.egui_ctx.clone();
            std::thread::spawn(move || {
                // One repaint after the idle period so the result is written (nothing else wakes
                // an idle window; this is the measurement's own clock, not the editor's).
                std::thread::sleep(idle + Duration::from_secs(3) + Duration::from_millis(200));
                ctx.request_repaint();
            });
            let mut opts = Options::for_test(paths, Arc::new(NoEngine));
            opts.lang = lang;
            Ok(Box::new(Measure {
                app: EditorApp::new(opts),
                started,
                first_frame: None,
                frames: 0,
                settled: None,
                idle,
                out,
                shot,
                select,
                starter,
                done: false,
            }))
        }),
    );
    let _ = std::fs::remove_dir_all(&root);
    result
}

fn write_bmp(path: &std::path::Path, w: usize, h: usize, px: &[egui::Color32]) {
    let row = (w * 3).div_ceil(4) * 4;
    let size = 54 + row * h;
    let mut b = Vec::with_capacity(size);
    b.extend_from_slice(b"BM");
    b.extend_from_slice(&u32::try_from(size).unwrap_or(0).to_le_bytes());
    b.extend_from_slice(&[0; 4]);
    b.extend_from_slice(&54u32.to_le_bytes());
    b.extend_from_slice(&40u32.to_le_bytes());
    b.extend_from_slice(&i32::try_from(w).unwrap_or(0).to_le_bytes());
    b.extend_from_slice(&i32::try_from(h).unwrap_or(0).to_le_bytes());
    b.extend_from_slice(&1u16.to_le_bytes());
    b.extend_from_slice(&24u16.to_le_bytes());
    b.extend_from_slice(&[0; 24]);
    for y in (0..h).rev() {
        let start = b.len();
        for x in 0..w {
            let c = px[y * w + x];
            b.extend_from_slice(&[c.b(), c.g(), c.r()]);
        }
        b.resize(start + row, 0);
    }
    std::fs::write(path, b).expect("write bmp");
}
