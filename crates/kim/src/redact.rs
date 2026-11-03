//! Secret redaction at every output boundary (KIM-SEC-013, docs/CLI_CONTRACT.md §10).
//!
//! Kim does not handle passwords or tokens itself, but users can put them into URLs (a private
//! mirror `https://user:token@host/...`, a signed download link `...?token=...`, a proxy in
//! `HTTPS_PROXY`), and URLs end up in plans, install records and error messages. Everything the
//! CLI prints — the JSON document, human output and diagnostics on stderr — and every URL Kim
//! stores in its own state passes through this module first:
//!
//! * the user-info part of a URL (`user:password@`) is replaced by `***@`;
//! * values of query or fragment parameters whose name looks secret (`token`, `key`, `secret`,
//!   `password`, `sig`, `auth`, `credential`, `session`, `code`, …) are replaced by `***`.
//!
//! Redaction is deliberately over-eager: hiding a harmless value is acceptable, leaking a
//! secret is not.

use serde_json::Value;

/// What a redacted secret is replaced with.
pub const MASK: &str = "***";

/// Substrings that mark a query/fragment parameter name as secret (compared lowercased).
const SECRET_NAME_PARTS: &[&str] = &[
    "token",
    "secret",
    "pass",
    "pwd",
    "key",
    "sig",
    "auth",
    "credential",
    "session",
    "code",
    "cookie",
];

/// Redacts one URL (or any string containing `scheme://`): user-info and secret parameters.
pub fn url(u: &str) -> String {
    let Some(i) = u.find("://") else {
        return u.to_string();
    };
    let (scheme, rest) = u.split_at(i + 3);
    let auth_end = rest.find(['/', '?', '#']).unwrap_or(rest.len());
    let (authority, tail) = rest.split_at(auth_end);
    let authority = match authority.rfind('@') {
        Some(at) => format!("{MASK}@{}", &authority[at + 1..]),
        None => authority.to_string(),
    };
    format!("{scheme}{authority}{}", redact_params(tail))
}

/// Redacts every URL that appears inside free text (error messages, hints, human output).
pub fn text(s: &str) -> String {
    if !s.contains("://") {
        return s.to_string();
    }
    let mut out = String::with_capacity(s.len());
    let mut rest = s;
    while let Some(pos) = rest.find("://") {
        // Walk back over the scheme (`https`, `git+ssh`, …): ASCII only, so bytes == chars.
        let scheme_len = rest[..pos]
            .bytes()
            .rev()
            .take_while(|b| b.is_ascii_alphanumeric() || matches!(b, b'+' | b'-' | b'.'))
            .count();
        let start = pos - scheme_len;
        out.push_str(&rest[..start]);
        let candidate = &rest[start..];
        let end = candidate
            .find(|c: char| c.is_whitespace() || matches!(c, '"' | '\'' | '<' | '>' | '`'))
            .unwrap_or(candidate.len());
        out.push_str(&url(&candidate[..end]));
        rest = &candidate[end..];
    }
    out.push_str(rest);
    out
}

/// Redacts every string inside a JSON value (object keys are left alone).
pub fn json(v: &mut Value) {
    match v {
        Value::String(s) => {
            if s.contains("://") {
                *s = text(s);
            }
        }
        Value::Array(items) => items.iter_mut().for_each(json),
        Value::Object(map) => map.values_mut().for_each(json),
        _ => {}
    }
}

fn is_secret_name(name: &str) -> bool {
    let n = name.to_ascii_lowercase();
    SECRET_NAME_PARTS.iter().any(|p| n.contains(p))
}

/// Redacts secret-looking `name=value` pairs in the query and fragment of a URL tail.
fn redact_params(tail: &str) -> String {
    let mut out = String::with_capacity(tail.len());
    let mut rest = tail;
    // Path part (up to the first `?` or `#`) is kept as is.
    let path_end = rest.find(['?', '#']).unwrap_or(rest.len());
    out.push_str(&rest[..path_end]);
    rest = &rest[path_end..];
    while !rest.is_empty() {
        // `rest` starts with `?` or `#`.
        let (sep, body) = rest.split_at(1);
        out.push_str(sep);
        let end = body.find(['?', '#']).unwrap_or(body.len());
        let params: Vec<String> = body[..end]
            .split('&')
            .map(|kv| match kv.split_once('=') {
                Some((k, v)) if !v.is_empty() && is_secret_name(k) => format!("{k}={MASK}"),
                _ => kv.to_string(),
            })
            .collect();
        out.push_str(&params.join("&"));
        rest = &body[end..];
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn user_info_is_removed() {
        assert_eq!(
            url("https://alice:s3cret@mirror.example/c.json"),
            "https://***@mirror.example/c.json"
        );
        assert_eq!(
            url("https://ghp_TOKEN@github.com/x"),
            "https://***@github.com/x"
        );
        assert_eq!(
            url("http://u:p@127.0.0.1:8080"),
            "http://***@127.0.0.1:8080"
        );
        // An `@` in the path is not user-info.
        assert_eq!(
            url("https://host/a@b?x=1"),
            "https://host/a@b?x=1",
            "nothing secret here"
        );
    }

    #[test]
    fn secret_parameters_are_masked_and_others_kept() {
        assert_eq!(
            url("https://h/p.zip?token=abc&v=2&X-Amz-Signature=ff#access_token=zz&state=1"),
            "https://h/p.zip?token=***&v=2&X-Amz-Signature=***#access_token=***&state=1"
        );
        assert_eq!(url("https://h/p?api_key="), "https://h/p?api_key=");
        assert_eq!(url("no url here"), "no url here");
    }

    #[test]
    fn urls_inside_text_and_json_are_redacted() {
        let msg = "cannot fetch https://bob:hunter2@h.example/c.json: connection refused \
                   (proxy http://p:proxypw@127.0.0.1:9)";
        let r = text(msg);
        assert!(!r.contains("hunter2") && !r.contains("proxypw"), "{r}");
        assert!(r.contains("https://***@h.example/c.json: connection refused"));
        assert!(r.contains("(proxy http://***@127.0.0.1:9)"));
        assert_eq!(text("C:\\Users\\me\\x.json"), "C:\\Users\\me\\x.json");

        let mut v = serde_json::json!({
            "url": "file://u:pw@localhost/c.json",
            "list": ["see \"https://a:b@c/d?sig=1\""],
            "n": 3
        });
        json(&mut v);
        let s = v.to_string();
        assert!(
            !s.contains(":pw@") && !s.contains("a:b@") && !s.contains("sig=1"),
            "{s}"
        );
        assert_eq!(v["n"], 3);
    }

    #[test]
    fn redaction_is_idempotent() {
        let once = text("x https://u:p@h/?token=t y");
        assert_eq!(text(&once), once);
    }
}
