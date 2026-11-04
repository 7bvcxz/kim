//! The model of one pane's terminal (Step 13, ADR 0030, docs/SCROLLBACK.md; shared with client
//! views since Step 15, ADR 0036, docs/TERMINAL_EMULATOR.md): a VT parser and screen emulator fed
//! with every byte the pane writes, a bounded scrollback, the snapshot that reproduces the pane on
//! a newly attached client, and capture for `pane.capture`. The Host and every client view run
//! this same code, so they agree on what each byte means.
//!
//! Portable: no OS calls, no allocation per byte in the common case, and nothing runs unless
//! output arrives (the pane's reader thread calls [`Screen::feed`]).
//!
//! **What is modelled** (the xterm subset Windows ConPTY emits, plus what full-screen programs
//! send through it on Linux later): printable text with East Asian wide characters and combining
//! marks, C0 controls, cursor movement and addressing, erase and insert/delete of characters and
//! lines, scroll regions (DECSTBM) and origin mode, scrolling (`SU`/`SD`, index, reverse index),
//! SGR colours (16, 256, truecolour) and attributes, the alternate screen (47/1047/1048/1049),
//! DEC private modes (cursor keys, autowrap, cursor visibility, reverse video, the mouse tracking
//! and encoding modes; every other mode is remembered and replayed as it was set: bracketed
//! paste, focus events, synchronized output…), the keypad mode, DEC line drawing, tab stops, the
//! saved cursor, the window title (OSC 0/2), OSC 8 hyperlinks and the cursor shape (DECSCUSR).
//! Device reports and queries are ignored by default: a terminal attached to the pane answers
//! them, never the Host. A client view that *is* the terminal turns
//! [`Screen::set_answer_reports`] on and sends [`Screen::take_replies`] back to the pane.
//!
//! **Scrollback.** A row that scrolls off the top of the primary screen while the scroll region
//! is the whole screen goes to the history (as xterm and tmux do), encoded compactly as text with
//! the SGR sequences it needs. The history is bounded by lines and by bytes; the oldest lines are
//! evicted first, so eviction is deterministic. The alternate screen has no history.
//!
//! **Snapshot.** [`Screen::snapshot`] serializes the model as a VT byte sequence: the history
//! (pushed into the client terminal's own scrollback), the primary screen, the alternate screen
//! when it is active, then the scroll region, modes, charsets, tab stops, saved cursor, title,
//! cursor and current attributes — and finally the raw bytes of an escape sequence or UTF-8
//! character that is still incomplete, so live output that follows continues it exactly.

use std::collections::{BTreeMap, VecDeque};
use std::fmt::Write as _;
use std::sync::Arc;

/// Default history length of a new pane, in rows (tmux `history-limit`).
pub const DEFAULT_HISTORY_LINES: usize = 2000;
/// Largest history length a pane may ask for.
pub const MAX_HISTORY_LINES: usize = 100_000;
/// Bytes of history one pane keeps at most, whatever its line limit (ADR 0008: 4 MiB).
pub const HISTORY_BYTES: usize = 4 * 1024 * 1024;
/// What one history line costs beyond its encoded bytes (its record in the ring).
pub const LINE_OVERHEAD: usize = 32;
/// Longest window title kept, in characters.
const TITLE_MAX: usize = 1024;
/// Raw bytes of an unfinished sequence kept for the snapshot (the start of a longer one).
const PENDING_MAX: usize = 4096;
/// Longest OSC string interpreted; longer ones are consumed and ignored.
const OSC_MAX: usize = 4096;
/// Most CSI parameter bytes interpreted.
const PARAMS_MAX: usize = 64;
/// Most DEC private modes remembered beyond the ones the model interprets.
const MODES_MAX: usize = 64;
/// Longest hyperlink target kept (OSC 8); longer ones are ignored, as are targets with controls.
pub const LINK_URI_MAX: usize = 2048;
/// Longest hyperlink `id=` parameter kept.
const LINK_ID_MAX: usize = 250;
/// Reply bytes kept for a client view that has not collected them yet.
const REPLIES_MAX: usize = 4096;

/// A colour of a cell.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub enum Color {
    #[default]
    Default,
    Indexed(u8),
    Rgb(u8, u8, u8),
}

const BOLD: u16 = 1;
const DIM: u16 = 1 << 1;
const ITALIC: u16 = 1 << 2;
const UNDERLINE: u16 = 1 << 3;
const DOUBLE_UNDERLINE: u16 = 1 << 4;
const BLINK: u16 = 1 << 5;
const INVERSE: u16 = 1 << 6;
const HIDDEN: u16 = 1 << 7;
const STRIKE: u16 = 1 << 8;
const OVERLINE: u16 = 1 << 9;

/// SGR flags in emission order with their SGR numbers.
const FLAG_CODES: [(u16, &str); 10] = [
    (BOLD, "1"),
    (DIM, "2"),
    (ITALIC, "3"),
    (UNDERLINE, "4"),
    (DOUBLE_UNDERLINE, "21"),
    (BLINK, "5"),
    (INVERSE, "7"),
    (HIDDEN, "8"),
    (STRIKE, "9"),
    (OVERLINE, "53"),
];

/// Graphic rendition of a cell.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub struct Attrs {
    pub fg: Color,
    pub bg: Color,
    flags: u16,
}

impl Attrs {
    fn is_default(&self) -> bool {
        *self == Self::default()
    }

    pub fn bold(&self) -> bool {
        self.flags & BOLD != 0
    }

    pub fn dim(&self) -> bool {
        self.flags & DIM != 0
    }

    pub fn italic(&self) -> bool {
        self.flags & ITALIC != 0
    }

    /// 0 = none, 1 = single, 2 = double.
    pub fn underline(&self) -> u8 {
        if self.flags & DOUBLE_UNDERLINE != 0 {
            2
        } else {
            u8::from(self.flags & UNDERLINE != 0)
        }
    }

    pub fn blink(&self) -> bool {
        self.flags & BLINK != 0
    }

    pub fn inverse(&self) -> bool {
        self.flags & INVERSE != 0
    }

    pub fn hidden(&self) -> bool {
        self.flags & HIDDEN != 0
    }

    pub fn strike(&self) -> bool {
        self.flags & STRIKE != 0
    }

    pub fn overline(&self) -> bool {
        self.flags & OVERLINE != 0
    }

    /// The SGR sequence that sets exactly these attributes from any state.
    fn sgr(&self, out: &mut String) {
        out.push_str("\x1b[0");
        for (f, code) in FLAG_CODES {
            if self.flags & f != 0 {
                out.push(';');
                out.push_str(code);
            }
        }
        color_sgr(self.fg, 30, out);
        color_sgr(self.bg, 40, out);
        out.push('m');
    }
}

fn color_sgr(c: Color, base: u8, out: &mut String) {
    match c {
        Color::Default => {}
        Color::Indexed(n) if n < 8 => {
            let _ = write!(out, ";{}", base + n);
        }
        Color::Indexed(n) if n < 16 => {
            let _ = write!(out, ";{}", base + 60 + n - 8);
        }
        Color::Indexed(n) => {
            let _ = write!(out, ";{};5;{n}", base + 8);
        }
        Color::Rgb(r, g, b) => {
            let _ = write!(out, ";{};2;{r};{g};{b}", base + 8);
        }
    }
}

/// What part of a (possibly wide) character a cell holds.
const NARROW: u8 = 0;
const WIDE: u8 = 1;
const TAIL: u8 = 2;

/// The target of an OSC 8 hyperlink. Shared by every cell printed while it was active.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Link {
    /// The `id=` parameter, which joins the cells of one link across rows.
    pub id: Option<Box<str>>,
    pub uri: Box<str>,
}

impl Link {
    /// The OSC 8 sequence that starts this link.
    fn open(&self, out: &mut String) {
        out.push_str("\x1b]8;");
        if let Some(id) = &self.id {
            out.push_str("id=");
            out.push_str(id);
        }
        out.push(';');
        out.push_str(&self.uri);
        out.push_str("\x1b\\");
    }
}

/// The OSC 8 sequence that ends a hyperlink.
const LINK_CLOSE: &str = "\x1b]8;;\x1b\\";

fn same_link(a: Option<&Arc<Link>>, b: Option<&Arc<Link>>) -> bool {
    match (a, b) {
        (None, None) => true,
        (Some(a), Some(b)) => Arc::ptr_eq(a, b) || a == b,
        _ => false,
    }
}

/// What part of a character one cell shows.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CellWidth {
    /// A one-column character.
    Narrow,
    /// The first column of a two-column character.
    Wide,
    /// The second column of a two-column character (draws nothing of its own).
    Tail,
}

/// One cell as a view or a renderer sees it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CellInfo<'a> {
    pub ch: char,
    pub width: CellWidth,
    pub attrs: Attrs,
    /// Combining marks drawn with the character (empty for most cells).
    pub combining: &'a str,
    /// The hyperlink the character belongs to (a tail cell reports its character's link).
    pub link: Option<&'a Arc<Link>>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Cell {
    ch: char,
    attrs: Attrs,
    kind: u8,
}

impl Cell {
    fn blank(attrs: Attrs) -> Self {
        Self {
            ch: ' ',
            attrs,
            kind: NARROW,
        }
    }

    fn is_plain_blank(&self) -> bool {
        self.ch == ' ' && self.kind == NARROW && self.attrs.is_default()
    }
}

/// One row of cells: a screen row, or a history line decoded by [`Screen::history_row`].
#[derive(Debug, Clone)]
pub struct Row {
    cells: Vec<Cell>,
    /// The line continues on the next row (autowrap happened here).
    wrapped: bool,
    /// Combining marks after the character of a column (rare).
    combining: Vec<(u16, String)>,
    /// Hyperlinks of the columns that start a linked character (rare).
    links: Vec<(u16, Arc<Link>)>,
}

impl Row {
    fn new(cols: usize, attrs: Attrs) -> Self {
        Self {
            cells: vec![Cell::blank(attrs); cols],
            wrapped: false,
            combining: Vec::new(),
            links: Vec::new(),
        }
    }

    /// Columns of the row.
    pub fn cols(&self) -> usize {
        self.cells.len()
    }

    /// The line continues on the next row (a soft wrap).
    pub fn wrapped(&self) -> bool {
        self.wrapped
    }

    /// The cell at column `x` (a blank cell past the end).
    pub fn cell(&self, x: usize) -> CellInfo<'_> {
        let Some(c) = self.cells.get(x) else {
            return CellInfo {
                ch: ' ',
                width: CellWidth::Narrow,
                attrs: Attrs::default(),
                combining: "",
                link: None,
            };
        };
        CellInfo {
            ch: c.ch,
            width: match c.kind {
                WIDE => CellWidth::Wide,
                TAIL => CellWidth::Tail,
                _ => CellWidth::Narrow,
            },
            attrs: c.attrs,
            combining: if c.kind == TAIL {
                ""
            } else {
                self.combining_at(x).unwrap_or("")
            },
            link: self.link_at(x),
        }
    }

    /// The hyperlink of the character at column `x` (looked up at its first column).
    pub fn link_at(&self, x: usize) -> Option<&Arc<Link>> {
        if self.links.is_empty() {
            return None;
        }
        let x = match self.cells.get(x) {
            Some(c) if c.kind == TAIL && x > 0 => x - 1,
            _ => x,
        };
        self.links
            .iter()
            .find(|(c, _)| usize::from(*c) == x)
            .map(|(_, l)| l)
    }

    /// Forgets combining marks and hyperlinks of columns `from..to` (they are being replaced).
    fn drop_extras(&mut self, from: usize, to: usize) {
        if !self.combining.is_empty() {
            self.combining
                .retain(|(c, _)| !(from..to).contains(&usize::from(*c)));
        }
        if !self.links.is_empty() {
            self.links
                .retain(|(c, _)| !(from..to).contains(&usize::from(*c)));
        }
    }

    fn combining_at(&self, x: usize) -> Option<&str> {
        self.combining
            .iter()
            .find(|(c, _)| usize::from(*c) == x)
            .map(|(_, s)| s.as_str())
    }

    /// Columns up to the last one that shows something (all of them when the row wraps).
    fn used(&self) -> usize {
        if self.wrapped {
            return self.cells.len();
        }
        let shown = self
            .cells
            .iter()
            .rposition(|c| !c.is_plain_blank())
            .map_or(0, |i| i + 1);
        let linked = self
            .links
            .iter()
            .map(|(c, _)| usize::from(*c) + 1)
            .max()
            .unwrap_or(0);
        shown.max(linked).min(self.cells.len())
    }

    /// The row as text with the SGR and OSC 8 sequences it needs (from default attributes and no
    /// hyperlink, back to them).
    fn encode(&self, out: &mut String) -> usize {
        let used = self.used();
        let mut cur = Attrs::default();
        let mut link: Option<&Arc<Link>> = None;
        for (x, c) in self.cells[..used].iter().enumerate() {
            if c.kind == TAIL {
                continue;
            }
            if c.attrs != cur {
                c.attrs.sgr(out);
                cur = c.attrs;
            }
            let l = self.link_at(x);
            if !same_link(l, link) {
                match l {
                    Some(l) => l.open(out),
                    None => out.push_str(LINK_CLOSE),
                }
                link = l;
            }
            out.push(c.ch);
            if let Some(m) = self.combining_at(x) {
                out.push_str(m);
            }
        }
        if link.is_some() {
            out.push_str(LINK_CLOSE);
        }
        if !cur.is_default() {
            out.push_str("\x1b[0m");
        }
        used
    }

    /// The row as plain text, all its columns (wide characters once, combining marks included).
    pub fn text(&self) -> String {
        let mut s = String::new();
        for (x, c) in self.cells.iter().enumerate() {
            if c.kind == TAIL {
                continue;
            }
            s.push(c.ch);
            if let Some(m) = self.combining_at(x) {
                s.push_str(m);
            }
        }
        s
    }
}

/// One line of history: the row as compact VT text.
#[derive(Debug, Clone)]
struct Line {
    vt: Box<str>,
    /// Columns it occupied.
    width: u32,
    /// It continues in the next line (a wrapped row).
    wrapped: bool,
}

impl Line {
    fn cost(&self) -> usize {
        self.vt.len() + LINE_OVERHEAD
    }

    /// The text without its SGR and OSC 8 sequences.
    fn text(&self) -> String {
        strip_vt(&self.vt)
    }
}

/// Removes the sequences [`Row::encode`] writes: SGR (`ESC [ … m`) and OSC 8
/// (`ESC ] 8 ; … ESC \`).
fn strip_vt(vt: &str) -> String {
    let mut out = String::with_capacity(vt.len());
    let mut it = vt.chars().peekable();
    while let Some(c) = it.next() {
        if c != '\x1b' {
            out.push(c);
            continue;
        }
        match it.next() {
            Some('[') => {
                for c in it.by_ref() {
                    if ('\x40'..='\x7e').contains(&c) {
                        break;
                    }
                }
            }
            Some(']') => {
                while let Some(c) = it.next() {
                    if c == '\x07' {
                        break;
                    }
                    if c == '\x1b' && it.peek() == Some(&'\\') {
                        it.next();
                        break;
                    }
                }
            }
            _ => {}
        }
    }
    out
}

/// Bounds of a pane's history.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Limits {
    /// Rows kept (0 keeps none).
    pub lines: usize,
    /// Encoded bytes kept, each line counting [`LINE_OVERHEAD`] more.
    pub bytes: usize,
}

impl Default for Limits {
    fn default() -> Self {
        Self {
            lines: DEFAULT_HISTORY_LINES,
            bytes: HISTORY_BYTES,
        }
    }
}

/// Counters of a pane's history (`host.stats`, `pane.capture`, `subak history`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct HistoryInfo {
    pub lines: usize,
    pub bytes: usize,
    pub limit_lines: usize,
    pub limit_bytes: usize,
    /// Lines dropped to stay within the limits, since the pane started.
    pub evicted_lines: u64,
    /// Lines removed by `pane.clear_history` or a program's `ESC[3J`.
    pub cleared_lines: u64,
    /// Lines that ever entered the history.
    pub total_lines: u64,
}

#[derive(Debug)]
struct History {
    lines: VecDeque<Line>,
    bytes: usize,
    limits: Limits,
    evicted: u64,
    cleared: u64,
    total: u64,
}

impl History {
    fn new(limits: Limits) -> Self {
        Self {
            lines: VecDeque::new(),
            bytes: 0,
            limits,
            evicted: 0,
            cleared: 0,
            total: 0,
        }
    }

    fn push(&mut self, row: &Row) {
        self.total += 1;
        if self.limits.lines == 0 {
            self.evicted += 1;
            return;
        }
        let mut vt = String::new();
        let width = row.encode(&mut vt);
        let line = Line {
            vt: vt.into_boxed_str(),
            width: u32::try_from(width).unwrap_or(u32::MAX),
            wrapped: row.wrapped,
        };
        self.bytes += line.cost();
        self.lines.push_back(line);
        self.evict();
    }

    /// Drops the oldest lines until both limits hold.
    fn evict(&mut self) {
        while self.lines.len() > self.limits.lines || self.bytes > self.limits.bytes {
            let Some(old) = self.lines.pop_front() else {
                break;
            };
            self.bytes -= old.cost();
            self.evicted += 1;
        }
    }

    fn clear(&mut self) -> usize {
        let n = self.lines.len();
        self.lines.clear();
        self.lines.shrink_to_fit();
        self.bytes = 0;
        self.cleared += n as u64;
        n
    }

    fn info(&self) -> HistoryInfo {
        HistoryInfo {
            lines: self.lines.len(),
            bytes: self.bytes,
            limit_lines: self.limits.lines,
            limit_bytes: self.limits.bytes,
            evicted_lines: self.evicted,
            cleared_lines: self.cleared,
            total_lines: self.total,
        }
    }
}

/// Which history lines a snapshot carries.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HistoryRange {
    /// None.
    None,
    /// The newest `n` lines.
    Last(usize),
    /// Lines that entered the history at or after this [`HistoryInfo::total_lines`] mark
    /// (what a lagging client missed), as far as they are still kept.
    Since(u64),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Charset {
    Ascii,
    DecGraphics,
    Uk,
}

/// The cursor and what DECSC saves with it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Saved {
    x: u16,
    y: u16,
    attrs: Attrs,
    origin: bool,
    charsets: [Charset; 2],
    shift: u8,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct Modes {
    app_cursor: bool,
    app_keypad: bool,
    origin: bool,
    autowrap: bool,
    insert: bool,
    newline: bool,
    cursor_visible: bool,
    reverse_video: bool,
    /// Mouse tracking: 0 (off), 9, 1000, 1002 or 1003. Setting one replaces the others and
    /// resetting any turns tracking off, as in xterm.
    mouse: u16,
    /// Mouse coordinate encoding: 0 (default), 1005, 1006, 1015 or 1016 (same rules).
    mouse_encoding: u16,
    /// Other DEC private modes as last set (bracketed paste, focus…), replayed as given.
    other: BTreeMap<u16, bool>,
}

impl Default for Modes {
    fn default() -> Self {
        Self {
            app_cursor: false,
            app_keypad: false,
            origin: false,
            autowrap: true,
            insert: false,
            newline: false,
            cursor_visible: true,
            reverse_video: false,
            mouse: 0,
            mouse_encoding: 0,
            other: BTreeMap::new(),
        }
    }
}

/// Which mouse events the program asked for (DEC private modes 9, 1000, 1002, 1003).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MouseTracking {
    Off,
    /// Mode 9: button presses only.
    X10,
    /// Mode 1000: presses, releases and the wheel.
    Normal,
    /// Mode 1002: also motion while a button is held.
    ButtonEvent,
    /// Mode 1003: also motion with no button held.
    AnyEvent,
}

/// How mouse coordinates are encoded (DEC private modes 1005, 1006, 1015, 1016).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MouseEncoding {
    /// `CSI M` with one byte per value (coordinates up to 223).
    Default,
    /// Mode 1005: `CSI M` with UTF-8 encoded values (up to 2015).
    Utf8,
    /// Mode 1006: `CSI < b ; x ; y M/m`.
    Sgr,
    /// Mode 1015: `CSI b ; x ; y M`.
    Urxvt,
    /// Mode 1016: like 1006 with pixel coordinates.
    SgrPixels,
}

/// The modes a client view needs to encode input and draw the screen.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ModeState {
    /// DECCKM: cursor keys send `SS3` sequences.
    pub app_cursor: bool,
    /// DECKPAM: the keypad sends application sequences.
    pub app_keypad: bool,
    /// LNM: Enter sends CR LF.
    pub newline: bool,
    pub autowrap: bool,
    pub insert: bool,
    pub origin: bool,
    /// DECTCEM.
    pub cursor_visible: bool,
    /// DECSCNM: default foreground and background swapped.
    pub reverse_video: bool,
    pub alternate_screen: bool,
    pub mouse: MouseTracking,
    pub mouse_encoding: MouseEncoding,
    /// Mode 2004.
    pub bracketed_paste: bool,
    /// Mode 1004.
    pub focus_events: bool,
    /// Mode 1007: the wheel sends cursor keys on the alternate screen.
    pub alternate_scroll: bool,
    /// Mode 9001: keys are sent as win32-input-mode sequences (ConPTY asks for it).
    pub win32_input: bool,
    /// Mode 2026: the program is between the start and end of a synchronized update.
    pub synchronized_output: bool,
    /// Mode 12: the cursor blinks (also implied by DECSCUSR 0, 1, 3, 5).
    pub cursor_blink: bool,
}

/// The cursor's shape (DECSCUSR).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CursorShape {
    Block,
    Underline,
    Bar,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum PState {
    Ground,
    Esc,
    Csi,
    CsiIgnore,
    Osc,
    OscEsc,
    /// DCS, SOS, PM, APC: consumed until ST.
    Str,
    StrEsc,
}

#[derive(Debug)]
struct Parser {
    state: PState,
    utf8: [u8; 4],
    utf8_len: u8,
    utf8_need: u8,
    params: Vec<u8>,
    private: u8,
    inter: Vec<u8>,
    osc: Vec<u8>,
    osc_overflow: bool,
    /// Raw bytes of the sequence (or UTF-8 character) in progress.
    raw: Vec<u8>,
}

impl Parser {
    fn new() -> Self {
        Self {
            state: PState::Ground,
            utf8: [0; 4],
            utf8_len: 0,
            utf8_need: 0,
            params: Vec::new(),
            private: 0,
            inter: Vec::new(),
            osc: Vec::new(),
            osc_overflow: false,
            raw: Vec::new(),
        }
    }

    fn record(&mut self, b: u8) {
        if self.raw.len() < PENDING_MAX {
            self.raw.push(b);
        }
    }
}

/// The cursor position as reported by [`Screen::cursor`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CursorInfo {
    pub x: u16,
    pub y: u16,
    pub visible: bool,
}

/// One captured range (`pane.capture`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Capture {
    pub lines: Vec<String>,
    /// The first and last line number captured (history lines are negative, -1 the newest;
    /// screen rows count from 0), or `None` when the range was empty.
    pub range: Option<(i64, i64)>,
}

/// One end of a capture range: a line number, or the oldest history line / the last screen row.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Pos {
    Line(i64),
    /// `-` in tmux: the start of the history (as `start`) or the end of the screen (as `end`).
    Edge,
}

/// The terminal model of one pane.
#[derive(Debug)]
pub struct Screen {
    cols: u16,
    rows: u16,
    primary: Vec<Row>,
    /// The alternate screen while it is active.
    alt: Option<Vec<Row>>,
    x: u16,
    y: u16,
    pending_wrap: bool,
    attrs: Attrs,
    /// DECSC slots of the primary and the alternate screen.
    saved: [Option<Saved>; 2],
    top: u16,
    bottom: u16,
    modes: Modes,
    charsets: [Charset; 2],
    shift: u8,
    tabs: Vec<bool>,
    title: String,
    cursor_style: u8,
    last_char: Option<char>,
    /// The OSC 8 hyperlink printed characters get.
    link: Option<Arc<Link>>,
    history: History,
    parser: Parser,
    /// Answer device reports (a client view that is the pane's terminal); never in the Host.
    answer: bool,
    replies: Vec<u8>,
}

impl Screen {
    pub fn new(cols: u16, rows: u16, limits: Limits) -> Self {
        let (cols, rows) = (cols.max(1), rows.max(1));
        Self {
            cols,
            rows,
            primary: blank_grid(cols, rows, Attrs::default()),
            alt: None,
            x: 0,
            y: 0,
            pending_wrap: false,
            attrs: Attrs::default(),
            saved: [None, None],
            top: 0,
            bottom: rows - 1,
            modes: Modes::default(),
            charsets: [Charset::Ascii; 2],
            shift: 0,
            tabs: default_tabs(cols),
            title: String::new(),
            cursor_style: 0,
            last_char: None,
            link: None,
            history: History::new(limits),
            parser: Parser::new(),
            answer: false,
            replies: Vec::new(),
        }
    }

    pub fn size(&self) -> (u16, u16) {
        (self.cols, self.rows)
    }

    pub fn alternate(&self) -> bool {
        self.alt.is_some()
    }

    pub fn cursor(&self) -> CursorInfo {
        CursorInfo {
            x: self.x,
            y: self.y,
            visible: self.modes.cursor_visible,
        }
    }

    pub fn history(&self) -> HistoryInfo {
        self.history.info()
    }

    /// Row `y` of the active screen.
    pub fn row(&self, y: u16) -> &Row {
        let g = self.grid();
        &g[usize::from(y).min(g.len() - 1)]
    }

    /// History lines kept (index 0 is the oldest).
    pub fn history_len(&self) -> usize {
        self.history.lines.len()
    }

    /// History line `i` (0 = the oldest kept) decoded into cells by this same parser, cut or
    /// padded to `cols` columns. The history keeps lines as compact VT text, so a view that
    /// scrolls back decodes the few lines it shows.
    pub fn history_row(&self, i: usize, cols: u16) -> Option<Row> {
        let line = self.history.lines.get(i)?;
        let width = u16::try_from(line.width).unwrap_or(u16::MAX).max(1);
        let mut scratch = Screen::new(width, 1, Limits { lines: 0, bytes: 0 });
        scratch.feed(line.vt.as_bytes());
        let mut row = scratch.primary.swap_remove(0);
        fit_row(&mut row, usize::from(cols.max(1)));
        row.wrapped = line.wrapped;
        Some(row)
    }

    /// The window title (OSC 0/2).
    pub fn title(&self) -> &str {
        &self.title
    }

    /// The hyperlink that characters printed now get (OSC 8).
    pub fn active_link(&self) -> Option<&Arc<Link>> {
        self.link.as_ref()
    }

    /// The cursor's shape and whether it blinks (DECSCUSR, mode 12).
    pub fn cursor_shape(&self) -> (CursorShape, bool) {
        let blink = self.modes.other.get(&12).copied().unwrap_or(false);
        match self.cursor_style {
            0 => (CursorShape::Block, blink),
            1 => (CursorShape::Block, true),
            3 => (CursorShape::Underline, true),
            4 => (CursorShape::Underline, false),
            5 => (CursorShape::Bar, true),
            6 => (CursorShape::Bar, false),
            _ => (CursorShape::Block, false),
        }
    }

    /// The modes input encoding and drawing depend on.
    pub fn modes(&self) -> ModeState {
        let m = &self.modes;
        let other = |n: u16| m.other.get(&n).copied().unwrap_or(false);
        ModeState {
            app_cursor: m.app_cursor,
            app_keypad: m.app_keypad,
            newline: m.newline,
            autowrap: m.autowrap,
            insert: m.insert,
            origin: m.origin,
            cursor_visible: m.cursor_visible,
            reverse_video: m.reverse_video,
            alternate_screen: self.alt.is_some(),
            mouse: match m.mouse {
                9 => MouseTracking::X10,
                1000 => MouseTracking::Normal,
                1002 => MouseTracking::ButtonEvent,
                1003 => MouseTracking::AnyEvent,
                _ => MouseTracking::Off,
            },
            mouse_encoding: match m.mouse_encoding {
                1005 => MouseEncoding::Utf8,
                1006 => MouseEncoding::Sgr,
                1015 => MouseEncoding::Urxvt,
                1016 => MouseEncoding::SgrPixels,
                _ => MouseEncoding::Default,
            },
            bracketed_paste: other(2004),
            focus_events: other(1004),
            alternate_scroll: other(1007),
            win32_input: other(9001),
            synchronized_output: other(2026),
            cursor_blink: self.cursor_shape().1,
        }
    }

    /// Answers device reports (DA, DSR, CPR, DECRQM) into [`Screen::take_replies`]. Off by
    /// default and always off in the Host: the terminal attached to a pane answers, never the
    /// Host. Only the view that owns the pane's size should turn it on, so a program gets one
    /// answer.
    pub fn set_answer_reports(&mut self, on: bool) {
        self.answer = on;
        if !on {
            self.replies.clear();
        }
    }

    /// The answers to device reports since the last call, to be written to the pane as input.
    pub fn take_replies(&mut self) -> Vec<u8> {
        std::mem::take(&mut self.replies)
    }

    fn reply(&mut self, s: &str) {
        if self.answer && self.replies.len() + s.len() <= REPLIES_MAX {
            self.replies.extend_from_slice(s.as_bytes());
        }
    }

    /// Approximate memory the model holds: both grids and the history.
    pub fn memory_bytes(&self) -> usize {
        let grid = |g: &Vec<Row>| {
            g.iter()
                .map(|r| {
                    r.cells.capacity() * size_of::<Cell>()
                        + size_of::<Row>()
                        + r.combining.iter().map(|(_, s)| s.len() + 32).sum::<usize>()
                })
                .sum::<usize>()
        };
        let links = |g: &Vec<Row>| {
            g.iter()
                .map(|r| r.links.capacity() * size_of::<(u16, Arc<Link>)>())
                .sum::<usize>()
        };
        grid(&self.primary)
            + self.alt.as_ref().map_or(0, grid)
            + links(&self.primary)
            + self.alt.as_ref().map_or(0, links)
            + self.history.bytes
    }

    /// Sets the history bounds; lines beyond them are evicted at once.
    #[cfg(test)]
    pub fn set_limits(&mut self, limits: Limits) {
        self.history.limits = limits;
        self.history.evict();
    }

    /// Removes every history line; returns how many there were.
    pub fn clear_history(&mut self) -> usize {
        self.history.clear()
    }

    // -----------------------------------------------------------------------------------------
    // Input.

    /// Feeds pane output.
    pub fn feed(&mut self, bytes: &[u8]) {
        for &b in bytes {
            if self.parser.state == PState::Ground
                && self.parser.utf8_need == 0
                && (0x20..0x7f).contains(&b)
            {
                self.print(char::from(b));
            } else {
                self.byte(b);
            }
        }
    }

    fn byte(&mut self, b: u8) {
        // ESC, CAN and SUB interrupt any sequence (inside strings ESC may start ST).
        match (b, self.parser.state) {
            (0x1b, PState::Osc) => {
                self.parser.record(b);
                self.parser.state = PState::OscEsc;
                return;
            }
            (0x1b, PState::Str) => {
                self.parser.record(b);
                self.parser.state = PState::StrEsc;
                return;
            }
            (0x1b, _) => {
                self.flush_utf8();
                self.parser.raw.clear();
                self.parser.record(b);
                self.parser.state = PState::Esc;
                self.parser.params.clear();
                self.parser.inter.clear();
                self.parser.private = 0;
                return;
            }
            (0x18 | 0x1a, _) => {
                self.flush_utf8();
                self.reset_parser();
                return;
            }
            _ => {}
        }
        match self.parser.state {
            PState::Ground => self.ground(b),
            PState::Esc => self.esc(b),
            PState::Csi | PState::CsiIgnore => self.csi_byte(b),
            PState::Osc => {
                if b == 0x07 {
                    self.osc_end();
                } else {
                    self.parser.record(b);
                    if self.parser.osc.len() < OSC_MAX {
                        self.parser.osc.push(b);
                    } else {
                        self.parser.osc_overflow = true;
                    }
                }
            }
            PState::OscEsc => {
                if b == b'\\' {
                    self.osc_end();
                } else {
                    // Not ST: the OSC ended, and this byte starts an escape sequence.
                    self.osc_end();
                    self.byte(0x1b);
                    self.byte(b);
                }
            }
            PState::Str => {
                if b == 0x07 {
                    self.reset_parser();
                } else {
                    self.parser.record(b);
                }
            }
            PState::StrEsc => {
                if b == b'\\' {
                    self.reset_parser();
                } else {
                    self.parser.state = PState::Str;
                    self.parser.record(b);
                }
            }
        }
    }

    fn reset_parser(&mut self) {
        self.parser.state = PState::Ground;
        self.parser.raw.clear();
        self.parser.osc.clear();
        self.parser.osc_overflow = false;
    }

    fn ground(&mut self, b: u8) {
        if b < 0x20 || b == 0x7f {
            self.flush_utf8();
            self.execute(b);
            return;
        }
        if b < 0x80 {
            self.flush_utf8();
            self.print(char::from(b));
            return;
        }
        let p = &mut self.parser;
        if p.utf8_need == 0 {
            let need = match b {
                0xc2..=0xdf => 1,
                0xe0..=0xef => 2,
                0xf0..=0xf4 => 3,
                _ => {
                    self.print('\u{fffd}');
                    return;
                }
            };
            p.raw.clear();
            p.record(b);
            p.utf8[0] = b;
            p.utf8_len = 1;
            p.utf8_need = need;
            return;
        }
        if b & 0xc0 != 0x80 {
            // A lead byte where a continuation was expected.
            self.flush_utf8();
            self.ground(b);
            return;
        }
        p.record(b);
        p.utf8[usize::from(p.utf8_len)] = b;
        p.utf8_len += 1;
        p.utf8_need -= 1;
        if p.utf8_need == 0 {
            let len = usize::from(p.utf8_len);
            let c = std::str::from_utf8(&p.utf8[..len])
                .ok()
                .and_then(|s| s.chars().next())
                .unwrap_or('\u{fffd}');
            p.utf8_len = 0;
            p.raw.clear();
            self.print(c);
        }
    }

    /// An incomplete UTF-8 character interrupted by something else becomes U+FFFD.
    fn flush_utf8(&mut self) {
        if self.parser.utf8_need > 0 {
            self.parser.utf8_need = 0;
            self.parser.utf8_len = 0;
            self.parser.raw.clear();
            self.print('\u{fffd}');
        }
    }

    fn esc(&mut self, b: u8) {
        if b < 0x20 {
            self.execute(b); // C0 inside a sequence is executed, not recorded
            return;
        }
        self.parser.record(b);
        match b {
            0x20..=0x2f => {
                if self.parser.inter.len() < 4 {
                    self.parser.inter.push(b);
                }
            }
            b'[' if self.parser.inter.is_empty() => self.parser.state = PState::Csi,
            b']' if self.parser.inter.is_empty() => {
                self.parser.osc.clear();
                self.parser.osc_overflow = false;
                self.parser.state = PState::Osc;
            }
            b'P' | b'X' | b'^' | b'_' if self.parser.inter.is_empty() => {
                self.parser.state = PState::Str;
            }
            _ => {
                let inter = self.parser.inter.first().copied();
                self.reset_parser();
                self.esc_dispatch(inter, b);
            }
        }
    }

    fn esc_dispatch(&mut self, inter: Option<u8>, b: u8) {
        match (inter, b) {
            (None, b'7') => self.save_cursor(),
            (None, b'8') => self.restore_cursor(),
            (None, b'D') => self.index(),
            (None, b'E') => {
                self.carriage_return();
                self.index();
            }
            (None, b'H') => {
                if let Some(t) = self.tabs.get_mut(usize::from(self.x)) {
                    *t = true;
                }
            }
            (None, b'M') => self.reverse_index(),
            (None, b'c') => self.reset(),
            (None, b'=') => self.modes.app_keypad = true,
            (None, b'>') => self.modes.app_keypad = false,
            (Some(b'#'), b'8') => {
                // DECALN: fill the screen with `E`.
                let a = Attrs::default();
                for row in self.grid_mut() {
                    row.cells.fill(Cell {
                        ch: 'E',
                        attrs: a,
                        kind: NARROW,
                    });
                    row.combining.clear();
                    row.links.clear();
                    row.wrapped = false;
                }
                self.top = 0;
                self.bottom = self.rows - 1;
                self.move_to(0, 0);
            }
            (Some(i @ (b'(' | b')')), f) => {
                let set = match f {
                    b'0' => Charset::DecGraphics,
                    b'A' => Charset::Uk,
                    _ => Charset::Ascii,
                };
                self.charsets[usize::from(i == b')')] = set;
            }
            _ => {}
        }
    }

    fn csi_byte(&mut self, b: u8) {
        if b < 0x20 {
            self.execute(b);
            return;
        }
        self.parser.record(b);
        let p = &mut self.parser;
        match b {
            b'0'..=b'9' | b';' | b':' => {
                if !p.inter.is_empty() {
                    p.state = PState::CsiIgnore;
                } else if p.params.len() < PARAMS_MAX {
                    p.params.push(b);
                } else {
                    p.state = PState::CsiIgnore;
                }
            }
            b'<'..=b'?' => {
                if p.params.is_empty() && p.private == 0 && p.inter.is_empty() {
                    p.private = b;
                } else {
                    p.state = PState::CsiIgnore;
                }
            }
            0x20..=0x2f => {
                if p.inter.len() < 4 {
                    p.inter.push(b);
                } else {
                    p.state = PState::CsiIgnore;
                }
            }
            0x40..=0x7e => {
                let ignore = p.state == PState::CsiIgnore;
                let params = std::mem::take(&mut p.params);
                let inter = std::mem::take(&mut p.inter);
                let private = p.private;
                self.reset_parser();
                if !ignore {
                    self.csi_dispatch(private, &inter, &params, b);
                }
                self.parser.params = params;
                self.parser.params.clear();
            }
            _ => p.state = PState::CsiIgnore,
        }
    }

    fn osc_end(&mut self) {
        let osc = std::mem::take(&mut self.parser.osc);
        let overflow = self.parser.osc_overflow;
        self.reset_parser();
        if overflow {
            return;
        }
        let Some(sep) = osc.iter().position(|&c| c == b';') else {
            return;
        };
        match &osc[..sep] {
            b"0" | b"2" => {
                let text = String::from_utf8_lossy(&osc[sep + 1..]);
                self.title = text
                    .chars()
                    .filter(|c| !c.is_control())
                    .take(TITLE_MAX)
                    .collect();
            }
            b"8" => self.set_link(&osc[sep + 1..]),
            _ => {}
        }
    }

    /// OSC 8 `params ; uri`: an empty URI ends the hyperlink. A target that is too long, is not
    /// UTF-8 or contains control characters is not a link (the text still prints).
    fn set_link(&mut self, rest: &[u8]) {
        let Some(sep) = rest.iter().position(|&c| c == b';') else {
            self.link = None;
            return;
        };
        let (params, uri) = (&rest[..sep], &rest[sep + 1..]);
        let ok = |s: &str, max: usize| {
            !s.is_empty() && s.len() <= max && !s.chars().any(|c| c.is_control() || c == ' ')
        };
        let Ok(uri) = std::str::from_utf8(uri) else {
            self.link = None;
            return;
        };
        if uri.is_empty() || !ok(uri, LINK_URI_MAX) {
            self.link = None;
            return;
        }
        let id = std::str::from_utf8(params).ok().and_then(|p| {
            p.split(':')
                .find_map(|kv| kv.strip_prefix("id="))
                .filter(|id| ok(id, LINK_ID_MAX))
        });
        if let Some(cur) = &self.link
            && *cur.uri == *uri
            && cur.id.as_deref() == id
        {
            return;
        }
        self.link = Some(Arc::new(Link {
            id: id.map(Into::into),
            uri: uri.into(),
        }));
    }

    fn execute(&mut self, b: u8) {
        match b {
            0x08 => {
                if self.pending_wrap {
                    self.pending_wrap = false;
                }
                self.x = self.x.saturating_sub(1);
            }
            0x09 => {
                let next = (usize::from(self.x) + 1..usize::from(self.cols))
                    .find(|&i| self.tabs[i])
                    .unwrap_or(usize::from(self.cols) - 1);
                self.x = u16::try_from(next).unwrap_or(self.cols - 1);
                self.pending_wrap = false;
            }
            0x0a..=0x0c => {
                if self.modes.newline {
                    self.carriage_return();
                }
                self.index();
            }
            0x0d => self.carriage_return(),
            0x0e => self.shift = 1,
            0x0f => self.shift = 0,
            _ => {}
        }
    }

    // -----------------------------------------------------------------------------------------
    // The grid.

    fn grid_mut(&mut self) -> &mut Vec<Row> {
        match &mut self.alt {
            Some(a) => a,
            None => &mut self.primary,
        }
    }

    fn grid(&self) -> &Vec<Row> {
        self.alt.as_ref().unwrap_or(&self.primary)
    }

    /// The attributes erased cells get (background colour erase).
    fn erase_attrs(&self) -> Attrs {
        Attrs {
            bg: self.attrs.bg,
            ..Attrs::default()
        }
    }

    fn print(&mut self, c: char) {
        let c = self.map_charset(c);
        let w = char_width(c);
        if w == 0 {
            self.combine(c);
            return;
        }
        let cols = usize::from(self.cols);
        if self.pending_wrap && self.modes.autowrap {
            self.wrap_line();
        }
        let mut x = usize::from(self.x);
        if w == 2 && x + 1 >= cols {
            if cols < 2 {
                return;
            }
            if self.modes.autowrap {
                let ea = self.erase_attrs();
                let y = usize::from(self.y);
                let row = &mut self.grid_mut()[y];
                fix_wide(row, x);
                row.cells[x] = Cell::blank(ea);
                self.wrap_line();
                x = 0;
            } else {
                x = cols - 2;
            }
        }
        if self.modes.insert {
            self.x = u16::try_from(x).unwrap_or(0);
            self.insert_cells(usize::from(w));
        }
        let attrs = self.attrs;
        let link = self.link.clone();
        let y = usize::from(self.y);
        let row = &mut self.grid_mut()[y];
        fix_wide(row, x);
        if w == 2 {
            fix_wide(row, x + 1);
        }
        row.drop_extras(x, x + usize::from(w));
        if let Some(l) = link {
            row.links.push((u16::try_from(x).unwrap_or(0), l));
        }
        row.cells[x] = Cell {
            ch: c,
            attrs,
            kind: if w == 2 { WIDE } else { NARROW },
        };
        if w == 2 {
            row.cells[x + 1] = Cell {
                ch: ' ',
                attrs,
                kind: TAIL,
            };
        }
        self.last_char = Some(c);
        let next = x + usize::from(w);
        if next >= cols {
            self.x = self.cols - 1;
            self.pending_wrap = self.modes.autowrap;
        } else {
            self.x = u16::try_from(next).unwrap_or(self.cols - 1);
            self.pending_wrap = false;
        }
    }

    /// A zero-width character joins the character before the cursor.
    fn combine(&mut self, c: char) {
        let mut x = usize::from(self.x);
        if !self.pending_wrap {
            if x == 0 {
                return;
            }
            x -= 1;
        }
        let y = usize::from(self.y);
        let row = &mut self.grid_mut()[y];
        if row.cells[x].kind == TAIL && x > 0 {
            x -= 1;
        }
        let col = u16::try_from(x).unwrap_or(0);
        match row.combining.iter_mut().find(|(i, _)| *i == col) {
            Some((_, s)) if s.len() < 16 => s.push(c),
            Some(_) => {}
            None => row.combining.push((col, c.to_string())),
        }
    }

    fn map_charset(&self, c: char) -> char {
        match self.charsets[usize::from(self.shift)] {
            Charset::Ascii => c,
            Charset::Uk => {
                if c == '#' {
                    '£'
                } else {
                    c
                }
            }
            Charset::DecGraphics => dec_graphics(c),
        }
    }

    fn wrap_line(&mut self) {
        let y = usize::from(self.y);
        self.grid_mut()[y].wrapped = true;
        self.x = 0;
        self.pending_wrap = false;
        self.index();
    }

    fn carriage_return(&mut self) {
        self.x = 0;
        self.pending_wrap = false;
    }

    /// Line feed: down one row, scrolling the region at its bottom.
    fn index(&mut self) {
        self.pending_wrap = false;
        if self.y == self.bottom {
            self.scroll_up(self.top, self.bottom, 1);
        } else if self.y < self.rows - 1 {
            self.y += 1;
        }
    }

    fn reverse_index(&mut self) {
        self.pending_wrap = false;
        if self.y == self.top {
            self.scroll_down(self.top, self.bottom, 1);
        } else if self.y > 0 {
            self.y -= 1;
        }
    }

    /// Scrolls rows `top..=bottom` up by `n`; rows leaving the top of a whole-screen region of
    /// the primary screen enter the history.
    fn scroll_up(&mut self, top: u16, bottom: u16, n: usize) {
        let (t, b) = (usize::from(top), usize::from(bottom));
        let n = n.min(b - t + 1);
        let keep = self.alt.is_none() && top == 0 && bottom == self.rows - 1;
        let ea = self.erase_attrs();
        let cols = usize::from(self.cols);
        let grid = match &mut self.alt {
            Some(a) => a,
            None => &mut self.primary,
        };
        for _ in 0..n {
            let row = grid.remove(t);
            if keep {
                self.history.push(&row);
            }
            grid.insert(b, Row::new(cols, ea));
        }
    }

    fn scroll_down(&mut self, top: u16, bottom: u16, n: usize) {
        let (t, b) = (usize::from(top), usize::from(bottom));
        let n = n.min(b - t + 1);
        let ea = self.erase_attrs();
        let cols = usize::from(self.cols);
        let grid = self.grid_mut();
        for _ in 0..n {
            grid.remove(b);
            grid.insert(t, Row::new(cols, ea));
        }
    }

    fn erase_cells(&mut self, y: usize, from: usize, to: usize) {
        let ea = self.erase_attrs();
        let row = &mut self.grid_mut()[y];
        let to = to.min(row.cells.len());
        if from >= to {
            return;
        }
        fix_wide(row, from);
        if to < row.cells.len() {
            fix_wide(row, to);
        }
        for c in &mut row.cells[from..to] {
            *c = Cell::blank(ea);
        }
        row.drop_extras(from, to);
        if to == row.cells.len() {
            row.wrapped = false;
        }
    }

    fn erase_rows(&mut self, from: usize, to: usize) {
        let cols = usize::from(self.cols);
        for y in from..to {
            self.erase_cells(y, 0, cols);
        }
    }

    fn insert_cells(&mut self, n: usize) {
        let ea = self.erase_attrs();
        let (x, y) = (usize::from(self.x), usize::from(self.y));
        let row = &mut self.grid_mut()[y];
        let cols = row.cells.len();
        let n = n.min(cols - x);
        fix_wide(row, x);
        for _ in 0..n {
            row.cells.insert(x, Cell::blank(ea));
        }
        row.cells.truncate(cols);
        if row.cells[cols - 1].kind == WIDE {
            row.cells[cols - 1] = Cell::blank(ea);
        }
        shift_extras(row, x, isize::try_from(n).unwrap_or(0), cols);
    }

    fn delete_cells(&mut self, n: usize) {
        let ea = self.erase_attrs();
        let (x, y) = (usize::from(self.x), usize::from(self.y));
        let row = &mut self.grid_mut()[y];
        let cols = row.cells.len();
        let n = n.min(cols - x);
        fix_wide(row, x);
        if x + n < cols {
            fix_wide(row, x + n);
        }
        row.drop_extras(x, x + n);
        row.cells.drain(x..x + n);
        row.cells.resize(cols, Cell::blank(ea));
        shift_extras(row, x + n, -isize::try_from(n).unwrap_or(0), cols);
    }

    fn move_to(&mut self, x: usize, y: usize) {
        self.x = u16::try_from(x.min(usize::from(self.cols) - 1)).unwrap_or(0);
        self.y = u16::try_from(y.min(usize::from(self.rows) - 1)).unwrap_or(0);
        self.pending_wrap = false;
    }

    /// CUP/HVP/VPA target row: relative to the region in origin mode.
    fn row_target(&self, row: usize) -> usize {
        if self.modes.origin {
            (usize::from(self.top) + row).min(usize::from(self.bottom))
        } else {
            row
        }
    }

    fn save_cursor(&mut self) {
        let slot = usize::from(self.alt.is_some());
        self.saved[slot] = Some(Saved {
            x: self.x,
            y: self.y,
            attrs: self.attrs,
            origin: self.modes.origin,
            charsets: self.charsets,
            shift: self.shift,
        });
    }

    fn restore_cursor(&mut self) {
        let slot = usize::from(self.alt.is_some());
        let s = self.saved[slot].unwrap_or(Saved {
            x: 0,
            y: 0,
            attrs: Attrs::default(),
            origin: false,
            charsets: [Charset::Ascii; 2],
            shift: 0,
        });
        self.attrs = s.attrs;
        self.modes.origin = s.origin;
        self.charsets = s.charsets;
        self.shift = s.shift;
        self.move_to(usize::from(s.x), usize::from(s.y));
    }

    fn enter_alt(&mut self, clear: bool) {
        if self.alt.is_some() {
            if clear {
                let rows = usize::from(self.rows);
                self.erase_rows(0, rows);
            }
            return;
        }
        self.alt = Some(blank_grid(self.cols, self.rows, Attrs::default()));
    }

    fn leave_alt(&mut self) {
        self.alt = None;
    }

    fn reset(&mut self) {
        let history = std::mem::replace(&mut self.history, History::new(Limits::default()));
        *self = Self::new(self.cols, self.rows, history.limits);
        self.history = history;
    }

    fn soft_reset(&mut self) {
        self.modes.cursor_visible = true;
        self.modes.insert = false;
        self.modes.origin = false;
        self.modes.autowrap = true;
        self.modes.app_cursor = false;
        self.modes.app_keypad = false;
        self.top = 0;
        self.bottom = self.rows - 1;
        self.attrs = Attrs::default();
        self.charsets = [Charset::Ascii; 2];
        self.shift = 0;
        self.saved = [None, None];
        self.pending_wrap = false;
    }

    // -----------------------------------------------------------------------------------------
    // Control sequences.

    #[allow(clippy::too_many_lines)]
    fn csi_dispatch(&mut self, private: u8, inter: &[u8], raw: &[u8], f: u8) {
        let params = parse_params(raw);
        let p = |i: usize, d: u16| -> u16 {
            match params.get(i).and_then(|g| g.first().copied().flatten()) {
                Some(0) | None => d,
                Some(v) => v,
            }
        };
        let n = |i: usize| usize::from(p(i, 1));
        let (cols, rows) = (usize::from(self.cols), usize::from(self.rows));
        let (x, y) = (usize::from(self.x), usize::from(self.y));
        if !inter.is_empty() {
            match (private, inter, f) {
                (0, b" ", b'q') => self.cursor_style = u8::try_from(p(0, 0)).unwrap_or(0).min(6),
                (0, b"!", b'p') => self.soft_reset(),
                // DECRQM: report whether a mode is set (1), reset (2) or unknown (0).
                (0 | b'?', b"$", b'p') => {
                    let m = params
                        .first()
                        .and_then(|g| g.first().copied().flatten())
                        .unwrap_or(0);
                    let state = if private == b'?' {
                        self.private_mode_state(m)
                    } else {
                        match m {
                            4 => 2 - u8::from(self.modes.insert),
                            20 => 2 - u8::from(self.modes.newline),
                            _ => 0,
                        }
                    };
                    let q = if private == b'?' { "?" } else { "" };
                    self.reply(&format!("\x1b[{q}{m};{state}$y"));
                }
                _ => {}
            }
            return;
        }
        if private != 0 {
            match (private, f) {
                (b'?', b'h' | b'l') => {
                    for g in &params {
                        if let Some(Some(m)) = g.first() {
                            self.private_mode(*m, f == b'h');
                        }
                    }
                }
                // DA2: a VT220-class terminal, firmware version 10.
                (b'>', b'c') if p(0, 0) == 0 => self.reply("\x1b[>1;10;0c"),
                // DECXCPR: the cursor position with the page.
                (b'?', b'n') if p(0, 0) == 6 => {
                    let (row, col) = self.report_position();
                    self.reply(&format!("\x1b[?{row};{col};1R"));
                }
                _ => {}
            }
            return;
        }
        match f {
            b'@' => self.insert_cells(n(0)),
            b'A' => {
                let top = if y >= usize::from(self.top) {
                    usize::from(self.top)
                } else {
                    0
                };
                self.move_to(x, y.saturating_sub(n(0)).max(top));
            }
            b'B' | b'e' => {
                let bottom = if y <= usize::from(self.bottom) {
                    usize::from(self.bottom)
                } else {
                    rows - 1
                };
                self.move_to(x, (y + n(0)).min(bottom));
            }
            b'C' | b'a' => self.move_to(x + n(0), y),
            b'D' => self.move_to(x.saturating_sub(n(0)), y),
            b'E' => {
                let bottom = usize::from(self.bottom).max(y);
                self.move_to(0, (y + n(0)).min(bottom));
            }
            b'F' => {
                let top = usize::from(self.top).min(y);
                self.move_to(0, y.saturating_sub(n(0)).max(top));
            }
            b'G' | b'`' => self.move_to(n(0) - 1, y),
            b'H' | b'f' => {
                let row = self.row_target(n(0) - 1);
                self.move_to(n(1) - 1, row);
            }
            b'I' => {
                for _ in 0..n(0) {
                    self.execute(0x09);
                }
            }
            b'J' => match p(0, 0) {
                0 => {
                    self.erase_cells(y, x, cols);
                    self.erase_rows(y + 1, rows);
                }
                1 => {
                    self.erase_rows(0, y);
                    self.erase_cells(y, 0, x + 1);
                }
                2 => self.erase_rows(0, rows),
                3 => {
                    self.history.clear();
                }
                _ => {}
            },
            b'K' => match p(0, 0) {
                0 => self.erase_cells(y, x, cols),
                1 => self.erase_cells(y, 0, x + 1),
                2 => self.erase_cells(y, 0, cols),
                _ => {}
            },
            b'L' | b'M' => {
                if (usize::from(self.top)..=usize::from(self.bottom)).contains(&y) {
                    let yy = self.y;
                    if f == b'L' {
                        self.scroll_down(yy, self.bottom, n(0));
                    } else {
                        // Deleted lines never enter the history.
                        let (t, b) = (usize::from(yy), usize::from(self.bottom));
                        let k = n(0).min(b - t + 1);
                        let ea = self.erase_attrs();
                        let grid = self.grid_mut();
                        for _ in 0..k {
                            grid.remove(t);
                            grid.insert(b, Row::new(cols, ea));
                        }
                    }
                    self.move_to(0, y);
                }
            }
            b'P' => self.delete_cells(n(0)),
            b'S' => self.scroll_up(self.top, self.bottom, n(0)),
            b'T' if params.len() <= 1 => self.scroll_down(self.top, self.bottom, n(0)),
            b'X' => self.erase_cells(y, x, x + n(0)),
            b'Z' => {
                let mut nx = x;
                for _ in 0..n(0) {
                    nx = (0..nx).rev().find(|&i| self.tabs[i]).unwrap_or(0);
                }
                self.move_to(nx, y);
            }
            // DA1: VT220 with ANSI colour.
            b'c' if p(0, 0) == 0 => self.reply("\x1b[?62;22c"),
            b'n' => match p(0, 0) {
                5 => self.reply("\x1b[0n"),
                6 => {
                    let (row, col) = self.report_position();
                    self.reply(&format!("\x1b[{row};{col}R"));
                }
                _ => {}
            },
            b'b' => {
                if let Some(c) = self.last_char {
                    for _ in 0..n(0).min(cols * rows) {
                        self.print(c);
                    }
                }
            }
            b'd' => {
                let row = self.row_target(n(0) - 1);
                self.move_to(x, row);
            }
            b'g' => match p(0, 0) {
                0 => {
                    if let Some(t) = self.tabs.get_mut(x) {
                        *t = false;
                    }
                }
                3 => self.tabs.iter_mut().for_each(|t| *t = false),
                _ => {}
            },
            b'h' | b'l' => {
                let on = f == b'h';
                for g in &params {
                    match g.first() {
                        Some(Some(4)) => self.modes.insert = on,
                        Some(Some(20)) => self.modes.newline = on,
                        _ => {}
                    }
                }
            }
            b'm' => self.sgr(&params),
            b'r' => {
                let t = usize::from(p(0, 1));
                let b = usize::from(p(1, self.rows)).min(rows);
                if t < b {
                    self.top = u16::try_from(t - 1).unwrap_or(0);
                    self.bottom = u16::try_from(b - 1).unwrap_or(self.rows - 1);
                    let home = self.row_target(0);
                    self.move_to(0, home);
                }
            }
            b's' if params.is_empty() => self.save_cursor(),
            b'u' if params.is_empty() => self.restore_cursor(),
            _ => {}
        }
    }

    fn private_mode(&mut self, m: u16, on: bool) {
        match m {
            1 => self.modes.app_cursor = on,
            5 => self.modes.reverse_video = on,
            6 => {
                self.modes.origin = on;
                let home = self.row_target(0);
                self.move_to(0, home);
            }
            7 => {
                self.modes.autowrap = on;
                if !on {
                    self.pending_wrap = false;
                }
            }
            25 => self.modes.cursor_visible = on,
            47 | 1047 => {
                if on {
                    self.enter_alt(m == 1047);
                } else {
                    if m == 1047 && self.alt.is_some() {
                        let rows = usize::from(self.rows);
                        self.erase_rows(0, rows);
                    }
                    self.leave_alt();
                }
            }
            1048 => {
                if on {
                    self.save_cursor();
                } else {
                    self.restore_cursor();
                }
            }
            1049 => {
                if on {
                    if self.alt.is_none() {
                        self.save_cursor();
                    }
                    self.enter_alt(true);
                } else if self.alt.is_some() {
                    self.leave_alt();
                    self.restore_cursor();
                }
            }
            // DECCOLM would resize the terminal; the pane's size is the Host's to decide.
            3 => {}
            9 | 1000 | 1002 | 1003 => self.modes.mouse = if on { m } else { 0 },
            1005 | 1006 | 1015 | 1016 => self.modes.mouse_encoding = if on { m } else { 0 },
            _ => {
                if self.modes.other.contains_key(&m) || self.modes.other.len() < MODES_MAX {
                    self.modes.other.insert(m, on);
                }
            }
        }
    }

    /// DECRQM's answer for DEC private mode `m`: 1 set, 2 reset, 0 not recognized.
    fn private_mode_state(&self, m: u16) -> u8 {
        let md = &self.modes;
        let flag = |on: bool| if on { 1 } else { 2 };
        match m {
            1 => flag(md.app_cursor),
            5 => flag(md.reverse_video),
            6 => flag(md.origin),
            7 => flag(md.autowrap),
            25 => flag(md.cursor_visible),
            47 | 1047 | 1049 => flag(self.alt.is_some()),
            9 | 1000 | 1002 | 1003 => flag(md.mouse == m),
            1005 | 1006 | 1015 | 1016 => flag(md.mouse_encoding == m),
            12 | 1004 | 1007 | 2004 | 2026 | 9001 => {
                flag(md.other.get(&m).copied().unwrap_or(false))
            }
            _ => md.other.get(&m).map_or(0, |on| flag(*on)),
        }
    }

    /// The cursor position as CPR reports it: 1-based, relative to the region in origin mode.
    fn report_position(&self) -> (usize, usize) {
        let row = if self.modes.origin {
            usize::from(self.y.saturating_sub(self.top)) + 1
        } else {
            usize::from(self.y) + 1
        };
        (row, usize::from(self.x) + 1)
    }

    fn sgr(&mut self, params: &[Vec<Option<u16>>]) {
        if params.is_empty() {
            self.attrs = Attrs::default();
            return;
        }
        let mut i = 0;
        while i < params.len() {
            let g = &params[i];
            let v = g.first().copied().flatten().unwrap_or(0);
            let a = &mut self.attrs;
            if g.len() > 1 {
                // Colon sub-parameters: 38:5:n, 38:2::r:g:b, 4:3 (underline style).
                match v {
                    38 | 48 => {
                        if let Some(c) = sub_color(g) {
                            if v == 38 {
                                a.fg = c;
                            } else {
                                a.bg = c;
                            }
                        }
                    }
                    4 => {
                        a.flags &= !(UNDERLINE | DOUBLE_UNDERLINE);
                        match g[1] {
                            Some(0) => {}
                            Some(2) => a.flags |= DOUBLE_UNDERLINE,
                            _ => a.flags |= UNDERLINE,
                        }
                    }
                    _ => {}
                }
                i += 1;
                continue;
            }
            match v {
                0 => *a = Attrs::default(),
                1 => a.flags |= BOLD,
                2 => a.flags |= DIM,
                3 => a.flags |= ITALIC,
                4 => a.flags |= UNDERLINE,
                5 | 6 => a.flags |= BLINK,
                7 => a.flags |= INVERSE,
                8 => a.flags |= HIDDEN,
                9 => a.flags |= STRIKE,
                21 => a.flags |= DOUBLE_UNDERLINE,
                22 => a.flags &= !(BOLD | DIM),
                23 => a.flags &= !ITALIC,
                24 => a.flags &= !(UNDERLINE | DOUBLE_UNDERLINE),
                25 => a.flags &= !BLINK,
                27 => a.flags &= !INVERSE,
                28 => a.flags &= !HIDDEN,
                29 => a.flags &= !STRIKE,
                30..=37 => a.fg = Color::Indexed(u8::try_from(v - 30).unwrap_or(0)),
                39 => a.fg = Color::Default,
                40..=47 => a.bg = Color::Indexed(u8::try_from(v - 40).unwrap_or(0)),
                49 => a.bg = Color::Default,
                53 => a.flags |= OVERLINE,
                55 => a.flags &= !OVERLINE,
                90..=97 => a.fg = Color::Indexed(u8::try_from(v - 90 + 8).unwrap_or(0)),
                100..=107 => a.bg = Color::Indexed(u8::try_from(v - 100 + 8).unwrap_or(0)),
                38 | 48 | 58 => {
                    let arg =
                        |k: usize| params.get(i + k).and_then(|g| g.first().copied().flatten());
                    let (c, used) = match arg(1) {
                        Some(5) => (arg(2).map(|n| Color::Indexed(clamp8(n))), 2),
                        Some(2) => (
                            match (arg(2), arg(3), arg(4)) {
                                (Some(r), Some(g), Some(b)) => {
                                    Some(Color::Rgb(clamp8(r), clamp8(g), clamp8(b)))
                                }
                                _ => None,
                            },
                            4,
                        ),
                        _ => (None, 0),
                    };
                    if let Some(c) = c {
                        if v == 38 {
                            a.fg = c;
                        } else if v == 48 {
                            a.bg = c;
                        }
                    }
                    i += used;
                }
                _ => {}
            }
            i += 1;
        }
    }

    // -----------------------------------------------------------------------------------------
    // Resize.

    /// Changes the size: rows keep their columns (cut or padded, no reflow; ConPTY repaints the
    /// screen after a resize anyway), rows that no longer fit above the cursor enter the history,
    /// and the scroll region becomes the whole screen.
    pub fn resize(&mut self, cols: u16, rows: u16) {
        let (cols, rows) = (cols.max(1), rows.max(1));
        if (cols, rows) == (self.cols, self.rows) {
            return;
        }
        let (c, r) = (usize::from(cols), usize::from(rows));
        // Rows above a screen's cursor that no longer fit leave through the top: into the
        // history from the primary screen (whose cursor is the one 1049 saved while the
        // alternate screen is active), dropped from the alternate screen.
        let alt_active = self.alt.is_some();
        let primary_y = if alt_active {
            self.saved[0].map_or(0, |s| usize::from(s.y))
        } else {
            usize::from(self.y)
        };
        let excess = (primary_y + 1).saturating_sub(r);
        for row in self.primary.drain(..excess) {
            self.history.push(&row);
        }
        let shift =
            |v: &mut u16, by: usize| *v = v.saturating_sub(u16::try_from(by).unwrap_or(u16::MAX));
        if alt_active {
            if let Some(s) = &mut self.saved[0] {
                shift(&mut s.y, excess);
            }
            let alt_excess = (usize::from(self.y) + 1).saturating_sub(r);
            if let Some(a) = &mut self.alt {
                a.drain(..alt_excess);
            }
            shift(&mut self.y, alt_excess);
            if let Some(s) = &mut self.saved[1] {
                shift(&mut s.y, alt_excess);
            }
        } else {
            shift(&mut self.y, excess);
            if let Some(s) = &mut self.saved[0] {
                shift(&mut s.y, excess);
            }
        }
        let fit = |g: &mut Vec<Row>| {
            g.truncate(r);
            for row in g.iter_mut() {
                fit_row(row, c);
            }
            while g.len() < r {
                g.push(Row::new(c, Attrs::default()));
            }
        };
        fit(&mut self.primary);
        if let Some(a) = &mut self.alt {
            fit(a);
        }
        self.cols = cols;
        self.rows = rows;
        self.top = 0;
        self.bottom = rows - 1;
        let old = self.tabs.len();
        self.tabs.resize(c, false);
        for i in old..c {
            self.tabs[i] = i % 8 == 0 && i > 0;
        }
        self.x = self.x.min(cols - 1);
        self.y = self.y.min(rows - 1);
        self.pending_wrap = false;
        for s in self.saved.iter_mut().flatten() {
            s.x = s.x.min(cols - 1);
            s.y = s.y.min(rows - 1);
        }
    }

    // -----------------------------------------------------------------------------------------
    // Output.

    /// The VT bytes that reproduce this terminal on a client terminal of the pane's size that is
    /// in its default state, followed by the start of an unfinished sequence (see the module
    /// docs). `resync` first cancels a sequence the client may have been left in.
    pub fn snapshot(&self, history: HistoryRange, resync: bool) -> Vec<u8> {
        let mut s = String::with_capacity(
            usize::from(self.cols) * usize::from(self.rows) * 2 + self.history.bytes / 4,
        );
        if resync {
            s.push('\x18'); // CAN: ends whatever sequence the client was in
        }
        // A known state: default rendition and charsets, no margins, no origin mode, autowrap
        // on, insert off, cursor hidden while painting.
        s.push_str("\x1b[0m\x1b(B\x1b)B\x0f\x1b[r\x1b[?6l\x1b[?7h\x1b[4l\x1b[?25l");
        let cols = usize::from(self.cols);
        let rows = usize::from(self.rows);
        let lines = self.history_lines(history);
        if !lines.is_empty() {
            // Print the history from the top, then scroll the rows it occupies on screen into
            // the client's own scrollback.
            s.push_str("\x1b[H\x1b[2J");
            let mut used = 0usize;
            let mut run = 0usize;
            for (i, l) in lines.iter().enumerate() {
                s.push_str(&l.vt);
                run += l.width as usize;
                if !l.wrapped || i + 1 == lines.len() {
                    s.push_str("\r\n");
                    used += run.div_ceil(cols).max(1);
                    run = 0;
                }
            }
            let visible = used.min(rows - 1);
            let _ = write!(s, "\x1b[{rows};1H");
            for _ in 0..visible {
                s.push('\n');
            }
        }
        paint(&self.primary, &mut s);
        if let Some(alt) = &self.alt {
            // The cursor 1049 saved (the primary's DECSC slot), then the switch.
            if let Some(sv) = self.saved[0] {
                let _ = write!(s, "\x1b[{};{}H", sv.y + 1, sv.x + 1);
                sv.attrs.sgr(&mut s);
                s.push_str("\x1b[?1049h\x1b[0m");
            } else {
                s.push_str("\x1b[?1049h");
            }
            paint(alt, &mut s);
        }
        // Tab stops, when they differ from the default.
        if self.tabs != default_tabs(self.cols) {
            s.push_str("\x1b[3g");
            for (i, _) in self.tabs.iter().enumerate().filter(|(_, t)| **t) {
                let _ = write!(s, "\x1b[1;{}H\x1bH", i + 1);
            }
        }
        // The saved cursor of the active screen.
        if let Some(sv) = self.saved[usize::from(self.alt.is_some())] {
            let _ = write!(s, "\x1b[{};{}H", sv.y + 1, sv.x + 1);
            sv.attrs.sgr(&mut s);
            for (i, set) in sv.charsets.iter().enumerate() {
                charset_seq(i, *set, &mut s);
            }
            s.push(if sv.shift == 1 { '\x0e' } else { '\x0f' });
            s.push_str("\x1b7\x1b[0m\x1b(B\x1b)B\x0f");
        }
        if self.top != 0 || self.bottom != self.rows - 1 {
            let _ = write!(s, "\x1b[{};{}r", self.top + 1, self.bottom + 1);
        }
        // The cursor; in pending-wrap state, the last cell is written again to get there.
        let grid = self.grid();
        let (x, y) = (usize::from(self.x), usize::from(self.y));
        if self.pending_wrap {
            let row = &grid[y];
            let mut lx = cols - 1;
            if row.cells[lx].kind == TAIL && lx > 0 {
                lx -= 1;
            }
            let c = row.cells[lx];
            let _ = write!(s, "\x1b[{};{}H", y + 1, lx + 1);
            c.attrs.sgr(&mut s);
            let link = row.link_at(lx);
            if let Some(l) = link {
                l.open(&mut s);
            }
            s.push(if c.kind == TAIL { ' ' } else { c.ch });
            if let Some(m) = row.combining_at(lx) {
                s.push_str(m);
            }
            if link.is_some() {
                s.push_str(LINK_CLOSE);
            }
        } else {
            let _ = write!(s, "\x1b[{};{}H", y + 1, x + 1);
        }
        if self.modes.origin {
            // Origin mode homes the cursor, and positions are relative to the region from then
            // on. (A pending wrap is not reproduced together with origin mode.)
            let _ = write!(
                s,
                "\x1b[?6h\x1b[{};{}H",
                y - usize::from(self.top) + 1,
                x + 1
            );
        }
        for (i, set) in self.charsets.iter().enumerate() {
            if *set != Charset::Ascii {
                charset_seq(i, *set, &mut s);
            }
        }
        if self.shift == 1 {
            s.push('\x0e');
        }
        let m = &self.modes;
        if m.app_cursor {
            s.push_str("\x1b[?1h");
        }
        if m.reverse_video {
            s.push_str("\x1b[?5h");
        }
        if !m.autowrap {
            s.push_str("\x1b[?7l");
        }
        if m.insert {
            s.push_str("\x1b[4h");
        }
        if m.newline {
            s.push_str("\x1b[20h");
        }
        if m.app_keypad {
            s.push_str("\x1b=");
        }
        if m.mouse != 0 {
            let _ = write!(s, "\x1b[?{}h", m.mouse);
        }
        if m.mouse_encoding != 0 {
            let _ = write!(s, "\x1b[?{}h", m.mouse_encoding);
        }
        for (mode, on) in &m.other {
            if *on {
                let _ = write!(s, "\x1b[?{mode}h");
            }
        }
        if self.cursor_style != 0 {
            let _ = write!(s, "\x1b[{} q", self.cursor_style);
        }
        if !self.title.is_empty() {
            let _ = write!(s, "\x1b]0;{}\x07", self.title);
        }
        self.attrs.sgr(&mut s);
        if let Some(l) = &self.link {
            l.open(&mut s);
        }
        if m.cursor_visible {
            s.push_str("\x1b[?25h");
        }
        let mut out = s.into_bytes();
        out.extend_from_slice(&self.parser.raw);
        out
    }

    fn history_lines(&self, range: HistoryRange) -> Vec<&Line> {
        let h = &self.history;
        let len = h.lines.len();
        let skip = match range {
            HistoryRange::None => len,
            HistoryRange::Last(n) => len.saturating_sub(n),
            HistoryRange::Since(mark) => {
                let first = h.total - len as u64;
                usize::try_from(mark.saturating_sub(first))
                    .unwrap_or(len)
                    .min(len)
            }
        };
        h.lines.iter().skip(skip).collect()
    }

    /// Lines of the history and the active screen (tmux `capture-pane`): screen rows are
    /// numbered from 0, history lines from -1 (the newest) down. `vt` keeps the SGR sequences;
    /// `join` joins wrapped rows into one line.
    pub fn capture(&self, start: Pos, end: Pos, vt: bool, join: bool) -> Capture {
        let hist = i64::try_from(self.history.lines.len()).unwrap_or(i64::MAX);
        let rows = i64::from(self.rows);
        let start = match start {
            Pos::Edge => -hist,
            Pos::Line(n) => n.max(-hist),
        };
        let end = match end {
            Pos::Edge => rows - 1,
            Pos::Line(n) => n.min(rows - 1),
        };
        if start > end {
            return Capture {
                lines: Vec::new(),
                range: None,
            };
        }
        let grid = self.grid();
        let trim = |mut s: String| {
            s.truncate(s.trim_end_matches(' ').len());
            s
        };
        let mut lines = Vec::new();
        let mut acc: Option<String> = None;
        for n in start..=end {
            let (text, wrapped) = if n < 0 {
                let l = &self.history.lines[usize::try_from(hist + n).unwrap_or(0)];
                (if vt { l.vt.to_string() } else { l.text() }, l.wrapped)
            } else {
                let row = &grid[usize::try_from(n).unwrap_or(0)];
                let t = if vt {
                    let mut s = String::new();
                    row.encode(&mut s);
                    s
                } else {
                    row.text()
                };
                (t, row.wrapped)
            };
            if join && wrapped && n < end {
                // The row continues on the next one: its trailing blanks are part of the line.
                acc.get_or_insert_with(String::new).push_str(&text);
                continue;
            }
            let mut cur = acc.take().unwrap_or_default();
            cur.push_str(&text);
            lines.push(if vt { cur } else { trim(cur) });
        }
        Capture {
            lines,
            range: Some((start, end)),
        }
    }
}

/// Paints a grid on a cleared client screen, row by row, at absolute positions.
fn paint(grid: &[Row], s: &mut String) {
    s.push_str("\x1b[H\x1b[2J");
    for (y, row) in grid.iter().enumerate() {
        if row.used() == 0 {
            continue;
        }
        let _ = write!(s, "\x1b[{};1H", y + 1);
        row.encode(s);
    }
}

fn charset_seq(slot: usize, set: Charset, s: &mut String) {
    s.push('\x1b');
    s.push(if slot == 0 { '(' } else { ')' });
    s.push(match set {
        Charset::Ascii => 'B',
        Charset::DecGraphics => '0',
        Charset::Uk => 'A',
    });
}

fn blank_grid(cols: u16, rows: u16, attrs: Attrs) -> Vec<Row> {
    (0..rows)
        .map(|_| Row::new(usize::from(cols), attrs))
        .collect()
}

fn default_tabs(cols: u16) -> Vec<bool> {
    (0..usize::from(cols))
        .map(|i| i % 8 == 0 && i > 0)
        .collect()
}

/// Before column `x` is overwritten: the other half of a wide character there becomes blank.
fn fix_wide(row: &mut Row, x: usize) {
    let Some(c) = row.cells.get(x).copied() else {
        return;
    };
    match c.kind {
        TAIL if x > 0 => {
            let a = row.cells[x - 1].attrs;
            row.cells[x - 1] = Cell::blank(a);
            row.cells[x] = Cell::blank(c.attrs);
            // The character's marks and hyperlink went with it.
            row.drop_extras(x - 1, x);
        }
        WIDE if x + 1 < row.cells.len() => {
            let a = row.cells[x + 1].attrs;
            row.cells[x + 1] = Cell::blank(a);
            row.cells[x] = Cell::blank(c.attrs);
        }
        _ => {}
    }
}

/// Cuts or pads a row to `c` columns (no reflow).
fn fit_row(row: &mut Row, c: usize) {
    if row.cells.len() > c {
        row.cells.truncate(c);
        if row.cells[c - 1].kind == WIDE {
            row.cells[c - 1] = Cell::blank(row.cells[c - 1].attrs);
        }
        row.drop_extras(c, usize::MAX);
        row.wrapped = false;
    } else {
        row.cells.resize(c, Cell::blank(Attrs::default()));
    }
}

/// Moves combining marks and hyperlinks at or after column `from` by `by` columns, dropping
/// those leaving the row.
fn shift_extras(row: &mut Row, from: usize, by: isize, cols: usize) {
    shift_cols(&mut row.combining, from, by, cols);
    shift_cols(&mut row.links, from, by, cols);
}

fn shift_cols<T>(v: &mut Vec<(u16, T)>, from: usize, by: isize, cols: usize) {
    if v.is_empty() {
        return;
    }
    v.retain_mut(|(c, _)| {
        let i = usize::from(*c);
        if i < from {
            return true;
        }
        match i.checked_add_signed(by) {
            Some(n) if n < cols => {
                *c = u16::try_from(n).unwrap_or(0);
                true
            }
            _ => false,
        }
    });
}

fn clamp8(v: u16) -> u8 {
    u8::try_from(v.min(255)).unwrap_or(255)
}

/// The colour of a colon form `38:5:n` / `38:2:[colourspace]:r:g:b`.
fn sub_color(g: &[Option<u16>]) -> Option<Color> {
    match g.get(1).copied().flatten() {
        Some(5) => g
            .get(2)
            .copied()
            .flatten()
            .map(|n| Color::Indexed(clamp8(n))),
        Some(2) => {
            let rgb = if g.len() >= 6 { &g[3..6] } else { g.get(2..5)? };
            Some(Color::Rgb(
                clamp8(rgb[0].unwrap_or(0)),
                clamp8(rgb[1].unwrap_or(0)),
                clamp8(rgb[2].unwrap_or(0)),
            ))
        }
        _ => None,
    }
}

/// CSI parameters: groups separated by `;`, sub-parameters by `:`; empty = `None`.
fn parse_params(raw: &[u8]) -> Vec<Vec<Option<u16>>> {
    if raw.is_empty() {
        return Vec::new();
    }
    raw.split(|&b| b == b';')
        .map(|g| {
            g.split(|&b| b == b':')
                .map(|n| {
                    if n.is_empty() {
                        None
                    } else {
                        Some(n.iter().fold(0u16, |acc, d| {
                            acc.saturating_mul(10).saturating_add(u16::from(d - b'0'))
                        }))
                    }
                })
                .collect()
        })
        .collect()
}

/// DEC special graphics (`ESC ( 0`) for the characters it replaces.
fn dec_graphics(c: char) -> char {
    const TABLE: [char; 32] = [
        ' ', '◆', '▒', '␉', '␌', '␍', '␊', '°', '±', '␤', '␋', '┘', '┐', '┌', '└', '┼', '⎺', '⎻',
        '─', '⎼', '⎽', '├', '┤', '┴', '┬', '│', '≤', '≥', 'π', '≠', '£', '·',
    ];
    match u32::from(c) {
        n @ 0x5f..=0x7e => TABLE[(n - 0x5f) as usize],
        _ => c,
    }
}

/// Columns a character occupies: 0 for combining marks and zero-width characters, 2 for East
/// Asian wide and fullwidth characters (Hangul, CJK, fullwidth forms) and emoji, else 1. A
/// compact table of the ranges that matter for terminals (docs/SCROLLBACK.md, known limits).
pub fn char_width(c: char) -> u8 {
    const ZERO: &[(u32, u32)] = &[
        (0x0300, 0x036F),
        (0x0483, 0x0489),
        (0x0591, 0x05BD),
        (0x05BF, 0x05C7),
        (0x0610, 0x061A),
        (0x064B, 0x065F),
        (0x0670, 0x0670),
        (0x06D6, 0x06ED),
        (0x0E31, 0x0E31),
        (0x0E34, 0x0E3A),
        (0x0E47, 0x0E4E),
        (0x1160, 0x11FF),
        (0x1AB0, 0x1AFF),
        (0x1DC0, 0x1DFF),
        (0x200B, 0x200F),
        (0x202A, 0x202E),
        (0x2060, 0x2064),
        (0x20D0, 0x20FF),
        (0xD7B0, 0xD7FF),
        (0xFE00, 0xFE0F),
        (0xFE20, 0xFE2F),
        (0xFEFF, 0xFEFF),
        (0xE0100, 0xE01EF),
    ];
    const WIDE_RANGES: &[(u32, u32)] = &[
        (0x1100, 0x115F),
        (0x231A, 0x231B),
        (0x2329, 0x232A),
        (0x23E9, 0x23EC),
        (0x23F0, 0x23F0),
        (0x23F3, 0x23F3),
        (0x25FD, 0x25FE),
        (0x2614, 0x2615),
        (0x2648, 0x2653),
        (0x267F, 0x267F),
        (0x2693, 0x2693),
        (0x26A1, 0x26A1),
        (0x26AA, 0x26AB),
        (0x26BD, 0x26BE),
        (0x26C4, 0x26C5),
        (0x26CE, 0x26CE),
        (0x26D4, 0x26D4),
        (0x26EA, 0x26EA),
        (0x26F2, 0x26F3),
        (0x26F5, 0x26F5),
        (0x26FA, 0x26FA),
        (0x26FD, 0x26FD),
        (0x2705, 0x2705),
        (0x270A, 0x270B),
        (0x2728, 0x2728),
        (0x274C, 0x274C),
        (0x274E, 0x274E),
        (0x2753, 0x2755),
        (0x2757, 0x2757),
        (0x2795, 0x2797),
        (0x27B0, 0x27B0),
        (0x27BF, 0x27BF),
        (0x2B1B, 0x2B1C),
        (0x2B50, 0x2B50),
        (0x2B55, 0x2B55),
        (0x2E80, 0x303E),
        (0x3041, 0x33FF),
        (0x3400, 0x4DBF),
        (0x4E00, 0x9FFF),
        (0xA000, 0xA4CF),
        (0xA960, 0xA97F),
        (0xAC00, 0xD7A3),
        (0xF900, 0xFAFF),
        (0xFE10, 0xFE19),
        (0xFE30, 0xFE6F),
        (0xFF00, 0xFF60),
        (0xFFE0, 0xFFE6),
        (0x16FE0, 0x16FE4),
        (0x17000, 0x18CFF),
        (0x1B000, 0x1B2FF),
        (0x1F004, 0x1F004),
        (0x1F0CF, 0x1F0CF),
        (0x1F18E, 0x1F18E),
        (0x1F191, 0x1F19A),
        (0x1F200, 0x1F251),
        (0x1F300, 0x1F64F),
        (0x1F680, 0x1F6FF),
        (0x1F7E0, 0x1F7EB),
        (0x1F90C, 0x1F9FF),
        (0x1FA70, 0x1FAFF),
        (0x20000, 0x2FFFD),
        (0x30000, 0x3FFFD),
    ];
    let n = u32::from(c);
    if n < 0x300 {
        return 1;
    }
    let within = |t: &[(u32, u32)]| {
        t.binary_search_by(|&(lo, hi)| {
            if hi < n {
                std::cmp::Ordering::Less
            } else if lo > n {
                std::cmp::Ordering::Greater
            } else {
                std::cmp::Ordering::Equal
            }
        })
        .is_ok()
    };
    if within(ZERO) {
        0
    } else if within(WIDE_RANGES) {
        2
    } else {
        1
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn screen(cols: u16, rows: u16) -> Screen {
        Screen::new(cols, rows, Limits::default())
    }

    fn text(s: &Screen) -> Vec<String> {
        s.capture(Pos::Line(0), Pos::Edge, false, false).lines
    }

    fn history(s: &Screen) -> Vec<String> {
        s.capture(Pos::Edge, Pos::Line(-1), false, false).lines
    }

    /// Feeds `snap` into a fresh client terminal of the same size and compares everything a
    /// viewer sees: both screens' cells, the cursor, modes, region, title and history.
    fn replays_exactly(s: &Screen) {
        let snap = s.snapshot(HistoryRange::Last(usize::MAX), false);
        let mut c = Screen::new(s.cols, s.rows, s.history.limits);
        c.feed(&snap);
        assert_same(s, &c);
        assert_eq!(history(s), history(&c), "history");
    }

    /// Everything but the history (whose rows a client re-wraps at its width after a resize).
    fn assert_same(s: &Screen, c: &Screen) {
        type Cells = Vec<Vec<(char, Attrs, u8, Option<Link>)>>;
        let cells = |g: &Vec<Row>| -> Cells {
            g.iter()
                .map(|r| {
                    r.cells
                        .iter()
                        .enumerate()
                        .map(|(x, c)| {
                            let link = r.link_at(x).map(|l| (**l).clone());
                            (c.ch, c.attrs, c.kind, link)
                        })
                        .collect()
                })
                .collect()
        };
        assert_eq!(cells(&s.primary), cells(&c.primary), "primary screen");
        assert_eq!(
            s.alt.as_ref().map(cells),
            c.alt.as_ref().map(cells),
            "alternate screen"
        );
        assert_eq!(
            (s.x, s.y, s.pending_wrap),
            (c.x, c.y, c.pending_wrap),
            "cursor"
        );
        assert_eq!(s.attrs, c.attrs, "attributes");
        assert_eq!(s.modes, c.modes, "modes");
        assert_eq!((s.top, s.bottom), (c.top, c.bottom), "region");
        assert_eq!(s.title, c.title, "title");
        assert_eq!(s.tabs, c.tabs, "tabs");
        assert_eq!(s.charsets, c.charsets, "charsets");
        assert_eq!(s.shift, c.shift, "shift");
        assert_eq!(s.cursor_style, c.cursor_style, "cursor style");
        assert_eq!(s.link, c.link, "active hyperlink");
        assert_eq!(
            s.saved[usize::from(s.alt.is_some())].map(|v| (v.x, v.y, v.attrs)),
            c.saved[usize::from(c.alt.is_some())].map(|v| (v.x, v.y, v.attrs)),
            "saved cursor"
        );
    }

    #[test]
    fn prints_wraps_and_scrolls_into_history() {
        let mut s = screen(10, 3);
        s.feed(b"hello\r\nworld\r\n0123456789abc");
        assert_eq!(text(&s), ["world", "0123456789", "abc"]);
        assert_eq!(history(&s), ["hello"]);
        assert!(s.primary[1].wrapped, "the long line wrapped");
        assert_eq!((s.x, s.y), (3, 2));
        replays_exactly(&s);
    }

    #[test]
    fn conpty_style_repaint_does_not_scroll() {
        // ConPTY repaints with CUP home and `\r\n` between rows, never past the bottom.
        let mut s = screen(20, 4);
        s.feed(b"\x1b[2J\x1b[m\x1b[HMicrosoft\x1b]0;cmd\x07\x1b[?25h\r\n(c)\r\n\x1b[5X\r\nC:\\>\x1b[3X");
        assert_eq!(text(&s), ["Microsoft", "(c)", "", "C:\\>"]);
        s.feed(b"\x1b[25l\x1b[H\x1b[K\r\nC:\\>\x1b[K\r\n\x1b[K\r\n\x1b[K\x1b[2;5H\x1b[?25h");
        assert_eq!(text(&s), ["", "C:\\>", "", ""]);
        assert!(
            history(&s).is_empty(),
            "a repaint keeps the history as it was"
        );
        assert_eq!(s.title, "cmd");
        assert_eq!((s.x, s.y), (4, 1));
        replays_exactly(&s);
    }

    #[test]
    fn colors_and_attributes_survive_snapshot_and_capture() {
        let mut s = screen(30, 4);
        s.feed(b"\x1b[1m\x1b[91m\x1b[44mTOP\x1b[m plain \x1b[38;2;255;135;0morange\x1b[0m");
        s.feed(b"\r\n\x1b[38;5;208;48:2::1:2:3;4;7mx\x1b[22;24;27m y\x1b[3;9;53mz");
        let row = &s.primary[0];
        assert_eq!(row.cells[0].attrs.fg, Color::Indexed(9));
        assert_eq!(row.cells[0].attrs.bg, Color::Indexed(4));
        assert_eq!(row.cells[0].attrs.flags, BOLD);
        assert_eq!(row.cells[10].attrs.fg, Color::Rgb(255, 135, 0));
        let r1 = &s.primary[1];
        assert_eq!(r1.cells[0].attrs.fg, Color::Indexed(208));
        assert_eq!(r1.cells[0].attrs.bg, Color::Rgb(1, 2, 3));
        assert_eq!(r1.cells[0].attrs.flags, UNDERLINE | INVERSE);
        assert_eq!(r1.cells[2].attrs.flags, 0);
        assert_eq!(r1.cells[3].attrs.flags, ITALIC | STRIKE | OVERLINE);
        replays_exactly(&s);
        let vt = s.capture(Pos::Line(0), Pos::Line(0), true, false).lines;
        assert!(
            vt[0].starts_with("\x1b[0;1;91;44mTOP\x1b[0m plain "),
            "{vt:?}"
        );
        assert_eq!(text(&s)[0], "TOP plain orange");
    }

    #[test]
    fn full_screen_program_on_the_alternate_screen() {
        let mut s = screen(40, 10);
        s.feed(b"C:\\> tui\r\nshell output\r\n");
        // Enter the alternate screen, draw, set a region, modes and a hidden cursor.
        s.feed(b"\x1b[?1049h\x1b[2J\x1b[H\x1b[?25l\x1b[1;31;44mTOP-LEFT\x1b[0m");
        s.feed(b"\x1b[5;10H\x1b[38;5;208mORANGE\x1b[0m\x1b[6;8r\x1b[8;1Ha\r\nb\r\nc");
        s.feed(
            b"\x1b[r\x1b[10;1H\x1b[7mSTATUS\x1b[0m\x1b[?2004h\x1b[?1000h\x1b[?1006h\x1b[?1h\x1b=",
        );
        assert!(s.alternate());
        let t = text(&s);
        assert_eq!(t[0], "TOP-LEFT");
        assert_eq!(t[4], "         ORANGE");
        assert_eq!(&t[5..8], ["a", "b", "c"]);
        assert_eq!(t[9], "STATUS");
        assert!(
            history(&s).is_empty(),
            "the alternate screen keeps no history"
        );
        replays_exactly(&s);
        // Leaving restores the shell screen and the cursor saved by 1049.
        s.feed(b"\x1b[?1049l");
        assert!(!s.alternate());
        assert_eq!(&text(&s)[..2], ["C:\\> tui", "shell output"]);
        assert_eq!((s.x, s.y), (0, 2));
        replays_exactly(&s);
    }

    #[test]
    fn scroll_regions_index_and_reverse_index() {
        let mut s = screen(10, 6);
        for i in 0..6 {
            s.feed(format!("l{i}").as_bytes());
            if i < 5 {
                s.feed(b"\r\n");
            }
        }
        s.feed(b"\x1b[2;4r\x1b[4;1H\nX");
        assert_eq!(text(&s), ["l0", "l2", "l3", "X", "l4", "l5"]);
        assert!(
            history(&s).is_empty(),
            "a partial region scrolls nothing into history"
        );
        s.feed(b"\x1b[2;1H\x1bMY");
        assert_eq!(text(&s), ["l0", "Y", "l2", "l3", "l4", "l5"]);
        s.feed(b"\x1b[3;1H\x1b[2L");
        assert_eq!(text(&s), ["l0", "Y", "", "", "l4", "l5"]);
        s.feed(b"\x1b[2;1H\x1b[M");
        assert_eq!(text(&s), ["l0", "", "", "", "l4", "l5"]);
        s.feed(b"\x1b[?6h\x1b[2;3HO");
        assert_eq!(text(&s)[2], "  O");
        replays_exactly(&s);
    }

    #[test]
    fn erase_insert_delete_and_repeat() {
        let mut s = screen(10, 2);
        s.feed(b"abcdefghij\x1b[1;3H\x1b[2@XY");
        assert_eq!(text(&s)[0], "abXYcdefgh");
        s.feed(b"\x1b[1;1H\x1b[3P");
        assert_eq!(text(&s)[0], "Ycdefgh");
        s.feed(b"\x1b[1;2H\x1b[2X");
        assert_eq!(text(&s)[0], "Y  efgh");
        s.feed(b"\x1b[1;5H\x1b[1K");
        assert_eq!(text(&s)[0], "     gh");
        s.feed(b"\x1b[2;1H-\x1b[4b");
        assert_eq!(text(&s)[1], "-----");
        s.feed(b"\x1b[1;1H\x1b[J");
        assert_eq!(text(&s), ["", ""]);
        replays_exactly(&s);
    }

    #[test]
    fn wide_characters_and_combining_marks() {
        let mut s = screen(6, 3);
        s.feed("한글a".as_bytes());
        assert_eq!(s.x, 5);
        s.feed("가".as_bytes()); // does not fit in the last column: wraps
        assert_eq!(text(&s), ["한글a", "가", ""]);
        assert!(s.primary[0].wrapped);
        s.feed("e\u{301}".as_bytes());
        assert_eq!(text(&s)[1], "가e\u{301}");
        // Overwriting half of a wide character blanks the other half.
        s.feed(b"\x1b[1;2Hx");
        assert_eq!(text(&s)[0], " x글a");
        replays_exactly(&s);
        assert_eq!(char_width('한'), 2);
        assert_eq!(char_width('A'), 1);
        assert_eq!(char_width('\u{301}'), 0);
        assert_eq!(char_width('😀'), 2);
        assert_eq!(char_width('ｱ'), 1, "halfwidth katakana");
    }

    #[test]
    fn pending_wrap_is_reproduced() {
        let mut s = screen(5, 3);
        s.feed(b"abcde");
        assert!(s.pending_wrap);
        replays_exactly(&s);
        let snap = s.snapshot(HistoryRange::None, false);
        let mut c = Screen::new(5, 3, Limits::default());
        c.feed(&snap);
        // The next character goes to the next row on both.
        s.feed(b"f");
        c.feed(b"f");
        assert_eq!(text(&s), text(&c));
        assert_eq!(text(&c), ["abcde", "f", ""]);
    }

    #[test]
    fn snapshot_mid_sequence_continues_with_live_output() {
        for split in 1..12 {
            let full = "\x1b[1;31m한x\x1b]0;title\x07".as_bytes();
            let mut s = screen(20, 3);
            s.feed(b"before ");
            s.feed(&full[..split]);
            let snap = s.snapshot(HistoryRange::None, false);
            let mut c = Screen::new(20, 3, Limits::default());
            c.feed(&snap);
            c.feed(&full[split..]);
            s.feed(&full[split..]);
            assert_same(&s, &c);
        }
    }

    #[test]
    fn history_is_bounded_by_lines_and_bytes_and_evicts_the_oldest() {
        let mut s = Screen::new(
            20,
            2,
            Limits {
                lines: 5,
                bytes: HISTORY_BYTES,
            },
        );
        for i in 0..20 {
            s.feed(format!("line-{i}\r\n").as_bytes());
        }
        assert_eq!(
            history(&s),
            ["line-14", "line-15", "line-16", "line-17", "line-18"]
        );
        let h = s.history();
        assert_eq!((h.lines, h.evicted_lines, h.total_lines), (5, 14, 19));
        // By bytes: each line costs its text plus LINE_OVERHEAD.
        let mut b = Screen::new(
            20,
            2,
            Limits {
                lines: 1000,
                bytes: 3 * (7 + LINE_OVERHEAD),
            },
        );
        for i in 0..10 {
            b.feed(format!("line-{i:02}\r\n").as_bytes());
        }
        assert_eq!(history(&b), ["line-06", "line-07", "line-08"]);
        assert_eq!(b.history().bytes, 3 * (7 + LINE_OVERHEAD));
        // Lowering the limit evicts at once; clearing removes all.
        b.set_limits(Limits {
            lines: 1,
            bytes: HISTORY_BYTES,
        });
        assert_eq!(history(&b), ["line-08"]);
        assert_eq!(b.clear_history(), 1);
        assert!(history(&b).is_empty());
        assert_eq!(b.history().cleared_lines, 1);
        // A limit of zero keeps nothing.
        let mut z = Screen::new(
            10,
            2,
            Limits {
                lines: 0,
                bytes: HISTORY_BYTES,
            },
        );
        z.feed(b"a\r\nb\r\nc\r\n");
        assert!(history(&z).is_empty());
        assert_eq!(z.history().evicted_lines, 2);
    }

    #[test]
    fn snapshot_history_puts_lines_in_the_client_scrollback_in_order() {
        let mut s = screen(10, 4);
        for i in 0..30 {
            s.feed(format!("{i}\r\n").as_bytes());
        }
        s.feed(b"\x1b[31mred\x1b[m");
        let mut c = Screen::new(10, 4, Limits::default());
        c.feed(&s.snapshot(HistoryRange::Last(10), false));
        assert_eq!(text(&c), text(&s));
        assert_eq!(
            history(&c),
            (17..27).map(|i| i.to_string()).collect::<Vec<_>>()
        );
        // A few lines only, and lines longer than the screen is wide.
        let mut s = screen(10, 6);
        s.feed(b"0123456789abcdefghijklmnopqrstuvwxyz\r\nshort\r\n\r\n\r\n\r\n\r\n\r\n\r\n\r\nend");
        let mut c = Screen::new(10, 6, Limits::default());
        c.feed(&s.snapshot(HistoryRange::Last(usize::MAX), false));
        assert_eq!(text(&c), text(&s));
        assert_eq!(history(&c), history(&s));
        // `Since` carries only what entered the history after a mark.
        let mark = s.history().total_lines;
        s.feed(b"\r\nnew-1\r\nnew-2\r\n\r\n\r\n\r\n\r\n\r\n");
        let mut c = Screen::new(10, 6, Limits::default());
        c.feed(b"\x1b[31mgarbage the client showed before it lagged\x1b[1;");
        c.feed(&s.snapshot(HistoryRange::Since(mark), true));
        let all = history(&s);
        let n = usize::try_from(s.history().total_lines - mark).unwrap();
        assert!(all[all.len() - n..].contains(&"new-2".to_string()));
        assert!(
            history(&c).ends_with(&all[all.len() - n..]),
            "{:?}",
            history(&c)
        );
        assert_eq!(text(&c), text(&s));
    }

    #[test]
    fn capture_ranges_join_and_edges() {
        let mut s = screen(5, 3);
        s.feed(b"h1\r\nh2\r\nabcdefgh\r\nlast");
        assert_eq!(history(&s), ["h1", "h2"]);
        let all = s.capture(Pos::Edge, Pos::Edge, false, false);
        assert_eq!(all.lines, ["h1", "h2", "abcde", "fgh", "last"]);
        assert_eq!(all.range, Some((-2, 2)));
        let joined = s.capture(Pos::Edge, Pos::Edge, false, true);
        assert_eq!(joined.lines, ["h1", "h2", "abcdefgh", "last"]);
        assert_eq!(
            s.capture(Pos::Line(-1), Pos::Line(0), false, false).lines,
            ["h2", "abcde"]
        );
        assert_eq!(
            s.capture(Pos::Line(-100), Pos::Line(-2), false, false)
                .lines,
            ["h1"]
        );
        let empty = s.capture(Pos::Line(2), Pos::Line(1), false, false);
        assert!(empty.lines.is_empty() && empty.range.is_none());
    }

    #[test]
    fn resize_cuts_pads_and_keeps_rows_above_the_cursor_in_history() {
        let mut s = screen(10, 4);
        s.feed(b"a\r\nb\r\nc\r\nd");
        s.resize(5, 2);
        assert_eq!(text(&s), ["c", "d"]);
        assert_eq!(history(&s), ["a", "b"]);
        assert_eq!((s.x, s.y), (1, 1));
        s.resize(8, 5);
        assert_eq!(text(&s), ["c", "d", "", "", ""]);
        assert_eq!(s.size(), (8, 5));
        replays_exactly(&s);
        // On the alternate screen nothing enters the history.
        s.feed(b"\x1b[?1049h\x1b[5;1Hbottom");
        s.resize(8, 2);
        assert_eq!(history(&s), ["a", "b"]);
        assert_eq!(text(&s), ["", "bottom"]);
        replays_exactly(&s);
    }

    #[test]
    fn tabs_charsets_saved_cursor_cursor_style_and_resets() {
        let mut s = screen(30, 3);
        s.feed(b"a\tb\x1b[3g\x1b[1;5H\x1bH\x1b[1;1H\tc");
        assert_eq!(text(&s)[0], "a   c   b");
        s.feed(b"\x1b(0lqk\x1b(B\x1b)0\x0eq\x0f");
        assert!(text(&s)[0].ends_with("┌─┐─"), "{:?}", text(&s));
        s.feed(b"\x1b[2;3H\x1b[33m\x1b7\x1b[0m\x1b[3;1H\x1b[4 q\x1b)0\x0e");
        replays_exactly(&s);
        s.feed(b"\x1b8x");
        assert_eq!(s.primary[1].cells[2].attrs.fg, Color::Indexed(3));
        s.feed(b"\x1b[!p");
        assert_eq!(s.attrs, Attrs::default());
        s.feed(b"\x1bc");
        assert_eq!(text(&s), ["", "", ""]);
        replays_exactly(&s);
    }

    #[test]
    fn strings_and_unknown_sequences_are_consumed_safely() {
        let mut s = screen(20, 2);
        s.feed(
            b"\x1bP1$r0m\x1b\\a\x1b]52;c;c2VjcmV0\x07b\x1b_apc\x1b\\c\x1b[>4;1md\x1b[?u\x1b[6ne",
        );
        s.feed(b"\x1b]8;;http://x\x1b\\f\x1b]8;;\x1b\\");
        assert_eq!(text(&s)[0], "abcdef");
        let snap = String::from_utf8_lossy(&s.snapshot(HistoryRange::None, false)).to_string();
        assert!(
            !snap.contains("52;"),
            "a clipboard request is never replayed"
        );
        // Invalid UTF-8 and bare continuation bytes become U+FFFD.
        s.feed(b"\r\n\xff\x80\xe2\x82x");
        assert_eq!(text(&s)[1], "\u{fffd}\u{fffd}\u{fffd}x");
        // Fuzz: never panics.
        let mut seed = 0x1234_5678_u64;
        let mut f = screen(7, 3);
        for _ in 0..200_000 {
            seed ^= seed << 13;
            seed ^= seed >> 7;
            seed ^= seed << 17;
            let b = [
                0x1b, b'[', b'?', b';', b'1', b'4', b'9', b'h', b'm', b'r', b'J', b'K', b'\n',
                b'\r', 0xe2, 0x80, b'L', b'M', b'@', b'P', b'X', b'\t', 0x08, b'7', b'8', b'H',
                b'0', b'(', b']', 0x07, b'b', b'S', b'T', 0xea, 0xb0, 0x80,
            ];
            f.feed(&[b[(seed % b.len() as u64) as usize]]);
            if seed.is_multiple_of(997) {
                f.resize(
                    u16::try_from(seed % 9 + 1).unwrap(),
                    u16::try_from(seed % 5 + 1).unwrap(),
                );
            }
            if seed.is_multiple_of(4999) {
                let mut c = Screen::new(f.cols, f.rows, f.history.limits);
                c.feed(&f.snapshot(HistoryRange::Last(usize::MAX), false));
                assert_same(&f, &c);
            }
        }
        let mut c = Screen::new(f.cols, f.rows, f.history.limits);
        c.feed(&f.snapshot(HistoryRange::Last(usize::MAX), false));
        assert_same(&f, &c);
    }

    #[test]
    fn feeding_is_fast_enough_for_conpty_output() {
        // ConPTY delivers ~1 MiB/s on Windows 10; the model must cost a small fraction of that.
        let mut s = screen(120, 30);
        let mut chunk = Vec::new();
        for i in 0..2000 {
            chunk.extend_from_slice(
                format!("\x1b[32mline {i:05}\x1b[m 0123456789 abcdefghij 한글 ABCDEFGHIJ\r\n")
                    .as_bytes(),
            );
        }
        let t = std::time::Instant::now();
        let mut fed = 0;
        while fed < 8 * 1024 * 1024 {
            s.feed(&chunk);
            fed += chunk.len();
        }
        let secs = t.elapsed().as_secs_f64();
        let rate = fed as f64 / secs / 1_048_576.0;
        eprintln!(
            "EVIDENCE screen: fed {fed} bytes in {secs:.3} s ({rate:.1} MiB/s), memory {} bytes",
            s.memory_bytes()
        );
        // Even unoptimized, on a slow CI runner (~3 MiB/s measured), the model must keep up
        // with what ConPTY delivers (~1 MiB/s); release builds run at ~68 MiB/s.
        assert!(rate > 1.0, "{rate:.1} MiB/s");
        assert!(s.memory_bytes() < HISTORY_BYTES + 512 * 1024);
    }
}
