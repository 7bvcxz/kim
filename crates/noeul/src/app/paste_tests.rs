//! The instance core's paste (Step 55; NOEUL-FR-070…NOEUL-FR-074, NOEUL-SEC-002, NOEUL-SEC-008,
//! NOEUL-SEC-009): `F3`, "Pin clipboard as…", `pin.create` from the clipboard and from files, drops
//! and `pin.copy-text`, with fake windows and an in-memory clipboard whose every format the test
//! synthesizes. Nothing here touches the user's clipboard, screen or network.

use super::tests::{FakeShell, app};
use super::*;
use crate::capture::DesktopState;
use crate::paste::{Format, Kind, Snapshot};
use crate::pins::ContentKind;

fn tmp(tag: &str) -> PathBuf {
    let d = std::env::temp_dir().join(format!("noeul-paste-{tag}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&d);
    std::fs::create_dir_all(&d).unwrap();
    d
}

fn png(w: u32, h: u32) -> Vec<u8> {
    crate::decode::tests::encode(
        crate::decode::Format::Png,
        w,
        h,
        &crate::decode::tests::ramp(w, h),
    )
}

fn cf_html(fragment: &str) -> Vec<u8> {
    format!("Version:0.9\r\nStartHTML:-1\r\nEndHTML:-1\r\nStartFragment:-1\r\nEndFragment:-1\r\n<html><body><!--StartFragment-->{fragment}<!--EndFragment--></body></html>").into_bytes()
}

fn with_clip(tag: &str, snap: Snapshot) -> App<FakeShell> {
    let mut a = app(&tmp(&format!("{tag}-app")), DesktopState::Available);
    a.shell.paste = Some(snap);
    a.shell.pointer_at = Some((5, 5));
    a
}

fn content(a: &App<FakeShell>, id: u64) -> crate::pins::Content {
    a.pins.content(PinId(id)).cloned().unwrap()
}

fn list(a: &mut App<FakeShell>) -> Value {
    let v = a.request("pin.list", &json!({})).unwrap();
    let e = crate::schema_errors("noeul-ipc-v1#pin_list_result", &v);
    assert!(e.is_empty(), "{e:?}\n{v:#}");
    v
}

/// Every kind becomes a pin that `pin.list` names by kind only.
#[test]
fn f3_pins_every_kind_of_clipboard_content() {
    let cases: Vec<(&str, Snapshot, ContentKind)> = vec![
        (
            "png",
            Snapshot::default().with(Format::Png, png(12, 8)),
            ContentKind::Image,
        ),
        (
            "dib",
            Snapshot::default().with(Format::DibV5, tests::solid_dib(6, 4)),
            ContentKind::Image,
        ),
        (
            "color",
            Snapshot::default().with_text("rgb(30, 144, 255)"),
            ContentKind::Color,
        ),
        (
            "html",
            Snapshot::default().with(Format::Html, cf_html("<h2>Title</h2><p>Body</p>")),
            ContentKind::Html,
        ),
        (
            "text",
            Snapshot::default().with_text("plain words"),
            ContentKind::Text,
        ),
    ];
    for (tag, snap, kind) in cases {
        let mut a = with_clip(tag, snap);
        a.action("paste.float", None);
        assert_eq!(
            a.status()["last_action"],
            json!({"action": "paste.float", "result": "pinned"}),
            "{tag}: {:?}",
            a.shell.notices
        );
        assert_eq!(a.pins.len(), 1, "{tag}");
        assert_eq!(content(&a, 1).kind, kind, "{tag}");
        let l = list(&mut a);
        assert_eq!(l["pins"][0]["content"], kind.as_str(), "{tag}");
        assert_eq!(l["pins"][0]["source"], "clipboard");
        let (_, _, focus) = a.shell.windows.log.borrow().created[0];
        assert!(focus, "{tag}: F3 is the user's own gesture");
    }
    // A PNG keeps its exact pixels and size at 100 %.
    let mut a = with_clip(
        "png-exact",
        Snapshot::default().with(Format::Png, png(12, 8)),
    );
    a.action("paste.float", None);
    let pic = a.pins.picture(PinId(1)).unwrap();
    assert_eq!((pic.width(), pic.height()), (12, 8));
    assert_eq!(a.pins.info(PinId(1)).unwrap().zoom, 100);
}

/// HTML before text; the menu offers the other kinds; choosing one reads the clipboard again,
/// once, and pins that kind.
#[test]
fn the_format_priority_and_pin_clipboard_as() {
    let snap = Snapshot::default()
        .with_text("Hello there")
        .with(Format::Html, cf_html("<b>Hello</b> there"));
    let mut a = with_clip("as", snap);
    a.action("paste.float", None);
    assert_eq!(content(&a, 1).kind, ContentKind::Html);
    assert_eq!(content(&a, 1).offered, [Kind::Html, Kind::Text]);
    let items = a.pin_menu_items(PinId(1));
    let labels: Vec<&str> = items.iter().map(|i| i.label.as_str()).collect();
    assert!(
        labels.contains(&"Pin clipboard as: Plain text"),
        "{labels:?}"
    );
    assert!(
        !labels.iter().any(|l| l.contains("HTML")),
        "not the kind it already is"
    );
    let copy_text = items
        .iter()
        .find(|i| i.action == Some("pin.copy-text"))
        .unwrap();
    assert!(copy_text.enabled, "a card has text to copy");
    let reads = a.shell.clipboard_reads;
    a.pin_menu_choice(PinId(1), "paste.as-text");
    assert_eq!(
        a.shell.clipboard_reads,
        reads + 1,
        "read once more, on request"
    );
    assert_eq!(a.pins.len(), 2);
    assert_eq!(content(&a, 2).kind, ContentKind::Text);
    // An image pin has no text: no Copy text.
    let mut b = with_clip("img", Snapshot::default().with(Format::Png, png(4, 4)));
    b.action("paste.float", None);
    assert!(
        b.pin_menu_items(PinId(1))
            .iter()
            .all(|i| i.action != Some("pin.copy-text"))
    );
    b.action("pin.copy-text", Some(PinId(1)));
    assert_eq!(b.status()["last_action"]["result"], "no_text");
}

/// Several image files at once (an Explorer copy), a non-image and a missing file among them:
/// the images are pinned, cascading; the others are counted; a notice says so.
#[test]
fn copied_image_files_are_pinned_and_others_counted() {
    let dir = tmp("files");
    let mut paths = Vec::new();
    for (i, f) in [
        crate::decode::Format::Png,
        crate::decode::Format::Bmp,
        crate::decode::Format::Tiff,
    ]
    .into_iter()
    .enumerate()
    {
        let p = dir.join(format!("p{i}.{}", f.as_str().to_ascii_lowercase()));
        std::fs::write(
            &p,
            crate::decode::tests::encode(f, 5, 3, &crate::decode::tests::ramp(5, 3)),
        )
        .unwrap();
        paths.push(p);
    }
    let gif = dir.join("anim.gif");
    let mut anim = Vec::new();
    {
        let mut enc = image::codecs::gif::GifEncoder::new(&mut anim);
        for _ in 0..4 {
            let buf = image::RgbaImage::from_raw(2, 2, [9u8, 8, 7, 255].repeat(4)).unwrap();
            enc.encode_frame(image::Frame::new(buf)).unwrap();
        }
    }
    std::fs::write(&gif, anim).unwrap();
    paths.push(gif);
    let txt = dir.join("notes.txt");
    std::fs::write(&txt, "not an image").unwrap();
    paths.push(txt);
    paths.push(dir.join("missing.png"));
    let mut a = with_clip("files", Snapshot::default().with_files(&paths));
    let r = a
        .request("pin.create", &json!({"source": "clipboard"}))
        .unwrap();
    let e = crate::schema_errors("noeul-ipc-v1#pin_create_result", &r);
    assert!(e.is_empty(), "{e:?}\n{r:#}");
    assert_eq!(r["kind"], "image_file");
    assert_eq!(r["pins"].as_array().unwrap().len(), 4, "{r:#}");
    assert_eq!(
        (r["skipped"].as_u64(), r["failed"].as_u64()),
        (Some(1), Some(1))
    );
    let rects: Vec<Rect> = a
        .pins
        .ids()
        .iter()
        .map(|id| a.pins.rect(*id).unwrap())
        .collect();
    assert!(
        rects[1].x > rects[0].x && rects[1].y > rects[0].y,
        "cascaded"
    );
    assert!(
        a.shell
            .notices
            .iter()
            .any(|n| n.starts_with("Pinned 4 of 6 files")),
        "{:?}",
        a.shell.notices
    );
    assert!(
        a.shell.notices.iter().any(|n| n.contains("4 frames")),
        "the GIF's first frame, said so"
    );
    let l = list(&mut a);
    assert_eq!(l["pins"][3]["animation"]["frames"], 4);
    // Nothing names a path.
    let all = format!("{r}{l}{:?}{}", a.shell.notices, a.status());
    assert!(!all.contains("p0.png") && !all.contains("notes"), "{all}");
    let _ = std::fs::remove_dir_all(&dir);
}

/// A copied path (Explorer's "Copy as path") pins the file; network paths are never read.
#[test]
fn a_copied_path_pins_its_image_file_and_network_paths_are_text() {
    let dir = tmp("path");
    let p = dir.join("shot.png");
    std::fs::write(&p, png(7, 7)).unwrap();
    let text = format!("\"{}\"", p.display());
    let mut a = with_clip("path", Snapshot::default().with_text(&text));
    a.action("paste.float", None);
    assert_eq!(a.pins.len(), 1);
    assert_eq!(a.pins.picture(PinId(1)).unwrap().width(), 7);
    assert_eq!(a.shell.files_read, [p]);
    let mut far = with_clip(
        "unc",
        Snapshot::default().with_text(r"\\nas\share\shot.png"),
    );
    far.action("paste.float", None);
    assert!(
        far.shell.files_read.is_empty(),
        "a network path is never opened"
    );
    assert_eq!(
        content(&far, 1).kind,
        ContentKind::Text,
        "pinned as its text"
    );
    let _ = std::fs::remove_dir_all(&dir);
}

/// FLOW: an empty, a held and a private clipboard change nothing and say why.
#[test]
fn empty_busy_and_private_clipboards_pin_nothing() {
    let mut a = with_clip("empty", Snapshot::default());
    a.action("paste.float", None);
    assert!(a.pins.is_empty());
    assert_eq!(a.shell.notices, ["Nothing to pin."]);
    assert_eq!(a.status()["last_action"]["result"], "nothing_to_pin");
    let mut b = with_clip("busy", Snapshot::default().with_text("x"));
    b.shell.clipboard_busy = true;
    b.action("paste.float", None);
    assert!(b.pins.is_empty());
    assert!(b.shell.notices[0].contains("in use by another program"));
    assert_eq!(b.status()["last_action"]["result"], "clipboard_unavailable");
    let secret = Snapshot::default()
        .with_text("correct horse battery staple")
        .with_marker("ExcludeClipboardContentFromMonitorProcessing", b"");
    let mut c = with_clip("private", secret);
    c.action("paste.float", None);
    assert!(c.pins.is_empty());
    assert!(c.shell.notices[0].contains("marked private"));
    assert!(
        c.shell.paste.as_ref().unwrap().reads.is_empty(),
        "nothing was read"
    );
    let e = c
        .request("pin.create", &json!({"source": "clipboard"}))
        .unwrap_err();
    assert_eq!(e.code, Code::NothingToPin);
    // While the overlay is open, F3 does not even read.
    let mut d = with_clip("overlay", Snapshot::default().with_text("x"));
    d.overlay = None;
    let reads = d.shell.clipboard_reads;
    d.capture_region(Origin::Cli, Instant::now(), false)
        .unwrap();
    d.action("paste.float", None);
    assert_eq!(d.shell.clipboard_reads, reads);
}

/// Malformed, oversized and decompression-bomb pictures are refused; nothing is left behind.
#[test]
fn invalid_and_oversized_pictures_pin_nothing_and_leak_nothing() {
    let bomb = crate::decode::tests::png_claiming(30_000, 30_000);
    let mut broken = png(8, 8);
    broken.truncate(30);
    for (tag, snap) in [
        ("bomb", Snapshot::default().with(Format::Png, bomb)),
        ("broken", Snapshot::default().with(Format::Png, broken)),
        (
            "not-png",
            Snapshot::default().with(Format::Png, b"<html>not a png</html>".to_vec()),
        ),
        (
            "dib",
            Snapshot::default().with(Format::Dib, vec![40, 0, 0, 0, 1, 2, 3]),
        ),
        (
            "huge",
            Snapshot::default().with(
                Format::Png,
                vec![0; usize::try_from(crate::decode::MAX_INPUT_BYTES).unwrap() + 1],
            ),
        ),
    ] {
        let mut a = with_clip(tag, snap);
        a.action("paste.float", None);
        assert!(a.pins.is_empty(), "{tag}");
        assert_eq!(
            a.status()["last_action"]["result"],
            "image_invalid",
            "{tag}: {:?}",
            a.shell.notices
        );
        assert!(a.shell.notices[0].contains("could not be read"), "{tag}");
        assert_eq!(a.ledger.live(), (0, 0), "{tag}: no partial picture");
    }
}

/// HTML naming remote images: placeholders, a notice, no file or network read of any kind.
#[test]
fn remote_html_images_are_placeholders_and_nothing_is_fetched() {
    let html = cf_html(
        "<p>Logo: <img src=\"https://example.com/logo.png\" alt=\"logo\"> \
         <img src=\"file:///C:/Windows/win.ini\"> <img src=\"\\\\\\\\server\\\\share\\\\a.png\"></p>",
    );
    let mut a = with_clip("remote", Snapshot::default().with(Format::Html, html));
    a.action("paste.float", None);
    assert_eq!(a.pins.len(), 1);
    assert!(
        a.shell.files_read.is_empty(),
        "no file is opened for an <img>"
    );
    assert!(
        a.shell.notices.iter().any(|n| n.contains("placeholders")),
        "{:?}",
        a.shell.notices
    );
    let text = content(&a, 1).text.unwrap();
    assert!(
        !text.contains("example.com") && !text.contains("win.ini"),
        "{text}"
    );
}

/// `pin.copy-text` puts the card's text back (all of it, also beyond what the card shows).
#[test]
fn copy_text_returns_the_cards_text() {
    let long = "line\n".repeat(400);
    let mut a = with_clip("copy", Snapshot::default().with_text(&long));
    a.action("paste.float", None);
    assert!(a.shell.notices.iter().any(|n| n.contains("long")));
    a.action("pin.copy-text", Some(PinId(1)));
    assert_eq!(a.shell.copied_text, [long]);
    a.shell.clipboard_busy = true;
    a.action("pin.copy-text", Some(PinId(1)));
    assert_eq!(a.status()["last_action"]["result"], "clipboard_unavailable");
    // A colour card copies its HEX value; an HTML card its sanitized text.
    let mut c = with_clip("hex", Snapshot::default().with_text("#1e90ff"));
    c.action("paste.float", None);
    c.action("pin.copy-text", Some(PinId(1)));
    assert_eq!(c.shell.copied_text, ["#1E90FF"]);
    let mut h = with_clip(
        "html-text",
        Snapshot::default().with(
            Format::Html,
            cf_html("<p>a <script>x()</script><b>b</b></p><ul><li>c</li></ul>"),
        ),
    );
    h.action("paste.float", None);
    h.action("pin.copy-text", Some(PinId(1)));
    assert_eq!(h.shell.copied_text, ["a b\n\u{2022} c"]);
}

/// A card is a pin like any other: it transforms, closes, reopens with its text and is copied
/// as a picture.
#[test]
fn cards_transform_and_close_like_pins() {
    let mut a = with_clip("card-pin", Snapshot::default().with_text("card"));
    a.action("paste.float", None);
    let before = a.pins.info(PinId(1)).unwrap();
    a.action("pin.rotate-cw", Some(PinId(1)));
    a.action("pin.opacity-down", Some(PinId(1)));
    let after = a.pins.info(PinId(1)).unwrap();
    assert_eq!(
        (after.rect.width, after.rect.height),
        (before.rect.height, before.rect.width),
        "turned"
    );
    assert_eq!(after.transforms.opacity, 90);
    a.action("pin.copy", Some(PinId(1)));
    assert_eq!(a.shell.copies.len(), 1, "copied as a picture");
    a.action("pin.close", Some(PinId(1)));
    assert!(a.pins.is_empty());
    a.request("pin.reopen", &json!({})).unwrap();
    assert_eq!(content(&a, 1).kind, ContentKind::Text);
    a.action("pin.copy-text", Some(PinId(1)));
    assert_eq!(a.shell.copied_text, ["card"]);
    a.action("pin.destroy", Some(PinId(1)));
    assert!(a.pins.is_empty() && a.pins.reopenable().is_none());
    assert_eq!(a.ledger.live(), (0, 0));
}

/// Files dropped on a pin are pinned where they were dropped (NOEUL-FR-073).
#[test]
fn files_dropped_on_a_pin_are_pinned_there() {
    let dir = tmp("drop");
    let p = dir.join("d.png");
    std::fs::write(&p, png(3, 3)).unwrap();
    let mut a = with_clip("drop", Snapshot::default().with(Format::Png, png(4, 4)));
    a.action("paste.float", None);
    a.pin_event(PinEvent::Files(
        PinId(1),
        vec![p, dir.join("x.txt")],
        (40, 30),
    ));
    assert_eq!(a.pins.len(), 2);
    assert_eq!(a.pins.rect(PinId(2)).unwrap(), Rect::new(40, 30, 3, 3));
    assert_eq!(a.pins.info(PinId(2)).unwrap().source, Source::File);
    assert_eq!(
        a.status()["last_action"],
        json!({"action": "pin.drop-files", "result": "pinned"})
    );
    // Only non-images: nothing, said so.
    a.pin_event(PinEvent::Files(PinId(1), vec![dir.join("x.txt")], (1, 1)));
    assert_eq!(a.pins.len(), 2);
    assert_eq!(a.status()["last_action"]["result"], "nothing_to_pin");
    let _ = std::fs::remove_dir_all(&dir);
}

/// `pin.create` with a file: a dry run reads nothing; a network path is a usage error; the result
/// names no path.
#[test]
fn pin_create_from_a_file() {
    let dir = tmp("cli-file");
    let p = dir.join("c.png");
    std::fs::write(&p, png(9, 6)).unwrap();
    let mut a = with_clip("cli-file", Snapshot::default());
    let path = p.display().to_string();
    let dry = a
        .request(
            "pin.create",
            &json!({"source": "file", "path": path, "dry_run": true}),
        )
        .unwrap();
    assert_eq!(dry["pin"], Value::Null);
    assert!(a.shell.files_read.is_empty(), "a dry run reads nothing");
    let r = a
        .request(
            "pin.create",
            &json!({"source": "file", "path": path, "at": {"x": 20, "y": 10}}),
        )
        .unwrap();
    let e = crate::schema_errors("noeul-ipc-v1#pin_create_result", &r);
    assert!(e.is_empty(), "{e:?}");
    assert_eq!(r["pin"]["source"], "file");
    assert_eq!(
        r["pin"]["rect"],
        json!({"x": 20, "y": 10, "width": 9, "height": 6})
    );
    assert!(!r.to_string().contains("c.png"), "no path in the result");
    assert!(
        a.shell
            .notices
            .contains(&"Noeul pinned a file (command line).".to_string())
    );
    for bad in [r"\\server\share\c.png", "c.png", ""] {
        let e = a
            .request("pin.create", &json!({"source": "file", "path": bad}))
            .unwrap_err();
        assert_eq!(e.code, Code::Usage, "{bad}");
    }
    let e = a
        .request("pin.create", &json!({"source": "clipboard", "as": "pdf"}))
        .unwrap_err();
    assert_eq!(e.code, Code::Usage);
    let _ = std::fs::remove_dir_all(&dir);
}

/// Secrets never reach notices, the status, the pin list or a result (NOEUL-SEC-008).
#[test]
fn pasted_content_never_appears_in_reports() {
    const MARK: &str = "s3cr3t-Pa55w0rd-marker";
    for snap in [
        Snapshot::default().with_text(MARK),
        Snapshot::default().with(Format::Html, cf_html(&format!("<p>{MARK}</p>"))),
        Snapshot::default().with_files(&[PathBuf::from(format!(r"C:\{MARK}\a.docx"))]),
    ] {
        let mut a = with_clip("secret", snap);
        a.settings.settings.paste.file_cards = true;
        let r = a
            .request("pin.create", &json!({"source": "clipboard"}))
            .unwrap();
        a.action("paste.float", None);
        let all = format!(
            "{r}{}{}{:?}",
            a.status(),
            a.request("pin.list", &json!({})).unwrap(),
            a.shell.notices
        );
        assert!(!all.contains(MARK), "{all}");
        assert!(!format!("{:?}", a.pins).contains(MARK), "Debug output");
    }
}
