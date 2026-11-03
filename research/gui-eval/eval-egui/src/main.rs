//! GUI evaluation prototype: eframe/egui with the glow (OpenGL) or wgpu renderer.
//!
//! Draws a Kim-styled tab bar and an 80x24 monospace grid with ASCII, Korean and emoji, then
//! idles (reactive repaint: no frames unless something happens) and exits after N seconds.
//! The window is placed off-screen, not activated and not shown in the taskbar so measuring
//! does not disturb the person using the machine.
//!
//! Usage: eval-egui <seconds> [--onscreen] [--screenshot <file.bmp>]

use eframe::egui;
use std::sync::Arc;
use std::time::{Duration, Instant};

struct App {
    screenshot: Option<std::path::PathBuf>,
    start: Instant,
    first_frame_reported: bool,
    frames: u64,
    lines: Vec<String>,
}

impl eframe::App for App {
    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        self.frames += 1;
        if !self.first_frame_reported {
            self.first_frame_reported = true;
            println!("first_frame_ms={}", self.start.elapsed().as_millis());
            if let Some(path) = self.screenshot.take() {
                ui.ctx().request_screenshot(move |img| {
                    write_bmp(&path, img.size[0], img.size[1], &img.pixels);
                    println!("screenshot={}", path.display());
                });
                ui.ctx().request_repaint();
            }
        }
        let bg = egui::Color32::from_rgb(0x0e, 0x0e, 0x12);
        egui::Frame::new().fill(bg).show(ui, |ui| {
            ui.horizontal(|ui| {
                for (i, name) in ["work", "서버", "claude"].iter().enumerate() {
                    let active = i == 0;
                    let fill = if active {
                        egui::Color32::from_rgb(0x1c, 0x1b, 0x24)
                    } else {
                        bg
                    };
                    egui::Frame::new()
                        .fill(fill)
                        .inner_margin(egui::Margin::symmetric(10, 4))
                        .show(ui, |ui| {
                            ui.colored_label(egui::Color32::from_rgb(0xb7, 0xa6, 0xf5), "●");
                            ui.label(*name);
                        });
                }
            });
            for line in &self.lines {
                ui.label(
                    egui::RichText::new(line)
                        .monospace()
                        .color(egui::Color32::from_rgb(0xd8, 0xd6, 0xe0)),
                );
            }
        });
    }
}

fn main() -> eframe::Result {
    let start = Instant::now();
    let secs: u64 = std::env::args()
        .nth(1)
        .and_then(|s| s.parse().ok())
        .unwrap_or(40);
    let onscreen = std::env::args().any(|a| a == "--onscreen");
    let args: Vec<String> = std::env::args().collect();
    let screenshot = args
        .iter()
        .position(|a| a == "--screenshot")
        .and_then(|i| args.get(i + 1))
        .map(std::path::PathBuf::from);
    let mut viewport = egui::ViewportBuilder::default()
        .with_inner_size([960.0, 640.0])
        .with_title("kim gui-eval (egui)");
    if !onscreen {
        viewport = viewport
            .with_position([-6000.0, -6000.0])
            .with_active(false)
            .with_taskbar(false);
    }
    let options = eframe::NativeOptions {
        viewport,
        ..Default::default()
    };
    eframe::run_native(
        "kim-gui-eval",
        options,
        Box::new(move |cc| {
            let mut fonts = egui::FontDefinitions::default();
            // Fallback chain from design/tokens.json: Korean, symbols, emoji (outline only).
            let extra = std::env::var("EVAL_FALLBACK").unwrap_or_else(|_| "malgun".into());
            for name in extra.split(',') {
                let file = match name {
                    "malgun" => "malgun.ttf",
                    "symbol" => "seguisym.ttf",
                    "emoji" => "seguiemj.ttf",
                    "cascadia" => "CascadiaMono.ttf",
                    _ => continue,
                };
                if let Ok(bytes) = std::fs::read(format!(r"C:\Windows\Fonts\{file}")) {
                    fonts
                        .font_data
                        .insert(name.into(), Arc::new(egui::FontData::from_owned(bytes)));
                    for fam in [egui::FontFamily::Proportional, egui::FontFamily::Monospace] {
                        let mono = fam == egui::FontFamily::Monospace;
                        let list = fonts.families.entry(fam).or_default();
                        if name == "cascadia" && mono {
                            list.insert(0, name.into());
                        } else {
                            list.push(name.into());
                        }
                    }
                }
            }
            cc.egui_ctx.set_fonts(fonts);
            let ctx = cc.egui_ctx.clone();
            std::thread::spawn(move || {
                std::thread::sleep(Duration::from_secs(secs));
                ctx.send_viewport_cmd(egui::ViewportCommand::Close);
                ctx.request_repaint();
            });
            let mut lines = Vec::new();
            for i in 0..24 {
                lines.push(match i % 4 {
                    0 => format!("{i:02} PS C:\\Users\\me\\src\\kim> cargo test --workspace   "),
                    1 => format!("{i:02} 안녕하세요, 수박 터미널입니다. 한글 너비 테스트 가나다라마바사"),
                    2 => format!("{i:02} ✓ build ok  ⚠ warning  ✗ error  → ← ↑ ↓  ─│┌┐└┘  😀🍉"),
                    _ => format!("{i:02} {}", "abcdefghijklmnopqrstuvwxyz0123456789".repeat(2)),
                });
            }
            Ok(Box::new(App {
                screenshot,
                start,
                first_frame_reported: false,
                frames: 0,
                lines,
            }))
        }),
    )
}

/// Minimal 24-bit BMP writer (bottom-up rows), to avoid an image-encoding dependency.
fn write_bmp(path: &std::path::Path, w: usize, h: usize, px: &[egui::Color32]) {
    let row = (w * 3).div_ceil(4) * 4;
    let size = 54 + row * h;
    let mut b = Vec::with_capacity(size);
    b.extend_from_slice(b"BM");
    b.extend_from_slice(&(size as u32).to_le_bytes());
    b.extend_from_slice(&[0; 4]);
    b.extend_from_slice(&54u32.to_le_bytes());
    b.extend_from_slice(&40u32.to_le_bytes());
    b.extend_from_slice(&(w as i32).to_le_bytes());
    b.extend_from_slice(&(h as i32).to_le_bytes());
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
