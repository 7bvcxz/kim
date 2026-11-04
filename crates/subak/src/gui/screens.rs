//! What the window says when it shows no live pane: the loading, empty, chooser and error
//! screens, and the one-line banner over a pane that is not live any more (disconnected, ended,
//! detached) or over a notice (DESIGN_SYSTEM §3.8: one muted sentence plus one action, never a
//! blank window; state is never colour-only — every screen and banner says it in words).
//!
//! Screens are VT text for a [`subak_term::TermView`] of the window's grid, drawn by the same
//! renderer as the pane (the terminal font chain, so Korean names render like the pane does).
//! The colours are the design tokens (design/tokens.json; checked by a test).

use super::model::{EndReason, Phase, TermInfo};
use subak_term::render::Rgb;

/// Design tokens the screens use (`kim-dark`, DESIGN_SYSTEM §2.1).
pub mod tokens {
    use subak_term::render::Rgb;
    pub const BG_BASE: Rgb = Rgb(0x0E, 0x0E, 0x12);
    pub const BG_RAISED: Rgb = Rgb(0x15, 0x15, 0x1B);
    pub const BG_OVERLAY: Rgb = Rgb(0x1C, 0x1B, 0x24);
    pub const BORDER_SUBTLE: Rgb = Rgb(0x26, 0x25, 0x2F);
    pub const TEXT_PRIMARY: Rgb = Rgb(0xE8, 0xE6, 0xF0);
    pub const TEXT_SECONDARY: Rgb = Rgb(0xA9, 0xA6, 0xB8);
    pub const TEXT_MUTED: Rgb = Rgb(0x95, 0x92, 0xA8);
    pub const ACCENT: Rgb = Rgb(0xB7, 0xA6, 0xF5);
    pub const ACCENT_SOFT: Rgb = Rgb(0x2B, 0x27, 0x42);
    pub const WARNING: Rgb = Rgb(0xE8, 0xC2, 0x7A);
    pub const DANGER: Rgb = Rgb(0xF0, 0x86, 0x8F);
    pub const INFO: Rgb = Rgb(0x8A, 0xB4, 0xF8);
}
use tokens::*;

fn fg(c: Rgb) -> String {
    format!("\x1b[38;2;{};{};{}m", c.0, c.1, c.2)
}

fn bg(c: Rgb) -> String {
    format!("\x1b[48;2;{};{};{}m", c.0, c.1, c.2)
}

const RESET: &str = "\x1b[0m";

/// The screen for Windows High Contrast (DESIGN_SYSTEM §4.3): the token colours are dropped, so
/// text uses the system's colours (the theme's foreground on its background), and a highlighted
/// background (the chooser's selection) becomes reverse video in the system's colours.
pub fn high_contrast(vt: &[u8]) -> Vec<u8> {
    let s = String::from_utf8_lossy(vt);
    let mut out = String::with_capacity(s.len());
    let mut rest = s.as_ref();
    while let Some(i) = rest.find("\x1b[") {
        out.push_str(&rest[..i]);
        let tail = &rest[i..];
        let end = tail
            .find('m')
            .filter(|e| tail[2..*e].bytes().all(|b| b.is_ascii_digit() || b == b';'));
        match end {
            Some(e) if tail[2..].starts_with("38;2;") => rest = &tail[e + 1..],
            Some(e) if tail[2..].starts_with("48;2;") => {
                out.push_str("\x1b[7m");
                rest = &tail[e + 1..];
            }
            _ => {
                out.push_str("\x1b[");
                rest = &tail[2..];
            }
        }
    }
    out.push_str(rest);
    out.into_bytes()
}

/// Display width of `s` in cells (wide characters count two).
fn width(s: &str) -> usize {
    s.chars()
        .map(|c| usize::from(subak_term::screen::char_width(c)))
        .sum()
}

/// `s` cut to `max` cells, with `…` when cut.
fn fit(s: &str, max: usize) -> String {
    if width(s) <= max {
        return s.to_string();
    }
    let mut out = String::new();
    let mut w = 0;
    for c in s.chars() {
        let cw = usize::from(subak_term::screen::char_width(c));
        if w + cw + 1 > max {
            break;
        }
        out.push(c);
        w += cw;
    }
    out.push('…');
    out
}

/// One line of a screen: a colour and a text.
struct Line(Rgb, String, bool);

/// Writes `lines` from row `top`, indented by `left` columns, cut to the grid.
fn page(cols: u16, rows: u16, top: u16, lines: &[Line]) -> Vec<u8> {
    let cols = usize::from(cols);
    let left = if cols >= 60 { 4 } else { 1 };
    let mut s = String::from("\x1b[?25l\x1b[2J");
    for (i, Line(color, text, bold)) in lines.iter().enumerate() {
        let row = usize::from(top) + i;
        if row >= usize::from(rows) {
            break;
        }
        s.push_str(&format!("\x1b[{};{}H", row + 1, left + 1));
        if *bold {
            s.push_str("\x1b[1m");
        }
        s.push_str(&fg(*color));
        s.push_str(&fit(text, cols.saturating_sub(left + 1)));
        s.push_str(RESET);
    }
    s.into_bytes()
}

/// The first row of a screen's text: about a third down, so short screens sit near the eye line.
fn top_row(rows: u16, lines: usize) -> u16 {
    let free = usize::from(rows).saturating_sub(lines);
    u16::try_from(free / 3).unwrap_or(0)
}

/// The rows the chooser's list starts at, for mapping a click to a Terminal.
pub fn chooser_first_row(rows: u16, count: usize, notice: bool) -> u16 {
    top_row(rows, count + 4 + usize::from(notice)) + 2 + u16::from(notice)
}

fn term_row(t: &TermInfo) -> String {
    let pid = t.pid.map_or_else(|| "—".to_string(), |p| p.to_string());
    format!(
        "{:<20}  {:<8}  pid {:<7}  {}",
        fit(&t.name, 20) + &" ".repeat(20usize.saturating_sub(width(&fit(&t.name, 20)))),
        t.state,
        pid,
        t.program
    )
}

/// The VT text of the state screen for `phase` on a `cols × rows` grid, or `None` when the
/// window shows a pane.
pub fn screen(phase: &Phase, endpoint: &str, cols: u16, rows: u16) -> Option<Vec<u8>> {
    let title = |t: &str| Line(ACCENT, t.to_string(), true);
    let say = |t: String| Line(TEXT_PRIMARY, t, false);
    let muted = |t: String| Line(TEXT_MUTED, t, false);
    let blank = || Line(TEXT_MUTED, String::new(), false);
    let lines: Vec<Line> = match phase {
        Phase::Connecting => vec![
            title("Subak"),
            blank(),
            say("Connecting to the Subak Host…".into()),
            muted(format!("endpoint {endpoint}")),
        ],
        Phase::Attaching(t) => vec![
            title("Subak"),
            blank(),
            say(format!("Opening `{}`…", t.name)),
        ],
        Phase::Empty { notice } => {
            let mut v = vec![title("Subak"), blank()];
            if let Some(n) = notice {
                v.push(Line(WARNING, n.clone(), false));
            }
            v.extend([
                say("No Terminal is running in the Host.".into()),
                muted("Start one from any console:  subak new <name>".into()),
                muted("It opens here as soon as it runs.".into()),
            ]);
            v
        }
        Phase::Choosing {
            list,
            selected,
            notice,
        } => {
            // Drawn below with highlighted rows.
            return Some(chooser(list, *selected, notice.as_deref(), cols, rows));
        }
        Phase::Failed {
            code,
            message,
            hint,
        } => {
            let mut v = vec![
                Line(DANGER, "Cannot reach the Subak Host".into(), true),
                blank(),
                say(message.clone()),
                muted(format!("error: {code}")),
            ];
            if let Some(h) = hint {
                v.push(muted(h.clone()));
            }
            v.extend([blank(), say("Press Enter to try again.".into())]);
            v
        }
        Phase::Disconnected {
            last: None,
            reason,
            attempt,
        } => vec![
            Line(WARNING, "Disconnected".into(), true),
            blank(),
            say(format!(
                "{}; reconnecting (attempt {})…",
                capital(reason),
                attempt + 1
            )),
            muted(
                "Press Enter to retry now. Terminals live in the Host, not in this window.".into(),
            ),
        ],
        Phase::Attached { .. } | Phase::Ended { .. } | Phase::Disconnected { .. } => return None,
    };
    let top = top_row(rows, lines.len());
    Some(page(cols, rows, top, &lines))
}

fn capital(s: &str) -> String {
    let mut c = s.chars();
    c.next()
        .map(|f| f.to_uppercase().collect::<String>() + c.as_str())
        .unwrap_or_default()
}

fn chooser(
    list: &[TermInfo],
    selected: usize,
    notice: Option<&str>,
    cols: u16,
    rows: u16,
) -> Vec<u8> {
    let n = list.len();
    let top = top_row(rows, n + 4 + usize::from(notice.is_some()));
    let left = if cols >= 60 { 4 } else { 1 };
    let usable = usize::from(cols).saturating_sub(left + 1);
    let mut s = String::from("\x1b[?25l\x1b[2J");
    let mut row = usize::from(top);
    let mut put = |row: usize, text: String| {
        if row < usize::from(rows) {
            s.push_str(&format!("\x1b[{};{}H{text}{RESET}", row + 1, left + 1));
        }
    };
    put(row, format!("\x1b[1m{}Choose a Terminal", fg(ACCENT)));
    row += 1;
    if let Some(n) = notice {
        put(row, format!("{}{}", fg(WARNING), fit(n, usable)));
        row += 1;
    }
    row += 1;
    for (i, t) in list.iter().enumerate() {
        let text = fit(&term_row(t), usable.saturating_sub(2));
        let pad = " ".repeat(usable.saturating_sub(2).saturating_sub(width(&text)));
        if i == selected {
            // Selected row: accent.soft fill and a lilac marker (DESIGN_SYSTEM §3.2), and a `›`
            // so the selection is not colour-only.
            put(
                row,
                format!(
                    "{}{}▌{}›{}{text}{pad}",
                    bg(ACCENT_SOFT),
                    fg(ACCENT),
                    fg(ACCENT),
                    fg(TEXT_PRIMARY)
                ),
            );
        } else {
            put(row, format!("{}  {text}", fg(TEXT_SECONDARY)));
        }
        row += 1;
    }
    row += 1;
    put(
        row,
        format!(
            "{}↑/↓ select · Enter open · the others keep running in the Host",
            fg(TEXT_MUTED)
        ),
    );
    s.into_bytes()
}

/// How a banner looks.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Tone {
    Info,
    Warning,
}

impl Tone {
    /// The status colour of the left marker.
    pub fn color(self) -> Rgb {
        match self {
            Tone::Info => INFO,
            Tone::Warning => WARNING,
        }
    }

    /// The banner fill: the status colour at 12 % over `bg.raised` (DESIGN_SYSTEM §3.8).
    pub fn fill(self) -> Rgb {
        BG_RAISED.mix(self.color(), 31)
    }
}

/// A one-line banner over the pane.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Banner {
    pub tone: Tone,
    pub text: String,
}

/// The banner for `phase` over a pane that is not live, if any.
pub fn banner(phase: &Phase) -> Option<Banner> {
    let (tone, text) = match phase {
        Phase::Disconnected {
            last: Some(t),
            reason,
            attempt,
        } => (
            Tone::Warning,
            format!(
                "Disconnected: {reason}. Reconnecting to `{}` (attempt {})… Enter retries now",
                t.name,
                attempt + 1
            ),
        ),
        Phase::Ended { term, why } => match why {
            EndReason::Exited(code) => (
                Tone::Info,
                format!(
                    "`{}` ended (exit code {}). Enter: choose a Terminal",
                    term.name,
                    code.map_or_else(|| "unknown".into(), |c| c.to_string())
                ),
            ),
            EndReason::DetachedBy(by) => (
                Tone::Info,
                format!(
                    "Detached by {}; `{}` keeps running. Enter: reattach",
                    by.as_deref().unwrap_or("another client"),
                    term.name
                ),
            ),
            EndReason::Removed => (
                Tone::Info,
                format!("`{}` was removed. Enter: choose a Terminal", term.name),
            ),
            EndReason::Stopped => (
                Tone::Info,
                format!("`{}` was stopped. Enter: choose a Terminal", term.name),
            ),
        },
        _ => return None,
    };
    Some(Banner { tone, text })
}

/// The VT text of a banner on a one-row view of `cols` cells.
pub fn banner_vt(b: &Banner, cols: u16) -> Vec<u8> {
    let usable = usize::from(cols).saturating_sub(2);
    format!(
        "\x1b[?25l\x1b[2J\x1b[1;2H{}{}{RESET}",
        fg(TEXT_PRIMARY),
        fit(&b.text, usable)
    )
    .into_bytes()
}

#[cfg(test)]
mod tests {
    use super::*;
    use subak_term::TermView;

    fn text_of(bytes: &[u8], cols: u16, rows: u16) -> Vec<String> {
        let mut v = TermView::new(cols, rows, 0);
        v.feed(bytes);
        (0..rows)
            .map(|r| v.row_at(r).text().trim_end().to_string())
            .collect()
    }

    fn t(name: &str, pid: u32) -> TermInfo {
        TermInfo {
            id: format!("t-{name}"),
            name: name.into(),
            state: "running".into(),
            pid: Some(pid),
            program: "cmd.exe".into(),
            history_limit: 2000,
        }
    }

    #[test]
    fn every_state_has_words_and_an_action() {
        let cases = [
            (Phase::Connecting, "Connecting to the Subak Host"),
            (Phase::Empty { notice: None }, "No Terminal is running"),
            (
                Phase::Failed {
                    code: "host_identity_mismatch".into(),
                    message: "`x` is not served by a Subak Host".into(),
                    hint: Some("use another --endpoint".into()),
                },
                "Press Enter to try again.",
            ),
            (
                Phase::Disconnected {
                    last: None,
                    attempt: 2,
                    reason: "the Host was stopped".into(),
                },
                "reconnecting (attempt 3)",
            ),
        ];
        for (phase, needle) in cases {
            let lines = text_of(&screen(&phase, "subak-test", 80, 24).unwrap(), 80, 24);
            let all = lines.join("\n");
            assert!(all.contains(needle), "{}: {all}", phase.name());
        }
        let failed = text_of(
            &screen(
                &Phase::Failed {
                    code: "host_identity_mismatch".into(),
                    message: "m".into(),
                    hint: None,
                },
                "e",
                80,
                24,
            )
            .unwrap(),
            80,
            24,
        );
        assert!(
            failed
                .iter()
                .any(|l| l.contains("error: host_identity_mismatch"))
        );
    }

    #[test]
    fn the_chooser_marks_the_selection_in_shape_and_maps_rows() {
        let list = vec![t("빌드", 11), t("claude", 22)];
        let phase = Phase::Choosing {
            list,
            selected: 1,
            notice: None,
        };
        let lines = text_of(&screen(&phase, "e", 80, 24).unwrap(), 80, 24);
        let first = usize::from(chooser_first_row(24, 2, false));
        assert!(
            lines[first].contains("빌드") && lines[first].contains("pid 11"),
            "{lines:?}"
        );
        assert!(lines[first + 1].contains("›") && lines[first + 1].contains("claude"));
        assert!(!lines[first].contains("›"));
        assert!(lines.iter().any(|l| l.contains("Enter open")));
        // A narrow window cuts with an ellipsis instead of wrapping.
        let narrow = text_of(&screen(&phase, "e", 24, 10).unwrap(), 24, 10);
        assert!(narrow.iter().all(|l| width(l) <= 24), "{narrow:?}");
    }

    #[test]
    fn banners_say_what_happened_and_what_enter_does() {
        let term = t("build", 1);
        let cases = [
            (
                Phase::Disconnected {
                    last: Some(term.clone()),
                    attempt: 0,
                    reason: "the connection to the Host was lost".into(),
                },
                Tone::Warning,
                "Reconnecting to `build`",
            ),
            (
                Phase::Ended {
                    term: term.clone(),
                    why: EndReason::Exited(Some(3)),
                },
                Tone::Info,
                "exit code 3",
            ),
            (
                Phase::Ended {
                    term,
                    why: EndReason::DetachedBy(None),
                },
                Tone::Info,
                "keeps running. Enter: reattach",
            ),
        ];
        for (phase, tone, needle) in cases {
            let b = banner(&phase).unwrap();
            assert_eq!(b.tone, tone);
            assert!(b.text.contains(needle), "{}", b.text);
            let row = text_of(&banner_vt(&b, 200), 200, 1);
            assert!(row[0].contains(needle));
        }
        assert_eq!(banner(&Phase::Connecting), None);
        // The fill is the status colour at 12 % over bg.raised.
        assert_eq!(Tone::Warning.fill(), BG_RAISED.mix(WARNING, 31));
    }

    #[test]
    fn high_contrast_drops_token_colours_and_keeps_the_selection_visible() {
        let list = vec![t("one", 1), t("two", 2)];
        let phase = Phase::Choosing {
            list,
            selected: 0,
            notice: None,
        };
        let vt = screen(&phase, "e", 80, 24).unwrap();
        let hc = high_contrast(&vt);
        let s = String::from_utf8(hc.clone()).unwrap();
        assert!(!s.contains("38;2;") && !s.contains("48;2;"), "{s}");
        assert!(s.contains("\x1b[7m"), "the selected row is reverse video");
        // The words are unchanged.
        assert_eq!(text_of(&vt, 80, 24), text_of(&hc, 80, 24));
        // A view of it uses only the default colours, with one inverse row.
        let mut v = TermView::new(80, 24, 0);
        v.feed(&hc);
        let first = chooser_first_row(24, 2, false);
        let row = v.row_at(first);
        assert!((0..80).any(|x| row.cell(x).attrs.inverse()));
        assert!((0..80).all(|x| row.cell(x).attrs.fg == subak_term::screen::Color::Default));
    }

    #[test]
    fn tokens_match_the_design_token_file() {
        let doc: serde_json::Value =
            serde_json::from_str(include_str!("../../../../design/tokens.json")).unwrap();
        let colors = &doc["themes"]["kim-dark"]["color"];
        let want = [
            ("bg.base", BG_BASE),
            ("bg.raised", BG_RAISED),
            ("bg.overlay", BG_OVERLAY),
            ("border.subtle", BORDER_SUBTLE),
            ("text.primary", TEXT_PRIMARY),
            ("text.secondary", TEXT_SECONDARY),
            ("text.muted", TEXT_MUTED),
            ("accent", ACCENT),
            ("accent.soft", ACCENT_SOFT),
            ("status.warning", WARNING),
            ("status.danger", DANGER),
            ("status.info", INFO),
        ];
        for (name, c) in want {
            let token = colors[name]
                .as_str()
                .unwrap_or_else(|| panic!("no token {name}"));
            assert_eq!(Rgb::hex(token), Some(c), "{name}");
        }
    }
}
