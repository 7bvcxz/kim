//! Measures the privacy tools' pixel work on large pictures (Step 57; ADR 0041; NOEUL-FR-082):
//! what one output (copy, save, a pin's window) or one edit costs on the CPU.
//!
//! ```text
//! cargo run --release -p noeul --example redact_measure [-- --runs 7]
//! ```
//!
//! On a generated 3840 × 2160 picture (a 4K monitor; nothing reads the screen, nothing is
//! written), the median of `--runs` runs of [`flatten`] — the only path to an output — with:
//! a full-picture solid redaction; a full-picture mosaic at the default and the strongest level;
//! a full-picture blur at the weakest, the default and the strongest level; a 1920 × 1080 blur; a
//! brush blur stroke across the picture; and the overlay's [`refresh`] after a pen stroke next to
//! a 1920 × 1080 blur (the blur is re-rendered whole). Then it checks that nothing is processed
//! while nothing changes ([`effect_passes`]). Only timings and sizes are printed.

use noeul::annotation::{
    Area, Bounds, Effect, Object, P, PALETTE, REDACT_COLOR, Shape, effect_passes, flatten, refresh,
};
use noeul::frame::{Frame, Ledger, Rect};
use std::time::{Duration, Instant};

fn picture(l: &std::sync::Arc<Ledger>, r: Rect) -> Frame {
    let mut px = Vec::with_capacity(r.bgra_bytes() as usize);
    for y in 0..r.height {
        for x in 0..r.width {
            px.extend_from_slice(&[
                (x * 7 + y * 13) as u8,
                (x * 3 + y * 5 + 100) as u8,
                (x * 11 + y * 2 + 50) as u8,
                255,
            ]);
        }
    }
    Frame::new(l, r, px).expect("a generated picture")
}

fn effect(e: Effect, area: Area, level: u8) -> Object {
    Object {
        id: 1,
        shape: Shape::Effect(e, area),
        color: PALETTE[REDACT_COLOR].1,
        level,
        dpi: 96,
    }
}

fn whole(r: Rect) -> Area {
    Area::Box(Bounds::of_rect(r))
}

fn median(mut v: Vec<Duration>) -> Duration {
    v.sort();
    v[v.len() / 2]
}

fn time(runs: usize, mut f: impl FnMut()) -> Duration {
    f(); // warm-up
    median(
        (0..runs)
            .map(|_| {
                let t = Instant::now();
                f();
                t.elapsed()
            })
            .collect(),
    )
}

fn main() {
    let runs = std::env::args()
        .skip_while(|a| a != "--runs")
        .nth(1)
        .and_then(|v| v.parse().ok())
        .unwrap_or(7usize)
        .max(1);
    let l = Ledger::new();
    let r = Rect::new(0, 0, 3840, 2160);
    let base = picture(&l, r);
    let half = Rect::new(960, 540, 1920, 1080);
    println!(
        "picture {}x{} ({} MB), median of {runs} runs, release build expected",
        r.width,
        r.height,
        r.bgra_bytes() / 1_000_000
    );
    let stroke: Vec<P> = (0..64)
        .map(|i| {
            P::new(
                100.0 + i as f32 * 56.0,
                1080.0 + (i as f32 * 0.7).sin() * 600.0,
            )
        })
        .collect();
    let cases: Vec<(&str, Vec<Object>)> = vec![
        ("no annotation (crop only)", vec![]),
        (
            "solid redaction, whole picture",
            vec![effect(Effect::Redact, whole(r), 0)],
        ),
        (
            "mosaic, whole picture, default (8 px cells)",
            vec![effect(Effect::Mosaic, whole(r), 2)],
        ),
        (
            "mosaic, whole picture, strongest (30 px)",
            vec![effect(Effect::Mosaic, whole(r), 6)],
        ),
        (
            "blur, whole picture, weakest (sigma 2)",
            vec![effect(Effect::Blur, whole(r), 0)],
        ),
        (
            "blur, whole picture, default (sigma 4)",
            vec![effect(Effect::Blur, whole(r), 2)],
        ),
        (
            "blur, whole picture, strongest (sigma 15)",
            vec![effect(Effect::Blur, whole(r), 6)],
        ),
        (
            "blur, 1920x1080, default",
            vec![effect(Effect::Blur, whole(half), 2)],
        ),
        (
            "blur brush across the picture, 60 px",
            vec![effect(Effect::Blur, Area::Brush(stroke), 6)],
        ),
    ];
    for (name, objects) in &cases {
        let t = time(runs, || {
            let f = flatten(&base, objects, r).expect("inside");
            std::hint::black_box(f);
        });
        println!("{name:<46} {:>8.1} ms", t.as_secs_f64() * 1000.0);
    }
    // The overlay's refresh after a pen stroke touching a 1920x1080 blur: the blur is re-rendered
    // whole (it is made from all of its pixels), the rest of the picture is not touched.
    let blur = effect(Effect::Blur, whole(half), 2);
    let pen = Object {
        id: 2,
        shape: Shape::Pen(vec![P::new(900.0, 600.0), P::new(1000.0, 700.0)]),
        color: PALETTE[0].1,
        level: 2,
        dpi: 96,
    };
    let objects = [blur, pen.clone()];
    let mut layer = flatten(&base, &objects[..1], r).expect("inside");
    let dirty = pen.bounds().to_rect().expect("a box");
    let t = time(runs, || {
        refresh(&mut layer, &base, &objects, dirty);
    });
    println!(
        "{:<46} {:>8.1} ms",
        "refresh after a pen stroke into a blur",
        t.as_secs_f64() * 1000.0
    );
    println!(
        "blur scratch: {} MB for the 1920x1080 box, {} MB for the whole picture (freed after the call)",
        half.bgra_bytes() / 1_000_000,
        r.bgra_bytes() / 1_000_000
    );
    // Idle: no call, no work.
    let before = effect_passes();
    std::thread::sleep(Duration::from_millis(500));
    println!(
        "effect passes while idle for 500 ms: {}",
        effect_passes() - before
    );
}
