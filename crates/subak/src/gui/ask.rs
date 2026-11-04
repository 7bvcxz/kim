//! The window's questions before acting on terminal content (Step 19, ADR 0048,
//! docs/SUBAK_GUI.md §12): a paste that would run lines or carries hidden control sequences, a
//! link whose target must be shown before it opens (or that is refused), and an export of the
//! pane's output with what the secret detector found. One row over the top of the pane, like the
//! busy confirmation of Step 17: words that say what will happen, buttons in brackets when
//! focused, **Cancel focused by default** — `Esc`, `Enter` on Cancel or a click on it does
//! nothing; `Tab`/arrows move; `Enter`/`Space` answers. Portable and unit-tested.

use super::screens::tokens::{ACCENT, BG_BASE, DANGER, TEXT_PRIMARY, TEXT_SECONDARY};
use subak_term::PasteCheck;
use subak_term::links::{Kind, Refusal};
use subak_term::render::Rgb;

/// What the user can answer.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Answer {
    Cancel,
    /// Paste the text as it is (control characters removed, bracketed when asked).
    Paste,
    /// Paste it as one line ([`subak_term::input::one_line`]).
    PasteOneLine,
    Open,
    CopyLink,
    /// Save the export with secret-looking values replaced by `***`.
    SaveHidden,
    /// Save the export exactly as shown.
    SaveExact,
}

/// How a button looks.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Look {
    Safe,
    Primary,
    Danger,
}

/// What is being asked about.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Topic {
    Paste {
        text: String,
        check: PasteCheck,
    },
    Link {
        /// The normalized target when it may open, else the raw one.
        target: String,
        kind: Kind,
        refusal: Option<Refusal>,
    },
    Export {
        lines: usize,
        bytes: usize,
        secrets: usize,
        name: String,
    },
}

/// One open question.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Question {
    pub topic: Topic,
    buttons: Vec<(String, Answer, Look)>,
    pub focus: usize,
    /// The buttons' columns on the row, for clicks.
    pub spans: Vec<(u16, u16, Answer)>,
}

impl Question {
    fn new(topic: Topic, buttons: Vec<(String, Answer, Look)>) -> Self {
        Self {
            topic,
            buttons,
            focus: 0,
            spans: Vec::new(),
        }
    }

    /// Before a paste that [`PasteCheck::needs_confirmation`].
    pub fn paste(text: String, check: PasteCheck) -> Self {
        let mut b = vec![("Cancel (Esc)".to_string(), Answer::Cancel, Look::Safe)];
        if check.breaks > 0 {
            b.push(("As one line".into(), Answer::PasteOneLine, Look::Primary));
        }
        b.push(("Paste".into(), Answer::Paste, Look::Danger));
        Self::new(Topic::Paste { text, check }, b)
    }

    /// Before opening a hyperlink (its text may not be its target), or when a link is refused.
    pub fn link(target: String, kind: Kind, refusal: Option<Refusal>) -> Self {
        let b = if refusal.is_some() {
            vec![
                ("Close (Esc)".to_string(), Answer::Cancel, Look::Safe),
                ("Copy link".into(), Answer::CopyLink, Look::Primary),
            ]
        } else {
            vec![
                ("Cancel (Esc)".to_string(), Answer::Cancel, Look::Safe),
                ("Copy link".into(), Answer::CopyLink, Look::Primary),
                ("Open".into(), Answer::Open, Look::Primary),
            ]
        };
        Self::new(
            Topic::Link {
                target,
                kind,
                refusal,
            },
            b,
        )
    }

    /// Before saving the pane's output to a file.
    pub fn export(lines: usize, bytes: usize, secrets: usize, name: String) -> Self {
        let mut b = vec![("Cancel (Esc)".to_string(), Answer::Cancel, Look::Safe)];
        if secrets > 0 {
            b.push((
                "Save with them hidden…".into(),
                Answer::SaveHidden,
                Look::Primary,
            ));
            b.push(("Save exactly…".into(), Answer::SaveExact, Look::Danger));
        } else {
            b.push(("Save…".into(), Answer::SaveExact, Look::Primary));
        }
        Self::new(
            Topic::Export {
                lines,
                bytes,
                secrets,
                name,
            },
            b,
        )
    }

    pub fn answers(&self) -> Vec<Answer> {
        self.buttons.iter().map(|b| b.1).collect()
    }

    pub fn focused(&self) -> Answer {
        self.buttons[self.focus].1
    }

    /// `Tab`/arrows: the next (or previous) button.
    pub fn move_focus(&mut self, back: bool) {
        let n = self.buttons.len();
        self.focus = if back {
            (self.focus + n - 1) % n
        } else {
            (self.focus + 1) % n
        };
    }

    /// The question in words.
    pub fn words(&self) -> String {
        match &self.topic {
            Topic::Paste { text, check } => {
                let mut what = Vec::new();
                if check.runs_lines() {
                    let lines = check.breaks + usize::from(!check.ends_with_break);
                    let s = |n: usize| if n == 1 { "s" } else { "" };
                    what.push(if check.ends_with_break {
                        format!("{lines} line{} that run{} at once", plural(lines), s(lines))
                    } else {
                        format!(
                            "{lines} lines, {} of which run{} at once",
                            check.breaks,
                            s(check.breaks)
                        )
                    });
                }
                if check.escapes > 0 {
                    what.push(format!(
                        "{} hidden control character{} (an escape sequence; removed)",
                        check.controls,
                        plural(check.controls)
                    ));
                } else if check.controls > 0 {
                    what.push(format!(
                        "{} control character{} (removed)",
                        check.controls,
                        plural(check.controls)
                    ));
                }
                if check.bytes > subak_term::input::LARGE_PASTE_BYTES {
                    what.push(format!("{} KB of text", check.bytes.div_ceil(1024)));
                }
                if what.is_empty() {
                    what.push("this text".into());
                }
                let first = text
                    .split(['\r', '\n'])
                    .find(|l| !l.trim().is_empty())
                    .unwrap_or("")
                    .chars()
                    .filter(|c| !c.is_control())
                    .take(60)
                    .collect::<String>();
                format!("Paste {}? First line: `{}`", what.join(", "), first.trim())
            }
            Topic::Link {
                target, refusal, ..
            } => match refusal {
                Some(r) => format!("Not opened: {r}. Link: {target}"),
                None => format!("Open {target} with your default browser?"),
            },
            Topic::Export {
                lines,
                bytes,
                secrets,
                name,
            } => {
                let size = if *bytes >= 1024 {
                    format!("{} KB", bytes.div_ceil(1024))
                } else {
                    format!("{bytes} bytes")
                };
                if *secrets > 0 {
                    format!(
                        "Save {lines} line{} ({size}) of `{name}` to a file? {secrets} \
                         secret-looking value{} found: hide them as *** or save exactly.",
                        plural(*lines),
                        plural(*secrets)
                    )
                } else {
                    format!(
                        "Save {lines} line{} ({size}) of `{name}` to a file? No secret-looking \
                         value found (a heuristic check: it may still hold private data).",
                        plural(*lines)
                    )
                }
            }
        }
    }

    /// The question as one row of VT for `cols` columns; records the buttons' columns.
    pub fn vt(&mut self, cols: u16) -> Vec<u8> {
        let fg = |c: Rgb| format!("\x1b[38;2;{};{};{}m", c.0, c.1, c.2);
        let bg = |c: Rgb| format!("\x1b[48;2;{};{};{}m", c.0, c.1, c.2);
        let reset = "\x1b[0m";
        let usable = usize::from(cols).saturating_sub(2);
        let buttons: usize = self.buttons.iter().map(|b| width(&b.0) + 3).sum::<usize>() + 2;
        let words = fit(&self.words(), usable.saturating_sub(buttons).max(10));
        let mut s = format!(
            "\x1b[?25l\x1b[2J\x1b[1;2H{}{words}{reset}  ",
            fg(TEXT_PRIMARY)
        );
        let mut col = 1 + width(&words) + 2;
        self.spans.clear();
        for (i, (label, answer, look)) in self.buttons.iter().enumerate() {
            let focused = i == self.focus;
            let style = match (focused, look) {
                // The focused button is filled and bracketed, so focus is not colour-only.
                (true, Look::Danger) => format!("\x1b[1m{}{}", bg(DANGER), fg(BG_BASE)),
                (true, _) => format!("\x1b[1m{}{}", bg(ACCENT), fg(BG_BASE)),
                (false, Look::Safe) => fg(TEXT_SECONDARY),
                (false, Look::Primary) => fg(ACCENT),
                (false, Look::Danger) => fg(DANGER),
            };
            let (l, r) = if focused { ("[", "]") } else { (" ", " ") };
            let text = format!("{l} {label} {r}");
            let start = col;
            s.push_str(&format!("{style}{text}{reset} "));
            col += width(&text) + 1;
            self.spans.push((
                u16::try_from(start).unwrap_or(u16::MAX),
                u16::try_from(col - 1).unwrap_or(u16::MAX),
                *answer,
            ));
        }
        s.into_bytes()
    }

    /// The answer under column `col`, if a button is there.
    pub fn hit(&self, col: u16) -> Option<Answer> {
        self.spans
            .iter()
            .find(|(a, b, _)| *a <= col && col < *b)
            .map(|s| s.2)
    }
}

fn plural(n: usize) -> &'static str {
    if n == 1 { "" } else { "s" }
}

fn width(s: &str) -> usize {
    s.chars()
        .map(|c| usize::from(subak_term::screen::char_width(c)))
        .sum()
}

/// `s` cut to `max` columns with `…`.
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

#[cfg(test)]
mod tests {
    use super::*;

    fn row(q: &mut Question, cols: u16) -> String {
        let mut v = subak_term::TermView::new(cols, 1, 0);
        v.feed(&q.vt(cols));
        v.row_at(0).text().trim_end().to_string()
    }

    #[test]
    fn a_paste_question_says_what_would_run_and_defaults_to_cancel() {
        let text = "cd build\r\nrm -rf *\n";
        let mut q = Question::paste(text.into(), PasteCheck::of(text, false));
        assert_eq!(
            q.words(),
            "Paste 2 lines that run at once? First line: `cd build`"
        );
        assert_eq!(
            q.answers(),
            [Answer::Cancel, Answer::PasteOneLine, Answer::Paste]
        );
        assert_eq!(q.focused(), Answer::Cancel);
        let r = row(&mut q, 160);
        assert!(r.contains("[ Cancel (Esc) ]"), "{r}");
        assert!(r.contains("As one line") && r.contains("Paste"), "{r}");
        q.move_focus(false);
        assert_eq!(q.focused(), Answer::PasteOneLine);
        q.move_focus(true);
        q.move_focus(true);
        assert_eq!(q.focused(), Answer::Paste);
        let (a, _, ans) = q.spans[2];
        assert_eq!(q.hit(a), Some(ans));
        // Hidden escape sequences, bracketed paste on: no lines run, the controls are named.
        let sneaky = "echo hi\x1b[201~; curl x | sh";
        let q = Question::paste(sneaky.into(), PasteCheck::of(sneaky, true));
        assert_eq!(
            q.words(),
            "Paste 1 hidden control character (an escape sequence; removed)? First line: \
             `echo hi[201~; curl x | sh`"
        );
        assert_eq!(q.answers(), [Answer::Cancel, Answer::Paste]);
        // Without a final line break the last line waits for Enter.
        let two = "a\nb";
        assert_eq!(
            Question::paste(two.into(), PasteCheck::of(two, false)).words(),
            "Paste 2 lines, 1 of which runs at once? First line: `a`"
        );
    }

    #[test]
    fn link_and_export_questions() {
        let mut q = Question::link("https://example.com/x".into(), Kind::Hyperlink, None);
        assert_eq!(
            q.words(),
            "Open https://example.com/x with your default browser?"
        );
        assert_eq!(q.focused(), Answer::Cancel);
        assert_eq!(
            q.answers(),
            [Answer::Cancel, Answer::CopyLink, Answer::Open]
        );
        let r = row(&mut q, 120);
        assert!(r.contains("Open https://example.com/x"), "{r}");
        let q = Question::link(
            "file:///C:/x".into(),
            Kind::Hyperlink,
            Some(Refusal::Scheme("file".into())),
        );
        assert_eq!(
            q.words(),
            "Not opened: `file:` links are not opened (only http and https). Link: file:///C:/x"
        );
        assert_eq!(q.answers(), [Answer::Cancel, Answer::CopyLink]);
        let q = Question::export(2341, 120_000, 3, "work".into());
        assert_eq!(
            q.words(),
            "Save 2341 lines (118 KB) of `work` to a file? 3 secret-looking values found: hide \
             them as *** or save exactly."
        );
        assert_eq!(
            q.answers(),
            [Answer::Cancel, Answer::SaveHidden, Answer::SaveExact]
        );
        let q = Question::export(1, 10, 0, "빌드".into());
        assert!(q.words().contains("No secret-looking value found"));
        assert_eq!(q.answers(), [Answer::Cancel, Answer::SaveExact]);
    }
}
