//! The window's outside world behind traits (Step 19, ADR 0048): the clipboard, the OS "open"
//! handler for links, and the save dialog for an output export. The Win32 implementations live
//! in `window.rs`; the off-screen test window and the unit tests use the recording fakes here,
//! so no automated test ever touches the user's real clipboard, opens a browser or shows a
//! dialog.
//!
//! - **Clipboard formats.** A copy writes plain text (`CF_UNICODETEXT`, CR LF line ends) and, for
//!   "Copy with colours", also `HTML Format` (CF_HTML). A paste reads **only** the plain text:
//!   rich formats (HTML, RTF, files, images) are never interpreted, so a paste is always plain
//!   text; a clipboard without text pastes nothing and says so.
//! - **Links** are handed to the OS with the verb `open` and the validated target as the file
//!   (`ShellExecuteW`), never through a shell command line.

use std::path::PathBuf;

/// The clipboard format names the fake records (the Win32 names).
pub const TEXT: &str = "CF_UNICODETEXT";
pub const HTML: &str = "HTML Format";

/// The system clipboard as the window uses it.
pub trait Clipboard: std::fmt::Debug {
    /// The clipboard's plain text, `Ok(None)` when it holds none (only an image, files, HTML…).
    fn read_text(&mut self) -> Result<Option<String>, String>;
    /// Replaces the clipboard's content with `text` (already with CR LF line ends) and, when
    /// given, a CF_HTML document.
    fn write(&mut self, text: &str, html: Option<&str>) -> Result<(), String>;
    /// The recording fake, for the test hooks.
    fn as_fake(&mut self) -> Option<&mut FakeClipboard> {
        None
    }
    fn fake(&self) -> Option<&FakeClipboard> {
        None
    }
}

/// A clipboard in memory that records its formats (tests and the off-screen test window).
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct FakeClipboard {
    /// `(format name, content)` in the order they were put there.
    pub formats: Vec<(String, String)>,
    pub writes: usize,
    pub reads: usize,
}

impl FakeClipboard {
    /// The content of format `name`.
    pub fn get(&self, name: &str) -> Option<&str> {
        self.formats
            .iter()
            .find(|(n, _)| n == name)
            .map(|(_, c)| c.as_str())
    }

    /// Puts arbitrary formats on it (what another program copied).
    pub fn set(&mut self, formats: Vec<(String, String)>) {
        self.formats = formats;
    }
}

impl Clipboard for FakeClipboard {
    fn read_text(&mut self) -> Result<Option<String>, String> {
        self.reads += 1;
        Ok(self.get(TEXT).map(String::from))
    }

    fn write(&mut self, text: &str, html: Option<&str>) -> Result<(), String> {
        self.writes += 1;
        self.formats = vec![(TEXT.to_string(), text.to_string())];
        if let Some(h) = html {
            self.formats.push((HTML.to_string(), h.to_string()));
        }
        Ok(())
    }

    fn as_fake(&mut self) -> Option<&mut FakeClipboard> {
        Some(self)
    }

    fn fake(&self) -> Option<&FakeClipboard> {
        Some(self)
    }
}

/// Line ends as the Windows clipboard expects them: every `\n` becomes CR LF.
pub fn crlf(text: &str) -> String {
    text.replace("\r\n", "\n").replace('\n', "\r\n")
}

/// The OS "open" handler for a validated link target.
pub trait Opener: std::fmt::Debug {
    fn open(&mut self, target: &str) -> Result<(), String>;
    /// The recording fake, for the test hooks.
    fn fake(&self) -> Option<&FakeOpener> {
        None
    }
}

/// Records what would have been opened, with the verb (tests and the test window).
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct FakeOpener {
    /// `(verb, target)`.
    pub opened: Vec<(String, String)>,
}

impl Opener for FakeOpener {
    fn open(&mut self, target: &str) -> Result<(), String> {
        self.opened.push(("open".to_string(), target.to_string()));
        Ok(())
    }

    fn fake(&self) -> Option<&FakeOpener> {
        Some(self)
    }
}

/// Where to save an export: the test window's stand-in for the save dialog (the real dialog is
/// modal and runs outside the window's state, `window.rs`).
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct FakeSavePicker {
    /// The path the "dialog" answers with (`None`: the user cancelled it).
    pub answer: Option<PathBuf>,
    /// The suggested file names it was shown.
    pub asked: Vec<String>,
}

impl FakeSavePicker {
    pub fn pick(&mut self, suggested: &str) -> Option<PathBuf> {
        self.asked.push(suggested.to_string());
        self.answer.clone()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_fake_records_formats_and_a_paste_reads_only_plain_text() {
        let mut c = FakeClipboard::default();
        assert_eq!(c.read_text(), Ok(None));
        c.write("a\r\nb", Some("<html>…</html>")).unwrap();
        assert_eq!(c.get(TEXT), Some("a\r\nb"));
        assert_eq!(c.get(HTML), Some("<html>…</html>"));
        assert_eq!(c.writes, 1);
        // Another program copied only HTML and files: there is no text to paste.
        c.set(vec![
            (HTML.into(), "<b>x</b>".into()),
            ("CF_HDROP".into(), "C:\\x.txt".into()),
        ]);
        assert_eq!(c.read_text(), Ok(None));
        c.set(vec![
            ("Rich Text Format".into(), "{\\rtf1 x}".into()),
            (TEXT.into(), "plain".into()),
        ]);
        assert_eq!(c.read_text(), Ok(Some("plain".into())));
        assert_eq!(c.reads, 3);
        assert_eq!(crlf("a\nb\r\nc"), "a\r\nb\r\nc");
    }

    #[test]
    fn the_fake_opener_records_the_verb_and_target() {
        let mut o = FakeOpener::default();
        o.open("https://example.com/").unwrap();
        assert_eq!(
            o.opened,
            [("open".to_string(), "https://example.com/".to_string())]
        );
        let mut p = FakeSavePicker::default();
        assert_eq!(p.pick("work.txt"), None);
        p.answer = Some(PathBuf::from("C:\\out.txt"));
        assert_eq!(p.pick("x.txt"), Some(PathBuf::from("C:\\out.txt")));
        assert_eq!(p.asked, ["work.txt", "x.txt"]);
    }
}
