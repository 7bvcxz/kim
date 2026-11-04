//! One client's view of a pane, testable without rendering (ADR 0036, docs/TERMINAL_EMULATOR.md
//! §3): the terminal model — the same [`Screen`] the Host runs, fed with the Host's snapshot and
//! live stream — plus what belongs to this view only: the scrollback viewport, the selection and
//! copy, hyperlink hit tests, and the input the user gives through it.
//!
//! Nothing here draws. A renderer reads [`TermView::row_at`], [`TermView::selection_cols`] and
//! [`TermView::cursor_in_view`]; a windowing layer calls [`TermView::key`], [`TermView::paste`],
//! [`TermView::mouse`] and [`TermView::focus`] and writes what they return to the pane.

use crate::input::{self, Key, Mods, MouseAction, MouseButton, MouseEvent};
use crate::screen::{
    Attrs, CellWidth, HISTORY_BYTES, Limits, Link, MouseTracking, Row, Screen, char_width,
};
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
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Point {
    pub line: u64,
    pub col: u16,
}

/// A run of cells from `start` to `end` (both included, in reading order), possibly over
/// several rows: a search match, a detected link.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Span {
    pub start: Point,
    pub end: Point,
}

/// One logical line: the rows of a line joined across soft wraps, as plain text (a wide
/// character once, combining marks after their character), with where each row starts.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct LogicalLine {
    pub text: String,
    /// `(line number, byte offset)` of each row in [`LogicalLine::text`], in order.
    pub rows: Vec<(u64, usize)>,
    /// Columns of the view (cells past them are clamped to the last one).
    cols: u16,
}

impl LogicalLine {
    /// The row segment holding byte `b`.
    fn segment(&self, b: usize) -> (u64, usize) {
        let i = self
            .rows
            .partition_point(|&(_, s)| s <= b)
            .saturating_sub(1);
        self.rows.get(i).copied().unwrap_or((0, 0))
    }

    /// The cell of the character that starts at byte `b` (a combining mark: its character's).
    pub fn point(&self, b: usize) -> Point {
        let (line, start) = self.segment(b);
        let mut col: usize = 0;
        for c in self.text[start..b.min(self.text.len())].chars() {
            col += usize::from(char_width(c));
        }
        if let Some(c) = self.text[b.min(self.text.len())..].chars().next()
            && char_width(c) == 0
        {
            col = col.saturating_sub(1);
        }
        let last = usize::from(self.cols.max(1) - 1);
        Point {
            line,
            col: u16::try_from(col.min(last)).unwrap_or(0),
        }
    }

    /// The cells of the text `a..b` (bytes; `b` exclusive) as a [`Span`]: from the first cell of
    /// its first character to the last cell of its last one (a wide character's second cell).
    pub fn span(&self, a: usize, b: usize) -> Span {
        let start = self.point(a);
        let Some((i, c)) = self.text[a..b.max(a)].char_indices().last() else {
            return Span { start, end: start };
        };
        let mut end = self.point(a + i);
        let w = char_width(c);
        if w == 2 && end.col + 1 < self.cols {
            end.col += 1;
        }
        Span {
            start,
            end: end.max(start),
        }
    }

    /// The byte offset of the character drawn in cell `p`, if the line has one there.
    pub fn byte_at(&self, p: Point) -> Option<usize> {
        let i = self.rows.iter().position(|&(l, _)| l == p.line)?;
        let start = self.rows[i].1;
        let end = self.rows.get(i + 1).map_or(self.text.len(), |r| r.1);
        let mut col = 0usize;
        for (b, c) in self.text[start..end].char_indices() {
            let w = usize::from(char_width(c));
            if w == 0 {
                continue;
            }
            if usize::from(p.col) < col + w {
                return Some(start + b);
            }
            col += w;
        }
        None
    }
}

/// How the keyboard moves the selection's free end in mark mode ([`TermView::mark_move`]).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Motion {
    Left,
    Right,
    Up,
    Down,
    WordLeft,
    WordRight,
    LineStart,
    LineEnd,
    PageUp,
    PageDown,
    Top,
    Bottom,
}

/// One selected cell for a formatted copy ([`TermView::selected_cells`]).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CopiedCell {
    /// The character with its combining marks.
    pub text: String,
    pub attrs: Attrs,
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
    /// Highlighted spans (search matches), sorted, and which one is the current one.
    highlights: Vec<Span>,
    current: Option<usize>,
    /// Keyboard selection (mark mode): the arrows move the selection's free end.
    marking: bool,
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
            highlights: Vec::new(),
            current: None,
            marking: false,
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
        if self.selection.is_none() {
            self.marking = false;
        }
    }

    /// The pane changed size (this view resized it, or another client did and the Host told us).
    pub fn resize(&mut self, cols: u16, rows: u16) {
        self.screen.resize(cols, rows);
        self.seen_total = self.screen.history().total_lines;
        self.selection = None;
        self.marking = false;
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
    pub fn first_line(&self) -> u64 {
        self.screen.history().total_lines - self.screen.history_len() as u64
    }

    /// The `line` number of the first screen row (every line before it is history).
    pub fn screen_line(&self) -> u64 {
        self.screen.history().total_lines
    }

    /// The first line the view can show: the oldest history line, or the first screen row on
    /// the alternate screen (which has no history).
    pub fn top_line(&self) -> u64 {
        if self.screen.alternate() {
            self.screen_line()
        } else {
            self.first_line()
        }
    }

    /// The `line` number of the last screen row.
    pub fn last_line(&self) -> u64 {
        self.screen_line() + u64::from(self.size().1) - 1
    }

    /// Appends the plain text of line `line` to `out` — a wide character once, combining marks
    /// after their character, the trailing blanks of a line that does not wrap left out — and
    /// returns whether it continues on the next line (a soft wrap). Nothing is decoded into
    /// cells for history lines (what a search reads quickly).
    pub fn line_text(&self, line: u64, out: &mut String) -> Option<bool> {
        let total = self.screen_line();
        let from = out.len();
        let wrapped = if line >= total {
            let y = u16::try_from(line - total).ok()?;
            if y >= self.size().1 {
                return None;
            }
            let row = self.screen.row(y);
            for x in 0..row.cols() {
                let c = row.cell(x);
                if c.width != CellWidth::Tail {
                    out.push(c.ch);
                    out.push_str(c.combining);
                }
            }
            row.wrapped()
        } else {
            let i = usize::try_from(line.checked_sub(self.first_line())?).ok()?;
            self.screen.history_text(i, out)?
        };
        if !wrapped {
            let keep = out[from..].trim_end_matches(' ').len();
            out.truncate(from + keep);
        }
        Some(wrapped)
    }

    /// Whether line `line` continues on the next one (a soft wrap).
    pub fn line_wrapped(&self, line: u64) -> Option<bool> {
        let total = self.screen_line();
        if line >= total {
            let y = u16::try_from(line - total).ok()?;
            (y < self.size().1).then(|| self.screen.row(y).wrapped())
        } else {
            let i = usize::try_from(line.checked_sub(self.first_line())?).ok()?;
            self.screen.history_wrapped(i)
        }
    }

    /// The first line of the logical line that `line` belongs to (back over soft wraps).
    pub fn logical_start(&self, line: u64) -> u64 {
        let top = self.top_line();
        let mut s = line;
        while s > top && self.line_wrapped(s - 1) == Some(true) {
            s -= 1;
        }
        s
    }

    /// The logical line `line` belongs to: its rows joined across soft wraps.
    pub fn logical_line(&self, line: u64) -> Option<LogicalLine> {
        if line < self.top_line() || line > self.last_line() {
            return None;
        }
        let mut l = LogicalLine {
            cols: self.size().0,
            ..LogicalLine::default()
        };
        self.logical_line_into(self.logical_start(line), &mut l);
        Some(l)
    }

    /// Fills `l` with the logical line starting at `start` (reusing its buffers) and returns
    /// the line after it.
    pub fn logical_line_into(&self, start: u64, l: &mut LogicalLine) -> u64 {
        l.text.clear();
        l.rows.clear();
        l.cols = self.size().0;
        let last = self.last_line();
        let mut n = start;
        loop {
            l.rows.push((n, l.text.len()));
            let wrapped = self.line_text(n, &mut l.text).unwrap_or(false);
            n += 1;
            if !wrapped || n > last {
                return n;
            }
        }
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

    /// The viewport row that shows `line`, if it is in view.
    pub fn row_of_line(&self, line: u64) -> Option<u16> {
        let first = self.line_of_row(0);
        let row = line.checked_sub(first)?;
        (row < u64::from(self.size().1)).then(|| u16::try_from(row).unwrap_or(0))
    }

    /// Scrolls so `line` is in view, below the first row (an overlay such as the search bar
    /// may cover it) when there is room: a line already in rows 1… stays where it is; else it
    /// is brought to about a third from the top. The alternate screen does not scroll.
    pub fn reveal(&mut self, line: u64) {
        if self.screen.alternate() {
            return;
        }
        let rows = self.size().1;
        if let Some(r) = self.row_of_line(line)
            && (r >= 1 || rows == 1)
        {
            return;
        }
        // Row `r` shows `total + r - scroll`: choose `r` a third of the way down.
        let r = u64::from(rows / 3).max(u64::from(rows > 1));
        let want = (self.screen_line() + r).saturating_sub(line);
        self.scroll = usize::try_from(want)
            .unwrap_or(usize::MAX)
            .min(self.max_scroll());
    }

    // -----------------------------------------------------------------------------------------
    // Highlights (search matches).

    /// Highlights `spans` (sorted, not overlapping), `current` drawn as the current one.
    pub fn set_highlights(&mut self, spans: Vec<Span>, current: Option<usize>) {
        self.highlights = spans;
        self.current = current.filter(|&i| i < self.highlights.len());
    }

    pub fn clear_highlights(&mut self) {
        self.highlights.clear();
        self.current = None;
    }

    pub fn highlights(&self) -> &[Span] {
        &self.highlights
    }

    pub fn current_highlight(&self) -> Option<usize> {
        self.current
    }

    /// The highlighted columns of viewport row `row` (inclusive), each with whether it is the
    /// current one, into `out` (cleared first).
    pub fn highlight_cols(&self, row: u16, out: &mut Vec<(u16, u16, bool)>) {
        out.clear();
        if self.highlights.is_empty() {
            return;
        }
        let line = self.line_of_row(row);
        let last = self.size().0 - 1;
        // Spans are sorted and do not overlap, so their ends are sorted too.
        let from = self.highlights.partition_point(|s| s.end.line < line);
        for (i, s) in self.highlights[from..].iter().enumerate() {
            if s.start.line > line {
                break;
            }
            let a = if s.start.line == line { s.start.col } else { 0 };
            let b = if s.end.line == line {
                s.end.col.min(last)
            } else {
                last
            };
            if a <= b {
                out.push((a, b, self.current == Some(from + i)));
            }
        }
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
        self.marking = false;
    }

    pub fn has_selection(&self) -> bool {
        self.selection.is_some()
    }

    /// Selects the cells of `span` (a search match, a link).
    pub fn select_span(&mut self, span: Span) {
        self.selection = Some(Selection {
            anchor: span.start,
            head: span.end,
            mode: SelectMode::Cell,
        });
    }

    /// Selects everything the view can show: the history and the screen (the alternate screen
    /// alone while a full-screen program uses it).
    pub fn select_all(&mut self) {
        self.selection = Some(Selection {
            anchor: Point {
                line: self.top_line(),
                col: 0,
            },
            head: Point {
                line: self.last_line(),
                col: self.size().0 - 1,
            },
            mode: SelectMode::Cell,
        });
    }

    /// The selection's ends in reading order, before words and lines are expanded.
    pub fn selection_span(&self) -> Option<Span> {
        let (a, b, _) = self.selection_bounds()?;
        Some(Span { start: a, end: b })
    }

    // -----------------------------------------------------------------------------------------
    // Keyboard selection (mark mode, as Windows Terminal's).

    /// Whether the keyboard moves the selection (mark mode).
    pub fn marking(&self) -> bool {
        self.marking
    }

    /// Starts mark mode: the selection's free end becomes a caret the arrows move. An existing
    /// selection is kept (its free end continues); otherwise the caret starts at the cursor (at
    /// the bottom row of a scrolled-back view when the cursor is not in view).
    pub fn mark_begin(&mut self) {
        self.marking = true;
        if self.selection.is_some() {
            return;
        }
        let p = match self.cursor_in_view() {
            Some((x, y)) => self.point_at(x, y),
            None => self.point_at(0, self.size().1 - 1),
        };
        self.selection = Some(Selection {
            anchor: p,
            head: p,
            mode: SelectMode::Cell,
        });
    }

    /// Leaves mark mode; the selection stays when `keep` (to be copied), else it is cleared.
    pub fn mark_end(&mut self, keep: bool) {
        self.marking = false;
        if !keep {
            self.selection = None;
        }
    }

    /// Moves the caret of mark mode; with `extend` the selection grows from its anchor (Shift),
    /// else the anchor moves with it (one cell selected). The view scrolls to keep the caret
    /// in view.
    pub fn mark_move(&mut self, m: Motion, extend: bool) {
        if !self.marking {
            self.mark_begin();
        }
        let Some(mut s) = self.selection else {
            return;
        };
        let (cols, rows) = self.size();
        let last_col = cols - 1;
        let (top, bottom) = (self.top_line(), self.last_line());
        let page = u64::from(rows.saturating_sub(1).max(1));
        let mut h = s.head;
        match m {
            Motion::Left => {
                if h.col > 0 {
                    h.col -= 1;
                } else if h.line > top {
                    h.line -= 1;
                    h.col = last_col;
                }
            }
            Motion::Right => {
                if h.col < last_col {
                    h.col += 1;
                } else if h.line < bottom {
                    h.line += 1;
                    h.col = 0;
                }
            }
            Motion::Up => h.line = h.line.saturating_sub(1).max(top),
            Motion::Down => h.line = (h.line + 1).min(bottom),
            Motion::PageUp => h.line = h.line.saturating_sub(page).max(top),
            Motion::PageDown => h.line = (h.line + page).min(bottom),
            Motion::LineStart => h.col = 0,
            Motion::LineEnd => {
                let mut t = String::new();
                self.line_text(h.line, &mut t);
                let used: usize = t.chars().map(|c| usize::from(char_width(c))).sum();
                h.col = u16::try_from(used.saturating_sub(1))
                    .unwrap_or(last_col)
                    .min(last_col);
            }
            Motion::Top => {
                h.line = top;
                h.col = 0;
            }
            Motion::Bottom => {
                h.line = bottom;
                h.col = last_col;
            }
            Motion::WordLeft | Motion::WordRight => {
                h = self.word_step(h, m == Motion::WordRight, extend);
            }
        }
        // A wide character is one place: growing forward onto it takes both its cells, and the
        // caret never sits on its second cell alone.
        if let Some(row) = self.line(h.line) {
            let forward = matches!(
                m,
                Motion::Right | Motion::WordRight | Motion::LineEnd | Motion::Bottom
            );
            match row.cell(usize::from(h.col)).width {
                CellWidth::Wide if extend && forward && h.col < last_col => h.col += 1,
                CellWidth::Tail if !(extend && forward) && h.col > 0 => h.col -= 1,
                _ => {}
            }
        }
        s.head = h;
        if !extend {
            s.anchor = h;
        }
        s.mode = SelectMode::Cell;
        self.selection = Some(s);
        self.reveal_any(h.line);
    }

    /// The start of the next (or previous) word from `p`, crossing line ends; growing a
    /// selection forward (`to_end`), the last cell of the word instead, so the selection ends
    /// with the word as in an editor.
    fn word_step(&self, p: Point, forward: bool, to_end: bool) -> Point {
        let (top, bottom) = (self.top_line(), self.last_line());
        let last_col = self.size().0 - 1;
        let word_at = |q: Point| -> Option<bool> {
            let row = self.line(q.line)?;
            let c = row.cell(usize::from(q.col));
            let ch = if c.width == CellWidth::Tail && q.col > 0 {
                row.cell(usize::from(q.col) - 1).ch
            } else {
                c.ch
            };
            Some(ch != ' ' && is_word(ch))
        };
        let step = |q: Point| -> Option<Point> {
            if forward {
                if q.col < last_col {
                    Some(Point {
                        line: q.line,
                        col: q.col + 1,
                    })
                } else if q.line < bottom {
                    Some(Point {
                        line: q.line + 1,
                        col: 0,
                    })
                } else {
                    None
                }
            } else if q.col > 0 {
                Some(Point {
                    line: q.line,
                    col: q.col - 1,
                })
            } else if q.line > top {
                Some(Point {
                    line: q.line - 1,
                    col: last_col,
                })
            } else {
                None
            }
        };
        let mut q = p;
        if forward && to_end {
            // At least one cell on, over the gap, then to the word's last cell (on its line).
            match step(q) {
                Some(n) => q = n,
                None => return q,
            }
            while word_at(q) == Some(false) {
                match step(q) {
                    Some(n) => q = n,
                    None => return q,
                }
            }
            while let Some(n) = step(q) {
                let hard_break = n.line != q.line && self.line_wrapped(q.line) != Some(true);
                if word_at(n) != Some(true) || hard_break {
                    break;
                }
                q = n;
            }
            q
        } else if forward {
            // Skip the rest of this word, then the gap, to the next word's first cell.
            while word_at(q) == Some(true) {
                match step(q) {
                    Some(n) => q = n,
                    None => return q,
                }
            }
            while word_at(q) == Some(false) {
                match step(q) {
                    Some(n) => q = n,
                    None => return q,
                }
            }
            q
        } else {
            // Back over the gap, then to the first cell of that word.
            if let Some(n) = step(q) {
                q = n;
            }
            while word_at(q) == Some(false) {
                match step(q) {
                    Some(n) => q = n,
                    None => return q,
                }
            }
            while let Some(n) = step(q) {
                if word_at(n) != Some(true) {
                    break;
                }
                q = n;
            }
            q
        }
    }

    /// Scrolls so `line` is in view (any row).
    fn reveal_any(&mut self, line: u64) {
        if self.screen.alternate() || self.row_of_line(line).is_some() {
            return;
        }
        let first = self.line_of_row(0);
        let rows = u64::from(self.size().1);
        let want = if line < first {
            // At the top.
            self.screen_line().saturating_sub(line)
        } else {
            // At the bottom.
            (self.screen_line() + rows - 1).saturating_sub(line)
        };
        self.scroll = usize::try_from(want)
            .unwrap_or(usize::MAX)
            .min(self.max_scroll());
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
        Some(self.text_between(a, b, mode))
    }

    /// The text of the cells of `span` (a search match, a detected link), as a copy gets it.
    pub fn span_text(&self, span: Span) -> String {
        self.text_between(span.start, span.end, SelectMode::Cell)
    }

    fn text_between(&self, a: Point, b: Point, mode: SelectMode) -> String {
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
        out
    }

    /// The selected cells with their attributes, line by line, for a formatted (HTML) copy: the
    /// same cells and lines as [`TermView::selected_text`] (a wide character once, trailing
    /// blanks removed, soft-wrapped rows joined).
    pub fn selected_cells(&self) -> Option<Vec<Vec<CopiedCell>>> {
        let (a, b, mode) = self.selection_bounds()?;
        let last = self.size().0 - 1;
        let mut out: Vec<Vec<CopiedCell>> = vec![Vec::new()];
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
            let mut x = usize::from(c0);
            if row.cell(x).width == CellWidth::Tail && x > 0 {
                x -= 1;
            }
            let cur = out.last_mut().expect("one line at least");
            while x <= usize::from(c1) && x < row.cols() {
                let c = row.cell(x);
                if c.width != CellWidth::Tail {
                    let mut text = String::from(c.ch);
                    text.push_str(c.combining);
                    cur.push(CopiedCell {
                        text,
                        attrs: c.attrs,
                    });
                }
                x += 1;
            }
            let joins = mode != SelectMode::Block && row.wrapped() && line != b.line;
            if !joins {
                while cur.last().is_some_and(|c| c.text == " ") {
                    cur.pop();
                }
                if line != b.line {
                    out.push(Vec::new());
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
        self.marking = false;
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
