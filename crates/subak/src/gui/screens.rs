//! What the window says when it shows no live pane: the loading, empty, stored-Terminal and
//! error screens, and the one-line banner over a pane that is not live any more (disconnected,
//! ended, detached), over a notice, or asking to confirm the end of a busy Terminal
//! (DESIGN_SYSTEM §3.8: one muted sentence plus one action, never a blank window; state is never
//! colour-only — every screen and banner says it in words; the safe choice is the default).
//!
//! Screens are VT text for a [`subak_term::TermView`] of the window's grid, drawn by the same
//! renderer as the pane (the terminal font chain, so Korean names render like the pane does).
//! The colours are the design tokens (design/tokens.json; checked by a test).

use super::model::{Confirm, EndReason, Phase};
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
    pub const SUCCESS: Rgb = Rgb(0x8B, 0xC7, 0x9A);
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

/// The keys the screens and banners name, as currently bound (`None`: unbound; the text then
/// points at the tab menu instead).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Hints {
    pub new: Option<String>,
    pub close: Option<String>,
    pub next: Option<String>,
}

impl Hints {
    fn key(k: Option<&String>, what: &str) -> String {
        k.map_or_else(|| format!("the tab menu's {what}"), |k| k.clone())
    }
}

/// The VT text of the state screen for `phase` on a `cols × rows` grid, or `None` when the
/// window shows a pane.
pub fn screen(
    phase: &Phase,
    endpoint: &str,
    cols: u16,
    rows: u16,
    hints: &Hints,
) -> Option<Vec<u8>> {
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
                say("No Terminal is open in this window.".into()),
                muted(format!(
                    "{} (or the + above) starts one; from any console:  subak new <name>",
                    Hints::key(hints.new.as_ref(), "New Terminal")
                )),
                muted("A Terminal that another client starts opens here as a tab.".into()),
            ]);
            v
        }
        Phase::Stored(t) => {
            let what = if t.state == "failed" {
                format!("`{}` could not start", t.name)
            } else {
                format!("`{}` is stopped", t.name)
            };
            vec![
                Line(WARNING, what, true),
                blank(),
                say(
                    "Its processes have ended; the Host keeps the Terminal until it is removed."
                        .into(),
                ),
                muted(format!(
                    "{} removes it (nothing runs in it) · {} shows the next tab",
                    Hints::key(hints.close.as_ref(), "Close Terminal"),
                    Hints::key(hints.next.as_ref(), "next tab")
                )),
                muted("Restarting a stopped Terminal comes with Step 30.".into()),
            ]
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

/// How a banner looks.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Tone {
    Info,
    Warning,
    Danger,
}

impl Tone {
    /// The status colour of the left marker.
    pub fn color(self) -> Rgb {
        match self {
            Tone::Info => INFO,
            Tone::Warning => WARNING,
            Tone::Danger => DANGER,
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
pub fn banner(phase: &Phase, hints: &Hints) -> Option<Banner> {
    let close = Hints::key(hints.close.as_ref(), "Close Terminal");
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
                    "`{}` ended (exit code {}). {}",
                    term.name,
                    code.map_or_else(|| "unknown".into(), |c| c.to_string()),
                    if term.gone() {
                        "Enter closes the tab".to_string()
                    } else {
                        format!("The Host keeps it as stopped; {close} removes it")
                    }
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
                format!(
                    "`{}` was removed by another client. Enter closes the tab",
                    term.name
                ),
            ),
            EndReason::Stopped => (
                Tone::Info,
                format!(
                    "`{}` was stopped; the Host keeps it. {close} removes it",
                    term.name
                ),
            ),
        },
        _ => return None,
    };
    Some(Banner { tone, text })
}

/// The two answers of a confirmation; the safe one comes first and is the default.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Choice {
    Cancel,
    End,
}

/// The confirmation banner for ending a busy Terminal (DESIGN_SYSTEM §3.8: it names what will
/// end; the default focus is the safe choice), with the columns of its two buttons.
pub fn confirm_vt(c: &Confirm, focus: Choice, cols: u16) -> (Vec<u8>, Vec<(u16, u16, Choice)>) {
    let usable = usize::from(cols).saturating_sub(2);
    let cancel = " Cancel (Esc) ";
    let end = " End Terminal ";
    let buttons = width(cancel) + width(end) + 6;
    let program = if c.program.is_empty() {
        String::new()
    } else {
        format!(" {}", c.program)
    };
    let question = format!(
        "End `{}`? It runs{program} ({} process{} in its job); they will be ended.",
        c.name,
        c.processes,
        if c.processes == 1 { "" } else { "es" }
    );
    let question = fit(&question, usable.saturating_sub(buttons).max(10));
    let mut s = format!(
        "\x1b[?25l\x1b[2J\x1b[1;2H{}{question}{RESET}  ",
        fg(TEXT_PRIMARY)
    );
    let mut col = 1 + width(&question) + 2;
    let mut spans = Vec::new();
    for (label, choice) in [(cancel, Choice::Cancel), (end, Choice::End)] {
        let focused = choice == focus;
        let style = match (focused, choice) {
            // The focused button is filled and bracketed, so focus is not colour-only.
            (true, Choice::Cancel) => format!("\x1b[1m{}{}", bg(ACCENT), fg(BG_BASE)),
            (true, Choice::End) => format!("\x1b[1m{}{}", bg(DANGER), fg(BG_BASE)),
            (false, Choice::Cancel) => fg(TEXT_SECONDARY),
            (false, Choice::End) => fg(DANGER),
        };
        let (l, r) = if focused { ("[", "]") } else { (" ", " ") };
        let text = format!("{l}{label}{r}");
        let start = col;
        s.push_str(&format!("{style}{text}{RESET} "));
        col += width(&text) + 1;
        spans.push((
            u16::try_from(start).unwrap_or(u16::MAX),
            u16::try_from(col - 1).unwrap_or(u16::MAX),
            choice,
        ));
    }
    (s.into_bytes(), spans)
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
    use super::super::model::TermInfo;
    use super::*;
    use subak_term::TermView;

    fn text_of(bytes: &[u8], cols: u16, rows: u16) -> Vec<String> {
        let mut v = TermView::new(cols, rows, 0);
        v.feed(bytes);
        (0..rows)
            .map(|r| v.row_at(r).text().trim_end().to_string())
            .collect()
    }

    fn t(name: &str, state: &str) -> TermInfo {
        TermInfo {
            id: format!("t-{name}"),
            name: name.into(),
            state: state.into(),
            pid: Some(1),
            program: "cmd.exe".into(),
            history_limit: 2000,
        }
    }

    fn hints() -> Hints {
        Hints {
            new: Some("Ctrl+T".into()),
            close: Some("Ctrl+W".into()),
            next: Some("Ctrl+Tab".into()),
        }
    }

    #[test]
    fn every_state_has_words_and_an_action() {
        let cases = [
            (Phase::Connecting, "Connecting to the Subak Host"),
            (
                Phase::Empty { notice: None },
                "Ctrl+T (or the + above) starts one",
            ),
            (Phase::Stored(t("kept", "stopped")), "Ctrl+W removes it"),
            (Phase::Stored(t("bad", "failed")), "`bad` could not start"),
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
            let lines = text_of(
                &screen(&phase, "subak-test", 100, 24, &hints()).unwrap(),
                100,
                24,
            );
            let all = lines.join("\n");
            assert!(all.contains(needle), "{}: {all}", phase.name());
        }
        // Unbound keys: the text points at the tab menu.
        let lines = text_of(
            &screen(
                &Phase::Stored(t("kept", "stopped")),
                "e",
                120,
                24,
                &Hints::default(),
            )
            .unwrap(),
            120,
            24,
        );
        assert!(
            lines
                .iter()
                .any(|l| l.contains("the tab menu's Close Terminal removes it")),
            "{lines:?}"
        );
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
                &hints(),
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
        // A narrow window cuts with an ellipsis instead of wrapping.
        let narrow = text_of(
            &screen(&Phase::Stored(t("kept", "stopped")), "e", 24, 10, &hints()).unwrap(),
            24,
            10,
        );
        assert!(narrow.iter().all(|l| width(l) <= 24), "{narrow:?}");
    }

    #[test]
    fn banners_say_what_happened_and_what_enter_does() {
        let cases = [
            (
                Phase::Disconnected {
                    last: Some(t("build", "running")),
                    attempt: 0,
                    reason: "the connection to the Host was lost".into(),
                },
                Tone::Warning,
                "Reconnecting to `build`",
            ),
            (
                Phase::Ended {
                    term: t("build", "exited"),
                    why: EndReason::Exited(Some(3)),
                },
                Tone::Info,
                "exit code 3). Enter closes the tab",
            ),
            (
                Phase::Ended {
                    term: t("build", "stopped"),
                    why: EndReason::Exited(Some(0)),
                },
                Tone::Info,
                "keeps it as stopped; Ctrl+W removes it",
            ),
            (
                Phase::Ended {
                    term: t("build", "running"),
                    why: EndReason::DetachedBy(None),
                },
                Tone::Info,
                "keeps running. Enter: reattach",
            ),
            (
                Phase::Ended {
                    term: t("build", "removed"),
                    why: EndReason::Removed,
                },
                Tone::Info,
                "removed by another client. Enter closes the tab",
            ),
        ];
        for (phase, tone, needle) in cases {
            let b = banner(&phase, &hints()).unwrap();
            assert_eq!(b.tone, tone);
            assert!(b.text.contains(needle), "{}", b.text);
            let row = text_of(&banner_vt(&b, 200), 200, 1);
            assert!(row[0].contains(needle));
        }
        assert_eq!(banner(&Phase::Connecting, &hints()), None);
        // The fill is the status colour at 12 % over bg.raised.
        assert_eq!(Tone::Warning.fill(), BG_RAISED.mix(WARNING, 31));
        assert_eq!(Tone::Danger.color(), DANGER);
    }

    /// The cells `[a, b)` of a row's text.
    fn cells(row: &str, a: u16, b: u16) -> String {
        let mut out = String::new();
        let mut col = 0u16;
        for c in row.chars() {
            if col >= a && col < b {
                out.push(c);
            }
            col += u16::from(subak_term::screen::char_width(c));
        }
        out
    }

    #[test]
    fn the_confirmation_names_what_ends_and_defaults_to_the_safe_choice() {
        let c = Confirm {
            id: "t-1".into(),
            name: "claude".into(),
            program: "claude.exe".into(),
            processes: 3,
        };
        let (vt, spans) = confirm_vt(&c, Choice::Cancel, 160);
        let row = text_of(&vt, 160, 1).remove(0);
        assert!(
            row.contains("End `claude`? It runs claude.exe (3 processes in its job)"),
            "{row}"
        );
        assert_eq!(spans.len(), 2);
        assert_eq!(spans[0].2, Choice::Cancel);
        assert_eq!(spans[1].2, Choice::End);
        // The focused (safe) button is bracketed: focus is not colour-only.
        assert!(
            cells(&row, spans[0].0, spans[0].1).starts_with("[ Cancel"),
            "{row}"
        );
        assert!(!cells(&row, spans[1].0, spans[1].1).starts_with('['));
        let (vt, spans) = confirm_vt(&c, Choice::End, 160);
        let row = text_of(&vt, 160, 1).remove(0);
        assert!(
            cells(&row, spans[1].0, spans[1].1).starts_with("[ End Terminal"),
            "{row}"
        );
        // A narrow window keeps both buttons.
        let (vt, spans) = confirm_vt(&c, Choice::Cancel, 50);
        let row = text_of(&vt, 70, 1).remove(0);
        assert!(
            row.contains("Cancel") && row.contains("End Terminal"),
            "{row}"
        );
        assert!(spans[1].1 <= 60, "{spans:?}");
    }

    #[test]
    fn high_contrast_drops_token_colours_and_keeps_fills_visible() {
        let c = Confirm {
            id: "t-1".into(),
            name: "x".into(),
            program: String::new(),
            processes: 2,
        };
        let (vt, _) = confirm_vt(&c, Choice::Cancel, 120);
        let hc = high_contrast(&vt);
        let s = String::from_utf8(hc.clone()).unwrap();
        assert!(!s.contains("38;2;") && !s.contains("48;2;"), "{s}");
        assert!(s.contains("\x1b[7m"), "the focused button is reverse video");
        assert_eq!(text_of(&vt, 120, 1), text_of(&hc, 120, 1));
        let mut v = TermView::new(120, 1, 0);
        v.feed(&hc);
        let row = v.row_at(0);
        assert!((0..120).any(|x| row.cell(x).attrs.inverse()));
        assert!((0..120).all(|x| row.cell(x).attrs.fg == subak_term::screen::Color::Default));
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
            ("status.success", SUCCESS),
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
