//! The sanitizer (Step 55, NOEUL-SEC-009): structure and formatting kept, everything active or
//! remote dropped, hostile input bounded.

use super::*;
use crate::card::{BlockKind, Doc};

fn text_of(doc: &Doc) -> String {
    doc.plain_text()
}

#[test]
fn the_fragment_comes_from_the_cf_html_header_or_its_markers() {
    let body = "<html><body><!--StartFragment--><b>Hi</b><!--EndFragment--></body></html>";
    let header_len = "Version:0.9\r\nStartHTML:0000000000\r\nEndHTML:0000000000\r\nStartFragment:0000000000\r\nEndFragment:0000000000\r\n".len();
    let s = header_len + body.find("<b>").unwrap();
    let e = header_len + body.find("<!--EndFragment").unwrap();
    let cf = format!(
        "Version:0.9\r\nStartHTML:{:010}\r\nEndHTML:{:010}\r\nStartFragment:{s:010}\r\nEndFragment:{e:010}\r\n{body}",
        header_len,
        header_len + body.len()
    );
    assert_eq!(fragment(cf.as_bytes()), "<b>Hi</b>");
    // Offsets of -1 (some browsers): the comment markers.
    let cf = format!(
        "Version:0.9\r\nStartHTML:-1\r\nEndHTML:-1\r\nStartFragment:-1\r\nEndFragment:-1\r\n{body}"
    );
    assert_eq!(fragment(cf.as_bytes()), "<b>Hi</b>");
    // Offsets outside the data are ignored.
    let cf = format!("Version:0.9\r\nStartFragment:5\r\nEndFragment:999999\r\n{body}");
    assert_eq!(fragment(cf.as_bytes()), "<b>Hi</b>");
    // No header at all: everything.
    assert_eq!(fragment(b"<i>x</i>"), "<i>x</i>");
    let d = parse_cf_html(cf.as_bytes());
    assert_eq!(text_of(&d), "Hi");
    assert!(d.blocks[0].runs[0].style.bold);
}

#[test]
fn structure_and_basic_formatting_are_kept() {
    let d = parse(
        "<h1>Title</h1><p>One <b>bold</b> and <i>italic</i>, <u>under</u> <s>gone</s> \
         <code>x()</code>.</p><ul><li>a</li><li>b<ol start=3><li>c</li><li>d</li></ol></li></ul>\
         <blockquote>said</blockquote><pre>  keep   spaces\nline 2</pre><hr>\
         <table><tr><th>H1</th><th>H2</th></tr><tr><td>1</td><td>2</td></tr></table>\
         <p>break<br>here &amp; &lt;there&gt; &#x1F600; &#169;</p>",
    );
    let kinds: Vec<&BlockKind> = d.blocks.iter().map(|b| &b.kind).collect();
    assert_eq!(kinds[0], &BlockKind::Heading(1));
    assert_eq!(kinds[1], &BlockKind::Paragraph);
    assert_eq!(kinds[2], &BlockKind::Item("\u{2022}".into()));
    assert_eq!(kinds[3], &BlockKind::Item("\u{2022}".into()));
    assert_eq!(kinds[4], &BlockKind::Item("3.".into()));
    assert_eq!(kinds[5], &BlockKind::Item("4.".into()));
    assert_eq!(d.blocks[4].indent, 2, "a nested list is indented twice");
    assert!(d.blocks[6].quote, "the quotation has its bar");
    assert_eq!(kinds[7], &BlockKind::Preformatted);
    assert_eq!(d.blocks[7].runs[0].text, "  keep   spaces\nline 2");
    assert!(d.blocks[7].runs[0].style.mono);
    assert_eq!(kinds[8], &BlockKind::Rule);
    let p = &d.blocks[1];
    let styled = |t: &str| p.runs.iter().find(|r| r.text == t).unwrap().style;
    assert!(styled("bold").bold);
    assert!(styled("italic").italic);
    assert!(styled("under").underline);
    assert!(styled("gone").strike);
    assert!(styled("x()").mono);
    let text = text_of(&d);
    assert!(text.contains("H1\u{2003}\u{2003}H2"), "{text}");
    assert!(text.contains("1\u{2003}\u{2003}2"), "{text}");
    assert!(text.contains("break\nhere & <there> \u{1F600} ©"), "{text}");
    assert!(!d.truncated);
}

/// Scripts, styles, frames, objects, forms, comments and processing instructions leave no
/// trace; event handlers and `javascript:` links are just dropped attributes.
#[test]
fn active_content_is_dropped_with_everything_inside() {
    let d = parse(
        "<p>before</p><script>alert('x')</script><style>p{display:none}</style>\
         <SCRIPT type=module>import('//evil')</SCRIPT><!-- secret comment -->\
         <iframe src=https://evil.example>frame text</iframe><object data=x.swf>obj</object>\
         <svg><text>svg text</text><script>bad()</script></svg><noscript>ns</noscript>\
         <template><b>tpl</b></template><select><option>opt</option></select>\
         <textarea>ta</textarea><?php echo 1 ?><!DOCTYPE html>\
         <p onclick=\"steal()\" onmouseover='x'>after <a href=\"javascript:alert(1)\">link</a></p>\
         <script>never closed",
    );
    let text = text_of(&d);
    assert_eq!(text, "before\nafter link", "{text:?}");
    for bad in [
        "alert",
        "evil",
        "secret",
        "frame text",
        "obj",
        "svg text",
        "bad",
        "ns",
        "tpl",
        "opt",
        "ta",
        "php",
        "steal",
        "javascript",
    ] {
        assert!(!text.contains(bad), "{bad} leaked: {text}");
    }
}

/// Images are placeholders: nothing is requested, local or remote; the URL is not even kept.
#[test]
fn images_become_placeholders_and_their_addresses_are_never_kept() {
    let d = parse(
        "<p>A <img src=\"https://tracker.example/pixel.gif?id=1\" alt=\"logo\"> B \
         <img src='file:///C:/Windows/win.ini'> C <img src=\"data:image/png;base64,iVBORw0KGgo=\" \
         alt=\"\"> <img srcset=\"//cdn.example/a.png 2x\"></p>\
         <div style=\"background:url(https://evil.example/bg.png)\">D</div>\
         <link rel=stylesheet href=https://evil.example/s.css><img src=x onerror=alert(1)>",
    );
    assert_eq!(d.placeholders, 5);
    let text = text_of(&d);
    for gone in [
        "tracker", "http", "file:", "win.ini", "data:", "cdn", "evil", "onerror",
    ] {
        assert!(!text.contains(gone), "{gone}: {text}");
    }
    let shown: String = d
        .blocks
        .iter()
        .flat_map(|b| b.runs.iter())
        .filter(|r| r.style.placeholder)
        .map(|r| r.text.as_str())
        .collect();
    assert!(shown.contains("[image: logo]"), "{shown}");
    assert!(shown.contains("[image]"), "{shown}");
    assert!(text.contains('D'));
    assert_eq!(d.background, None, "a url() background is ignored");
}

/// CSS that hides or moves text is ignored; only the supported properties apply.
#[test]
fn css_tricks_cannot_hide_or_move_text() {
    let d = parse(
        "<p><span style=\"display:none\">hidden1</span>\
         <span style=\"visibility:hidden;position:absolute;left:-9999px\">hidden2</span>\
         <span style=\"font-size:0\">hidden3</span>\
         <span style=\"opacity:0\">hidden4</span>\
         <span style=\"color:transparent\">hidden5</span>\
         <span style=\"color: rgba(0,0,0,0)\">hidden6</span>\
         <span style=\"color:#ff0000; font-weight:700; font-style:italic; \
          text-decoration: underline line-through\">red</span>\
         <span style=\"color:expression(alert(1))\">expr</span>\
         <span style=\"font-family: 'Fira Code', monospace\">mono</span></p>",
    );
    let text = text_of(&d);
    for t in [
        "hidden1", "hidden2", "hidden3", "hidden4", "hidden5", "hidden6",
    ] {
        assert!(text.contains(t), "{t} must stay visible: {text}");
    }
    let runs = &d.blocks[0].runs;
    let style = |t: &str| runs.iter().find(|r| r.text.contains(t)).unwrap().style;
    assert_eq!(style("hidden5").color, None, "transparent is no colour");
    assert_eq!(style("hidden6").color, None);
    let red = style("red");
    assert_eq!(red.color, Some([255, 0, 0]));
    assert!(red.bold && red.italic && red.underline && red.strike);
    assert_eq!(style("expr").color, None);
    assert!(style("mono").mono);
}

/// An editor's copy (inline colours, `white-space: pre`, a dark background) keeps its syntax
/// highlighting and its background.
#[test]
fn an_editors_syntax_highlighting_survives() {
    let d = parse(
        "<div style=\"color: #d4d4d4;background-color: #1e1e1e;font-family: Consolas, 'Courier New', \
         monospace;font-weight: normal;font-size: 14px;line-height: 19px;white-space: pre;\">\
         <div><span style=\"color: #569cd6;\">fn</span><span style=\"color: #d4d4d4;\"> </span>\
         <span style=\"color: #dcdcaa;\">main</span><span style=\"color: #d4d4d4;\">() {</span></div>\
         <div><br></div><div><span style=\"color: #d4d4d4;\">    x</span></div></div>",
    );
    assert_eq!(d.background, Some([0x1E, 0x1E, 0x1E]));
    assert_eq!(d.blocks.len(), 3, "{:#?}", d.blocks);
    assert!(d.blocks.iter().all(|b| b.kind == BlockKind::Preformatted));
    let fn_ = d.blocks[0]
        .runs
        .iter()
        .find(|r| r.text == "fn")
        .unwrap()
        .style;
    assert_eq!(fn_.color, Some([0x56, 0x9C, 0xD6]));
    assert!(fn_.mono);
    assert_eq!(d.blocks[2].runs[0].text, "    x", "indentation kept");
    assert_eq!(text_of(&d), "fn main() {\n\n    x");
}

/// A hundred thousand nested elements: one linear pass, the text kept, no stack overflow.
#[test]
fn huge_nesting_and_huge_input_are_bounded() {
    let deep = "<div><b><i>".repeat(100_000) + "deep" + &"</i></b></div>".repeat(100_000);
    let started = std::time::Instant::now();
    let d = parse(&deep);
    assert!(text_of(&d).contains("deep"));
    assert!(
        started.elapsed() < std::time::Duration::from_secs(5),
        "linear: {:?}",
        started.elapsed()
    );
    // Unclosed tags and stray end tags.
    let d = parse("</p></div><b>open <i>never closed <u>at all");
    assert_eq!(text_of(&d), "open never closed at all");
    // Endless text is cut at the card's limit and says so.
    let long = "<p>".to_string() + &"word ".repeat(10_000) + "</p>";
    let d = parse(&long);
    assert!(d.truncated);
    assert!(d.chars() <= MAX_CHARS);
    // Endless paragraphs: the block limit.
    let many = "<p>x</p>".repeat(MAX_BLOCKS + 50);
    let d = parse(&many);
    assert!(d.blocks.len() <= MAX_BLOCKS);
    assert!(d.truncated);
    // Malformed attributes and a lone '<'.
    let d = parse("1 < 2 <3 <p =x>y");
    assert_eq!(text_of(&d), "1 < 2 <3\ny");
    // An unterminated quote swallows the rest, as in a browser: no text, no crash.
    let d = parse("<p a=\"unterminated>text</p>");
    assert_eq!(text_of(&d), "");
}

#[test]
fn entities_and_spoofing_characters_are_handled() {
    assert_eq!(decode_entities("a &amp; b"), "a & b");
    assert_eq!(decode_entities("&lt;&gt;&quot;&apos;&nbsp;"), "<>\"'\u{A0}");
    assert_eq!(decode_entities("&#65;&#x42;&#X43;"), "ABC");
    assert_eq!(
        decode_entities("&#0; &#xD800; &#1114112;"),
        "\u{FFFD} \u{FFFD} \u{FFFD}"
    );
    assert_eq!(decode_entities("&unknown; & &;"), "&unknown; & &;");
    assert_eq!(decode_entities("&amp"), "&amp", "named references need ';'");
    assert_eq!(decode_entities("&#65"), "A", "numeric ones do not");
    // Bidirectional overrides that reorder what is shown are removed (text spoofing).
    let d = parse("<p>file\u{202E}gpj.exe and \u{2066}x\u{2069} \u{0007}bell</p>");
    assert_eq!(text_of(&d), "filegpj.exe and x bell");
}

#[test]
fn colours_parse_like_css_but_never_translucent() {
    assert_eq!(css_color("#fff"), Some([255, 255, 255]));
    assert_eq!(css_color(" rgb(1, 2, 3) !important"), Some([1, 2, 3]));
    assert_eq!(css_color("Navy"), Some([0, 0, 128]));
    assert_eq!(css_color("transparent"), None);
    assert_eq!(css_color("rgba(0,0,0,0.1)"), None);
    assert_eq!(css_color("#00000080"), None);
    assert_eq!(css_color("currentColor"), None);
}
