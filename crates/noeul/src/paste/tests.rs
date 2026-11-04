//! What a paste takes (Step 55; NOEUL-FR-070, NOEUL-FR-071, NOEUL-FR-074, NOEUL-SEC-002): every
//! clipboard here is an in-memory snapshot whose formats the test synthesizes.

use super::*;

fn png() -> Vec<u8> {
    crate::decode::tests::encode(
        crate::decode::Format::Png,
        2,
        2,
        &crate::decode::tests::ramp(2, 2),
    )
}

fn cf_html(fragment: &str) -> Vec<u8> {
    format!("Version:0.9\r\nStartHTML:-1\r\nEndHTML:-1\r\nStartFragment:-1\r\nEndFragment:-1\r\n<html><body><!--StartFragment-->{fragment}<!--EndFragment--></body></html>").into_bytes()
}

#[test]
fn colors_parse_in_every_documented_notation_and_nothing_else() {
    let c = |s: &str| parse_color(s);
    for (text, want) in [
        ("#F80", Rgba::rgb(0xFF, 0x88, 0x00)),
        ("#ff8800", Rgba::rgb(0xFF, 0x88, 0x00)),
        ("  #1E1E1E\t", Rgba::rgb(0x1E, 0x1E, 0x1E)),
        (
            "#FF880080",
            Rgba {
                a: 0x80,
                ..Rgba::rgb(0xFF, 0x88, 0x00)
            },
        ),
        (
            "#F808",
            Rgba {
                a: 0x88,
                ..Rgba::rgb(0xFF, 0x88, 0x00)
            },
        ),
        ("rgb(255, 136, 0)", Rgba::rgb(255, 136, 0)),
        ("RGB(255 136 0)", Rgba::rgb(255, 136, 0)),
        ("rgb(100%, 50%, 0%)", Rgba::rgb(255, 128, 0)),
        (
            "rgba(255, 136, 0, 0.5)",
            Rgba {
                a: 128,
                ..Rgba::rgb(255, 136, 0)
            },
        ),
        (
            "rgb(255 136 0 / 25%)",
            Rgba {
                a: 64,
                ..Rgba::rgb(255, 136, 0)
            },
        ),
        ("255, 136, 0", Rgba::rgb(255, 136, 0)),
        ("255 136 0", Rgba::rgb(255, 136, 0)),
        ("(255;136;0)", Rgba::rgb(255, 136, 0)),
        ("0, 0, 1", Rgba::rgb(0, 0, 1)),
        ("1.0, 0.5, 0", Rgba::rgb(255, 128, 0)),
        ("0.0 0.0 1.0", Rgba::rgb(0, 0, 255)),
        // Step 59: every format the colour picker copies comes back.
        ("hsl(39, 100%, 50%)", Rgba::rgb(255, 166, 0)),
        ("HSL(39deg 100% 50%)", Rgba::rgb(255, 166, 0)),
        ("hsl(399, 100%, 50%)", Rgba::rgb(255, 166, 0)),
        ("hsl(0, 0%, 100%)", Rgba::rgb(255, 255, 255)),
        (
            "hsla(240, 100%, 50%, 0.502)",
            Rgba {
                a: 128,
                ..Rgba::rgb(0, 0, 255)
            },
        ),
        (
            "hsl(240 100% 50% / 25%)",
            Rgba {
                a: 64,
                ..Rgba::rgb(0, 0, 255)
            },
        ),
        ("rgba(58, 110, 165, 1)", Rgba::rgb(58, 110, 165)),
        ("#3a6ea5ff", Rgba::rgb(58, 110, 165)),
    ] {
        assert_eq!(c(text), Some(want), "{text:?}");
    }
    for text in [
        "",
        "#",
        "#12",
        "#12345",
        "#GGGGGG",
        "ff8800",
        "255, 136",
        "255, 136, 0, 4",
        "256, 0, 0",
        "-1, 0, 0",
        "1.5, 0, 0",
        "0.5, 0.5, NaN",
        "rgb(300, 0, 0)",
        "rgb(1, 2)",
        "rgba(1, 2, 3, 2)",
        "hsl(10, 50, 50)",
        "hsl(10, 50%, 101%)",
        "hsl(10, 50%)",
        "hsl(x, 50%, 50%)",
        "hsla(10, 50%, 50%, 1.5)",
        "hello world",
        "#FF8800\n#000000",
        "1, 2, 3 apples",
        "12345678901234567890, 0, 0",
    ] {
        assert_eq!(c(text), None, "{text:?}");
    }
    assert_eq!(Rgba::rgb(255, 136, 0).hex(), "#FF8800");
    assert_eq!(Rgba::rgb(255, 136, 0).css(), "rgb(255, 136, 0)");
    let half = Rgba {
        a: 128,
        ..Rgba::rgb(1, 2, 3)
    };
    assert_eq!(half.hex(), "#01020380");
    assert_eq!(half.css(), "rgba(1, 2, 3, 0.50)");
}

#[test]
fn copied_paths_are_local_image_files_only() {
    for (text, want) in [
        (
            r"C:\Users\me\Pictures\cat.png",
            r"C:\Users\me\Pictures\cat.png",
        ),
        (
            r#""C:\Users\me\My Pictures\cat.JPG""#,
            r"C:\Users\me\My Pictures\cat.JPG",
        ),
        ("  D:/shots/a.gif  ", "D:/shots/a.gif"),
        ("file:///C:/Users/me/a%20b.tif", r"C:\Users\me\a b.tif"),
        ("FILE://localhost/C:/x.bmp", r"C:\x.bmp"),
    ] {
        assert_eq!(copied_path(text), Some(PathBuf::from(want)), "{text}");
    }
    for text in [
        r"\\server\share\cat.png",
        r"\\?\UNC\server\share\cat.png",
        r"\\?\C:\cat.png",
        r"\\.\PhysicalDrive0",
        r"cat.png",
        r".\cat.png",
        "file://server/share/cat.png",
        "https://example.com/cat.png",
        r"C:\Users\me\notes.txt",
        r"C:\Users\me\cat",
        "C:\\a.png\nC:\\b.png",
        "",
    ] {
        assert_eq!(copied_path(text), None, "{text}");
    }
    assert!(is_local_path(Path::new(r"C:\a.png")));
    assert!(!is_local_path(Path::new(r"\\host\a.png")));
}

/// NOEUL-FR-071's order: image, image file, color, HTML, text; another kind only when asked.
#[test]
fn the_format_priority_is_image_file_color_html_text() {
    let o = Options::default();
    // A browser's "Copy image": the bitmap, its PNG and an HTML <img>.
    let mut browser = Snapshot::default()
        .with(Format::Png, png())
        .with(Format::Dib, vec![0; 40])
        .with(Format::Html, cf_html("<img src=https://example.com/a.png>"));
    let t = take(&mut browser, None, o).unwrap();
    assert_eq!(t.kind, Kind::Image);
    assert_eq!(t.offered, [Kind::Image, Kind::Html]);
    assert!(matches!(
        t.payload,
        Payload::Image {
            format: Format::Png,
            ..
        }
    ));
    assert_eq!(browser.reads.get(&Format::Html), None, "HTML not even read");
    assert_eq!(
        browser.reads.get(&Format::Dib),
        None,
        "only the chosen format"
    );
    // A color as text with HTML (an editor's copy): the color wins over HTML.
    let mut color = Snapshot::default()
        .with_text("#1E90FF")
        .with(Format::Html, cf_html("<span>#1E90FF</span>"));
    let t = take(&mut color, None, o).unwrap();
    assert_eq!(t.offered, [Kind::Color, Kind::Html, Kind::Text]);
    assert!(matches!(t.payload, Payload::Color(c) if c == Rgba::rgb(0x1E, 0x90, 0xFF)));
    // Pin as… text instead.
    let t = take(&mut color, Some(Kind::Text), o).unwrap();
    assert!(matches!(t.payload, Payload::Text(ref s) if s == "#1E90FF"));
    let t = take(&mut color, Some(Kind::Html), o).unwrap();
    assert!(matches!(t.payload, Payload::Html(_)));
    // Asking for what is not there says what is.
    assert_eq!(
        take(&mut color, Some(Kind::Image), o).unwrap_err(),
        Refusal::NotOffered(Kind::Image, vec![Kind::Color, Kind::Html, Kind::Text])
    );
    // HTML before text.
    let mut rich = Snapshot::default()
        .with_text("Hello")
        .with(Format::Html, cf_html("<b>Hello</b>"));
    assert_eq!(take(&mut rich, None, o).unwrap().kind, Kind::Html);
    // Plain text alone.
    let mut plain = Snapshot::default().with_text("just words");
    let t = take(&mut plain, None, o).unwrap();
    assert_eq!((t.kind, t.offered), (Kind::Text, vec![Kind::Text]));
    // An image file copied in Explorer beats its own path text.
    let mut explorer = Snapshot::default()
        .with_files(&[PathBuf::from(r"C:\shots\a.png")])
        .with_text(r"C:\shots\a.png");
    let t = take(&mut explorer, None, o).unwrap();
    assert_eq!(t.kind, Kind::ImageFile);
    assert_eq!(t.offered, [Kind::ImageFile, Kind::Text]);
    // A DIB before a GIF; the GIF next to it gives the frame count.
    let mut gif = Snapshot::default()
        .with(Format::Dib, vec![1; 40])
        .with(Format::Gif, crate::gif::tests::sample(7));
    match take(&mut gif, None, o).unwrap().payload {
        Payload::Image {
            format, gif_frames, ..
        } => assert_eq!((format, gif_frames), (Format::Dib, 7)),
        p => panic!("{p:?}"),
    }
}

#[test]
fn an_empty_clipboard_or_one_with_nothing_pinnable_takes_nothing() {
    let o = Options::default();
    assert_eq!(
        take(&mut Snapshot::default(), None, o).unwrap_err(),
        Refusal::Nothing
    );
    // Copied files that are not images: nothing, unless file cards are on.
    let docs = [
        PathBuf::from(r"C:\a\report.docx"),
        PathBuf::from(r"C:\a\data.csv"),
    ];
    let mut files = Snapshot::default().with_files(&docs);
    assert_eq!(take(&mut files, None, o).unwrap_err(), Refusal::Nothing);
    let cards = Options {
        file_cards: true,
        ..o
    };
    match take(&mut files, None, cards).unwrap().payload {
        Payload::Files { names, skipped } => {
            assert_eq!(names, ["report.docx", "data.csv"], "names, never folders");
            assert_eq!(skipped, 0);
        }
        p => panic!("{p:?}"),
    }
    // Copied paths can be turned off.
    let mut path = Snapshot::default().with_text(r"C:\shots\cat.png");
    let off = Options { paths: false, ..o };
    assert_eq!(take(&mut path, None, off).unwrap().kind, Kind::Text);
    match take(&mut path, None, o).unwrap().payload {
        Payload::ImageFiles { paths, skipped } => {
            assert_eq!(
                (paths, skipped),
                (vec![PathBuf::from(r"C:\shots\cat.png")], 0)
            );
        }
        p => panic!("{p:?}"),
    }
}

/// Several image files at once: up to MAX_FILES are pinned, the rest (and non-images) counted;
/// network paths are never taken.
#[test]
fn multi_file_pastes_are_bounded_and_local() {
    let o = Options::default();
    let mut many: Vec<PathBuf> = (0..20)
        .map(|i| PathBuf::from(format!(r"C:\shots\{i}.png")))
        .collect();
    many.push(PathBuf::from(r"C:\shots\notes.txt"));
    many.push(PathBuf::from(r"\\nas\photos\far.png"));
    let mut clip = Snapshot::default().with_files(&many);
    match take(&mut clip, None, o).unwrap().payload {
        Payload::ImageFiles { paths, skipped } => {
            assert_eq!(paths.len(), MAX_FILES);
            assert!(paths.iter().all(|p| is_local_path(p)));
            assert_eq!(skipped, 22 - MAX_FILES);
        }
        p => panic!("{p:?}"),
    }
    // Only a network image: no image file is offered.
    let mut far = Snapshot::default().with_files(&[PathBuf::from(r"\\nas\photos\far.png")]);
    assert_eq!(take(&mut far, None, o).unwrap_err(), Refusal::Nothing);
}

/// Password managers' markers: nothing is read at all.
#[test]
fn private_content_is_refused_before_anything_is_read() {
    let o = Options::default();
    for (marker, data) in [
        ("ExcludeClipboardContentFromMonitorProcessing", &b""[..]),
        ("Clipboard Viewer Ignore", b""),
        ("CanIncludeInClipboardHistory", &[0, 0, 0, 0]),
    ] {
        let mut secret = Snapshot::default()
            .with_text("hunter2")
            .with_marker(marker, data);
        assert_eq!(
            take(&mut secret, None, o).unwrap_err(),
            Refusal::Private,
            "{marker}"
        );
        assert!(secret.reads.is_empty(), "{marker}: not one byte read");
    }
    // History allowed explicitly (1) is not private.
    let mut allowed = Snapshot::default()
        .with_text("fine")
        .with_marker("CanIncludeInClipboardHistory", &[1, 0, 0, 0]);
    assert_eq!(take(&mut allowed, None, o).unwrap().kind, Kind::Text);
}

#[test]
fn oversized_formats_are_refused_without_being_copied() {
    let o = Options::default();
    let mut big = Snapshot::default().with(Format::Html, vec![b'a'; Limits::HTML + 1]);
    assert_eq!(
        take(&mut big, None, o).unwrap_err(),
        Refusal::TooLarge(Format::Html, (Limits::HTML + 1) as u64)
    );
    // Too much HTML next to its plain text: the text, unless HTML was asked for.
    let mut doc = Snapshot::default()
        .with(Format::Html, vec![b'a'; Limits::HTML + 1])
        .with_text("the document's text");
    let t = take(&mut doc, None, o).unwrap();
    assert_eq!(t.kind, Kind::Text);
    assert!(matches!(t.payload, Payload::Text(ref s) if s == "the document's text"));
    assert!(matches!(
        take(&mut doc, Some(Kind::Html), o).unwrap_err(),
        Refusal::TooLarge(Format::Html, _)
    ));
    let mut text = Snapshot::default().with(Format::Text, vec![b'a'; Limits::TEXT + 2]);
    assert!(matches!(
        take(&mut text, None, o).unwrap_err(),
        Refusal::TooLarge(Format::Text, _)
    ));
    // A long text is never inspected as a color or a path, but pins as text below the limit.
    let long = "word ".repeat(2000);
    let mut ok = Snapshot::default().with_text(&long);
    let t = take(&mut ok, None, o).unwrap();
    assert_eq!(t.offered, [Kind::Text]);
    assert!(matches!(t.payload, Payload::Text(ref s) if s.len() == long.len()));
}

/// The test hook's stand-in reads a folder of synthesized formats: never the user's clipboard.
#[test]
fn the_stand_in_reads_a_folder_of_formats() {
    let dir = std::env::temp_dir().join(format!("noeul-paste-stand-in-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    assert!(
        !Snapshot::from_dir(&dir).has(Format::Text),
        "no folder: empty"
    );
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(dir.join("CF_UNICODETEXT"), "héllo 한글").unwrap();
    std::fs::write(dir.join("HTML Format"), cf_html("<i>x</i>")).unwrap();
    std::fs::write(dir.join("CF_HDROP"), "C:\\a.png\r\n\r\nC:\\b.txt\r\n").unwrap();
    std::fs::write(dir.join("Clipboard Viewer Ignore"), "").unwrap();
    let mut s = Snapshot::from_dir(&dir);
    assert!(s.has(Format::Text) && s.has(Format::Html) && s.has(Format::Files));
    assert!(!s.has(Format::Png));
    assert_eq!(
        utf16_text(&s.read(Format::Text, 1024).unwrap().unwrap()),
        "héllo 한글"
    );
    assert_eq!(s.files(10).unwrap().0.len(), 2);
    assert!(s.private());
    std::fs::remove_file(dir.join("Clipboard Viewer Ignore")).unwrap();
    assert!(!Snapshot::from_dir(&dir).private());
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn kinds_have_stable_names() {
    for k in Kind::PRIORITY {
        assert_eq!(Kind::parse(k.as_str()), Some(k));
        assert!(!k.title().is_empty());
    }
    assert_eq!(Kind::parse("pdf"), None);
    assert_eq!(utf16_text(&utf16_bytes("a\u{1F600}b")), "a\u{1F600}b");
    assert_eq!(utf16_text(&[0x41, 0, 0, 0, 0x42, 0]), "A", "up to the NUL");
}
