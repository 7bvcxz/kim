//! Search in a pane's retained output (Step 19, ADR 0048, docs/SUBAK_GUI.md §12,
//! docs/SCROLLBACK.md §4.1): the history and the screen of a [`TermView`] — the same bounded
//! model the Host keeps (ADR 0030), so a search sees exactly what the Host retained, including
//! output printed while no window was attached — matched as plain text, logical line by logical
//! line (rows joined across soft wraps, so a wrapped URL or path is found whole).
//!
//! **Bounded by construction.** A query is a literal or a regular expression of the `regex`
//! crate, whose matching time is linear in the text (no backtracking, so no catastrophic
//! patterns); its pattern is at most [`MAX_PATTERN_CHARS`] characters, and its compiled size, its
//! lazy DFA and its nesting are capped (a pattern over a cap is refused as too complex, never
//! run). A search stops after [`Budget::max_matches`] matches or [`Budget::time`], newest lines
//! first, and says so ([`Stop`]); the history itself is bounded (2 000 rows / 4 MiB per pane by
//! default, 100 000 rows at most). Nothing is copied into GPU memory (the renderer is the CPU
//! one) or kept beside the model: history lines are read as text straight from their compact
//! form (no cell decoding), and only the matches are stored.
//!
//! **Incremental.** [`Search::refresh`] after new output keeps the matches in history lines
//! that cannot change and searches only the lines that entered the history since, and the
//! screen, so a search stays live while a program prints.
//!
//! Portable and headless: no OS calls, no drawing. The Subak window's search bar
//! (`subak::gui::find`) is its only user; the Host and the CLI never call it.

use crate::view::{LogicalLine, Span, TermView};
use regex::{Regex, RegexBuilder};
use std::fmt;
use std::time::{Duration, Instant};

/// The longest pattern accepted, in characters.
pub const MAX_PATTERN_CHARS: usize = 1000;
/// The compiled program of a pattern may not exceed this (`regex` size limit).
pub const REGEX_SIZE_LIMIT: usize = 512 * 1024;
/// Memory for the lazy DFA's cache of one pattern.
pub const DFA_SIZE_LIMIT: usize = 2 * 1024 * 1024;
/// Nesting depth of groups and repetitions.
pub const NEST_LIMIT: u32 = 64;
/// Matches kept at most by default (the newest).
pub const MAX_MATCHES: usize = 10_000;
/// Time one search may take by default.
pub const TIME_BUDGET: Duration = Duration::from_millis(250);

/// What to look for.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Query {
    pub text: String,
    /// `text` is a regular expression (the `regex` crate's syntax), else literal text.
    pub regex: bool,
    /// Letters must match in case (off: Unicode simple case folding).
    pub case_sensitive: bool,
    /// A match must not be part of a longer word (letters, digits, `_`, Hangul…).
    pub whole_word: bool,
}

/// Why a query is not searched.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum QueryError {
    Empty,
    TooLong {
        max: usize,
    },
    /// Not a valid regular expression; the parser's message.
    Invalid(String),
    /// Valid, but over the size or nesting caps.
    TooComplex,
}

impl fmt::Display for QueryError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Empty => write!(f, "type something to search"),
            Self::TooLong { max } => write!(f, "the search is longer than {max} characters"),
            Self::Invalid(m) => write!(f, "not a valid regular expression: {m}"),
            Self::TooComplex => write!(
                f,
                "the regular expression is too complex (over the size or nesting limit)"
            ),
        }
    }
}

/// A compiled query.
#[derive(Debug, Clone)]
pub struct Matcher {
    re: Regex,
    whole_word: bool,
}

impl Matcher {
    pub fn new(q: &Query) -> Result<Self, QueryError> {
        if q.text.is_empty() {
            return Err(QueryError::Empty);
        }
        if q.text.chars().count() > MAX_PATTERN_CHARS {
            return Err(QueryError::TooLong {
                max: MAX_PATTERN_CHARS,
            });
        }
        let pattern = if q.regex {
            q.text.clone()
        } else {
            regex::escape(&q.text)
        };
        let re = RegexBuilder::new(&pattern)
            .case_insensitive(!q.case_sensitive)
            .unicode(true)
            .size_limit(REGEX_SIZE_LIMIT)
            .dfa_size_limit(DFA_SIZE_LIMIT)
            .nest_limit(NEST_LIMIT)
            .build()
            .map_err(|e| match e {
                regex::Error::CompiledTooBig(_) => QueryError::TooComplex,
                regex::Error::Syntax(s) if s.contains("nest") => QueryError::TooComplex,
                regex::Error::Syntax(s) => QueryError::Invalid(short_error(&s)),
                other => QueryError::Invalid(short_error(&other.to_string())),
            })?;
        Ok(Self {
            re,
            whole_word: q.whole_word,
        })
    }

    /// The matches in `text` as byte ranges, in order (empty matches are skipped).
    pub fn find_in(&self, text: &str, out: &mut Vec<(usize, usize)>) {
        for m in self.re.find_iter(text) {
            if m.start() == m.end() {
                continue;
            }
            if self.whole_word {
                let before = text[..m.start()].chars().next_back();
                let after = text[m.end()..].chars().next();
                if before.is_some_and(is_word_char) || after.is_some_and(is_word_char) {
                    continue;
                }
            }
            out.push((m.start(), m.end()));
        }
    }
}

/// A character that is part of a word for whole-word search.
fn is_word_char(c: char) -> bool {
    c.is_alphanumeric() || c == '_'
}

/// The essential line of a `regex` syntax error (it draws the pattern with a caret above).
fn short_error(s: &str) -> String {
    let line = s
        .lines()
        .rev()
        .map(str::trim)
        .find(|l| !l.is_empty())
        .unwrap_or("invalid pattern");
    line.trim_start_matches("error:").trim().to_string()
}

/// The limits of one search.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Budget {
    pub max_matches: usize,
    pub time: Duration,
}

impl Default for Budget {
    fn default() -> Self {
        Self {
            max_matches: MAX_MATCHES,
            time: TIME_BUDGET,
        }
    }
}

/// Why a search stopped before the oldest line.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Stop {
    /// It found [`Budget::max_matches`] matches (the newest ones are kept).
    Matches,
    /// It used its [`Budget::time`].
    Time,
}

/// A search over one view, kept current as output arrives.
#[derive(Debug, Clone)]
pub struct Search {
    matcher: Matcher,
    budget: Budget,
    matches: Vec<Span>,
    /// Lines below this are history lines whose matches are final.
    settled: u64,
    alternate: bool,
    /// The search did not reach the oldest line (sticky until a full search completes).
    pub stopped: Option<Stop>,
    /// Lines read and time taken by the last run (full or incremental).
    pub lines_searched: u64,
    pub elapsed: Duration,
}

impl Search {
    /// Compiles `query` and searches `view`.
    pub fn new(view: &TermView, query: &Query, budget: Budget) -> Result<Self, QueryError> {
        let mut s = Self {
            matcher: Matcher::new(query)?,
            budget,
            matches: Vec::new(),
            settled: 0,
            alternate: false,
            stopped: None,
            lines_searched: 0,
            elapsed: Duration::ZERO,
        };
        s.full(view);
        Ok(s)
    }

    /// The matches, oldest first.
    pub fn matches(&self) -> &[Span] {
        &self.matches
    }

    /// Searches the whole view again.
    pub fn full(&mut self, view: &TermView) {
        self.alternate = view.screen().alternate();
        self.matches.clear();
        let (found, stopped, lines, took) = self.scan(view, view.top_line(), self.budget);
        self.matches = found;
        self.stopped = stopped;
        self.lines_searched = lines;
        self.elapsed = took;
        self.settle(view);
    }

    /// After new output (or a resize): drops matches in lines that left the history, keeps
    /// those in history lines that cannot change, and searches the rest — the lines that
    /// entered the history since the last run and the screen. Switching to or from the
    /// alternate screen searches again from scratch.
    pub fn refresh(&mut self, view: &TermView) {
        if view.screen().alternate() != self.alternate || view.screen().alternate() {
            // The alternate screen is a few rows: searched whole each time.
            self.full(view);
            return;
        }
        let top = view.top_line();
        // The logical line around the old screen's first row may have grown: search it again.
        let from = view.logical_start(self.settled.max(top)).max(top);
        self.matches
            .retain(|m| m.start.line >= top && m.end.line < from);
        let (found, stopped, lines, took) = self.scan(view, from, self.budget);
        self.matches.extend(found);
        if self.matches.len() > self.budget.max_matches {
            let extra = self.matches.len() - self.budget.max_matches;
            self.matches.drain(..extra);
            self.stopped = Some(Stop::Matches);
        }
        if stopped.is_some() {
            self.stopped = stopped;
        }
        self.lines_searched = lines;
        self.elapsed = took;
        self.settle(view);
    }

    /// The screen's rows can still change; history lines cannot.
    fn settle(&mut self, view: &TermView) {
        self.settled = view.screen_line();
    }

    /// Searches the logical lines from `from` to the last screen row, newest first. Returns
    /// the matches oldest first, why it stopped early, the lines read and the time taken.
    fn scan(
        &self,
        view: &TermView,
        from: u64,
        budget: Budget,
    ) -> (Vec<Span>, Option<Stop>, u64, Duration) {
        let started = Instant::now();
        let last = view.last_line();
        let mut found: Vec<Span> = Vec::new();
        let mut stopped = None;
        let mut lines = 0u64;
        let mut line = LogicalLine::default();
        let mut hits: Vec<(usize, usize)> = Vec::new();
        let mut end = last;
        let mut logical = 0u32;
        loop {
            if end < from {
                break;
            }
            // The logical line ending at `end` starts after the previous non-wrapped line.
            let mut start = end;
            while start > from && view.line_wrapped(start - 1) == Some(true) {
                start -= 1;
            }
            let next = view.logical_line_into(start, &mut line);
            debug_assert_eq!(next, end + 1);
            lines += end - start + 1;
            hits.clear();
            self.matcher.find_in(&line.text, &mut hits);
            // Newest first: this line's matches backwards.
            for &(a, b) in hits.iter().rev() {
                found.push(line.span(a, b));
                if found.len() >= budget.max_matches {
                    stopped = Some(Stop::Matches);
                    break;
                }
            }
            if stopped.is_some() || start == 0 || start <= from {
                break;
            }
            end = start - 1;
            logical += 1;
            if logical.is_multiple_of(32) && started.elapsed() > budget.time {
                stopped = Some(Stop::Time);
                break;
            }
        }
        found.reverse();
        (found, stopped, lines, started.elapsed())
    }

    /// The index of the match to start at: the newest one that begins at or above `line`
    /// (the viewport's last row), else the oldest one below it.
    pub fn nearest(&self, line: u64) -> Option<usize> {
        if self.matches.is_empty() {
            return None;
        }
        let i = self.matches.partition_point(|m| m.start.line <= line);
        Some(i.saturating_sub(1))
    }

    /// The index of `span` after a refresh (the same match, or the one that took its place).
    pub fn find(&self, span: Span) -> Option<usize> {
        if self.matches.is_empty() {
            return None;
        }
        Some(
            self.matches
                .partition_point(|m| *m < span)
                .min(self.matches.len() - 1),
        )
    }
}
