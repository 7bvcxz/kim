//! Redaction of secret-looking text in captured terminal output (Step 13, docs/SCROLLBACK.md §6).
//!
//! `pane.capture` (and so `subak capture`) redacts by default: a capture is often read by
//! scripts and agents, and `KIM-SEC-013` keeps secrets out of JSON output. The rules are
//! heuristic and over-eager on purpose, and they never claim completeness — what a program
//! printed in a form no rule knows stays visible, and `redact: false` (`--no-redact`) shows the
//! exact text. Portable: no OS calls.
//!
//! Rules, per line:
//! 1. the value of a secret-looking `name=value` or `name: value` (the names of
//!    [`crate::session::secret_name`]: token, secret, pass, pwd, key, auth, credential, cookie);
//!    after `:` the value needs 4 characters or quotes, so `Passed: 10` stays; an HTTP scheme
//!    word (`Bearer`, `Basic`, `Token`) keeps its name and hides the word after it;
//! 2. `Bearer <credential>` anywhere;
//! 3. well-known token shapes, keeping their prefix: GitHub (`ghp_`, `gho_`, `ghu_`, `ghs_`,
//!    `ghr_`, `github_pat_`), GitLab (`glpat-`), Slack (`xox?-`), OpenAI/Anthropic/Stripe-style
//!    `sk-` and `sk_live_`/`rk_live_`, npm (`npm_`), AWS access key ids (`AKIA`/`ASIA`), Google
//!    API keys (`AIza`), and JSON Web Tokens (`eyJ….eyJ….…`);
//! 4. the password of a URL (`scheme://user:password@host`);
//! 5. every line between `-----BEGIN … PRIVATE KEY-----` and its `-----END` line.

/// What replaces a hidden value.
pub const MASK: &str = "***";

/// Redacts lines in order (a private key block spans lines).
#[derive(Debug, Default)]
pub struct Redactor {
    in_private_key: bool,
    /// Values hidden so far.
    pub hidden: usize,
}

const TOKEN_PREFIXES: &[(&str, usize)] = &[
    ("github_pat_", 20),
    ("ghp_", 20),
    ("gho_", 20),
    ("ghu_", 20),
    ("ghs_", 20),
    ("ghr_", 20),
    ("glpat-", 16),
    ("xoxb-", 10),
    ("xoxp-", 10),
    ("xoxa-", 10),
    ("xoxr-", 10),
    ("xoxs-", 10),
    ("sk_live_", 16),
    ("sk_test_", 16),
    ("rk_live_", 16),
    ("sk-", 20),
    ("npm_", 30),
    ("AKIA", 16),
    ("ASIA", 16),
    ("AIza", 30),
];

const SCHEMES: &[&str] = &["bearer", "basic", "token", "digest"];

fn word_byte(c: u8) -> bool {
    c.is_ascii_alphanumeric() || matches!(c, b'_' | b'-' | b'+' | b'/' | b'=' | b'.')
}

fn name_byte(c: u8) -> bool {
    c.is_ascii_alphanumeric() || matches!(c, b'_' | b'-' | b'.')
}

/// Where a token of a known shape starts hiding (after its prefix), if `w` is one.
fn token_shape(w: &str) -> Option<usize> {
    for (prefix, min) in TOKEN_PREFIXES {
        if let Some(rest) = w.strip_prefix(prefix) {
            let ok = rest.len() >= *min
                && rest
                    .bytes()
                    .all(|c| c.is_ascii_alphanumeric() || matches!(c, b'_' | b'-'));
            if ok {
                return Some(prefix.len());
            }
        }
    }
    // A JSON Web Token: three base64url parts, header and payload starting with `eyJ`.
    let parts: Vec<&str> = w.split('.').collect();
    if parts.len() == 3
        && parts[0].starts_with("eyJ")
        && parts[1].starts_with("eyJ")
        && parts[0].len() + parts[1].len() >= 20
        && parts.iter().all(|p| {
            p.bytes()
                .all(|c| c.is_ascii_alphanumeric() || matches!(c, b'_' | b'-'))
        })
    {
        return Some(0);
    }
    None
}

impl Redactor {
    /// One line with its secret-looking values replaced by [`MASK`].
    pub fn line(&mut self, line: &str) -> String {
        if self.in_private_key {
            if line.contains("-----END") {
                self.in_private_key = false;
                return line.to_string();
            }
            if line.trim().is_empty() {
                return line.to_string();
            }
            self.hidden += 1;
            return MASK.to_string();
        }
        if line.contains("-----BEGIN") && line.contains("PRIVATE KEY") {
            self.in_private_key = !line.contains("-----END");
            return line.to_string();
        }
        let mut masks = Vec::new();
        let b = line.as_bytes();
        token_masks(line, &mut masks);
        assignment_masks(line, b, &mut masks);
        bearer_masks(line, b, &mut masks);
        url_masks(b, &mut masks);
        if masks.is_empty() {
            return line.to_string();
        }
        masks.sort_unstable();
        let mut out = String::with_capacity(line.len());
        let mut at = 0;
        for (from, to) in masks {
            if to <= at {
                continue;
            }
            let from = from.max(at);
            out.push_str(&line[at..from]);
            out.push_str(MASK);
            self.hidden += 1;
            at = to;
        }
        out.push_str(&line[at..]);
        out
    }
}

/// Rule 3: words of a known token shape.
fn token_masks(line: &str, masks: &mut Vec<(usize, usize)>) {
    let b = line.as_bytes();
    let mut i = 0;
    while i < b.len() {
        if !word_byte(b[i]) {
            i += 1;
            continue;
        }
        let start = i;
        while i < b.len() && word_byte(b[i]) {
            i += 1;
        }
        // Trailing punctuation is not part of a token.
        let mut end = i;
        while end > start && matches!(b[end - 1], b'.' | b'=' | b'/' | b'+') {
            end -= 1;
        }
        if let Some(keep) = token_shape(&line[start..end]) {
            masks.push((start + keep, end));
        }
    }
}

/// The end of a value starting at `i`: a quoted string, or up to a blank or separator.
fn value_end(b: &[u8], i: usize) -> (usize, usize, bool) {
    if i < b.len() && matches!(b[i], b'"' | b'\'') {
        let q = b[i];
        let end = b[i + 1..]
            .iter()
            .position(|&c| c == q)
            .map_or(b.len(), |p| i + 1 + p);
        return (i + 1, end, true);
    }
    let end = b[i..]
        .iter()
        .position(|&c| {
            c.is_ascii_whitespace()
                || matches!(c, b',' | b';' | b'&' | b'|' | b')' | b'}' | b'"' | b'\'')
        })
        .map_or(b.len(), |p| i + p);
    (i, end, false)
}

/// Rule 1: `name=value`, `name: value`, `"name": "value"`.
fn assignment_masks(line: &str, b: &[u8], masks: &mut Vec<(usize, usize)>) {
    for (j, &sep) in b.iter().enumerate() {
        if sep != b'=' && sep != b':' {
            continue;
        }
        // `://` belongs to a URL, `==` to a comparison or base64 padding.
        if b.get(j + 1) == Some(&b'/') || b.get(j + 1) == Some(&b'=') || (j > 0 && b[j - 1] == b'=')
        {
            continue;
        }
        let mut k = j;
        if k > 0 && matches!(b[k - 1], b'"' | b'\'') {
            k -= 1;
        }
        let name_end = k;
        while k > 0 && name_byte(b[k - 1]) {
            k -= 1;
        }
        if k == name_end || !crate::session::secret_name(&line[k..name_end]) {
            continue;
        }
        let mut v = j + 1;
        while v < b.len() && b[v] == b' ' {
            v += 1;
        }
        let (from, to, quoted) = value_end(b, v);
        if from >= to || (sep == b':' && !quoted && to - from < 4) {
            continue;
        }
        let word = line[from..to].to_ascii_lowercase();
        if !quoted && SCHEMES.contains(&word.as_str()) {
            // `Authorization: Bearer <credential>`: hide the credential.
            let mut n = to;
            while n < b.len() && b[n] == b' ' {
                n += 1;
            }
            let (f2, t2, _) = value_end(b, n);
            if f2 < t2 {
                masks.push((f2, t2));
            }
            continue;
        }
        masks.push((from, to));
    }
}

/// Rule 2: `Bearer <credential>` anywhere.
fn bearer_masks(line: &str, b: &[u8], masks: &mut Vec<(usize, usize)>) {
    let lower = line.to_ascii_lowercase();
    let mut from = 0;
    while let Some(p) = lower[from..].find("bearer ") {
        let start = from + p + "bearer ".len();
        let mut i = start;
        while i < b.len() && b[i] == b' ' {
            i += 1;
        }
        let (f, t, _) = value_end(b, i);
        if t - f >= 8 {
            masks.push((f, t));
        }
        from = start;
    }
}

/// Rule 4: `scheme://user:password@host`.
fn url_masks(b: &[u8], masks: &mut Vec<(usize, usize)>) {
    let mut i = 0;
    while i + 3 <= b.len() {
        if &b[i..i + 3] != b"://" {
            i += 1;
            continue;
        }
        let start = i + 3;
        let end = b[start..]
            .iter()
            .position(|&c| c.is_ascii_whitespace() || c == b'/' || c == b'"' || c == b'\'')
            .map_or(b.len(), |p| start + p);
        if let Some(at) = b[start..end].iter().rposition(|&c| c == b'@') {
            let at = start + at;
            if let Some(colon) = b[start..at].iter().position(|&c| c == b':') {
                let colon = start + colon;
                if colon + 1 < at {
                    masks.push((colon + 1, at));
                }
            }
        }
        i = end.max(i + 3);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn r(line: &str) -> String {
        Redactor::default().line(line)
    }

    #[test]
    fn assignments_and_headers() {
        assert_eq!(r("TOKEN=abc123"), "TOKEN=***");
        assert_eq!(
            r("set GITHUB_TOKEN=xyz & echo ok"),
            "set GITHUB_TOKEN=*** & echo ok"
        );
        assert_eq!(r("password: hunter2"), "password: ***");
        assert_eq!(
            r(r#"{"api_key": "abcd", "user": "me"}"#),
            r#"{"api_key": "***", "user": "me"}"#
        );
        assert_eq!(
            r("Authorization: Bearer abc.def.ghi"),
            "Authorization: Bearer ***"
        );
        assert_eq!(r("curl -H 'X-Auth: secretvalue'"), "curl -H 'X-Auth: ***'");
        assert_eq!(r("--password=s3cr3t"), "--password=***");
        // Not secrets: short values after a colon, comparisons, URLs, unrelated names.
        assert_eq!(r("Passed: 10, Failed: 0"), "Passed: 10, Failed: 0");
        assert_eq!(r("if a == b"), "if a == b");
        assert_eq!(r("Elapsed: 00:01:02"), "Elapsed: 00:01:02");
        assert_eq!(
            r("see https://example.com/x?a=b"),
            "see https://example.com/x?a=b"
        );
        assert_eq!(r("C:\\Users\\me> dir"), "C:\\Users\\me> dir");
    }

    #[test]
    fn token_shapes_keep_their_prefix() {
        let gh = format!("ghp_{}", "a1B2".repeat(9));
        assert_eq!(r(&format!("token is {gh}.")), "token is ghp_***.");
        assert_eq!(r("AKIAIOSFODNN7EXAMPLE"), "AKIA***");
        let sk = format!("sk-ant-api03-{}", "x".repeat(40));
        assert_eq!(r(&format!("echo {sk}")), "echo sk-***");
        let jwt = "eyJhbGciOiJIUzI1NiJ9.eyJzdWIiOiIxMjM0In0.c2lnbmF0dXJl";
        assert_eq!(r(&format!("jwt {jwt} end")), "jwt *** end");
        assert_eq!(r("Bearer abcdefghijkl"), "Bearer ***");
        // Too short or not shaped like one: kept.
        assert_eq!(r("ghp_short sk-1 AKIA"), "ghp_short sk-1 AKIA");
    }

    #[test]
    fn urls_and_private_keys() {
        assert_eq!(
            r("git clone https://me:p4ss@github.com/x.git"),
            "git clone https://me:***@github.com/x.git"
        );
        assert_eq!(r("https://github.com/x"), "https://github.com/x");
        let mut red = Redactor::default();
        let out: Vec<String> = [
            "before",
            "-----BEGIN OPENSSH PRIVATE KEY-----",
            "b3BlbnNzaC1rZXktdjEAAAAABG5vbmUAAAAEbm9uZQ",
            "AAAAMwAAAAtzc2gtZWQyNTUxOQ",
            "-----END OPENSSH PRIVATE KEY-----",
            "after",
        ]
        .iter()
        .map(|l| red.line(l))
        .collect();
        assert_eq!(
            out,
            [
                "before",
                "-----BEGIN OPENSSH PRIVATE KEY-----",
                "***",
                "***",
                "-----END OPENSSH PRIVATE KEY-----",
                "after"
            ]
        );
        assert_eq!(red.hidden, 2);
    }

    #[test]
    fn several_secrets_in_one_line_and_unicode() {
        let mut red = Redactor::default();
        assert_eq!(
            red.line("한글 token=abc password=def 끝"),
            "한글 token=*** password=*** 끝"
        );
        assert_eq!(red.hidden, 2);
    }
}
