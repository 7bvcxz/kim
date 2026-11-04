//! Typed text: encoding, the variable whitelist, the insertion point, case conformity, previews
//! and the text editor's data model (docs/DDALGAK_RULE_MODEL.md §5.3, §5.4; `DDALGAK-FR-012`,
//! `DDALGAK-FR-070`, `DDALGAK-SEC-011`; ADR 0034).
//!
//! **Encoding** ([`events`], [`chunks`]): every character of a `type_text` step becomes one
//! `KEYEVENTF_UNICODE` down and up per UTF-16 unit ([`OutCode::Unicode`]), so Hangul syllables,
//! conjoining jamo and emoji (two units, a surrogate pair) arrive independent of the keyboard
//! layout and the IME mode; nothing is normalized, so composed (`가`, U+AC00) and decomposed
//! (`가`, U+1100 U+1161) Hangul are typed exactly as written. `\n` is a tap of Enter and `\t` a
//! tap of Tab (virtual keys: applications treat them as those keys). Text is sent in chunks of
//! [`CHUNK`] characters per `SendInput` call, or one character per call with a key delay, so the
//! worker can check focus and cancellation between chunks.
//!
//! **Variables** (a whitelist, only when the step says `variables: true`): `{date}`, `{time}`,
//! `{datetime}`, `{year}`, `{month}`, `{day}`, `{hour}`, `{minute}`, `{second}` (the local time when
//! the rule runs) and `{cursor}`, the insertion point. `{{` and `}}` type a brace. Nothing else is
//! a variable: no environment, clipboard or user-defined variables, so a snippet can never pull
//! other data into what it types. Without `variables: true` the text is typed literally.
//!
//! **Insertion point**: after the text, the caret moves back with one Left per character typed
//! after `{cursor}` (Unicode scalar values; a newline is one character). Applications that move
//! over a whole emoji sequence or a decomposed Hangul syllable with one Left are not modelled:
//! the preview says how many Left presses are sent.
//!
//! Previews and summaries carry **counts only** (`characters`, `lines`, the variable names, the
//! number of Left presses), never the text (`DDALGAK-SEC-011`).

use crate::inject::{OutCode, OutEvent};
use crate::remap::Field;
use crate::time::LocalTime;
use serde_json::{Value, json};

pub const VK_BACK: u16 = 0x08;
pub const VK_TAB: u16 = 0x09;
pub const VK_RETURN: u16 = 0x0D;
pub const VK_LEFT: u16 = 0x25;
pub const VK_LCONTROL: u16 = 0xA2;
pub const VK_V: u16 = 0x56;

/// Characters per `SendInput` call when no key delay is set (the worker checks focus and
/// cancellation between calls).
pub const CHUNK: usize = 32;

/// The longest `type_text` text (schema `step_type_text`).
pub const MAX_TEXT: usize = 4096;

/// One variable of the whitelist.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Var {
    Date,
    Time,
    DateTime,
    Year,
    Month,
    Day,
    Hour,
    Minute,
    Second,
}

impl Var {
    pub const ALL: [Var; 9] = [
        Var::Date,
        Var::Time,
        Var::DateTime,
        Var::Year,
        Var::Month,
        Var::Day,
        Var::Hour,
        Var::Minute,
        Var::Second,
    ];

    pub fn name(self) -> &'static str {
        match self {
            Var::Date => "date",
            Var::Time => "time",
            Var::DateTime => "datetime",
            Var::Year => "year",
            Var::Month => "month",
            Var::Day => "day",
            Var::Hour => "hour",
            Var::Minute => "minute",
            Var::Second => "second",
        }
    }

    /// What the editor shows next to the variable.
    pub fn help(self) -> &'static str {
        match self {
            Var::Date => "today's date, 2026-11-04",
            Var::Time => "the time, 14:05",
            Var::DateTime => "date and time, 2026-11-04 14:05",
            Var::Year => "the year, 2026",
            Var::Month => "the month, 11",
            Var::Day => "the day of the month, 04",
            Var::Hour => "the hour (24-hour clock), 14",
            Var::Minute => "the minute, 05",
            Var::Second => "the second, 09",
        }
    }

    pub fn parse(name: &str) -> Option<Var> {
        Var::ALL.into_iter().find(|v| v.name() == name)
    }

    /// The value at `t` (fixed width, so counts do not depend on the time).
    pub fn value(self, t: &LocalTime) -> String {
        match self {
            Var::Date => format!("{:04}-{:02}-{:02}", t.year, t.month, t.day),
            Var::Time => format!("{:02}:{:02}", t.hour, t.minute),
            Var::DateTime => format!(
                "{:04}-{:02}-{:02} {:02}:{:02}",
                t.year, t.month, t.day, t.hour, t.minute
            ),
            Var::Year => format!("{:04}", t.year),
            Var::Month => format!("{:02}", t.month),
            Var::Day => format!("{:02}", t.day),
            Var::Hour => format!("{:02}", t.hour),
            Var::Minute => format!("{:02}", t.minute),
            Var::Second => format!("{:02}", t.second),
        }
    }
}

/// The name of the insertion-point variable.
pub const CURSOR: &str = "cursor";

/// One piece of a template.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Piece<'a> {
    Text(&'a str),
    /// `{{` or `}}`.
    Brace(char),
    Var(Var),
    Cursor,
}

/// Why a template is refused (the schema refuses the same texts; these messages say where).
#[derive(Clone, PartialEq, Eq, Debug)]
pub enum TemplateError {
    /// `{name}` that is not in the whitelist (`{clipboard}`, `{env:PATH}`, `{USERNAME}`).
    UnknownVariable { at: usize, name: String },
    /// A `{` or `}` that is neither a variable nor doubled.
    LoneBrace { at: usize },
    /// `{cursor}` more than once.
    SecondCursor { at: usize },
}

impl std::fmt::Display for TemplateError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            TemplateError::UnknownVariable { at, name } => write!(
                f,
                "character {at}: `{{{name}}}` is not a variable; the variables are {} and {{cursor}} (no environment or clipboard variables)",
                Var::ALL
                    .iter()
                    .map(|v| format!("{{{}}}", v.name()))
                    .collect::<Vec<_>>()
                    .join(", ")
            ),
            TemplateError::LoneBrace { at } => write!(
                f,
                "character {at}: a single brace; type {{{{ or }}}} for a literal brace"
            ),
            TemplateError::SecondCursor { at } => {
                write!(f, "character {at}: {{cursor}} may appear only once")
            }
        }
    }
}

/// Splits a template into pieces, left to right: `{{` and `}}` are braces, `{name}` a variable.
pub fn parse(text: &str) -> Result<Vec<Piece<'_>>, TemplateError> {
    let mut out = Vec::new();
    let mut start = 0;
    let mut cursor = false;
    let bytes = text.as_bytes();
    let mut i = 0;
    let char_at = |b: usize| text[..b].chars().count();
    while i < bytes.len() {
        match bytes[i] {
            b'{' | b'}' if bytes.get(i + 1) == Some(&bytes[i]) => {
                if start < i {
                    out.push(Piece::Text(&text[start..i]));
                }
                out.push(Piece::Brace(char::from(bytes[i])));
                i += 2;
                start = i;
            }
            b'{' => {
                let Some(end) = text[i + 1..].find('}').map(|e| i + 1 + e) else {
                    return Err(TemplateError::LoneBrace { at: char_at(i) });
                };
                let name = &text[i + 1..end];
                let piece = if name == CURSOR {
                    if cursor {
                        return Err(TemplateError::SecondCursor { at: char_at(i) });
                    }
                    cursor = true;
                    Piece::Cursor
                } else if let Some(v) = Var::parse(name) {
                    Piece::Var(v)
                } else if name.contains('{') || name.is_empty() {
                    return Err(TemplateError::LoneBrace { at: char_at(i) });
                } else {
                    return Err(TemplateError::UnknownVariable {
                        at: char_at(i),
                        name: name.to_string(),
                    });
                };
                if start < i {
                    out.push(Piece::Text(&text[start..i]));
                }
                out.push(piece);
                i = end + 1;
                start = i;
            }
            b'}' => return Err(TemplateError::LoneBrace { at: char_at(i) }),
            _ => i += 1,
        }
    }
    if start < text.len() {
        out.push(Piece::Text(&text[start..]));
    }
    Ok(out)
}

/// What a step types, ready to send.
#[derive(Clone, PartialEq, Eq, Debug, Default)]
pub struct Rendered {
    pub text: String,
    /// Left presses after the text (characters after `{cursor}`).
    pub cursor_back: usize,
    /// Whether the text has an insertion point.
    pub cursor: bool,
    /// The variables used, in first-use order (names only).
    pub variables: Vec<&'static str>,
}

/// Renders a step's text: literally, or with the variables at `now` when `variables` is set.
pub fn render(text: &str, variables: bool, now: &LocalTime) -> Result<Rendered, TemplateError> {
    if !variables {
        return Ok(Rendered {
            text: text.to_string(),
            ..Rendered::default()
        });
    }
    let mut r = Rendered::default();
    let mut after: Option<usize> = None;
    for p in parse(text)? {
        match p {
            Piece::Text(t) => r.text.push_str(t),
            Piece::Brace(c) => r.text.push(c),
            Piece::Var(v) => {
                r.text.push_str(&v.value(now));
                if !r.variables.contains(&v.name()) {
                    r.variables.push(v.name());
                }
            }
            Piece::Cursor => {
                r.cursor = true;
                after = Some(r.text.chars().count());
            }
        }
    }
    if let Some(a) = after {
        r.cursor_back = r.text.chars().count() - a;
    }
    Ok(r)
}

/// What is wrong with a `type_text` text, **without repeating it** (validation messages reach
/// `status`, `rule check` and agents; the text never does): its length, the position of a
/// control character, or the position and kind of a template error. `None` when it is valid.
pub fn text_problem(text: &str, variables: bool) -> Option<String> {
    let len = text.chars().count();
    if len == 0 || len > MAX_TEXT {
        return Some(format!(
            "the text has {len} characters; 1–{MAX_TEXT} are allowed (the text is not shown)"
        ));
    }
    if let Some((i, c)) = text
        .chars()
        .enumerate()
        .find(|(_, c)| c.is_control() && *c != '\n' && *c != '\t')
    {
        return Some(format!(
            "character {i} is the control character U+{:04X}; only new lines and tabs can be typed (the text is not shown)",
            u32::from(c)
        ));
    }
    if variables && let Err(e) = parse(text) {
        let (at, what) = match e {
            TemplateError::UnknownVariable { at, .. } => (
                at,
                "names no variable of the whitelist (date and time variables and {cursor}; no environment or clipboard variables)",
            ),
            TemplateError::LoneBrace { at } => {
                (at, "is a single brace; write {{ or }} for a brace")
            }
            TemplateError::SecondCursor { at } => (at, "is a second {cursor}"),
        };
        return Some(format!("character {at} {what} (the text is not shown)"));
    }
    None
}

/// Interprets the CLI's backslash escapes (`--escapes`): `\n`, `\t`, `\\` and `\u{…}` (one to six
/// hex digits). Anything else after a backslash is refused, so a Windows path is never mangled
/// silently.
pub fn unescape(s: &str) -> Result<String, String> {
    let mut out = String::new();
    let mut it = s.chars().peekable();
    while let Some(c) = it.next() {
        if c != '\\' {
            out.push(c);
            continue;
        }
        match it.next() {
            Some('n') => out.push('\n'),
            Some('t') => out.push('\t'),
            Some('\\') => out.push('\\'),
            Some('u') if it.peek() == Some(&'{') => {
                it.next();
                let hex: String = it.by_ref().take_while(|c| *c != '}').collect();
                let ok = (1..=6).contains(&hex.len()) && hex.chars().all(|c| c.is_ascii_hexdigit());
                let ch = ok
                    .then(|| u32::from_str_radix(&hex, 16).ok())
                    .flatten()
                    .and_then(char::from_u32)
                    .ok_or_else(|| format!("`\\u{{{hex}}}` is not a Unicode character"))?;
                out.push(ch);
            }
            Some(other) => {
                return Err(format!(
                    "unknown escape `\\{other}`: use \\n, \\t, \\\\ or \\u{{…}}, or leave out --escapes"
                ));
            }
            None => return Err("a trailing backslash; write \\\\ for one backslash".into()),
        }
    }
    Ok(out)
}

/// How a hotstring's replacement follows the case of what was typed (DDALGAK_RULE_MODEL §3.4,
/// AutoHotkey's case conformity).
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub enum CaseForm {
    /// Typed in lower case, or the rule does not conform: the replacement as written.
    #[default]
    AsWritten,
    /// Every letter typed in upper case (`BTW`): the replacement in upper case.
    Upper,
    /// The first letter typed in upper case (`Btw`): the replacement's first letter in upper case.
    Title,
}

/// Applies a case form to a replacement.
pub fn apply_case(text: &str, f: CaseForm) -> String {
    match f {
        CaseForm::AsWritten => text.to_string(),
        CaseForm::Upper => text.to_uppercase(),
        CaseForm::Title => {
            let mut out = String::with_capacity(text.len());
            let mut done = false;
            for c in text.chars() {
                if !done && c.is_alphabetic() {
                    out.extend(c.to_uppercase());
                    done = true;
                } else {
                    out.push(c);
                }
            }
            out
        }
    }
}

fn tap(code: OutCode, out: &mut Vec<OutEvent>) {
    out.push(OutEvent::down(code));
    out.push(OutEvent::up(code));
}

/// The events of one character: Enter or Tab by virtual key, anything else as Unicode units.
pub fn char_events(c: char, out: &mut Vec<OutEvent>) {
    match c {
        '\n' => tap(OutCode::Vk(VK_RETURN), out),
        '\t' => tap(OutCode::Vk(VK_TAB), out),
        _ => {
            let mut buf = [0u16; 2];
            for u in c.encode_utf16(&mut buf) {
                tap(OutCode::Unicode(*u), out);
            }
        }
    }
}

/// Every event of a text, in order.
pub fn events(text: &str) -> Vec<OutEvent> {
    let mut out = Vec::new();
    for c in text.chars() {
        char_events(c, &mut out);
    }
    out
}

/// The text as `SendInput` calls: [`CHUNK`] characters per call, or one per call with a delay.
pub fn chunks(text: &str, key_delay_ms: u32) -> Vec<Vec<OutEvent>> {
    let per = if key_delay_ms > 0 { 1 } else { CHUNK };
    let chars: Vec<char> = text.chars().collect();
    chars
        .chunks(per)
        .map(|cs| {
            let mut out = Vec::new();
            for c in cs {
                char_events(*c, &mut out);
            }
            out
        })
        .collect()
}

/// `n` taps of a virtual key in one call (Backspaces that erase an abbreviation, Left presses
/// to the insertion point).
pub fn taps(vk: u16, n: usize) -> Vec<OutEvent> {
    let mut out = Vec::with_capacity(n * 2);
    for _ in 0..n {
        tap(OutCode::Vk(vk), &mut out);
    }
    out
}

/// Ctrl+V (the paste method).
pub fn paste_keys() -> Vec<OutEvent> {
    vec![
        OutEvent::down(OutCode::Vk(VK_LCONTROL)),
        OutEvent::down(OutCode::Vk(VK_V)),
        OutEvent::up(OutCode::Vk(VK_V)),
        OutEvent::up(OutCode::Vk(VK_LCONTROL)),
    ]
}

/// Characters (Unicode scalar values, a newline counts) and lines of a text.
pub fn count(text: &str) -> (usize, usize) {
    (text.chars().count(), text.matches('\n').count() + 1)
}

/// The step's text rendered for counting (the time does not change the counts: every variable has
/// a fixed width). An invalid template counts as written.
fn for_counts(text: &str, variables: bool) -> Rendered {
    render(text, variables, &LocalTime::default()).unwrap_or_else(|_| Rendered {
        text: text.to_string(),
        ..Rendered::default()
    })
}

/// The playground preview of a `type_text` step (§5.4, §11): exactly the `preview_type_text`
/// shape of `ddalgak-simulation-v1`, counts and names only.
pub fn preview(text: &str, variables: bool, method: &str) -> Value {
    let r = for_counts(text, variables);
    let (characters, lines) = count(&r.text);
    let mut v = json!({ "characters": characters.max(1), "lines": lines, "method": method });
    if !r.variables.is_empty() {
        v["variables"] = json!(r.variables);
    }
    if r.cursor {
        v["cursor_back"] = json!(r.cursor_back);
    }
    v
}

/// The editor's live summary sentence (GUI_JOURNEYS J4): "Types 13 characters on 2 lines".
pub fn summary(text: &str, variables: bool, method: &str) -> String {
    let r = for_counts(text, variables);
    let (characters, lines) = count(&r.text);
    let verb = if method == "paste" { "Pastes" } else { "Types" };
    let plural = |n: usize, one: &str, many: &str| {
        if n == 1 {
            format!("{n} {one}")
        } else {
            format!("{n} {many}")
        }
    };
    let mut s = format!(
        "{verb} {} on {}",
        plural(characters, "character", "characters"),
        plural(lines, "line", "lines")
    );
    if !r.variables.is_empty() {
        s.push_str(&format!(
            " with {}",
            r.variables
                .iter()
                .map(|v| format!("{{{v}}}"))
                .collect::<Vec<_>>()
                .join(", ")
        ));
    }
    if r.cursor {
        s.push_str(&format!(
            ", then moves the cursor back {}",
            plural(r.cursor_back, "character", "characters")
        ));
    }
    s
}

/// One problem of an editor field (shown under the field; the text itself is never echoed).
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct FieldProblem {
    /// The JSON path inside the rule (`steps[0].text`).
    pub path: &'static str,
    pub message: String,
}

/// The type-text editor's draft (Step 74 draws it): the fields it edits, the live preview, and
/// what the explicit **Save** and **Test** controls may do now. Nothing is saved or typed by
/// itself: Save writes through the rule store (revision checked, a draft or the saved file) only
/// when the user presses it and the draft is valid; Test runs the draft in the playground's own
/// field (Step 76), never in another window.
#[derive(Clone, PartialEq, Eq, Debug, Default)]
pub struct EditorDraft {
    /// The snippet's name: the rule's `title` (1–80 characters).
    pub title: String,
    pub text: String,
    pub variables: bool,
    /// `unicode` or `paste`.
    pub method: String,
    pub key_delay_ms: u32,
    /// The draft as last saved (`None` for a new snippet).
    pub saved: Option<Box<EditorDraft>>,
}

/// What the editor shows next to the text box while the user types.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct LivePreview {
    /// The text as it would be typed now (variables filled in), for the editor's own field only.
    pub rendered: String,
    pub characters: usize,
    pub lines: usize,
    pub cursor_back: Option<usize>,
    pub summary: String,
    pub problems: Vec<FieldProblem>,
}

impl EditorDraft {
    /// The field problems of the draft (the same rules as the schema, with positions).
    pub fn problems(&self) -> Vec<FieldProblem> {
        let mut out = Vec::new();
        let title_len = self.title.chars().count();
        if !(1..=80).contains(&title_len) {
            out.push(FieldProblem {
                path: "title",
                message: "the snippet name needs 1–80 characters".into(),
            });
        }
        let len = self.text.chars().count();
        if len == 0 || len > MAX_TEXT {
            out.push(FieldProblem {
                path: "steps[0].text",
                message: format!("the text needs 1–{MAX_TEXT} characters"),
            });
        }
        if let Some((i, c)) = self
            .text
            .chars()
            .enumerate()
            .find(|(_, c)| c.is_control() && *c != '\n' && *c != '\t')
        {
            out.push(FieldProblem {
                path: "steps[0].text",
                message: format!(
                    "character {i}: control character U+{:04X} cannot be typed (only new lines and tabs)",
                    u32::from(c)
                ),
            });
        }
        if self.variables
            && let Err(e) = parse(&self.text)
        {
            out.push(FieldProblem {
                path: "steps[0].text",
                message: e.to_string(),
            });
        }
        if self.method != "unicode" && self.method != "paste" {
            out.push(FieldProblem {
                path: "steps[0].method",
                message: "type or paste".into(),
            });
        }
        if self.key_delay_ms > 200 {
            out.push(FieldProblem {
                path: "steps[0].key_delay_ms",
                message: "0–200 ms between characters".into(),
            });
        }
        out
    }

    /// The live preview at `now`.
    pub fn preview(&self, now: &LocalTime) -> LivePreview {
        let problems = self.problems();
        let r = render(&self.text, self.variables, now).unwrap_or_else(|_| Rendered {
            text: self.text.clone(),
            ..Rendered::default()
        });
        let (characters, lines) = count(&r.text);
        LivePreview {
            characters,
            lines,
            cursor_back: r.cursor.then_some(r.cursor_back),
            summary: summary(&self.text, self.variables, &self.method),
            rendered: r.text,
            problems,
        }
    }

    /// Whether the draft differs from what was saved (Save is offered only then).
    pub fn dirty(&self) -> bool {
        match &self.saved {
            None => true,
            Some(s) => {
                s.title != self.title
                    || s.text != self.text
                    || s.variables != self.variables
                    || s.method != self.method
                    || s.key_delay_ms != self.key_delay_ms
            }
        }
    }

    /// Save is enabled for a changed, valid draft; otherwise the problems say why not.
    pub fn can_save(&self) -> Result<(), Vec<FieldProblem>> {
        let p = self.problems();
        if !p.is_empty() {
            return Err(p);
        }
        if !self.dirty() {
            return Err(vec![FieldProblem {
                path: "",
                message: "nothing changed since the last save".into(),
            }]);
        }
        Ok(())
    }

    /// Test is enabled for a valid draft (saved or not): it types into the playground only.
    pub fn can_test(&self) -> bool {
        self.problems().is_empty()
    }

    /// The step the draft saves as (canonical: defaults left out).
    pub fn step(&self) -> Value {
        let mut s = json!({ "type": "type_text", "text": self.text });
        if self.variables {
            s["variables"] = json!(true);
        }
        if self.method == "paste" {
            s["method"] = json!("paste");
        }
        if self.key_delay_ms > 0 {
            s["key_delay_ms"] = json!(self.key_delay_ms);
        }
        s
    }
}

/// The "Type text" form, in display order (GUI_JOURNEYS J4, Step 74).
pub const TYPE_TEXT_FIELDS: &[Field] = &[
    Field {
        path: "title",
        input: "text",
        required: true,
        default: None,
        label: "Snippet name",
        help: "Shown in the rule list and the tray menu instead of the text, which stays private.",
    },
    Field {
        path: "steps[0].text",
        input: "multiline",
        required: true,
        default: None,
        label: "Text",
        help: "Up to 4096 characters: Hangul, emoji, new lines and tabs. Typed exactly as written; rules.json keeps it in plain form, so it is not a password store.",
    },
    Field {
        path: "steps[0].variables",
        input: "toggle",
        required: false,
        default: Some("false"),
        label: "Fill in variables",
        help: "{date}, {time}, {datetime}, {year}, {month}, {day}, {hour}, {minute}, {second} and {cursor} (where the cursor ends up); {{ and }} type a brace. No environment or clipboard variables.",
    },
    Field {
        path: "steps[0].method",
        input: "choice",
        required: false,
        default: Some("unicode"),
        label: "Paste instead of typing",
        help: "Puts the text on the clipboard, presses Ctrl+V and restores what was there before. Needs the clipboard permission.",
    },
    Field {
        path: "steps[0].key_delay_ms",
        input: "number",
        required: false,
        default: Some("0"),
        label: "Delay between characters (ms)",
        help: "0–200, for applications that drop fast input.",
    },
];

/// The "Press a shortcut" form (§5.3).
pub const SEND_KEYS_FIELDS: &[Field] = &[
    Field {
        path: "steps[0].keys",
        input: "capture",
        required: true,
        default: None,
        label: "Keys",
        help: "One to eight chords, pressed in order (Ctrl+K Ctrl+V).",
    },
    Field {
        path: "steps[0].step_delay_ms",
        input: "number",
        required: false,
        default: Some("0"),
        label: "Delay between chords (ms)",
        help: "0–1000.",
    },
];

#[cfg(test)]
mod tests {
    use super::*;

    const U: fn(u16) -> OutCode = OutCode::Unicode;

    fn t(code: OutCode) -> [OutEvent; 2] {
        [OutEvent::down(code), OutEvent::up(code)]
    }

    fn now() -> LocalTime {
        LocalTime {
            year: 2026,
            month: 11,
            day: 4,
            hour: 9,
            minute: 5,
            second: 7,
        }
    }

    #[test]
    fn hangul_composed_and_decomposed_are_typed_exactly_as_written() {
        // 가 composed: one unit.
        assert_eq!(events("가"), t(U(0xAC00)));
        // 가 decomposed: two conjoining jamo, never normalized.
        let mut want = t(U(0x1100)).to_vec();
        want.extend(t(U(0x1161)));
        assert_eq!(events("\u{1100}\u{1161}"), want);
        assert_ne!(events("가"), events("\u{1100}\u{1161}"));
        assert_eq!(events("감사합니다").len(), 10);
    }

    #[test]
    fn emoji_are_surrogate_pairs_newlines_and_tabs_are_keys() {
        // 👍 U+1F44D = D83D DC4D: down/up per unit, high first.
        let mut want = t(U(0xD83D)).to_vec();
        want.extend(t(U(0xDC4D)));
        assert_eq!(events("👍"), want);
        let e = events("a\nb\tc");
        let mut want = t(U(u16::from(b'a'))).to_vec();
        want.extend(t(OutCode::Vk(VK_RETURN)));
        want.extend(t(U(u16::from(b'b'))));
        want.extend(t(OutCode::Vk(VK_TAB)));
        want.extend(t(U(u16::from(b'c'))));
        assert_eq!(e, want);
        // Characters that are syntax elsewhere are plain text here.
        assert_eq!(
            events("{^+!#}"),
            "{^+!#}"
                .chars()
                .flat_map(|c| t(U(c as u16)))
                .collect::<Vec<_>>()
        );
        // Every down has its up right after it: nothing is ever left pressed.
        for pair in events("감사👍\n\t").chunks(2) {
            assert!(!pair[0].up && pair[1].up && pair[0].code == pair[1].code);
        }
    }

    #[test]
    fn chunks_split_by_characters_never_inside_a_surrogate_pair() {
        let text: String = "👍".repeat(40);
        let c = chunks(&text, 0);
        assert_eq!(c.len(), 2, "32 + 8 characters");
        assert_eq!(c[0].len(), 32 * 4);
        assert_eq!(c.concat(), events(&text));
        let slow = chunks("ab👍", 15);
        assert_eq!(slow.len(), 3, "one character per call with a delay");
        assert_eq!(slow[2].len(), 4);
    }

    #[test]
    fn variables_are_a_whitelist_with_braces_and_one_cursor() {
        let r = render("{date} {time} {{x}} {datetime}|{second}", true, &now()).unwrap();
        assert_eq!(r.text, "2026-11-04 09:05 {x} 2026-11-04 09:05|07");
        assert_eq!(r.variables, ["date", "time", "datetime", "second"]);
        assert!(!r.cursor);
        let r = render("({cursor})\n끝", true, &now()).unwrap();
        assert_eq!(r.text, "()\n끝");
        assert_eq!((r.cursor, r.cursor_back), (true, 3));
        // Without `variables: true` the text is literal.
        assert_eq!(render("{date}", false, &now()).unwrap().text, "{date}");
        for (bad, why) in [
            ("{clipboard}", "not a variable"),
            ("{env:PATH}", "not a variable"),
            ("{USERNAME}", "not a variable"),
            ("a { b", "single brace"),
            ("a } b", "single brace"),
            ("{}", "single brace"),
            ("{cursor}{cursor}", "only once"),
        ] {
            let e = render(bad, true, &now()).unwrap_err();
            assert!(e.to_string().contains(why), "{bad}: {e}");
        }
    }

    #[test]
    fn templates_agree_with_the_schema_pattern() {
        let schema: Value = serde_json::from_str(crate::schema::RULES_SCHEMA).unwrap();
        let pattern = schema["$defs"]["text_template"]["pattern"]
            .as_str()
            .unwrap();
        let re = regex::Regex::new(pattern).unwrap();
        for s in [
            "{date}",
            "a{{b}}c",
            "{{cursor}}",
            "{{{date}}}",
            "x{cursor}y{time}",
            "감사합니다 {date}\n{cursor}",
            "{clipboard}",
            "{",
            "}",
            "a}b",
            "{cursor}{cursor}",
            "{date}{cursor}{{",
            "{env:X}",
            "{}",
        ] {
            assert_eq!(
                re.is_match(s),
                parse(s).is_ok(),
                "{s}: the schema and the renderer must agree"
            );
        }
    }

    #[test]
    fn text_problems_never_repeat_the_text() {
        assert_eq!(text_problem("fine", false), None);
        assert_eq!(text_problem("{date}", true), None);
        for (t, vars, needle) in [
            ("", false, "0 characters"),
            ("secret\u{7}", false, "character 6"),
            ("secret {clipboard}", true, "character 7"),
            ("secret {", true, "single brace"),
            ("{cursor}secret{cursor}", true, "second"),
        ] {
            let p = text_problem(t, vars).unwrap();
            assert!(p.contains(needle), "{p}");
            assert!(!p.contains("secret") && !p.contains("{clipboard}"), "{p}");
        }
        assert!(text_problem(&"x".repeat(MAX_TEXT + 1), false).is_some());
    }

    #[test]
    fn cli_escapes_are_explicit_and_strict() {
        assert_eq!(unescape(r"a\nb\tc\\d").unwrap(), "a\nb\tc\\d");
        assert_eq!(unescape(r"\u{AC00}\u{1F44D}").unwrap(), "가👍");
        assert!(unescape(r"C:\new").is_ok(), "\\n is an escape");
        assert!(
            unescape(r"C:\Users").is_err(),
            "\\U is not: refused, not mangled"
        );
        assert!(unescape(r"\u{110000}").is_err());
        assert!(unescape(r"\u{}").is_err());
        assert!(unescape("a\\").is_err());
    }

    #[test]
    fn case_conformity() {
        assert_eq!(apply_case("by the way", CaseForm::Upper), "BY THE WAY");
        assert_eq!(apply_case("by the way", CaseForm::Title), "By the way");
        assert_eq!(apply_case("(by) the way", CaseForm::Title), "(By) the way");
        assert_eq!(apply_case("감사합니다", CaseForm::Upper), "감사합니다");
        assert_eq!(apply_case("Mixed", CaseForm::AsWritten), "Mixed");
    }

    #[test]
    fn previews_and_summaries_carry_counts_only() {
        let text = "감사합니다.\n김민수 드림";
        let p = preview(text, false, "unicode");
        assert_eq!(
            p,
            json!({ "characters": 13, "lines": 2, "method": "unicode" })
        );
        assert_eq!(
            summary(text, false, "unicode"),
            "Types 13 characters on 2 lines"
        );
        let p = preview("Dear {cursor},\n{date}", true, "paste");
        assert_eq!(
            p,
            json!({ "characters": 17, "lines": 2, "method": "paste",
                    "variables": ["date"], "cursor_back": 12 })
        );
        assert_eq!(
            summary("Dear {cursor},\n{date}", true, "paste"),
            "Pastes 17 characters on 2 lines with {date}, then moves the cursor back 12 characters"
        );
        // Every preview fits the playground contract (ddalgak-simulation-v1 `preview_type_text`).
        let sim: Value = serde_json::from_str(include_str!(
            "../../../docs/contracts/schemas/ddalgak-simulation-v1.schema.json"
        ))
        .unwrap();
        let mut shape = sim["$defs"]["preview_type_text"].clone();
        shape["$defs"] = sim["$defs"].clone();
        let v = crate::jsonschema::Validator::new(&shape);
        for p in [
            preview(text, false, "unicode"),
            preview("{time}{cursor}", true, "unicode"),
            preview("👍", false, "paste"),
        ] {
            assert!(v.validate(&p).unwrap().is_empty(), "{p}");
            assert!(!p.to_string().contains("감사"), "{p}");
        }
    }

    #[test]
    fn the_editor_draft_validates_previews_and_gates_save_and_test() {
        let mut d = EditorDraft {
            title: "Sign-off".into(),
            text: "감사합니다 {date}{cursor}".into(),
            variables: true,
            method: "unicode".into(),
            ..EditorDraft::default()
        };
        assert!(d.problems().is_empty());
        let p = d.preview(&now());
        assert_eq!(p.rendered, "감사합니다 2026-11-04");
        assert_eq!((p.characters, p.lines, p.cursor_back), (16, 1, Some(0)));
        assert!(d.dirty() && d.can_save().is_ok() && d.can_test());
        d.saved = Some(Box::new(d.clone()));
        assert!(!d.dirty());
        assert!(d.can_save().is_err(), "nothing changed");
        d.text = "{clipboard}".into();
        let problems = d.can_save().unwrap_err();
        assert_eq!(problems[0].path, "steps[0].text");
        assert!(!problems[0].message.contains("감사"));
        assert!(!d.can_test());
        d.text = "a\u{7}b".into();
        assert!(d.problems()[0].message.contains("U+0007"));
        d.text = "ok".into();
        d.title = String::new();
        d.method = "fax".into();
        d.key_delay_ms = 500;
        let paths: Vec<&str> = d.problems().iter().map(|p| p.path).collect();
        assert_eq!(paths, ["title", "steps[0].method", "steps[0].key_delay_ms"]);
        d.title = "x".into();
        d.method = "paste".into();
        d.key_delay_ms = 10;
        assert_eq!(
            d.step(),
            json!({"type": "type_text", "text": "ok", "variables": true, "method": "paste", "key_delay_ms": 10})
        );
    }

    #[test]
    fn the_form_fields_are_fields_of_the_published_schema() {
        let schema: Value = serde_json::from_str(crate::schema::RULES_SCHEMA).unwrap();
        let defs = &schema["$defs"];
        for (fields, step) in [
            (TYPE_TEXT_FIELDS, "step_type_text"),
            (SEND_KEYS_FIELDS, "step_send_keys"),
        ] {
            for f in fields {
                let known = match f.path.split_once('.') {
                    Some(("steps[0]", name)) => defs[step]["properties"].get(name).is_some(),
                    None => defs["rule"]["properties"].get(f.path).is_some(),
                    _ => false,
                };
                assert!(known, "{} is not a schema field", f.path);
            }
        }
    }

    #[test]
    fn helper_key_sequences() {
        assert_eq!(taps(VK_BACK, 2).len(), 4);
        assert!(taps(VK_LEFT, 0).is_empty());
        assert_eq!(paste_keys()[0], OutEvent::down(OutCode::Vk(VK_LCONTROL)));
        assert_eq!(count("a\n\nb"), (4, 3));
    }
}
