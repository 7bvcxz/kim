//! Developer preview of Kim Manager with sample data, for visual review and resource measurement
//! without disturbing the desktop: the window is off-screen, not activated, not in the taskbar.
//!
//!     cargo run --release -p kim-manager --example preview -- --screenshot shot.bmp --idle 30
//!     cargo run --release -p kim-manager --example preview -- --select noeul --screenshot n.bmp
//!
//! Sample data lives in a temp directory (a test-signed catalog and one fake install record); the
//! real per-user locations are never touched. Not shipped.

use eframe::egui;
use kim::catalog::signature::{self, TrustedKey};
use kim::paths::Paths;
use kim::platform::Platform;
use kim_manager::{Backend, KimManagerApp};
use std::sync::Arc;
use std::time::{Duration, Instant};

const SEED: [u8; 32] = [33u8; 32];

fn sample_backend() -> Backend {
    let root = std::env::temp_dir().join(format!("kim-manager-preview-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    let paths = Paths::under(&root);
    let asset = |v: &str, os: &str, arch: &str, min: Option<u32>| {
        let mut a = serde_json::json!({"os": os, "arch": arch, "url": format!("https://example.invalid/{v}.zip"),
            "size": 4_194_304, "sha256": "0".repeat(64), "kind": "zip", "entrypoint": "tool.exe"});
        if let Some(m) = min {
            a["min_os_build"] = m.into();
        }
        a
    };
    let rel = |v: &str, assets: Vec<serde_json::Value>| {
        serde_json::json!({"version": v, "channel": "stable",
        "published_at": "2026-11-01T00:00:00Z", "release_notes": format!("Release {v}."), "assets": assets})
    };
    let doc = serde_json::json!({
        "schema": "kim.catalog", "schema_version": 1, "catalog_version": 900,
        "generated_at": "2026-11-04T00:00:00Z", "expires_at": "2027-05-04T00:00:00Z", "publisher": "preview",
        "tools": [
            {"id": "kim", "name": "Kim", "summary": "Catalog and explicit installer", "description": "Kim itself.",
             "license": "MIT OR Apache-2.0", "support": "in-development", "releases": []},
            {"id": "subak", "name": "Subak", "summary": "Persistent Windows terminal", "description":
             "Sessions live in a per-user background Host and survive closing every window. 한글 지원.",
             "license": "MIT OR Apache-2.0", "support": "preview",
             "releases": [rel("0.1.0", vec![asset("0.1.0", "windows", "x86_64", Some(17763))])]},
            {"id": "notes", "name": "Notes", "summary": "Hypothetical sample tool (installed)", "description": "Sample data.",
             "license": "MIT", "support": "stable",
             "releases": [rel("1.0.0", vec![asset("1.0.0", "windows", "x86_64", None)]), rel("1.1.0", vec![asset("1.1.0", "windows", "x86_64", None)])]},
            {"id": "arm-only", "name": "ARM Only", "summary": "Hypothetical sample tool (incompatible)", "description": "Sample data.",
             "license": "MIT", "support": "stable", "releases": [rel("2.0.0", vec![asset("2.0.0", "windows", "aarch64", None)])]}
        ]
    });
    // The real Noeul and Ddalgak entries (planned, with their metadata), as the bundled catalog
    // lists them (Step 47).
    let mut doc = doc;
    let bundled: serde_json::Value =
        serde_json::from_slice(kim::catalog::store::BUNDLED_CATALOG).expect("bundled catalog");
    for t in bundled["tools"].as_array().expect("tools") {
        if t["id"] == "noeul" || t["id"] == "ddalgak" {
            doc["tools"].as_array_mut().expect("tools").push(t.clone());
        }
    }
    let bytes = serde_json::to_vec_pretty(&doc).expect("json");
    let sig = serde_json::to_vec(&signature::sign(&bytes, &SEED)).expect("json");
    std::fs::create_dir_all(paths.catalog_cache_dir()).expect("dir");
    std::fs::write(paths.catalog_cache_dir().join("catalog.json"), bytes).expect("write");
    std::fs::write(paths.catalog_cache_dir().join("catalog.json.sig"), sig).expect("write");
    std::fs::create_dir_all(paths.tool_dir("notes")).expect("dir");
    std::fs::write(paths.install_record("notes"), br#"{"version":"1.0.0"}"#).expect("write");
    let public: &'static str = Box::leak(signature::public_key_hex(&SEED).into_boxed_str());
    let id: &'static str = Box::leak(
        signature::key_id(&signature::unhex::<32>(public).expect("hex")).into_boxed_str(),
    );
    let mut keys = vec![TrustedKey {
        key_id: id,
        public_key_hex: public,
        label: "preview key",
    }];
    keys.extend_from_slice(signature::TRUSTED_KEYS);
    Backend {
        paths,
        keys,
        downloader: Arc::new(kim::fetch::DefaultFetcher::default()),
        settings_path: Some(root.join("settings.json")),
        platform: Platform::current(),
        catalog_source: "http://127.0.0.1:9/none.json".into(),
    }
}

struct Preview {
    app: KimManagerApp,
    start: Instant,
    shot: Option<std::path::PathBuf>,
    idle: Duration,
    frames: u32,
}

impl eframe::App for Preview {
    fn ui(&mut self, ui: &mut egui::Ui, frame: &mut eframe::Frame) {
        self.frames += 1;
        eframe::App::ui(&mut self.app, ui, frame);
        if self.frames == 3
            && let Some(path) = self.shot.take()
        {
            ui.ctx().request_screenshot(move |img| {
                write_bmp(&path, img.size[0], img.size[1], &img.pixels);
                println!("screenshot={}", path.display());
            });
        }
        if self.frames < 4 {
            ui.ctx().request_repaint();
        }
        let ctx = ui.ctx().clone();
        let remaining = self.idle.saturating_sub(self.start.elapsed());
        if remaining.is_zero() {
            ctx.send_viewport_cmd(egui::ViewportCommand::Close);
        }
    }
}

fn main() -> eframe::Result {
    let args: Vec<String> = std::env::args().collect();
    let flag = |f: &str| {
        args.iter()
            .position(|a| a == f)
            .and_then(|i| args.get(i + 1))
            .cloned()
    };
    let shot = flag("--screenshot").map(std::path::PathBuf::from);
    let idle = Duration::from_secs(flag("--idle").and_then(|s| s.parse().ok()).unwrap_or(5));
    // `--select noeul` shows that tool's details instead of the first one.
    let select = flag("--select");
    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_title("Kim (preview)")
            .with_inner_size([900.0, 560.0])
            .with_position([-6000.0, -6000.0])
            .with_active(false)
            .with_taskbar(false),
        renderer: eframe::Renderer::Glow,
        ..Default::default()
    };
    eframe::run_native(
        "kim-manager-preview",
        options,
        Box::new(move |cc| {
            let ctx = cc.egui_ctx.clone();
            std::thread::spawn(move || {
                std::thread::sleep(idle + Duration::from_millis(500));
                ctx.send_viewport_cmd(egui::ViewportCommand::Close);
                ctx.request_repaint();
            });
            let mut app = KimManagerApp::new(sample_backend(), Some(&cc.egui_ctx));
            if let Some(id) = &select {
                app.select(id);
            }
            Ok(Box::new(Preview {
                app,
                start: Instant::now(),
                shot,
                idle,
                frames: 0,
            }))
        }),
    )
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
