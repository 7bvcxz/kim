//! The tab bar (Step 17, DESIGN_SYSTEM §3.5, docs/SUBAK_GUI.md §2): one tab per Terminal of the
//! Host that this window shows, in order, with its state, its name, a close `×` that **hides**
//! the tab (a detach; the Terminal keeps running, ADR 0020 §7) and a `+` that creates a Terminal.
//!
//! The bar is one row of cells drawn by the window's own renderer (the terminal font chain, so a
//! Korean name looks as it does in the pane), from VT text built here. This module is pure: it
//! lays the tabs out in columns, scrolls them when they do not fit (the active tab always stays
//! visible, `‹`/`›` say that more tabs are hidden on that side), and returns the clickable spans,
//! so the layout is unit-tested without a window. State is never colour-only (DESIGN_SYSTEM
//! §3.8): every state has its own shape, the active tab is bold with its own fill (and a lilac
//! marker drawn by the window), and High Contrast turns fills into reverse video.

use super::screens::tokens::{
    ACCENT, BG_BASE, BG_OVERLAY, BORDER_SUBTLE, DANGER, SUCCESS, TEXT_MUTED, TEXT_PRIMARY,
    TEXT_SECONDARY, WARNING,
};
use subak_term::render::Rgb;

/// A tab's Terminal state, as the bar shows it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Status {
    Running,
    Starting,
    Stopping,
    /// Kept by the Host without a process (`stopped`).
    Stopped,
    /// Kept after its start failed.
    Failed,
    /// No longer in the Host (exited or removed); only the shown tab stays, with its last screen.
    Gone,
    /// Not known while the window is disconnected from the Host.
    Unknown,
}

impl Status {
    /// The bar's status from a Host state (`terminal.list`, events).
    pub fn from_state(state: &str) -> Self {
        match state {
            "running" => Self::Running,
            "starting" => Self::Starting,
            "stopping" => Self::Stopping,
            "stopped" => Self::Stopped,
            "failed" => Self::Failed,
            "removed" | "exited" | "gone" => Self::Gone,
            _ => Self::Unknown,
        }
    }

    /// The shape (DESIGN_SYSTEM §3.1: shape and colour, never colour alone).
    pub fn glyph(self) -> char {
        match self {
            Self::Running => '●',
            Self::Starting | Self::Stopping => '◐',
            Self::Stopped => '■',
            Self::Failed => '▲',
            Self::Gone | Self::Unknown => '○',
        }
    }

    /// The state in words (test reports, the context menu).
    pub fn word(self) -> &'static str {
        match self {
            Self::Running => "running",
            Self::Starting => "starting",
            Self::Stopping => "stopping",
            Self::Stopped => "stopped",
            Self::Failed => "failed",
            Self::Gone => "gone",
            Self::Unknown => "unknown",
        }
    }

    /// The state's colour (always beside the shape).
    pub fn color(self) -> Rgb {
        match self {
            Self::Running => SUCCESS,
            Self::Starting | Self::Stopping => WARNING,
            Self::Failed => DANGER,
            Self::Stopped | Self::Gone | Self::Unknown => TEXT_MUTED,
        }
    }
}

/// One tab to lay out.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Item<'a> {
    pub name: &'a str,
    pub status: Status,
}

/// What a click on a column of the bar means.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Hit {
    /// The tab: select it (double click: rename; middle click: hide; right click: its menu).
    Tab(usize),
    /// The tab's `×`: hide the tab (a detach; the Terminal keeps running).
    Close(usize),
    /// `+`: a new Terminal.
    New,
    /// `‹` / `›`: scroll the tabs.
    Left,
    Right,
}

/// The rename field shown in place of the active tab's name.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Edit<'a> {
    pub text: &'a str,
    /// The caret, in characters from the start of `text`.
    pub cursor: usize,
}

/// A laid-out bar.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Bar {
    /// VT text for a one-row view of the bar's width.
    pub vt: Vec<u8>,
    /// Clickable column ranges `[start, end)`.
    pub spans: Vec<(u16, u16, Hit)>,
    /// The active tab's columns (the window fills it to the pane and draws the marker above it).
    pub active: Option<(u16, u16)>,
    /// The rename caret's column, while renaming.
    pub caret: Option<u16>,
    /// The first visible tab.
    pub first: usize,
}

impl Bar {
    /// What a click at `col` means.
    pub fn hit(&self, col: u16) -> Option<Hit> {
        self.spans
            .iter()
            .find(|(a, b, _)| (*a..*b).contains(&col))
            .map(|s| s.2)
    }
}

/// Tab widths in cells (DESIGN_SYSTEM §2.4: 96–220 px at the UI size).
pub const TAB_MIN: usize = 12;
pub const TAB_MAX: usize = 28;
/// The rename field's widest extent.
const EDIT_MAX: usize = 40;

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

fn fg(c: Rgb) -> String {
    format!("\x1b[38;2;{};{};{}m", c.0, c.1, c.2)
}

fn bg(c: Rgb) -> String {
    format!("\x1b[48;2;{};{};{}m", c.0, c.1, c.2)
}

const RESET: &str = "\x1b[0m";

/// The rename field: the visible part of `text` around the caret, the caret's cell offset, and
/// the field's width in cells.
fn edit_window(e: &Edit<'_>) -> (String, usize, usize) {
    let chars: Vec<char> = e.text.chars().collect();
    let cursor = e.cursor.min(chars.len());
    let cw = |c: &char| usize::from(subak_term::screen::char_width(*c));
    let total: usize = chars.iter().map(cw).sum::<usize>() + 1;
    let field = total.clamp(8, EDIT_MAX);
    // Show the text so that the caret (one cell after `cursor` chars) fits in `field` cells.
    let mut start = 0;
    while chars[start..cursor].iter().map(cw).sum::<usize>() + 1 > field {
        start += 1;
    }
    let mut shown = String::new();
    let mut w = 0;
    for c in &chars[start..] {
        if w + cw(c) > field {
            break;
        }
        shown.push(*c);
        w += cw(c);
    }
    let caret = chars[start..cursor].iter().map(cw).sum::<usize>();
    (shown, caret, field)
}

/// The width in cells of tab `i`.
fn tab_width(it: &Item<'_>, edit: Option<&Edit<'_>>) -> usize {
    match edit {
        Some(e) => edit_window(e).2 + 6,
        None => (width(it.name) + 6).clamp(TAB_MIN, TAB_MAX),
    }
}

/// Lays out `items` on a bar of `cols` cells. `active` is the shown tab, `edit` the rename field
/// (of the active tab), `first` the first visible tab of the last layout (kept while the active
/// tab stays visible).
pub fn layout(
    items: &[Item<'_>],
    active: Option<usize>,
    edit: Option<&Edit<'_>>,
    cols: u16,
    first: usize,
) -> Bar {
    let cols = usize::from(cols);
    let widths: Vec<usize> = items
        .iter()
        .enumerate()
        .map(|(i, it)| tab_width(it, edit.filter(|_| Some(i) == active)))
        .collect();
    const PLUS: usize = 3;
    const ARROW: usize = 2;
    let total: usize = widths.iter().sum::<usize>() + PLUS;
    // Which tabs are visible.
    let (first, last) = if total <= cols || items.is_empty() {
        (0, items.len())
    } else {
        let avail = cols.saturating_sub(PLUS + 2 * ARROW);
        let mut first = first.min(items.len() - 1);
        if let Some(a) = active {
            first = first.min(a);
            while first < a && widths[first..=a].iter().sum::<usize>() > avail {
                first += 1;
            }
        }
        let mut last = first;
        let mut used = 0;
        while last < items.len() && used + widths[last] <= avail.max(widths[first]) {
            used += widths[last];
            last += 1;
        }
        (first, last.max(first + 1))
    };
    let mut vt = String::from("\x1b[?25l\x1b[2J\x1b[1;1H");
    let mut spans = Vec::new();
    let mut col = 0usize;
    let mut active_cols = None;
    let mut caret = None;
    let put = |vt: &mut String, col: &mut usize, text: &str| {
        vt.push_str(text);
        *col += width(&strip(text));
    };
    if first > 0 {
        let at = col;
        put(
            &mut vt,
            &mut col,
            &format!("{}‹ {RESET}", fg(TEXT_SECONDARY)),
        );
        spans.push((cell(at), cell(col), Hit::Left));
    }
    for i in first..last {
        let it = &items[i];
        let w = widths[i];
        let start = col;
        let is_active = Some(i) == active;
        let fill = if is_active {
            bg(BG_BASE)
        } else {
            String::new()
        };
        // A hairline between two inactive tabs.
        let lead = if !is_active && i > first && Some(i - 1) != active {
            format!("{}│", fg(BORDER_SUBTLE))
        } else {
            " ".to_string()
        };
        put(&mut vt, &mut col, &format!("{fill}{lead}"));
        put(
            &mut vt,
            &mut col,
            &format!("{fill}{}{} ", fg(it.status.color()), it.status.glyph()),
        );
        match edit.filter(|_| is_active) {
            Some(e) => {
                let (shown, at, field) = edit_window(e);
                caret = Some(cell(col + at));
                let mut s = format!("{}{}\x1b[4m", bg(BG_OVERLAY), fg(TEXT_PRIMARY));
                let mut w = 0;
                for (k, c) in shown.chars().enumerate() {
                    let before: usize = shown.chars().take(k).map(|c| width(&c.to_string())).sum();
                    if before == at {
                        s.push_str(&format!("\x1b[7m{c}\x1b[27m"));
                    } else {
                        s.push(c);
                    }
                    w += width(&c.to_string());
                }
                if at >= w {
                    s.push_str("\x1b[7m \x1b[27m");
                    w += 1;
                }
                s.push_str(&" ".repeat(field.saturating_sub(w)));
                s.push_str(RESET);
                put(&mut vt, &mut col, &s);
            }
            None => {
                let room = w - 6;
                let name = fit(it.name, room);
                let pad = " ".repeat(room.saturating_sub(width(&name)));
                let style = if is_active {
                    format!("\x1b[1m{}", fg(TEXT_PRIMARY))
                } else {
                    fg(TEXT_SECONDARY)
                };
                put(
                    &mut vt,
                    &mut col,
                    &format!("{fill}{style}{name}{pad}{RESET}"),
                );
            }
        }
        let close_at = col;
        put(
            &mut vt,
            &mut col,
            &format!("{fill}{} × {RESET}", fg(TEXT_MUTED)),
        );
        spans.push((cell(start), cell(close_at), Hit::Tab(i)));
        spans.push((cell(close_at), cell(col), Hit::Close(i)));
        if is_active {
            active_cols = Some((cell(start), cell(col)));
        }
    }
    if last < items.len() {
        let at = col;
        put(
            &mut vt,
            &mut col,
            &format!("{} ›{RESET}", fg(TEXT_SECONDARY)),
        );
        spans.push((cell(at), cell(col), Hit::Right));
    }
    if col + PLUS <= cols.max(PLUS) {
        let at = col;
        put(&mut vt, &mut col, &format!("{} + {RESET}", fg(ACCENT)));
        spans.push((cell(at), cell(col), Hit::New));
    }
    Bar {
        vt: vt.into_bytes(),
        spans,
        active: active_cols,
        caret,
        first,
    }
}

fn cell(n: usize) -> u16 {
    u16::try_from(n).unwrap_or(u16::MAX)
}

/// The printable text of a VT fragment (escape sequences removed), to count its cells.
fn strip(s: &str) -> String {
    let mut out = String::new();
    let mut it = s.chars().peekable();
    while let Some(c) = it.next() {
        if c == '\x1b' {
            if it.peek() == Some(&'[') {
                it.next();
                for d in it.by_ref() {
                    if d.is_ascii_alphabetic() {
                        break;
                    }
                }
            }
        } else {
            out.push(c);
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use subak_term::TermView;

    fn text(bar: &Bar, cols: u16) -> String {
        let mut v = TermView::new(cols, 1, 0);
        v.feed(&bar.vt);
        v.row_at(0).text().trim_end().to_string()
    }

    fn items<'a>(names: &[&'a str]) -> Vec<Item<'a>> {
        names
            .iter()
            .map(|n| Item {
                name: n,
                status: Status::Running,
            })
            .collect()
    }

    /// The cells of `[a, b)` as text.
    fn cells(line: &str, a: u16, b: u16) -> String {
        let mut out = String::new();
        let mut col = 0u16;
        for c in line.chars() {
            if col >= a && col < b {
                out.push(c);
            }
            col += u16::from(subak_term::screen::char_width(c));
        }
        out
    }

    #[test]
    fn no_tabs_is_just_the_new_button() {
        let bar = layout(&[], None, None, 80, 0);
        assert_eq!(text(&bar, 80).trim(), "+");
        assert_eq!(bar.spans.len(), 1);
        assert_eq!(bar.hit(1), Some(Hit::New));
        assert_eq!(bar.active, None);
    }

    #[test]
    fn one_and_many_tabs_show_state_name_close_and_plus() {
        let mut list = items(&["빌드", "claude", "server"]);
        list[2].status = Status::Stopped;
        let bar = layout(&list, Some(1), None, 120, 0);
        let line = text(&bar, 120);
        assert!(line.contains("● 빌드"), "{line}");
        assert!(line.contains("● claude"), "{line}");
        assert!(line.contains("■ server"), "{line}");
        assert_eq!(line.matches('×').count(), 3, "{line}");
        assert!(line.ends_with('+'), "{line}");
        // The spans tile the bar from column 0 and name what is under them.
        let mut at = 0;
        for (a, b, hit) in &bar.spans {
            assert_eq!(*a, at, "{:?}", bar.spans);
            at = *b;
            let under = cells(&line, *a, *b);
            match hit {
                Hit::Tab(i) => assert!(under.contains(list[*i].name), "{under}"),
                Hit::Close(_) => assert_eq!(under.trim(), "×"),
                Hit::New => assert_eq!(under.trim(), "+"),
                Hit::Left | Hit::Right => panic!("no scrolling"),
            }
        }
        let (a, b) = bar.active.unwrap();
        assert!(cells(&line, a, b).contains("claude"));
        assert!((TAB_MIN..=TAB_MAX).contains(&usize::from(b - a)));
        // Every state has its own shape.
        let shapes: std::collections::HashSet<char> = [
            Status::Running,
            Status::Starting,
            Status::Stopped,
            Status::Failed,
            Status::Gone,
        ]
        .iter()
        .map(|s| s.glyph())
        .collect();
        assert_eq!(shapes.len(), 5);
        assert_eq!(Status::from_state("stopped"), Status::Stopped);
        assert_eq!(Status::from_state("bogus"), Status::Unknown);
    }

    #[test]
    fn long_names_are_cut_and_too_many_tabs_scroll_with_the_active_one_visible() {
        let long = "a-very-long-terminal-name-that-does-not-fit-in-a-tab";
        let bar = layout(&items(&[long]), Some(0), None, 120, 0);
        let line = text(&bar, 120);
        assert!(line.contains('…'), "{line}");
        let names: Vec<String> = (0..30).map(|i| format!("term-{i:02}")).collect();
        let refs: Vec<&str> = names.iter().map(String::as_str).collect();
        let list = items(&refs);
        for active in [0usize, 7, 15, 29] {
            let bar = layout(&list, Some(active), None, 80, 0);
            let line = text(&bar, 80);
            assert!(
                line.contains(&format!("term-{active:02}")),
                "active {active}: {line}"
            );
            assert!(width(&line) <= 80, "{line}");
            assert_eq!(bar.first > 0, line.starts_with('‹'), "{line}");
            let more = bar.spans.iter().any(|s| s.2 == Hit::Right);
            assert_eq!(more, active < 29, "{line}");
        }
        // The previous first tab is kept while the active one stays visible.
        let bar = layout(&list, Some(8), None, 80, 5);
        assert_eq!(bar.first, 5);
        // A very narrow bar still shows the active tab.
        let bar = layout(&list, Some(12), None, 20, 0);
        assert!(text(&bar, 20).contains("term-1"), "{}", text(&bar, 20));
    }

    #[test]
    fn the_rename_field_replaces_the_active_name_with_a_caret() {
        let list = items(&["one", "two"]);
        let e = Edit {
            text: "새이름",
            cursor: 1,
        };
        let bar = layout(&list, Some(1), Some(&e), 80, 0);
        let line = text(&bar, 80);
        assert!(line.contains("새이름"), "{line}");
        assert!(!line.contains("two"), "{line}");
        let (a, _) = bar.active.unwrap();
        // " ● " then the field: the caret is after the first (wide) syllable.
        assert_eq!(bar.caret, Some(a + 3 + 2));
        // The caret at the end sits after the text.
        let e = Edit {
            text: "ab",
            cursor: 2,
        };
        let bar = layout(&list, Some(0), Some(&e), 80, 0);
        let (a, _) = bar.active.unwrap();
        assert_eq!(bar.caret, Some(a + 3 + 2));
        // A long text scrolls so the caret stays in the field.
        let long = "x".repeat(100);
        let e = Edit {
            text: &long,
            cursor: 100,
        };
        let bar = layout(&list, Some(0), Some(&e), 120, 0);
        let (a, b) = bar.active.unwrap();
        let caret = bar.caret.unwrap();
        assert!(caret > a && caret < b, "{a} {caret} {b}");
    }
}
