//! One client's view of a pane, testable without rendering (ADR 0036, docs/TERMINAL_EMULATOR.md
//! §3): the terminal model — the same [`Screen`] the Host runs, fed with the Host's snapshot and
//! live stream — plus what belongs to this view only: the scrollback viewport, the selection and
//! copy, hyperlink hit tests, and the input the user gives through it.
//!
//! Nothing here draws. A renderer reads [`TermView::row_at`], [`TermView::selection_cols`] and
//! [`TermView::cursor_in_view`]; a windowing layer calls [`TermView::key`], [`TermView::paste`],
//! [`TermView::mouse`] and [`TermView::focus`] and writes what they return to the pane.

use crate::input::{self, Key, Mods, MouseAction, MouseButton, MouseEvent};
use crate::screen::{CellWidth, HISTORY_BYTES, Limits, Link, MouseTracking, Row, Screen};
use std::borrow::Cow;
use std::sync::Arc;

/// How a selection grows from where it started (click, double click, triple click, Alt+drag).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SelectMode {
    /// Character by character.
    Cell,
    /// Whole words.
    Word,
    /// Whole lines (a soft-wrapped line counts as one).
    Line,
    /// A rectangle.
    Block,
}

/// A position in the whole pane. `line` numbers every line that ever entered the history first,
/// then the screen rows, so it stays the same while output scrolls the screen into the history.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub struct Point {
    pub line: u64,
    pub col: u16,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Selection {
    anchor: Point,
    head: Point,
    mode: SelectMode,
}

/// What the view did with a mouse event.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MouseOutcome {
    /// The program tracks the mouse: send these bytes to the pane.
    Send(Vec<u8>),
    /// The view scrolled its own viewport (the wheel, with no tracking program).
    Scrolled,
    /// Not the program's: the windowing layer selects text (Shift always gets here).
    Local,
}

/// Characters that end a word for double-click selection (Windows Terminal's default set).
pub const WORD_DELIMITERS: &str = " /\\()\"'-.,:;<>~!@#$%^&*|+=[]{}?\u{2502}";

/// Lines the wheel scrolls per notch.
pub const WHEEL_LINES: usize = 3;

/// A client's view of one pane.
#[derive(Debug)]
pub struct TermView {
    screen: Screen,
    /// Lines scrolled back from the live screen (0 = following output).
    scroll: usize,
    selection: Option<Selection>,
    /// The history's `total_lines` when output was last fed.
    seen_total: u64,
    was_alt: bool,
    focused: bool,
    /// The button held for motion reports (modes 1002/1003).
    held: Option<MouseButton>,
    /// The cell of the last motion report, so one cell sends one report.
    last_motion: Option<(u16, u16)>,
}

impl TermView {
    /// A view of a pane of `cols × rows` keeping up to `history_lines` lines of scrollback.
    pub fn new(cols: u16, rows: u16, history_lines: usize) -> Self {
        Self::with_screen(Screen::new(
            cols,
            rows,
            Limits {
                lines: history_lines,
                bytes: HISTORY_BYTES,
            },
        ))
    }

    pub fn with_screen(screen: Screen) -> Self {
        let seen_total = screen.history().total_lines;
        let was_alt = screen.alternate();
        Self {
            screen,
            scroll: 0,
            selection: None,
            seen_total,
            was_alt,
            focused: true,
            held: None,
            last_motion: None,
        }
    }

    pub fn screen(&self) -> &Screen {
        &self.screen
    }

    pub fn size(&self) -> (u16, u16) {
        self.screen.size()
    }

    pub fn focused(&self) -> bool {
        self.focused
    }

    // -----------------------------------------------------------------------------------------
    // Output.

    /// Feeds pane output (snapshot frames and live frames alike). A scrolled-back viewport stays
    /// on the lines it shows while new ones arrive; switching screens resets the viewport and
    /// the selection.
    pub fn feed(&mut self, bytes: &[u8]) {
        self.screen.feed(bytes);
        let total = self.screen.history().total_lines;
        let alt = self.screen.alternate();
        if alt != self.was_alt {
            self.was_alt = alt;
            self.scroll = 0;
            self.selection = None;
        }
        if self.scroll > 0 {
            self.scroll += usize::try_from(total - self.seen_total).unwrap_or(usize::MAX);
        }
        self.seen_total = total;
        self.scroll = self.scroll.min(self.max_scroll());
        if let Some(s) = self.selection
            && s.anchor.min(s.head).line < self.first_line()
        {
            // Its first line left the history.
            self.selection = None;
        }
    }

    /// The pane changed size (this view resized it, or another client did and the Host told us).
    pub fn resize(&mut self, cols: u16, rows: u16) {
        self.screen.resize(cols, rows);
        self.seen_total = self.screen.history().total_lines;
        self.selection = None;
        self.scroll = self.scroll.min(self.max_scroll());
    }

    /// Answer device reports (only the view that owns the pane's size, see
    /// [`Screen::set_answer_reports`]).
    pub fn set_answer_reports(&mut self, on: bool) {
        self.screen.set_answer_reports(on);
    }

    /// Answers to device reports, to be written to the pane.
    pub fn take_replies(&mut self) -> Vec<u8> {
        self.screen.take_replies()
    }

    // -----------------------------------------------------------------------------------------
    // The viewport.

    fn max_scroll(&self) -> usize {
        if self.screen.alternate() {
            0
        } else {
            self.screen.history_len()
        }
    }

    /// Lines scrolled back (0 = live).
    pub fn scroll_offset(&self) -> usize {
        self.scroll
    }

    /// Scrolls the viewport `lines` back (negative: forward), within the history. The alternate
    /// screen has no history.
    pub fn scroll_lines(&mut self, lines: isize) {
        let max = self.max_scroll();
        self.scroll = self.scroll.saturating_add_signed(lines).min(max);
    }

    pub fn scroll_to_bottom(&mut self) {
        self.scroll = 0;
    }

    /// The `line` number of the oldest history line kept.
    fn first_line(&self) -> u64 {
        self.screen.history().total_lines - self.screen.history_len() as u64
    }

    /// The `line` number viewport row `row` shows.
    fn line_of_row(&self, row: u16) -> u64 {
        (self.screen.history().total_lines + u64::from(row)).saturating_sub(self.scroll as u64)
    }

    /// The pane position under viewport cell (`col`, `row`).
    pub fn point_at(&self, col: u16, row: u16) -> Point {
        let (cols, rows) = self.size();
        Point {
            line: self.line_of_row(row.min(rows - 1)),
            col: col.min(cols - 1),
        }
    }

    /// The row with `line` number `line`: a screen row, or a history line decoded at the view's
    /// width.
    pub fn line(&self, line: u64) -> Option<Cow<'_, Row>> {
        let total = self.screen.history().total_lines;
        if line >= total {
            let y = u16::try_from(line - total).ok()?;
            (y < self.size().1).then(|| Cow::Borrowed(self.screen.row(y)))
        } else {
            let first = self.first_line();
            let i = usize::try_from(line.checked_sub(first)?).ok()?;
            self.screen.history_row(i, self.size().0).map(Cow::Owned)
        }
    }

    /// What viewport row `row` shows.
    pub fn row_at(&self, row: u16) -> Cow<'_, Row> {
        self.line(self.line_of_row(row))
            .unwrap_or_else(|| Cow::Borrowed(self.screen.row(row)))
    }

    /// The cursor's viewport cell when it is in view (scrolled-back views may not show it).
    pub fn cursor_in_view(&self) -> Option<(u16, u16)> {
        let c = self.screen.cursor();
        let y = usize::from(c.y) + self.scroll;
        (y < usize::from(self.size().1)).then(|| (c.x, u16::try_from(y).unwrap_or(0)))
    }

    /// The hyperlink under viewport cell (`col`, `row`). The view never opens it: the windowing
    /// layer shows the target and opens it only on the user's explicit action (`SUBAK-SEC-013`).
    pub fn link_at(&self, col: u16, row: u16) -> Option<Arc<Link>> {
        self.row_at(row).link_at(usize::from(col)).cloned()
    }

    // -----------------------------------------------------------------------------------------
    // Selection and copy.

    /// Starts a selection at viewport cell (`col`, `row`).
    pub fn select(&mut self, col: u16, row: u16, mode: SelectMode) {
        let p = self.point_at(col, row);
        self.selection = Some(Selection {
            anchor: p,
            head: p,
            mode,
        });
    }

    /// Moves the selection's free end to viewport cell (`col`, `row`).
    pub fn extend_selection(&mut self, col: u16, row: u16) {
        let p = self.point_at(col, row);
        if let Some(s) = &mut self.selection {
            s.head = p;
        }
    }

    pub fn clear_selection(&mut self) {
        self.selection = None;
    }

    pub fn has_selection(&self) -> bool {
        self.selection.is_some()
    }

    /// The selection's first and last cell after expanding words and lines.
    fn selection_bounds(&self) -> Option<(Point, Point, SelectMode)> {
        let s = self.selection?;
        let (mut a, mut b) = if s.anchor <= s.head {
            (s.anchor, s.head)
        } else {
            (s.head, s.anchor)
        };
        let last_col = self.size().0 - 1;
        match s.mode {
            SelectMode::Cell => {}
            SelectMode::Block => {
                let (l, r) = (s.anchor.col.min(s.head.col), s.anchor.col.max(s.head.col));
                a.col = l;
                b.col = r;
            }
            SelectMode::Word => {
                if let Some(row) = self.line(a.line) {
                    a.col = word_start(&row, a.col);
                }
                if let Some(row) = self.line(b.line) {
                    b.col = word_end(&row, b.col);
                }
            }
            SelectMode::Line => {
                // Whole logical lines: back to the row that does not continue a wrapped one,
                // forward to the row that does not wrap.
                while a.line > self.first_line()
                    && self.line(a.line - 1).is_some_and(|r| r.wrapped())
                {
                    a.line -= 1;
                }
                while self.line(b.line).is_some_and(|r| r.wrapped())
                    && self.line(b.line + 1).is_some()
                {
                    b.line += 1;
                }
                a.col = 0;
                b.col = last_col;
            }
        }
        Some((a, b, s.mode))
    }

    /// The selected columns of viewport row `row` (inclusive), if any.
    pub fn selection_cols(&self, row: u16) -> Option<(u16, u16)> {
        let (a, b, mode) = self.selection_bounds()?;
        let line = self.line_of_row(row);
        if line < a.line || line > b.line {
            return None;
        }
        let last = self.size().0 - 1;
        Some(match mode {
            SelectMode::Block => (a.col, b.col),
            _ => (
                if line == a.line { a.col } else { 0 },
                if line == b.line { b.col } else { last },
            ),
        })
    }

    /// The selected text, as a clipboard gets it: a wide character once, combining marks with
    /// their character, trailing blanks of each line removed, soft-wrapped rows joined into one
    /// line, lines separated by `\n` (the Windows clipboard layer converts to CR LF).
    pub fn selected_text(&self) -> Option<String> {
        let (a, b, mode) = self.selection_bounds()?;
        let last = self.size().0 - 1;
        let mut out = String::new();
        for line in a.line..=b.line {
            let Some(row) = self.line(line) else {
                continue;
            };
            let (c0, c1) = match mode {
                SelectMode::Block => (a.col, b.col),
                _ => (
                    if line == a.line { a.col } else { 0 },
                    if line == b.line { b.col } else { last },
                ),
            };
            let mut text = String::new();
            let mut x = usize::from(c0);
            if row.cell(x).width == CellWidth::Tail && x > 0 {
                x -= 1; // start with the whole wide character
            }
            while x <= usize::from(c1) && x < row.cols() {
                let c = row.cell(x);
                if c.width != CellWidth::Tail {
                    text.push(c.ch);
                    text.push_str(c.combining);
                }
                x += 1;
            }
            let joins = mode != SelectMode::Block && row.wrapped() && line != b.line;
            if joins {
                out.push_str(&text);
            } else {
                out.push_str(text.trim_end_matches(' '));
                if line != b.line {
                    out.push('\n');
                }
            }
        }
        Some(out)
    }

    // -----------------------------------------------------------------------------------------
    // Input.

    /// A key pressed in the view: the bytes for the pane (win32-input-mode is the windowing
    /// layer's, see [`input::encode_win32_key`]). Typing returns the viewport to the live screen
    /// and clears the selection.
    pub fn key(&mut self, key: Key, mods: Mods) -> Vec<u8> {
        self.scroll = 0;
        self.selection = None;
        input::encode_key(key, mods, &self.screen.modes())
    }

    /// Text pasted into the view: the bytes for the pane (sanitized, bracketed when the program
    /// asked for it). Returns the viewport to the live screen.
    pub fn paste(&mut self, text: &str) -> Vec<u8> {
        self.scroll = 0;
        input::encode_paste(text, self.screen.modes().bracketed_paste)
    }

    /// A mouse event over the grid. A program that tracks the mouse gets its report (Shift keeps
    /// the mouse for selection, as in xterm); otherwise the wheel scrolls the viewport — or sends
    /// cursor keys on the alternate screen when the program enabled alternate scroll (1007) — and
    /// everything else is left to the windowing layer's selection.
    pub fn mouse(&mut self, ev: &MouseEvent) -> MouseOutcome {
        let modes = self.screen.modes();
        if modes.mouse != MouseTracking::Off && !ev.mods.shift {
            match ev.action {
                MouseAction::Press(b) if !b.is_wheel() => self.held = Some(b),
                MouseAction::Release(b) if Some(b) == self.held => self.held = None,
                MouseAction::Move => {
                    if self.last_motion == Some((ev.col, ev.row)) {
                        return MouseOutcome::Send(Vec::new());
                    }
                    self.last_motion = Some((ev.col, ev.row));
                }
                _ => {}
            }
            return MouseOutcome::Send(
                input::encode_mouse(ev, self.held, &modes).unwrap_or_default(),
            );
        }
        if let MouseAction::Press(b @ (MouseButton::WheelUp | MouseButton::WheelDown)) = ev.action {
            if modes.alternate_screen {
                if modes.alternate_scroll {
                    let key = if b == MouseButton::WheelUp {
                        Key::Up
                    } else {
                        Key::Down
                    };
                    let one = input::encode_key(key, Mods::NONE, &modes);
                    return MouseOutcome::Send(one.repeat(WHEEL_LINES));
                }
                return MouseOutcome::Local;
            }
            let n = WHEEL_LINES.cast_signed();
            self.scroll_lines(if b == MouseButton::WheelUp { n } else { -n });
            return MouseOutcome::Scrolled;
        }
        MouseOutcome::Local
    }

    /// The view gained or lost keyboard focus: the report for the pane, if it asked for one.
    pub fn focus(&mut self, focused: bool) -> Option<Vec<u8>> {
        if self.focused == focused {
            return None;
        }
        self.focused = focused;
        input::encode_focus(focused, &self.screen.modes())
    }
}

fn is_word(c: char) -> bool {
    !WORD_DELIMITERS.contains(c)
}

/// The first column of the word (or run of delimiters) at `col`.
fn word_start(row: &Row, col: u16) -> u16 {
    let at = |x: usize| {
        let c = row.cell(x);
        if c.width == CellWidth::Tail && x > 0 {
            row.cell(x - 1).ch
        } else {
            c.ch
        }
    };
    let mut x = usize::from(col).min(row.cols().saturating_sub(1));
    let kind = is_word(at(x));
    while x > 0 && is_word(at(x - 1)) == kind {
        x -= 1;
    }
    u16::try_from(x).unwrap_or(0)
}

/// The last column of the word (or run of delimiters) at `col`.
fn word_end(row: &Row, col: u16) -> u16 {
    let at = |x: usize| {
        let c = row.cell(x);
        if c.width == CellWidth::Tail && x > 0 {
            row.cell(x - 1).ch
        } else {
            c.ch
        }
    };
    let last = row.cols().saturating_sub(1);
    let mut x = usize::from(col).min(last);
    let kind = is_word(at(x));
    while x < last && is_word(at(x + 1)) == kind {
        x += 1;
    }
    u16::try_from(x).unwrap_or(0)
}
