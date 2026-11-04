//! Measures what a paste costs before its window appears (Step 55; ADR 0047; NOEUL-NFR-005): the
//! decode of a 3840 × 2160 PNG and the layout and GDI glyphs of a 10 KB text card and of an HTML
//! card, on this machine, in memory.
//!
//! ```text
//! cargo run --release -p noeul --example paste_measure [-- --runs 15]
//! ```
//!
//! Every picture and text is generated here; nothing reads the screen or the clipboard, no window
//! is created and nothing is written. The window's creation and first paint come on top (the pin
//! engine's own measurement, `pin_measure`). Only timings and sizes are printed.

#[cfg(windows)]
fn main() {
    use noeul::card::{Doc, render};
    use noeul::decode::{Format, decode, sniff};
    use noeul::frame::Ledger;
    use std::time::{Duration, Instant};

    let runs: usize = std::env::args()
        .skip_while(|a| a != "--runs")
        .nth(1)
        .and_then(|v| v.parse().ok())
        .unwrap_or(15);
    let median = |mut v: Vec<Duration>| {
        v.sort();
        v[v.len() / 2]
    };
    let ledger = Ledger::new();
    // A 4K screenshot-like PNG (opaque, varied content so it does not compress to nothing).
    let (w, h) = (3840u32, 2160u32);
    let mut px = Vec::with_capacity((w * h * 4) as usize);
    for y in 0..h {
        for x in 0..w {
            px.extend_from_slice(&[(x * 7 + y) as u8, (x + y * 3) as u8, (x ^ y) as u8, 255]);
        }
    }
    let mut png = Vec::new();
    {
        let mut enc = png::Encoder::new(&mut png, w, h);
        enc.set_color(png::ColorType::Rgba);
        enc.set_depth(png::BitDepth::Eight);
        enc.write_header().unwrap().write_image_data(&px).unwrap();
    }
    drop(px);
    let f = sniff(&png, false).expect("a PNG");
    let mut t = Vec::new();
    for _ in 0..runs {
        let start = Instant::now();
        let d = decode(&ledger, &png, f).expect("decodes");
        t.push(start.elapsed());
        assert_eq!((d.frame.width(), d.frame.height()), (w, h));
    }
    println!(
        "decode PNG {w}x{h} ({} KiB): median {:.1} ms over {runs} runs",
        png.len() / 1024,
        median(t).as_secs_f64() * 1000.0
    );
    assert_eq!(Format::Png, f);
    // A 10 KB text card (prose and some code, Korean included).
    let mut text = String::new();
    while text.len() < 10 * 1024 {
        text.push_str("Noeul pins what the clipboard holds; 노을은 클립보드를 핀으로 띄웁니다.\n");
        text.push_str("    fn paste(&mut self) -> Result<Pin> { take(clipboard) }\n");
    }
    let mut engine = noeul::win::card::GdiText::new();
    for (name, doc) in [
        ("text card (10 KB)", Doc::from_text(&text)),
        (
            "HTML card",
            noeul::html::parse(&format!(
                "<h1>Notes</h1>{}",
                "<p>Some <b>bold</b>, <i>italic</i> and <a href=x>linked</a> words.</p>".repeat(60)
            )),
        ),
    ] {
        for dpi in [96, 144] {
            let mut t = Vec::new();
            let mut size = (0, 0);
            for _ in 0..runs {
                let start = Instant::now();
                let card = render(&ledger, &doc, dpi, &mut engine).expect("renders");
                t.push(start.elapsed());
                size = (card.width(), card.height());
            }
            println!(
                "{name} at {} %: {}x{} px, median {:.1} ms over {runs} runs",
                dpi * 100 / 96,
                size.0,
                size.1,
                median(t).as_secs_f64() * 1000.0
            );
        }
    }
    assert_eq!(ledger.live(), (0, 0), "nothing kept");
}

#[cfg(not(windows))]
fn main() {
    eprintln!("paste_measure runs on Windows only");
}
