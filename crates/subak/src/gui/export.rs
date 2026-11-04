//! "Save output…" (Step 19, ADR 0048, docs/SUBAK_GUI.md §12.5): the pane's retained output —
//! its history and screen as this window's view holds them, the same bounded model the Host
//! keeps — as a text file, only after the user's explicit choice in a question that reports what
//! Subak's one secret detector (`crate::redact`, the rules of `subak capture`) found, and with
//! those values hidden unless the user chose to save exactly. The file goes only where the user
//! chose in the save dialog; nothing is sent anywhere, nothing is written otherwise.

use crate::redact::Redactor;
use std::io::Write as _;
use std::path::Path;
use subak_term::TermView;
use subak_term::view::LogicalLine;

/// The output of a pane, exact and with secret-looking values hidden.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Export {
    /// Logical lines (soft wraps joined), as shown.
    pub lines: Vec<String>,
    /// The same lines through the secret detector.
    pub hidden: Vec<String>,
    /// Values the detector hid.
    pub secrets: usize,
}

impl Export {
    /// Everything the view retains: the history and the screen (on the alternate screen, what
    /// the full-screen program shows), without the empty rows after the last output.
    pub fn of(view: &TermView) -> Self {
        let mut lines = Vec::new();
        let mut l = LogicalLine::default();
        let mut n = view.top_line();
        let last = view.last_line();
        while n <= last {
            n = view.logical_line_into(n, &mut l);
            lines.push(l.text.clone());
        }
        while lines.last().is_some_and(|l: &String| l.trim().is_empty()) {
            lines.pop();
        }
        let mut r = Redactor::default();
        let hidden = lines.iter().map(|l| r.line(l)).collect();
        Self {
            lines,
            hidden,
            secrets: r.hidden,
        }
    }

    /// The file's content: UTF-8 with CR LF line ends, hidden or exact.
    pub fn content(&self, hide: bool) -> String {
        let src = if hide { &self.hidden } else { &self.lines };
        let mut s = src.join("\r\n");
        if !s.is_empty() {
            s.push_str("\r\n");
        }
        s
    }

    pub fn bytes(&self) -> usize {
        self.content(false).len()
    }
}

/// A file name to suggest: `<terminal>-output-<YYYYMMDD-HHMMSS>.txt` (UTC), with characters
/// Windows does not allow in a name replaced.
pub fn suggested_name(terminal: &str, unix_secs: u64) -> String {
    let safe: String = terminal
        .chars()
        .map(|c| {
            if c.is_control() || matches!(c, '<' | '>' | ':' | '"' | '/' | '\\' | '|' | '?' | '*') {
                '_'
            } else {
                c
            }
        })
        .collect();
    let safe = safe.trim().trim_end_matches('.');
    let safe = if safe.is_empty() { "subak" } else { safe };
    let (y, mo, d) = civil(unix_secs / 86_400);
    let t = unix_secs % 86_400;
    format!(
        "{safe}-output-{y:04}{mo:02}{d:02}-{:02}{:02}{:02}.txt",
        t / 3600,
        (t / 60) % 60,
        t % 60
    )
}

/// Days since 1970-01-01 → (year, month, day) (Howard Hinnant's civil-from-days).
fn civil(days: u64) -> (i64, u32, u32) {
    let z = days as i64 + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = (doy - (153 * mp + 2) / 5 + 1) as u32;
    let m = if mp < 10 { mp + 3 } else { mp - 9 } as u32;
    (yoe + era * 400 + i64::from(m <= 2), m, d)
}

/// Writes `content` to `path` through a temporary file in the same folder and a rename, so an
/// interrupted save never leaves half a file under the chosen name.
pub fn write(path: &Path, content: &str) -> std::io::Result<()> {
    let dir = path.parent().filter(|d| !d.as_os_str().is_empty());
    let name = path
        .file_name()
        .ok_or_else(|| std::io::Error::other("no file name"))?;
    let tmp = dir
        .unwrap_or(Path::new("."))
        .join(format!(".{}.subak-tmp", name.to_string_lossy()));
    {
        let mut f = std::fs::File::create(&tmp)?;
        f.write_all(content.as_bytes())?;
        f.sync_all()?;
    }
    std::fs::rename(&tmp, path).inspect_err(|_| {
        let _ = std::fs::remove_file(&tmp);
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_export_holds_history_and_screen_and_hides_secrets_on_request() {
        let mut v = TermView::new(30, 4, 100);
        v.feed(
            "build 빌드 ok\r\nGITHUB_TOKEN=ghp_abcdefghijklmnopqrstuvwxyz0123\r\n\
             a long line that wraps over two rows here\r\n$ "
                .as_bytes(),
        );
        let e = Export::of(&v);
        assert_eq!(
            e.lines,
            [
                "build 빌드 ok",
                "GITHUB_TOKEN=ghp_abcdefghijklmnopqrstuvwxyz0123",
                "a long line that wraps over two rows here",
                "$"
            ]
        );
        assert_eq!(e.secrets, 1);
        assert_eq!(e.hidden[1], "GITHUB_TOKEN=***");
        let hidden = e.content(true);
        assert!(!hidden.contains("ghp_abc"), "{hidden}");
        assert!(hidden.ends_with("$\r\n"));
        assert!(
            e.content(false)
                .contains("ghp_abcdefghijklmnopqrstuvwxyz0123\r\n")
        );
        assert_eq!(e.bytes(), e.content(false).len());
        // A private key block spans lines.
        let mut v = TermView::new(40, 6, 100);
        v.feed(b"-----BEGIN RSA PRIVATE KEY-----\r\nMIIEow\r\n-----END RSA PRIVATE KEY-----\r\n");
        let e = Export::of(&v);
        assert_eq!(e.secrets, 1);
        assert_eq!(e.hidden[1], "***");
    }

    #[test]
    fn names_and_atomic_writes() {
        assert_eq!(
            suggested_name("빌드: a/b", 1_793_793_600),
            "빌드_ a_b-output-20261104-120000.txt"
        );
        assert_eq!(suggested_name("..", 0), "subak-output-19700101-000000.txt");
        let dir = std::env::temp_dir().join(format!("subak-export-unit-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let p = dir.join("out.txt");
        write(&p, "one\r\n").unwrap();
        write(&p, "two\r\n").unwrap();
        assert_eq!(std::fs::read_to_string(&p).unwrap(), "two\r\n");
        assert_eq!(
            std::fs::read_dir(&dir).unwrap().count(),
            1,
            "no temporary left"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }
}
