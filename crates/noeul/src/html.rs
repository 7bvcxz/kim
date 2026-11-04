//! Noeul's own HTML sanitizer for clipboard HTML (Step 55; NOEUL-FR-071, NOEUL-SEC-009,
//! NOEUL-NFR-008; ADR 0047).
//!
//! Clipboard HTML is untrusted. This module never runs, loads or fetches anything: it reads the
//! `HTML Format` (CF_HTML) fragment with a small tokenizer of its own and keeps **text and basic
//! formatting only**, as a card [`Doc`]:
//!
//! - **Kept:** paragraphs and line breaks, headings, lists (bullets and numbers), quotations,
//!   preformatted text, rules, table rows (cells side by side), bold, italic, underline,
//!   strikethrough, monospace, highlights, links as underlined text (their targets are dropped,
//!   never followed), and from the `style` attribute only `color`, `background-color` (a plain
//!   colour), `font-weight`, `font-style`, `text-decoration`, a monospace `font-family` and
//!   `white-space: pre`: enough for an editor's syntax highlighting.
//! - **Dropped with their content:** `script`, `style`, `head`, `title`, `template`, `noscript`,
//!   `iframe`, `object`, `embed`, `applet`, `svg`, `math`, `canvas`, `video`, `audio`, `select`,
//!   form inputs, comments, `<!…>` and `<?…>`.
//! - **Images** become a placeholder ("[image: alt]"); nothing is requested, local or remote.
//! - **Every other CSS property is ignored** (`display: none`, `position`, `font-size`,
//!   `opacity`, `url(…)`, `expression(…)`, …), so text cannot be hidden or moved away; colours
//!   that would hide text are replaced when the card is drawn (`crate::card::legible`).
//! - **Bounds:** at most [`MAX_DEPTH`] open elements are tracked (deeper ones keep their text,
//!   without their style), at most [`crate::card::MAX_CHARS`] characters and [`MAX_BLOCKS`]
//!   blocks are kept; the input is already capped at 4 MiB by the clipboard read. The tokenizer is
//!   one linear pass without recursion, so hostile nesting costs nothing more.

use crate::card::{Block, BlockKind, Doc, MAX_CHARS, Rgb, Run, Size, Style, clean};

/// The most open elements whose style is tracked.
pub const MAX_DEPTH: usize = 128;
/// The most blocks a card keeps.
pub const MAX_BLOCKS: usize = 2_000;

/// The HTML fragment of CF_HTML bytes: between `StartFragment` and `EndFragment` (byte offsets
/// in the header), else between the `<!--StartFragment-->` comments, else everything after the
/// header.
pub fn fragment(cf_html: &[u8]) -> String {
    let text = String::from_utf8_lossy(cf_html);
    let header_end = text.find('<').unwrap_or(text.len());
    let header = &text[..header_end];
    let offset = |key: &str| -> Option<usize> {
        header
            .lines()
            .find_map(|l| l.trim().strip_prefix(key)?.strip_prefix(':'))
            .and_then(|v| v.trim().parse::<i64>().ok())
            .and_then(|v| usize::try_from(v).ok())
    };
    if let (Some(s), Some(e)) = (offset("StartFragment"), offset("EndFragment"))
        && s <= e
        && e <= cf_html.len()
        && s >= header_end
    {
        return String::from_utf8_lossy(&cf_html[s..e]).into_owned();
    }
    let lower = text.to_ascii_lowercase();
    if let (Some(s), Some(e)) = (
        lower.find("<!--startfragment-->"),
        lower.find("<!--endfragment-->"),
    ) && s < e
    {
        return text[s + "<!--StartFragment-->".len()..e].to_string();
    }
    text[header_end..].to_string()
}

/// How a tag is treated.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Class {
    /// Dropped with everything inside.
    Drop,
    /// No content (`br`, `img`, `hr`, …).
    Void,
    Block,
    Inline,
}

fn class(name: &str) -> Class {
    match name {
        "script" | "style" | "head" | "title" | "template" | "noscript" | "iframe" | "object"
        | "embed" | "applet" | "svg" | "math" | "canvas" | "video" | "audio" | "select"
        | "textarea" | "frameset" | "noembed" | "noframes" | "xmp" | "plaintext" | "button"
        | "datalist" | "dialog" | "map" | "picture" => Class::Drop,
        "br" | "img" | "hr" | "input" | "meta" | "link" | "base" | "area" | "col" | "param"
        | "source" | "track" | "wbr" | "keygen" => Class::Void,
        "p" | "div" | "h1" | "h2" | "h3" | "h4" | "h5" | "h6" | "ul" | "ol" | "li" | "dl"
        | "dt" | "dd" | "pre" | "blockquote" | "table" | "thead" | "tbody" | "tfoot" | "tr"
        | "caption" | "section" | "article" | "header" | "footer" | "main" | "nav" | "aside"
        | "figure" | "figcaption" | "address" | "details" | "summary" | "form" | "fieldset"
        | "legend" | "center" | "body" | "html" | "listing" | "menu" => Class::Block,
        _ => Class::Inline,
    }
}

/// One open element.
#[derive(Debug, Clone)]
struct Open {
    name: String,
    style: Style,
    /// Whitespace kept inside (`pre`, `white-space: pre`).
    pre: bool,
    indent: u8,
    quote: bool,
    /// Lists: ordered, and the next number.
    list: Option<(bool, u32)>,
}

/// A start or end tag, or text.
#[derive(Debug)]
enum Token {
    Start {
        name: String,
        attrs: Vec<(String, String)>,
    },
    End(String),
    Text(String),
}

/// The tokenizer: one pass, no recursion; comments, doctypes and processing instructions are
/// skipped; the content of dropped elements is skipped up to their end tag.
struct Tokens<'a> {
    s: &'a str,
    at: usize,
}

impl Tokens<'_> {
    fn skip_to(&mut self, end: &str) {
        let rest = &self.s[self.at..];
        match find_ci(rest, end) {
            Some(i) => self.at += i + end.len(),
            None => self.at = self.s.len(),
        }
    }

    /// Skips the content of a dropped element up to its end tag (`</name`), or to the end.
    fn skip_element(&mut self, name: &str) {
        let close = format!("</{name}");
        let rest = &self.s[self.at..];
        match find_ci(rest, &close) {
            Some(i) => {
                self.at += i;
                // The end tag itself is read normally (and ignored).
            }
            None => self.at = self.s.len(),
        }
    }

    fn next(&mut self) -> Option<Token> {
        let s = self.s;
        while self.at < s.len() {
            let rest = &s[self.at..];
            if let Some(after) = rest.strip_prefix('<') {
                if after.starts_with("!--") {
                    self.at += 4;
                    self.skip_to("-->");
                    continue;
                }
                if after.starts_with('!') || after.starts_with('?') {
                    self.at += 1;
                    self.skip_to(">");
                    continue;
                }
                let (end, body) = match after.strip_prefix('/') {
                    Some(b) => (true, b),
                    None => (false, after),
                };
                if !body.starts_with(|c: char| c.is_ascii_alphabetic()) {
                    // A lone '<' is text.
                    self.at += 1;
                    return Some(Token::Text("<".into()));
                }
                let name_len = body
                    .find(|c: char| !(c.is_ascii_alphanumeric() || c == '-' || c == ':'))
                    .unwrap_or(body.len());
                let name = body[..name_len].to_ascii_lowercase();
                let mut i = self.at + 1 + usize::from(end) + name_len;
                let mut attrs = Vec::new();
                // Attributes up to '>' (quoted values may hold '>').
                loop {
                    let b = s.as_bytes();
                    while i < s.len() && (b[i].is_ascii_whitespace() || b[i] == b'/') {
                        i += 1;
                    }
                    if i >= s.len() {
                        break;
                    }
                    if b[i] == b'>' {
                        i += 1;
                        break;
                    }
                    let an = i;
                    while i < s.len()
                        && !b[i].is_ascii_whitespace()
                        && !matches!(b[i], b'=' | b'>' | b'/')
                    {
                        i += 1;
                    }
                    let aname = s[an..i].to_ascii_lowercase();
                    while i < s.len() && b[i].is_ascii_whitespace() {
                        i += 1;
                    }
                    let mut value = String::new();
                    if i < s.len() && b[i] == b'=' {
                        i += 1;
                        while i < s.len() && b[i].is_ascii_whitespace() {
                            i += 1;
                        }
                        if i < s.len() && (b[i] == b'"' || b[i] == b'\'') {
                            let q = b[i];
                            let vs = i + 1;
                            let ve = s[vs..].find(q as char).map_or(s.len(), |k| vs + k);
                            value = decode_entities(&s[vs..ve]);
                            i = (ve + 1).min(s.len());
                        } else {
                            let vs = i;
                            while i < s.len() && !b[i].is_ascii_whitespace() && b[i] != b'>' {
                                i += 1;
                            }
                            value = decode_entities(&s[vs..i]);
                        }
                    }
                    if !aname.is_empty() && attrs.len() < 32 {
                        attrs.push((aname, value));
                    }
                }
                self.at = i;
                return Some(if end {
                    Token::End(name)
                } else {
                    Token::Start { name, attrs }
                });
            }
            let len = rest.find('<').unwrap_or(rest.len());
            self.at += len;
            return Some(Token::Text(decode_entities(&rest[..len])));
        }
        None
    }
}

/// `haystack.find(needle)`, ASCII case-insensitive.
fn find_ci(haystack: &str, needle: &str) -> Option<usize> {
    let h = haystack.as_bytes();
    let n = needle.as_bytes();
    if n.is_empty() || h.len() < n.len() {
        return None;
    }
    (0..=h.len() - n.len()).find(|&i| h[i..i + n.len()].eq_ignore_ascii_case(n))
}

/// Decodes character references: numeric ones and the common named ones (unknown names stay
/// as they are).
pub fn decode_entities(s: &str) -> String {
    if !s.contains('&') {
        return s.to_string();
    }
    let mut out = String::with_capacity(s.len());
    let mut rest = s;
    while let Some(i) = rest.find('&') {
        out.push_str(&rest[..i]);
        rest = &rest[i..];
        let end = rest[1..].find(|c: char| !(c.is_ascii_alphanumeric() || c == '#'));
        let (body, consumed) = match end {
            Some(e) if rest.as_bytes().get(e + 1) == Some(&b';') => (&rest[1..=e], e + 2),
            Some(e) => (&rest[1..=e], e + 1),
            None => (&rest[1..], rest.len()),
        };
        let semicolon = rest.as_bytes().get(consumed - 1) == Some(&b';');
        let ch = if let Some(num) = body.strip_prefix('#') {
            let v = match num.strip_prefix(['x', 'X']) {
                Some(h) => u32::from_str_radix(h, 16).ok(),
                None => num.parse::<u32>().ok(),
            };
            v.map(|v| match char::from_u32(v) {
                Some(c) if v != 0 => c,
                _ => '\u{FFFD}',
            })
        } else if semicolon {
            named(body)
        } else {
            None
        };
        match ch {
            Some(c) if !body.is_empty() => {
                out.push(c);
                rest = &rest[consumed..];
            }
            _ => {
                out.push('&');
                rest = &rest[1..];
            }
        }
    }
    out.push_str(rest);
    out
}

fn named(name: &str) -> Option<char> {
    Some(match name {
        "amp" => '&',
        "lt" => '<',
        "gt" => '>',
        "quot" => '"',
        "apos" => '\'',
        "nbsp" => '\u{A0}',
        "ensp" => '\u{2002}',
        "emsp" => '\u{2003}',
        "thinsp" => '\u{2009}',
        "copy" => '©',
        "reg" => '®',
        "trade" => '™',
        "hellip" => '…',
        "mdash" => '—',
        "ndash" => '–',
        "lsquo" => '\u{2018}',
        "rsquo" => '\u{2019}',
        "ldquo" => '\u{201C}',
        "rdquo" => '\u{201D}',
        "bull" => '•',
        "middot" => '·',
        "laquo" => '«',
        "raquo" => '»',
        "deg" => '°',
        "plusmn" => '±',
        "times" => '×',
        "divide" => '÷',
        "euro" => '€',
        "pound" => '£',
        "yen" => '¥',
        "cent" => '¢',
        "sect" => '§',
        "para" => '¶',
        "larr" => '←',
        "rarr" => '→',
        "uarr" => '↑',
        "darr" => '↓',
        "check" => '✓',
        "shy" => '\u{AD}',
        "zwj" => '\u{200D}',
        "zwnj" => '\u{200C}',
        _ => return None,
    })
}

/// A CSS colour value: `#rgb`, `#rrggbb` (with optional alpha, which must be opaque enough),
/// `rgb(…)`/`rgba(…)`, or a basic colour name. `transparent` and translucent colours are `None`
/// (they cannot hide text: the default colour is used).
pub fn css_color(v: &str) -> Option<Rgb> {
    let v = v.trim().trim_end_matches("!important").trim();
    let named: Option<Rgb> = match v.to_ascii_lowercase().as_str() {
        "black" => Some([0, 0, 0]),
        "white" => Some([255, 255, 255]),
        "red" => Some([255, 0, 0]),
        "green" => Some([0, 128, 0]),
        "lime" => Some([0, 255, 0]),
        "blue" => Some([0, 0, 255]),
        "yellow" => Some([255, 255, 0]),
        "orange" => Some([255, 165, 0]),
        "purple" => Some([128, 0, 128]),
        "fuchsia" | "magenta" => Some([255, 0, 255]),
        "aqua" | "cyan" => Some([0, 255, 255]),
        "teal" => Some([0, 128, 128]),
        "navy" => Some([0, 0, 128]),
        "maroon" => Some([128, 0, 0]),
        "olive" => Some([128, 128, 0]),
        "silver" => Some([192, 192, 192]),
        "gray" | "grey" => Some([128, 128, 128]),
        "brown" => Some([165, 42, 42]),
        "pink" => Some([255, 192, 203]),
        _ => None,
    };
    if named.is_some() {
        return named;
    }
    let c = crate::paste::parse_color(v)?;
    (c.a >= 200).then_some([c.r, c.g, c.b])
}

/// Applies the supported properties of a `style` attribute; everything else is ignored.
/// Returns `(pre, background)`.
fn apply_css(style: &mut Style, css: &str) -> (Option<bool>, Option<Rgb>) {
    let mut pre = None;
    let mut background = None;
    for decl in css.split(';').take(64) {
        let Some((prop, value)) = decl.split_once(':') else {
            continue;
        };
        let prop = prop.trim().to_ascii_lowercase();
        let value = value.trim();
        let lower = value.to_ascii_lowercase();
        if lower.contains("url(") || lower.contains("expression(") || lower.contains("var(") {
            continue;
        }
        match prop.as_str() {
            "color" => {
                if let Some(c) = css_color(value) {
                    style.color = Some(c);
                }
            }
            "background-color" | "background" => {
                if let Some(c) = css_color(value) {
                    background = Some(c);
                }
            }
            "font-weight" => {
                style.bold = match lower.as_str() {
                    "bold" | "bolder" => true,
                    "normal" | "lighter" => false,
                    n => n.parse::<u32>().map_or(style.bold, |w| w >= 600),
                };
            }
            "font-style" => style.italic = lower.starts_with("italic") || lower == "oblique",
            "text-decoration" | "text-decoration-line" => {
                if lower.contains("none") {
                    style.underline = false;
                    style.strike = false;
                }
                if lower.contains("underline") {
                    style.underline = true;
                }
                if lower.contains("line-through") {
                    style.strike = true;
                }
            }
            "font-family" => {
                if [
                    "monospace",
                    "consolas",
                    "courier",
                    "menlo",
                    "monaco",
                    "mono",
                    "code",
                ]
                .iter()
                .any(|m| lower.contains(m))
                {
                    style.mono = true;
                }
            }
            "white-space" => {
                pre = Some(matches!(
                    lower.as_str(),
                    "pre" | "pre-wrap" | "break-spaces" | "pre-line"
                ));
            }
            _ => {}
        }
    }
    (pre, background)
}

/// Builds the card document.
struct Builder {
    doc: Doc,
    block: Option<Block>,
    stack: Vec<Open>,
    chars: usize,
    /// Text was emitted (a background found after it is not the card's).
    any_text: bool,
    /// Cells seen in the current table row.
    cells: u32,
}

impl Builder {
    fn top(&self) -> Open {
        self.stack.last().cloned().unwrap_or(Open {
            name: String::new(),
            style: Style::default(),
            pre: false,
            indent: 0,
            quote: false,
            list: None,
        })
    }

    fn full(&self) -> bool {
        self.chars >= MAX_CHARS || self.doc.blocks.len() >= MAX_BLOCKS
    }

    /// Ends the current block (empty ones vanish, except preformatted blank lines).
    fn end_block(&mut self) {
        if let Some(b) = self.block.take() {
            let empty = b.runs.iter().all(|r| r.text.trim().is_empty());
            if !empty || matches!(b.kind, BlockKind::Rule | BlockKind::Preformatted) {
                let mut b = b;
                if b.kind != BlockKind::Preformatted {
                    // Trailing spaces of a block are not drawn.
                    if let Some(r) = b.runs.last_mut() {
                        let t = r.text.trim_end_matches(' ').to_string();
                        r.text = t;
                    }
                }
                self.doc.blocks.push(b);
            }
        }
        self.cells = 0;
    }

    fn start_block(&mut self, kind: BlockKind) {
        self.end_block();
        let top = self.top();
        let mut b = Block::new(kind);
        b.indent = top.indent;
        b.quote = top.quote;
        self.block = Some(b);
    }

    fn block_mut(&mut self) -> &mut Block {
        if self.block.is_none() {
            let top = self.top();
            let kind = if top.pre {
                BlockKind::Preformatted
            } else {
                BlockKind::Paragraph
            };
            let mut b = Block::new(kind);
            b.indent = top.indent;
            b.quote = top.quote;
            self.block = Some(b);
        }
        self.block.as_mut().expect("just set")
    }

    fn text(&mut self, raw: &str, style: Style, pre: bool) {
        if self.full() {
            self.doc.truncated = true;
            return;
        }
        let raw = clean(raw);
        let text: String = if pre {
            raw.replace("\r\n", "\n").replace('\t', "    ")
        } else {
            // Collapse whitespace; no space at a block's start.
            let mut t = String::with_capacity(raw.len());
            let mut space = false;
            for c in raw.chars() {
                if c.is_ascii_whitespace() {
                    space = true;
                } else {
                    if space {
                        t.push(' ');
                    }
                    space = false;
                    t.push(if c == '\u{A0}' { ' ' } else { c });
                }
            }
            if space {
                t.push(' ');
            }
            let at_start = self
                .block
                .as_ref()
                .is_none_or(|b| b.runs.last().is_none_or(|r| r.text.ends_with([' ', '\n'])));
            if at_start {
                t.trim_start().to_string()
            } else {
                t
            }
        };
        if text.is_empty() {
            return;
        }
        let room = MAX_CHARS - self.chars;
        let text: String = if text.chars().count() > room {
            self.doc.truncated = true;
            text.chars().take(room).collect()
        } else {
            text
        };
        self.chars += text.chars().count();
        if !text.trim().is_empty() {
            self.any_text = true;
        }
        let b = self.block_mut();
        match b.runs.last_mut() {
            Some(r) if r.style == style => r.text.push_str(&text),
            _ => b.runs.push(Run { text, style }),
        }
    }

    fn placeholder(&mut self, alt: &str) {
        let alt: String = clean(alt).chars().take(60).collect();
        let label = if alt.trim().is_empty() {
            "[image]".to_string()
        } else {
            format!("[image: {}]", alt.trim())
        };
        self.doc.placeholders += 1;
        let style = Style {
            placeholder: true,
            ..Style::default()
        };
        let pre = self.top().pre;
        self.text(&label, style, pre);
    }

    fn start(&mut self, name: &str, attrs: &[(String, String)]) {
        let attr = |k: &str| attrs.iter().find(|(n, _)| n == k).map(|(_, v)| v.as_str());
        match name {
            "br" => {
                let style = self.top().style;
                let b = self.block_mut();
                b.runs.push(Run {
                    text: "\n".into(),
                    style,
                });
                return;
            }
            "hr" => {
                self.start_block(BlockKind::Rule);
                self.end_block();
                return;
            }
            "img" => {
                self.placeholder(attr("alt").unwrap_or(""));
                return;
            }
            _ => {}
        }
        if class(name) == Class::Void {
            return;
        }
        let mut open = self.top();
        open.name = name.to_string();
        open.list = None;
        let s = &mut open.style;
        match name {
            "b" | "strong" => s.bold = true,
            "i" | "em" | "cite" | "var" | "dfn" | "address" => s.italic = true,
            "u" | "ins" => s.underline = true,
            "s" | "strike" | "del" => s.strike = true,
            "code" | "kbd" | "samp" | "tt" => s.mono = true,
            "a" => s.link = attr("href").is_some(),
            "mark" => s.highlight = Some([0xF5, 0xD7, 0x9C]),
            "small" | "sub" | "sup" => s.size = Size::Small,
            "th" | "dt" | "summary" | "caption" | "legend" => s.bold = true,
            "pre" | "listing" => {
                s.mono = true;
                open.pre = true;
            }
            "ul" | "menu" => {
                open.indent = open.indent.saturating_add(1);
                open.list = Some((false, 1));
            }
            "ol" => {
                open.indent = open.indent.saturating_add(1);
                let start = attr("start")
                    .and_then(|v| v.trim().parse::<u32>().ok())
                    .unwrap_or(1)
                    .min(1_000_000);
                open.list = Some((true, start));
            }
            "blockquote" | "dd" => {
                open.indent = open.indent.saturating_add(1);
                open.quote |= name == "blockquote";
            }
            _ => {}
        }
        if name == "font"
            && let Some(c) = attr("color").and_then(css_color)
        {
            open.style.color = Some(c);
        }
        let mut background = attr("bgcolor").and_then(css_color);
        if let Some(css) = attr("style") {
            let (pre, bg) = apply_css(&mut open.style, css);
            if let Some(p) = pre {
                open.pre = p;
            }
            background = bg.or(background);
        }
        if let Some(bg) = background {
            if class(name) == Class::Block && !self.any_text && self.doc.background.is_none() {
                // The outermost background before any text: the card's own (an editor's theme).
                self.doc.background = Some(bg);
            } else {
                open.style.highlight = Some(bg);
            }
        }
        // Block structure.
        match name {
            "li" => {
                let marker = self.list_marker();
                self.start_block(BlockKind::Item(marker));
            }
            "h1" | "h2" | "h3" | "h4" | "h5" | "h6" => {
                let level = name.as_bytes()[1] - b'0';
                self.start_block(BlockKind::Heading(level));
            }
            "pre" | "listing" => self.start_block(BlockKind::Preformatted),
            "td" | "th" => {
                if self.cells > 0 {
                    let style = open.style;
                    self.text("\u{2003}\u{2003}", style, true);
                }
                self.cells += 1;
            }
            _ if class(name) == Class::Block => self.end_block(),
            _ => {}
        }
        if self.stack.len() < MAX_DEPTH {
            self.stack.push(open);
        }
    }

    /// The marker of a new list item: a bullet by nesting level, or the next number.
    fn list_marker(&mut self) -> String {
        let depth = self.stack.iter().filter(|o| o.list.is_some()).count();
        match self.stack.iter_mut().rev().find_map(|o| o.list.as_mut()) {
            Some((true, n)) => {
                let m = format!("{n}.");
                *n = n.saturating_add(1);
                m
            }
            _ => match depth {
                0 | 1 => "\u{2022}".into(),
                2 => "\u{25E6}".into(),
                _ => "\u{25AA}".into(),
            },
        }
    }

    fn end(&mut self, name: &str) {
        let Some(pos) = self.stack.iter().rposition(|o| o.name == name) else {
            return; // a stray end tag
        };
        self.stack.truncate(pos);
        if class(name) == Class::Block || name == "pre" {
            self.end_block();
        }
    }
}

/// The card document of an HTML fragment (see the module documentation).
pub fn parse(html: &str) -> Doc {
    let mut b = Builder {
        doc: Doc::default(),
        block: None,
        stack: Vec::new(),
        chars: 0,
        any_text: false,
        cells: 0,
    };
    let mut t = Tokens { s: html, at: 0 };
    while let Some(tok) = t.next() {
        if b.full() {
            b.doc.truncated = true;
            break;
        }
        match tok {
            Token::Start { name, attrs } => {
                if class(&name) == Class::Drop {
                    t.skip_element(&name);
                    continue;
                }
                b.start(&name, &attrs);
            }
            Token::End(name) => b.end(&name),
            Token::Text(text) => {
                let top = b.top();
                b.text(&text, top.style, top.pre);
            }
        }
    }
    b.end_block();
    b.doc
}

/// The card document of CF_HTML bytes.
pub fn parse_cf_html(cf_html: &[u8]) -> Doc {
    parse(&fragment(cf_html))
}

#[cfg(test)]
mod tests;
