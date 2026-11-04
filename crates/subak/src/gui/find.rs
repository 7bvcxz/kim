//! The window's search bar (Step 19, ADR 0048, docs/SUBAK_GUI.md §12.1): the query field and
//! its options, the state of the search it runs over the pane's view (`subak_term::search`), and
//! the one-row VT it is drawn from. Portable and unit-tested; the window (`window.rs`) routes keys
//! to it, runs the search and draws the row over the top of the pane, where banners appear.
//!
//! Matches are numbered oldest first; the search starts at the newest match at or above the
//! last visible row, `Enter` (or `F3`, `Up`) goes to the next older match and `Shift+Enter`
//! (`Shift+F3`, `Down`) to the next newer one, wrapping around. Typing searches again at once
//! (incremental); new output refreshes the matches without moving the view.

use subak_term::search::{MAX_PATTERN_CHARS, Query, QueryError, Search, Stop};
use subak_term::{Span, TermView};

use super::screens::tokens::{ACCENT, BG_BASE, TEXT_MUTED, TEXT_PRIMARY, TEXT_SECONDARY, WARNING};
use subak_term::render::Rgb;

/// The editing keys of the field.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Edit {
    Backspace,
    Delete,
    Left,
    Right,
    Home,
    End,
    /// Ctrl+Backspace: the word before the caret.
    BackWord,
}

/// The options a click or `Alt+C`/`Alt+R`/`Alt+W` toggles.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Toggle {
    Case,
    Regex,
    Word,
}

/// The search bar.
#[derive(Debug, Clone, Default)]
pub struct FindBar {
    pub text: String,
    /// The caret, in characters.
    pub cursor: usize,
    pub case_sensitive: bool,
    pub regex: bool,
    pub whole_word: bool,
    /// The running search (none for an empty or invalid query).
    pub search: Option<Search>,
    pub error: Option<QueryError>,
    /// The current match, as an index into the matches and as its cells (to find it again).
    pub current: Option<(usize, Span)>,
    /// The toggles' columns on the bar (clickable).
    pub spans: Vec<(u16, u16, Toggle)>,
}

impl FindBar {
    pub fn query(&self) -> Query {
        Query {
            text: self.text.clone(),
            regex: self.regex,
            case_sensitive: self.case_sensitive,
            whole_word: self.whole_word,
        }
    }

    fn byte(&self, i: usize) -> usize {
        self.text
            .char_indices()
            .nth(i)
            .map_or(self.text.len(), |(b, _)| b)
    }

    /// Types `c` at the caret; `false` when nothing changed (a control character, the limit).
    pub fn insert(&mut self, c: char) -> bool {
        if c.is_control() || self.text.chars().count() >= MAX_PATTERN_CHARS {
            return false;
        }
        let b = self.byte(self.cursor);
        self.text.insert(b, c);
        self.cursor += 1;
        true
    }

    /// Pastes `s` into the field: one line (line breaks become spaces, control characters go).
    pub fn insert_str(&mut self, s: &str) -> bool {
        let mut changed = false;
        for c in s.chars() {
            let c = if matches!(c, '\r' | '\n' | '\t') {
                ' '
            } else {
                c
            };
            changed |= self.insert(c);
        }
        changed
    }

    /// An editing key; returns whether the text changed (the caret may move either way).
    pub fn edit(&mut self, e: Edit) -> bool {
        let n = self.text.chars().count();
        match e {
            Edit::Backspace if self.cursor > 0 => {
                let (a, b) = (self.byte(self.cursor - 1), self.byte(self.cursor));
                self.text.replace_range(a..b, "");
                self.cursor -= 1;
                true
            }
            Edit::Delete if self.cursor < n => {
                let (a, b) = (self.byte(self.cursor), self.byte(self.cursor + 1));
                self.text.replace_range(a..b, "");
                true
            }
            Edit::BackWord if self.cursor > 0 => {
                let chars: Vec<char> = self.text.chars().collect();
                let mut i = self.cursor;
                while i > 0 && chars[i - 1] == ' ' {
                    i -= 1;
                }
                while i > 0 && chars[i - 1] != ' ' {
                    i -= 1;
                }
                let (a, b) = (self.byte(i), self.byte(self.cursor));
                self.text.replace_range(a..b, "");
                self.cursor = i;
                true
            }
            Edit::Left => {
                self.cursor = self.cursor.saturating_sub(1);
                false
            }
            Edit::Right => {
                self.cursor = (self.cursor + 1).min(n);
                false
            }
            Edit::Home => {
                self.cursor = 0;
                false
            }
            Edit::End => {
                self.cursor = n;
                false
            }
            _ => false,
        }
    }

    pub fn toggle(&mut self, t: Toggle) {
        match t {
            Toggle::Case => self.case_sensitive = !self.case_sensitive,
            Toggle::Regex => self.regex = !self.regex,
            Toggle::Word => self.whole_word = !self.whole_word,
        }
    }

    /// Searches `view` again for the query and starts at the newest match at or above line
    /// `bottom` (the last visible row). Returns the current match to show.
    pub fn run(&mut self, view: &TermView, bottom: u64) -> Option<Span> {
        self.search = None;
        self.error = None;
        self.current = None;
        if self.text.is_empty() {
            return None;
        }
        match Search::new(view, &self.query(), Default::default()) {
            Ok(s) => {
                self.current = s.nearest(bottom).map(|i| (i, s.matches()[i]));
                self.search = Some(s);
            }
            Err(e) => self.error = Some(e),
        }
        self.current.map(|c| c.1)
    }

    /// New output (or a fresh view when `full`): refreshes the matches and finds the current
    /// one again, without moving the view.
    pub fn refresh(&mut self, view: &TermView, full: bool) {
        let Some(s) = &mut self.search else {
            return;
        };
        if full {
            s.full(view);
        } else {
            s.refresh(view);
        }
        self.current = match self.current {
            Some((_, span)) => s.find(span).map(|i| (i, s.matches()[i])),
            None => s.nearest(view.last_line()).map(|i| (i, s.matches()[i])),
        };
    }

    /// Moves to the next older (`older`) or newer match, wrapping around.
    pub fn step(&mut self, older: bool) -> Option<Span> {
        let s = self.search.as_ref()?;
        let n = s.matches().len();
        if n == 0 {
            return None;
        }
        let i = match self.current {
            Some((i, _)) if older => (i + n - 1) % n,
            Some((i, _)) => (i + 1) % n,
            None => n - 1,
        };
        self.current = Some((i, s.matches()[i]));
        Some(s.matches()[i])
    }

    pub fn matches(&self) -> &[Span] {
        self.search.as_ref().map_or(&[], |s| s.matches())
    }

    /// The count in words: "3 of 17", "No matches", the reason a query is not searched, or the
    /// limit a search stopped at.
    pub fn status(&self) -> String {
        if let Some(e) = &self.error {
            return capital(&e.to_string());
        }
        let Some(s) = &self.search else {
            return String::new();
        };
        let n = s.matches().len();
        if n == 0 {
            return match s.stopped {
                Some(Stop::Time) => "No matches in the newest lines (stopped after 250 ms)".into(),
                _ => "No matches".into(),
            };
        }
        let at = self.current.map_or(0, |c| c.0 + 1);
        match s.stopped {
            Some(Stop::Matches) => format!("{at} of {} (the newest; more exist)", group(n)),
            Some(Stop::Time) => {
                format!("{at} of {} (newest lines; stopped after 250 ms)", group(n))
            }
            None => format!("{at} of {}", group(n)),
        }
    }

    /// The bar as one row of VT for `cols` columns, and the caret's column. Words carry the state
    /// (a check mark for an option that is on), never colour alone.
    pub fn vt(&mut self, cols: u16) -> (Vec<u8>, u16) {
        let fg = |c: Rgb| format!("\x1b[38;2;{};{};{}m", c.0, c.1, c.2);
        let bg = |c: Rgb| format!("\x1b[48;2;{};{};{}m", c.0, c.1, c.2);
        let reset = "\x1b[0m";
        let cols = usize::from(cols);
        let label = " Find ";
        let mut s = format!(
            "\x1b[2J\x1b[1;2H\x1b[1m{}{}{label}{reset} ",
            bg(ACCENT),
            fg(BG_BASE)
        );
        let mut col = 1 + width(label) + 1;
        // The field: the query, cut from the left when it does not fit.
        let room = (cols / 4).clamp(10, 40);
        let chars: Vec<char> = self.text.chars().collect();
        let mut first = 0;
        while width_of(&chars[first..self.cursor.min(chars.len())]) >= room && first < chars.len() {
            first += 1;
        }
        let mut shown = String::new();
        let mut w = 0;
        for c in &chars[first..] {
            let cw = usize::from(subak_term::screen::char_width(*c));
            if w + cw > room {
                break;
            }
            shown.push(*c);
            w += cw;
        }
        let caret = col + width_of(&chars[first..self.cursor.min(chars.len())]);
        s.push_str(&format!("{}\x1b[4m{shown}", fg(TEXT_PRIMARY)));
        s.push_str(&" ".repeat(room.saturating_sub(w) + 1));
        s.push_str(reset);
        col += room + 1;
        // The count.
        let status = self.status();
        let tone = if self.error.is_some() || self.matches().is_empty() && self.search.is_some() {
            WARNING
        } else {
            TEXT_SECONDARY
        };
        s.push_str(&format!(" {}{status}{reset}  ", fg(tone)));
        col += 1 + width(&status) + 2;
        // The options, clickable.
        self.spans.clear();
        for (t, name, key, on) in [
            (Toggle::Case, "Aa case", "Alt+C", self.case_sensitive),
            (Toggle::Regex, ".* regex", "Alt+R", self.regex),
            (Toggle::Word, "ab word", "Alt+W", self.whole_word),
        ] {
            let text = format!("{name}{} ({key})", if on { " ✓" } else { "" });
            let style = if on {
                format!("\x1b[1m{}", fg(ACCENT))
            } else {
                fg(TEXT_MUTED)
            };
            if col + width(&text) + 2 > cols {
                break;
            }
            s.push_str(&format!("{style}{text}{reset}  "));
            self.spans.push((
                u16::try_from(col).unwrap_or(u16::MAX),
                u16::try_from(col + width(&text)).unwrap_or(u16::MAX),
                t,
            ));
            col += width(&text) + 2;
        }
        let hint = "Enter older · Shift+Enter newer · Esc close";
        if col + width(hint) < cols {
            s.push_str(&format!("{}{hint}{reset}", fg(TEXT_MUTED)));
        }
        // The caret: a bar cursor in the field.
        let caret = u16::try_from(caret).unwrap_or(0);
        s.push_str(&format!("\x1b[6 q\x1b[1;{}H\x1b[?25h", caret + 1));
        (s.into_bytes(), caret)
    }

    /// The option under column `col` of the bar, if any.
    pub fn hit(&self, col: u16) -> Option<Toggle> {
        self.spans
            .iter()
            .find(|(a, b, _)| *a <= col && col < *b)
            .map(|s| s.2)
    }
}

fn width(s: &str) -> usize {
    s.chars()
        .map(|c| usize::from(subak_term::screen::char_width(c)))
        .sum()
}

fn width_of(c: &[char]) -> usize {
    c.iter()
        .map(|c| usize::from(subak_term::screen::char_width(*c)))
        .sum()
}

fn capital(s: &str) -> String {
    let mut c = s.chars();
    c.next()
        .map(|f| f.to_uppercase().chain(c).collect())
        .unwrap_or_default()
}

/// `12345` → `12 345` (a thin grouping that reads in any locale).
fn group(n: usize) -> String {
    let s = n.to_string();
    let mut out = String::new();
    for (i, c) in s.chars().enumerate() {
        if i > 0 && (s.len() - i).is_multiple_of(3) {
            out.push(' ');
        }
        out.push(c);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn view(text: &str) -> TermView {
        let mut v = TermView::new(60, 6, 100);
        v.feed(text.as_bytes());
        v
    }

    fn row_text(vt: &[u8], cols: u16) -> String {
        let mut v = TermView::new(cols, 1, 0);
        v.feed(vt);
        v.row_at(0).text().trim_end().to_string()
    }

    #[test]
    fn typing_searches_incrementally_and_enter_walks_older_matches() {
        let v = view("빌드 one\r\nbuild two\r\n빌드 three\r\nend");
        let mut f = FindBar::default();
        for c in "빌".chars() {
            assert!(f.insert(c));
        }
        let first = f.run(&v, v.last_line()).unwrap();
        assert_eq!(f.matches().len(), 2);
        assert_eq!(first.start.line, 2, "the newest match first");
        assert_eq!(f.status(), "2 of 2");
        assert_eq!(f.step(true).unwrap().start.line, 0);
        assert_eq!(f.status(), "1 of 2");
        assert_eq!(f.step(true).unwrap().start.line, 2, "wraps around");
        assert_eq!(f.step(false).unwrap().start.line, 0);
        // Editing: Backspace empties it, then a new word.
        assert!(f.edit(Edit::Backspace));
        assert!(f.run(&v, v.last_line()).is_none());
        assert_eq!(f.status(), "");
        f.insert_str("BUILD\nx");
        assert_eq!(f.text, "BUILD x");
        f.edit(Edit::BackWord);
        f.edit(Edit::BackWord);
        assert_eq!(f.text, "");
        f.insert_str("BUILD");
        f.run(&v, v.last_line());
        assert_eq!(f.matches().len(), 1, "case-insensitive by default");
        f.toggle(Toggle::Case);
        f.run(&v, v.last_line());
        assert_eq!(f.status(), "No matches");
    }

    #[test]
    fn errors_and_limits_are_said_in_words() {
        let v = view("abc");
        let mut f = FindBar::default();
        f.insert_str("a(b");
        f.toggle(Toggle::Regex);
        f.run(&v, 0);
        assert!(
            f.status().starts_with("Not a valid regular expression"),
            "{}",
            f.status()
        );
        assert!(f.matches().is_empty());
        assert_eq!(group(10000), "10 000");
        assert_eq!(group(999), "999");
    }

    #[test]
    fn new_output_keeps_the_current_match() {
        let mut v = view("x hit\r\ny hit\r\n");
        let mut f = FindBar::default();
        f.insert_str("hit");
        let cur = f.run(&v, v.last_line()).unwrap();
        v.feed(b"z hit\r\n");
        f.refresh(&v, false);
        assert_eq!(f.matches().len(), 3);
        assert_eq!(
            f.current.unwrap().1,
            cur,
            "the view does not jump to new output"
        );
        assert_eq!(f.status(), "2 of 3");
    }

    #[test]
    fn the_bar_shows_the_field_count_options_and_hints() {
        let v = view("한글 hit");
        let mut f = FindBar::default();
        f.insert_str("한글");
        f.run(&v, v.last_line());
        f.toggle(Toggle::Word);
        let (vt, caret) = f.vt(160);
        let text = row_text(&vt, 160);
        assert!(text.trim_start().starts_with("Find  한글"), "{text}");
        assert!(text.contains("1 of 1"), "{text}");
        assert!(text.contains("Aa case (Alt+C)"), "{text}");
        assert!(text.contains("ab word ✓ (Alt+W)"), "{text}");
        assert!(text.contains("Esc close"), "{text}");
        // The caret sits after the two wide characters.
        assert_eq!(caret, 1 + 6 + 1 + 4);
        let (a, _, t) = f.spans[2];
        assert_eq!(f.hit(a), Some(t));
        assert_eq!(t, Toggle::Word);
        // Narrow: the hints and options give way, the field and count stay.
        let (vt, _) = f.vt(40);
        let text = row_text(&vt, 40);
        assert!(
            text.contains("1 of 1") && !text.contains("Esc close"),
            "{text}"
        );
    }
}
