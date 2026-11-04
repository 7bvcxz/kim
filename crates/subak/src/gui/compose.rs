//! The client area as the window shows it, composed in memory from its layers: the terminal
//! frame (pane or state screen) at the top-left, the theme's background around it, the banner
//! across the top, and the IME composition at the cursor. `WM_PAINT` presents the same layers
//! with GDI in the same order; tests and the test hook's frame shots use this function, so the
//! golden frames below are the window's own content, never a capture of the screen.

use super::screens::tokens;
use subak_term::render::{Frame, Rgb};

/// The banner layer.
#[derive(Debug)]
pub struct BannerLayer<'a> {
    pub frame: &'a Frame,
    /// Its fill, carried across the full width.
    pub fill: Rgb,
    /// The status colour of the left marker, and its width; `None` in High Contrast.
    pub marker: Option<(Rgb, u32)>,
}

fn blit(out: &mut Frame, f: &Frame, x0: u32, y0: u32) {
    let (w, h) = (out.width, out.height);
    for y in 0..f.height.min(h.saturating_sub(y0)) {
        let src = (y * f.width) as usize;
        let dst = ((y0 + y) * w + x0) as usize;
        let n = f.width.min(w.saturating_sub(x0)) as usize;
        out.pixels[dst..dst + n].copy_from_slice(&f.pixels[src..src + n]);
    }
}

/// Composes a `client` area.
pub fn compose(
    client: (u32, u32),
    background: Rgb,
    main: &Frame,
    banner: Option<&BannerLayer<'_>>,
    ime: Option<(&Frame, (u32, u32))>,
) -> Frame {
    let (w, h) = (client.0.max(1), client.1.max(1));
    let mut out = Frame::new(w, h, background);
    blit(&mut out, main, 0, 0);
    if let Some(b) = banner {
        let bh = b.frame.height;
        out.fill(0, 0, w, bh, b.fill);
        blit(&mut out, b.frame, 0, 0);
        if let Some((color, width)) = b.marker {
            out.fill(0, 0, width, bh, color);
            out.fill(0, bh.saturating_sub(1), w, 1, tokens::BORDER_SUBTLE);
        }
    }
    if let Some((f, (x, y))) = ime {
        blit(&mut out, f, x, y);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::super::model::{EndReason, Phase, TermInfo};
    use super::super::screens::{self, Tone};
    use super::*;
    use subak_term::TermView;
    use subak_term::render::{RenderOptions, Renderer, Theme};

    /// A golden of a composed frame: its size and hash, and for every cell of the grid a
    /// background letter and an ink letter (`.` for none) from a legend of the colours seen.
    fn golden(name: &str, f: &Frame, cell: (u32, u32), pad: u32, grid: (u16, u16)) -> String {
        let mut legend: Vec<Rgb> = Vec::new();
        let mut letter = |c: Rgb| -> char {
            let i = legend.iter().position(|l| *l == c).unwrap_or_else(|| {
                legend.push(c);
                legend.len() - 1
            });
            char::from(b'a' + u8::try_from(i % 26).unwrap())
        };
        let mut body = String::new();
        for row in 0..grid.1 {
            let (mut bgs, mut inks) = (String::new(), String::new());
            for col in 0..grid.0 {
                let (x0, y0) = (pad + u32::from(col) * cell.0, pad + u32::from(row) * cell.1);
                let bg = f.pixel(x0, y0 + cell.1 - 1);
                let ink = (y0..y0 + cell.1)
                    .flat_map(|y| (x0..x0 + cell.0).map(move |x| (x, y)))
                    .map(|(x, y)| f.pixel(x, y))
                    .find(|p| *p != bg);
                bgs.push(letter(bg));
                inks.push(ink.map_or('.', &mut letter));
            }
            body.push_str(&format!("{row:2} bg {bgs}\n{row:2} fg {inks}\n"));
        }
        let legend: Vec<String> = legend
            .iter()
            .enumerate()
            .map(|(i, c)| {
                format!(
                    "{}=#{:02x}{:02x}{:02x}",
                    char::from(b'a' + i as u8),
                    c.0,
                    c.1,
                    c.2
                )
            })
            .collect();
        format!(
            "# {name}: {}x{} client area, cell {}x{}, hash {:016x}\n# legend {}\n{body}",
            f.width,
            f.height,
            cell.0,
            cell.1,
            f.hash(),
            legend.join(" ")
        )
    }

    #[track_caller]
    fn check(name: &str, actual: &str) {
        let dir = format!("{}/tests/golden", env!("CARGO_MANIFEST_DIR"));
        let path = format!("{dir}/gui_{name}.txt");
        if std::env::var_os("SUBAK_BLESS").is_some() {
            std::fs::create_dir_all(&dir).unwrap();
            std::fs::write(&path, actual).unwrap();
            return;
        }
        let want = std::fs::read_to_string(&path)
            .unwrap_or_else(|_| {
                panic!("missing golden {path}; run with SUBAK_BLESS=1 and review it")
            })
            .replace("\r\n", "\n");
        assert_eq!(
            actual, want,
            "golden gui_{name} differs (SUBAK_BLESS=1 refreshes it)"
        );
    }

    fn term(name: &str, pid: u32) -> TermInfo {
        TermInfo {
            id: format!("t-{pid:012x}"),
            name: name.into(),
            state: "running".into(),
            pid: Some(pid),
            program: "cmd.exe".into(),
            history_limit: 2000,
        }
    }

    fn renderer(scale: f32, padding: f32) -> Renderer {
        Renderer::builtin(
            Theme::kim_dark(),
            RenderOptions {
                scale,
                padding,
                ..RenderOptions::default()
            },
        )
    }

    /// The window at `scale` with the state screen of `phase` on a 60 × 14 grid.
    fn state_frame(phase: &Phase, scale: f32) -> (Frame, (u32, u32), u32) {
        let (cols, rows) = (60u16, 14u16);
        let mut r = renderer(scale, 8.0);
        let mut v = TermView::new(cols, rows, 0);
        v.feed(&screens::screen(phase, "subak-golden", cols, rows).unwrap());
        r.render(&v);
        let m = r.metrics();
        let client = (r.frame().width + 5, r.frame().height + 3);
        let f = compose(client, Theme::kim_dark().background, r.frame(), None, None);
        (f, (m.width, m.height), r.padding())
    }

    #[test]
    fn golden_state_screens_at_100_and_200_percent() {
        let phases = [
            ("connecting", Phase::Connecting),
            ("empty", Phase::Empty { notice: None }),
            (
                "choosing",
                Phase::Choosing {
                    list: vec![term("빌드", 4242), term("claude", 77)],
                    selected: 1,
                    notice: Some("`old` is no longer running".into()),
                },
            ),
            (
                "failed",
                Phase::Failed {
                    code: "host_identity_mismatch".into(),
                    message: "`subak-golden` is not served by a Subak Host of this user".into(),
                    hint: Some("use another --endpoint".into()),
                },
            ),
        ];
        for (name, phase) in &phases {
            for (scale, pct) in [(1.0, 100), (2.0, 200)] {
                let (f, cell, pad) = state_frame(phase, scale);
                // Around the grid: the background, never garbage.
                assert_eq!(f.pixel(f.width - 1, f.height - 1), tokens::BG_BASE);
                check(
                    &format!("{name}_{pct}"),
                    &golden(&format!("{name}_{pct}"), &f, cell, pad, (60, 14)),
                );
            }
        }
    }

    #[test]
    fn golden_banner_over_a_pane_and_inline_ime_composition() {
        let (cols, rows) = (40u16, 8u16);
        let mut pane = TermView::new(cols, rows, 100);
        pane.feed("C:\\>echo 한글 ┌─┐\r\n한글 ┌─┐\r\n\r\nC:\\>".as_bytes());
        let mut r = renderer(1.0, 8.0);
        r.set_focused(false);
        r.render(&pane);
        let m = r.metrics();
        let client = (r.frame().width, r.frame().height);
        // The banner of a detached window: info tone, its own one-row renderer.
        let phase = Phase::Ended {
            term: term("work", 1),
            why: EndReason::DetachedBy(Some("cli client (pid 42)".into())),
        };
        let b = screens::banner(&phase).unwrap();
        assert_eq!(b.tone, Tone::Info);
        let mut theme = Theme::kim_dark();
        theme.background = b.tone.fill();
        theme.foreground = tokens::TEXT_PRIMARY;
        let mut br = Renderer::builtin(
            theme,
            RenderOptions {
                padding: 4.0,
                ..RenderOptions::default()
            },
        );
        let bcols = u16::try_from((client.0 - 8) / br.metrics().width).unwrap();
        let mut bv = TermView::new(bcols, 1, 0);
        bv.feed(&screens::banner_vt(&b, bcols));
        br.render(&bv);
        let layer = BannerLayer {
            frame: br.frame(),
            fill: b.tone.fill(),
            marker: Some((b.tone.color(), 3)),
        };
        let f = compose(client, tokens::BG_BASE, r.frame(), Some(&layer), None);
        assert_eq!(f.pixel(1, 1), tokens::INFO, "the status marker");
        assert_eq!(
            f.pixel(client.0 - 1, 2),
            b.tone.fill(),
            "the fill across the width"
        );
        check(
            "banner_100",
            &golden("banner_100", &f, (m.width, m.height), 8, (cols, rows)),
        );
        // The IME composition: an underlined syllable on bg.overlay at the cursor.
        let mut theme = Theme::kim_dark();
        theme.background = tokens::BG_OVERLAY;
        theme.foreground = tokens::TEXT_PRIMARY;
        let mut ir = Renderer::builtin(
            theme,
            RenderOptions {
                padding: 0.0,
                ..RenderOptions::default()
            },
        );
        let mut iv = TermView::new(2, 1, 0);
        iv.feed("\x1b[?25l\x1b[4m한".as_bytes());
        ir.render(&iv);
        let (cc, cr) = pane.cursor_in_view().unwrap();
        let at = (8 + u32::from(cc) * m.width, 8 + u32::from(cr) * m.height);
        let f = compose(
            client,
            tokens::BG_BASE,
            r.frame(),
            None,
            Some((ir.frame(), at)),
        );
        assert_eq!(f.pixel(at.0, at.1), tokens::BG_OVERLAY);
        check(
            "ime_100",
            &golden("ime_100", &f, (m.width, m.height), 8, (cols, rows)),
        );
    }
}
