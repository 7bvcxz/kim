//! Links in terminal output (Step 19, ADR 0048, docs/SUBAK_GUI.md §12.4): OSC 8 hyperlinks the
//! model keeps (ADR 0036) and web addresses detected in plain text, and the validation every
//! target passes before a window may hand it to the operating system (`SUBAK-SEC-013`).
//!
//! - **Detection** finds `http://` and `https://` addresses in a logical line (rows joined across
//!   soft wraps, so a wrapped address is found whole), ending at a blank, a quote, `<`, `>` and
//!   the like; trailing sentence punctuation and an unbalanced closing bracket are not part of
//!   it.
//! - **Validation** ([`validate`]) accepts only `http` and `https` by default ([`ALLOWED`]); a
//!   target with a user name or password (`https://bank.example@evil.example`), a backslash in
//!   its authority, blanks or control characters, no host, or more than [`MAX_TARGET`]
//!   characters is refused, whatever its scheme. It returns the normalized target (scheme and
//!   ASCII host in lower case, non-ASCII and unsafe characters of the path, query and fragment
//!   percent-encoded): the exact string shown to the user and handed to the OS "open" handler
//!   (never a shell command line).
//!
//! Nothing here opens anything: portable and headless, no OS calls.

use crate::view::{Span, TermView};

/// Schemes a window opens.
pub const ALLOWED: &[&str] = &["http", "https"];
/// The longest target accepted, in characters (the model's OSC 8 limit).
pub const MAX_TARGET: usize = crate::screen::LINK_URI_MAX;

/// Where a link under the pointer came from.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    /// An OSC 8 hyperlink: its visible text may differ from its target.
    Hyperlink,
    /// An address detected in the text: what is shown is the target.
    Text,
}

/// A link found in a view.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Found {
    pub target: String,
    pub kind: Kind,
    /// The cells of a detected address (`None` for a hyperlink, whose cells are marked by the
    /// model).
    pub span: Option<Span>,
}

/// Why a target is not opened.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Refusal {
    /// Not one of [`ALLOWED`] (`file`, `javascript`, `ms-settings`, …).
    Scheme(String),
    NoScheme,
    /// A user name or password in the address (it can disguise the real host).
    Credentials,
    /// Blanks, control characters or a backslash where they do not belong.
    Characters,
    NoHost,
    TooLong,
}

impl std::fmt::Display for Refusal {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Scheme(s) => write!(f, "`{s}:` links are not opened (only http and https)"),
            Self::NoScheme => write!(f, "it is not a web address"),
            Self::Credentials => write!(f, "it contains a user name or password"),
            Self::Characters => write!(f, "it contains blanks, control characters or `\\`"),
            Self::NoHost => write!(f, "it names no host"),
            Self::TooLong => write!(f, "it is longer than {MAX_TARGET} characters"),
        }
    }
}

/// The link under viewport cell (`col`, `row`): an OSC 8 hyperlink first, else a web address
/// detected in the text there.
pub fn link_at(view: &TermView, col: u16, row: u16) -> Option<Found> {
    if let Some(l) = view.link_at(col, row) {
        return Some(Found {
            target: l.uri.to_string(),
            kind: Kind::Hyperlink,
            span: None,
        });
    }
    let p = view.point_at(col, row);
    let line = view.logical_line(p.line)?;
    let b = line.byte_at(p)?;
    find_urls(&line.text)
        .into_iter()
        .find(|&(s, e)| s <= b && b < e)
        .map(|(s, e)| Found {
            target: line.text[s..e].to_string(),
            kind: Kind::Text,
            span: Some(line.span(s, e)),
        })
}

/// Characters that end a detected address.
fn ends_url(c: char) -> bool {
    c.is_whitespace()
        || c.is_control()
        || matches!(
            c,
            '"' | '<' | '>' | '`' | '{' | '}' | '|' | '\\' | '^' | '\u{3000}'..='\u{303F}'
        )
}

/// The byte ranges of the `http://` and `https://` addresses in `text`.
pub fn find_urls(text: &str) -> Vec<(usize, usize)> {
    let lower = text.to_ascii_lowercase();
    let mut out = Vec::new();
    let mut at = 0;
    while let Some(i) = lower[at..].find("http").map(|i| i + at) {
        at = i + 4;
        let rest = &lower[i..];
        let scheme = if rest.starts_with("https://") {
            8
        } else if rest.starts_with("http://") {
            7
        } else {
            continue;
        };
        // Not the end of a longer word (`xhttp://`).
        if text[..i]
            .chars()
            .next_back()
            .is_some_and(|c| c.is_alphanumeric() || c == '_')
        {
            continue;
        }
        let body = &text[i + scheme..];
        let len = body.find(ends_url).unwrap_or(body.len());
        let mut end = i + scheme + len;
        // Trailing punctuation belongs to the sentence; a closing bracket only when unbalanced.
        while let Some(c) = text[..end].chars().next_back() {
            let url = &text[i..end];
            let unbalanced = |open: char, close: char| {
                c == close && url.matches(open).count() < url.matches(close).count()
            };
            if matches!(c, '.' | ',' | ':' | ';' | '!' | '?' | '\'' | '*')
                || unbalanced('(', ')')
                || unbalanced('[', ']')
            {
                end -= c.len_utf8();
            } else {
                break;
            }
        }
        if end > i + scheme {
            out.push((i, end));
            at = end;
        }
    }
    out
}

/// Checks `target` against [`ALLOWED`] and the rules of the module and returns the normalized
/// address to show and open.
pub fn validate(target: &str) -> Result<String, Refusal> {
    validate_with(target, ALLOWED)
}

/// [`validate`] with another list of schemes.
pub fn validate_with(target: &str, allowed: &[&str]) -> Result<String, Refusal> {
    if target.chars().count() > MAX_TARGET {
        return Err(Refusal::TooLong);
    }
    if target.chars().any(|c| c.is_control() || c.is_whitespace()) {
        return Err(Refusal::Characters);
    }
    let Some((scheme, rest)) = target.split_once(':') else {
        return Err(Refusal::NoScheme);
    };
    let valid_scheme = scheme
        .chars()
        .next()
        .is_some_and(|c| c.is_ascii_alphabetic())
        && scheme
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '+' | '-' | '.'));
    if !valid_scheme {
        return Err(Refusal::NoScheme);
    }
    let scheme = scheme.to_ascii_lowercase();
    if !allowed.contains(&scheme.as_str()) {
        return Err(Refusal::Scheme(scheme));
    }
    let Some(rest) = rest.strip_prefix("//") else {
        return Err(Refusal::NoHost);
    };
    let auth_end = rest.find(['/', '?', '#']).unwrap_or(rest.len());
    let (authority, tail) = rest.split_at(auth_end);
    if authority.contains('@') {
        return Err(Refusal::Credentials);
    }
    if authority.contains('\\') {
        return Err(Refusal::Characters);
    }
    let host = authority.rsplit_once(':').map_or(authority, |(h, port)| {
        if port.chars().all(|c| c.is_ascii_digit()) {
            h
        } else {
            authority
        }
    });
    if host.is_empty() || host.starts_with('.') {
        return Err(Refusal::NoHost);
    }
    let mut out = format!("{scheme}://");
    for c in authority.chars() {
        out.push(c.to_ascii_lowercase());
    }
    for c in tail.chars() {
        if c.is_ascii() && !matches!(c, '"' | '<' | '>' | '`' | '{' | '}' | '|' | '\\' | '^') {
            out.push(c);
        } else {
            let mut buf = [0u8; 4];
            for b in c.encode_utf8(&mut buf).bytes() {
                out.push_str(&format!("%{b:02X}"));
            }
        }
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn urls(t: &str) -> Vec<&str> {
        find_urls(t).into_iter().map(|(a, b)| &t[a..b]).collect()
    }

    #[test]
    fn addresses_are_found_without_sentence_punctuation() {
        assert_eq!(
            urls("see https://example.com/a?b=1#c, then http://x.io."),
            ["https://example.com/a?b=1#c", "http://x.io"]
        );
        assert_eq!(
            urls("(docs: https://en.wikipedia.org/wiki/Rust_(language))"),
            ["https://en.wikipedia.org/wiki/Rust_(language)"]
        );
        assert_eq!(
            urls("\"https://q.example/x\" <https://a.example>"),
            ["https://q.example/x", "https://a.example"]
        );
        assert_eq!(
            urls("HTTPS://Example.COM/Path"),
            ["HTTPS://Example.COM/Path"]
        );
        assert_eq!(
            urls("한글 https://example.com/경로 다음"),
            ["https://example.com/경로"]
        );
        assert!(urls("xhttp://no https:// http:/x ftp://a").is_empty());
    }

    #[test]
    fn only_web_addresses_with_a_host_and_no_credentials_are_valid() {
        assert_eq!(
            validate("HTTPS://Example.COM:8443/Path?q=한#f").as_deref(),
            Ok("https://example.com:8443/Path?q=%ED%95%9C#f")
        );
        assert_eq!(validate("http://x.io").as_deref(), Ok("http://x.io"));
        assert_eq!(
            validate("file:///C:/Windows/System32/calc.exe"),
            Err(Refusal::Scheme("file".into()))
        );
        assert_eq!(
            validate("javascript:alert(1)"),
            Err(Refusal::Scheme("javascript".into()))
        );
        assert_eq!(
            validate("ms-settings:privacy"),
            Err(Refusal::Scheme("ms-settings".into()))
        );
        assert_eq!(
            validate("https://bank.example@evil.example/"),
            Err(Refusal::Credentials)
        );
        assert_eq!(
            validate("https://good.example\\@evil.example"),
            Err(Refusal::Credentials)
        );
        assert_eq!(validate("https://a.example\\b"), Err(Refusal::Characters));
        assert_eq!(validate("https://a b"), Err(Refusal::Characters));
        assert_eq!(validate("https://a\u{1b}[2J"), Err(Refusal::Characters));
        assert_eq!(validate("https:///path"), Err(Refusal::NoHost));
        assert_eq!(validate("https:example.com"), Err(Refusal::NoHost));
        assert_eq!(validate("example.com"), Err(Refusal::NoScheme));
        assert_eq!(validate("1http://x"), Err(Refusal::NoScheme));
        let long = format!("https://a.example/{}", "x".repeat(MAX_TARGET));
        assert_eq!(validate(&long), Err(Refusal::TooLong));
        assert_eq!(
            validate_with("FTP://Host.example/x", &["ftp"]).as_deref(),
            Ok("ftp://host.example/x")
        );
    }
}
