//! Copy with colours (Step 19, docs/SUBAK_GUI.md §12.3): the selected cells as an HTML fragment
//! in the view's theme and font, and the Windows clipboard's `HTML Format` (CF_HTML) document
//! around it. Plain text is always copied as well; this is the optional rich form some editors
//! and mail programs paste. Pure functions, unit-tested; the windowing layer puts the result on
//! the clipboard.

use super::{MONO_FAMILY, Theme, cell_colors};
use crate::screen::Attrs;
use crate::view::CopiedCell;
use std::fmt::Write as _;

/// The selected lines as `<pre>` with one `<span>` per run of equal attributes.
pub fn fragment(lines: &[Vec<CopiedCell>], theme: &Theme, font_px: f32) -> String {
    let fonts: Vec<String> = MONO_FAMILY.iter().map(|f| format!("'{f}'")).collect();
    let mut out = format!(
        "<pre style=\"font-family:{},monospace;font-size:{}px;color:{};background-color:{};\
         padding:4px;margin:0\">",
        fonts.join(","),
        font_px.round(),
        hex(theme.foreground),
        hex(theme.background)
    );
    for (n, line) in lines.iter().enumerate() {
        if n > 0 {
            out.push('\n');
        }
        let mut i = 0;
        while i < line.len() {
            let attrs = line[i].attrs;
            let mut j = i;
            let mut text = String::new();
            while j < line.len() && line[j].attrs == attrs {
                text.push_str(&line[j].text);
                j += 1;
            }
            push_run(&mut out, &text, attrs, theme);
            i = j;
        }
    }
    out.push_str("</pre>");
    out
}

fn push_run(out: &mut String, text: &str, a: Attrs, theme: &Theme) {
    if a.is_default() {
        escape(out, text);
        return;
    }
    let (fg, bg) = cell_colors(theme, a, true, false);
    let mut style = String::new();
    if fg != theme.foreground {
        let _ = write!(style, "color:{};", hex(fg));
    }
    if bg != theme.background {
        let _ = write!(style, "background-color:{};", hex(bg));
    }
    if a.bold() {
        style.push_str("font-weight:bold;");
    }
    if a.italic() {
        style.push_str("font-style:italic;");
    }
    let mut deco = Vec::new();
    if a.underline() > 0 {
        deco.push("underline");
    }
    if a.strike() {
        deco.push("line-through");
    }
    if a.overline() {
        deco.push("overline");
    }
    if !deco.is_empty() {
        let _ = write!(style, "text-decoration:{};", deco.join(" "));
    }
    if style.is_empty() {
        escape(out, text);
        return;
    }
    let _ = write!(out, "<span style=\"{style}\">");
    escape(out, text);
    out.push_str("</span>");
}

fn hex(c: super::Rgb) -> String {
    format!("#{:02x}{:02x}{:02x}", c.0, c.1, c.2)
}

fn escape(out: &mut String, s: &str) {
    for c in s.chars() {
        match c {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '"' => out.push_str("&quot;"),
            c => out.push(c),
        }
    }
}

/// The `HTML Format` clipboard document (CF_HTML, Version 0.9) around `fragment`: a header with
/// the UTF-8 byte offsets of the document and of the fragment.
pub fn cf_html(fragment: &str) -> String {
    const HEADER: &str = "Version:0.9\r\nStartHTML:{sh}\r\nEndHTML:{eh}\r\n\
                          StartFragment:{sf}\r\nEndFragment:{ef}\r\n";
    let pre = "<html><body>\r\n<!--StartFragment-->";
    let post = "<!--EndFragment-->\r\n</body></html>";
    // Every offset is written with 10 digits, so the header's length is known up front.
    let header_len = HEADER
        .replace("{sh}", "0000000000")
        .replace("{eh}", "0000000000")
        .replace("{sf}", "0000000000")
        .replace("{ef}", "0000000000")
        .len();
    let start_html = header_len;
    let start_fragment = start_html + pre.len();
    let end_fragment = start_fragment + fragment.len();
    let end_html = end_fragment + post.len();
    let header = HEADER
        .replace("{sh}", &format!("{start_html:010}"))
        .replace("{eh}", &format!("{end_html:010}"))
        .replace("{sf}", &format!("{start_fragment:010}"))
        .replace("{ef}", &format!("{end_fragment:010}"));
    format!("{header}{pre}{fragment}{post}")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::view::{SelectMode, TermView};

    #[test]
    fn coloured_runs_and_escaping_survive_into_html() {
        let mut v = TermView::new(40, 3, 10);
        v.feed("plain \x1b[1;31mbold red\x1b[0m <a&b> 한글\r\n\x1b[4munder\x1b[0m".as_bytes());
        v.select(0, 0, SelectMode::Cell);
        v.extend_selection(39, 1);
        let lines = v.selected_cells().unwrap();
        assert_eq!(lines.len(), 2);
        let theme = Theme::kim_dark();
        let html = fragment(&lines, &theme, 13.0);
        assert!(
            html.starts_with("<pre style=\"font-family:'Cascadia Mono'"),
            "{html}"
        );
        assert!(html.contains("plain "), "{html}");
        // Bold red uses the bright red (bold is bright) and is bold.
        let red = hex(theme.ansi[9]);
        assert!(
            html.contains(&format!(
                "<span style=\"color:{red};font-weight:bold;\">bold red</span>"
            )),
            "{html}"
        );
        assert!(html.contains("&lt;a&amp;b&gt; 한글"), "{html}");
        assert!(
            html.contains("<span style=\"text-decoration:underline;\">under</span>"),
            "{html}"
        );
        assert!(html.ends_with("</pre>"));
        assert!(!html.contains("한글 "), "trailing blanks are not copied");
    }

    #[test]
    fn cf_html_offsets_point_at_the_document_and_the_fragment() {
        let frag = "<pre>x 한</pre>";
        let doc = cf_html(frag);
        let field = |name: &str| -> usize {
            let at = doc.find(name).unwrap() + name.len();
            doc[at..at + 10].parse().unwrap()
        };
        let (sh, eh, sf, ef) = (
            field("StartHTML:"),
            field("EndHTML:"),
            field("StartFragment:"),
            field("EndFragment:"),
        );
        let b = doc.as_bytes();
        assert!(doc[sh..].starts_with("<html>"));
        assert_eq!(eh, b.len());
        assert_eq!(&b[sf..ef], frag.as_bytes(), "offsets are UTF-8 bytes");
        assert!(doc[..sf].ends_with("<!--StartFragment-->"));
        assert!(doc[ef..].starts_with("<!--EndFragment-->"));
    }
}
