//! `kim-manager` — the optional Kim Manager window. Started only by the user; closing it ends
//! the process (no tray icon, no background updater).

#![windows_subsystem = "windows"]

use eframe::egui;
use kim_manager::{Backend, KimManagerApp};

fn main() -> eframe::Result {
    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_title("Kim")
            .with_inner_size([900.0, 560.0])
            .with_min_inner_size([720.0, 480.0]),
        renderer: eframe::Renderer::Glow,
        ..Default::default()
    };
    eframe::run_native(
        "kim-manager",
        options,
        Box::new(|cc| {
            let backend = Backend::real().map_err(|e| e.to_string())?;
            Ok(Box::new(KimManagerApp::new(backend, Some(&cc.egui_ctx))))
        }),
    )
}
