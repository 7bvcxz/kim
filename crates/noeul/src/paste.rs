//! What `paste.float` (`F3`) takes from the clipboard (Step 55; NOEUL-FR-070, NOEUL-FR-071,
//! NOEUL-FR-074, NOEUL-SEC-002; ADR 0047).
//!
//! The clipboard is read **once, when the user pastes**, through the narrow [`Clipboard`] trait:
//! the Win32 clipboard while it is open (`win::clipboard`), the debug-build test hook's stand-in
//! ([`Snapshot::from_dir`]) and in-memory snapshots in tests. Nothing here polls, listens, logs or
//! keeps clipboard content: [`take`] looks at which formats are offered, reads only what it needs
//! to choose, then reads the chosen format (bounded) and hands it on.
//!
//! **What is pinned** ([`Kind`], in this order unless the user chooses another one with "Pin as…"
//! or `--as`):
//!
//! 1. **Image** — the registered `PNG` format, `CF_DIBV5`, `CF_DIB` (Windows synthesizes them from
//!    bitmaps), the registered `GIF` format, then `JFIF`;
//! 2. **Image file** — image files copied in Explorer (`CF_HDROP`), or the text of one copied
//!    local path to an image file (Explorer's "Copy as path", with or without quotes, or a
//!    `file:///` URL); only drive-letter paths: network paths (`\\server\share`) and device paths
//!    are never opened (NOEUL-NFR-008);
//! 3. **Color** — text that is one color value ([`parse_color`]);
//! 4. **HTML** — the `HTML Format` (CF_HTML), rendered by Noeul's own sanitizer (`crate::html`);
//! 5. **Text** — `CF_UNICODETEXT`;
//! 6. **Files** — the names of copied non-image files, only with `[paste] file_cards = "on"`.
//!
//! **Private content.** Programs that copy secrets (password managers) mark the clipboard with
//! `ExcludeClipboardContentFromMonitorProcessing`, `Clipboard Viewer Ignore` or
//! `CanIncludeInClipboardHistory` = 0. Noeul then pins nothing and reads nothing else: a floating,
//! always-on-top card would show the secret on every screen share ([`Taken`] is never built).
//!
//! **Bounds.** Every read is capped before it is copied ([`Limits`]): images at the decoder's
//! limit (an uncompressed DIB at the pixel limit), HTML at 4 MiB, text at 2 MiB; at most
//! [`MAX_FILES`] files of one paste are pinned.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

/// A clipboard format Noeul reads.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Format {
    /// The registered `PNG` format (and `image/png`).
    Png,
    DibV5,
    Dib,
    /// The registered `GIF` format.
    Gif,
    /// The registered `JFIF` format (a JPEG file's bytes).
    Jfif,
    /// `CF_HDROP`: copied files.
    Files,
    /// The registered `HTML Format` (CF_HTML: a header and UTF-8 HTML).
    Html,
    /// `CF_UNICODETEXT` (UTF-16LE, NUL-terminated).
    Text,
}

impl Format {
    pub const ALL: [Format; 8] = [
        Format::Png,
        Format::DibV5,
        Format::Dib,
        Format::Gif,
        Format::Jfif,
        Format::Files,
        Format::Html,
        Format::Text,
    ];

    /// The format's name as Windows lists it (the stand-in's file names).
    pub fn name(self) -> &'static str {
        match self {
            Format::Png => "PNG",
            Format::DibV5 => "CF_DIBV5",
            Format::Dib => "CF_DIB",
            Format::Gif => "GIF",
            Format::Jfif => "JFIF",
            Format::Files => "CF_HDROP",
            Format::Html => "HTML Format",
            Format::Text => "CF_UNICODETEXT",
        }
    }

    fn is_image(self) -> bool {
        matches!(
            self,
            Format::Png | Format::DibV5 | Format::Dib | Format::Gif | Format::Jfif
        )
    }
}

/// The image formats, in the order Noeul prefers them: lossless with alpha first.
pub const IMAGE_FORMATS: [Format; 5] = [
    Format::Png,
    Format::DibV5,
    Format::Dib,
    Format::Gif,
    Format::Jfif,
];

/// The markers of private clipboard content (documented by Windows for clipboard history and
/// cloud clipboard; set by password managers).
pub const PRIVATE_MARKERS: [&str; 3] = [
    "ExcludeClipboardContentFromMonitorProcessing",
    "Clipboard Viewer Ignore",
    "CanIncludeInClipboardHistory",
];

/// A clipboard while it is open (or a snapshot standing in for one).
pub trait Clipboard {
    /// Whether the format is offered.
    fn has(&self, format: Format) -> bool;
    /// The bytes of `format`: `None` when it is not offered or unreadable, `Some(Err(size))` when
    /// it is larger than `max` (nothing is copied).
    fn read(&mut self, format: Format, max: usize) -> Option<Result<Vec<u8>, u64>>;
    /// The paths of `CF_HDROP` (at most `max` of them; `(paths, total)`).
    fn files(&mut self, max: usize) -> Option<(Vec<PathBuf>, usize)>;
    /// The program that copied the content marked it private.
    fn private(&self) -> bool;
}

/// What can be pinned, in priority order (NOEUL-FR-071).
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Kind {
    Image,
    ImageFile,
    Color,
    Html,
    Text,
    Files,
}

impl Kind {
    pub const PRIORITY: [Kind; 6] = [
        Kind::Image,
        Kind::ImageFile,
        Kind::Color,
        Kind::Html,
        Kind::Text,
        Kind::Files,
    ];

    pub fn as_str(self) -> &'static str {
        match self {
            Kind::Image => "image",
            Kind::ImageFile => "image_file",
            Kind::Color => "color",
            Kind::Html => "html",
            Kind::Text => "text",
            Kind::Files => "files",
        }
    }

    pub fn parse(s: &str) -> Option<Kind> {
        Kind::PRIORITY.into_iter().find(|k| k.as_str() == s)
    }

    /// The menu's words ("Pin clipboard as …").
    pub fn title(self) -> &'static str {
        match self {
            Kind::Image => "Image",
            Kind::ImageFile => "Image file",
            Kind::Color => "Color swatch",
            Kind::Html => "Formatted text (HTML)",
            Kind::Text => "Plain text",
            Kind::Files => "File names",
        }
    }
}

/// Paste settings (`[paste]` in config.toml).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Options {
    /// The text of a copied path to an image file pins that file (`[paste] paths`, on).
    pub paths: bool,
    /// Copied non-image files become a card of their names (`[paste] file_cards`, off).
    pub file_cards: bool,
}

impl Default for Options {
    fn default() -> Options {
        Options {
            paths: true,
            file_cards: false,
        }
    }
}

/// The most files of one paste or drop that become pins.
pub const MAX_FILES: usize = 16;
/// The most file names a file card lists.
pub const MAX_NAMES: usize = 64;

/// Read limits per format, in bytes.
#[derive(Debug, Clone, Copy)]
pub struct Limits;

impl Limits {
    /// A compressed picture (PNG, GIF, JFIF).
    pub const IMAGE: usize = crate::decode::MAX_INPUT_BYTES as usize;
    /// An uncompressed DIB: 32-bit pixels at the pixel limit, plus headers.
    pub const DIB: usize = (crate::decode::MAX_PIXELS * 4) as usize + 64 * 1024;
    pub const HTML: usize = 4 * 1024 * 1024;
    /// UTF-16: about a million characters.
    pub const TEXT: usize = 2 * 1024 * 1024;
    /// Text that may be a color value or a path is looked at only when it is this short.
    pub const SHORT_TEXT: usize = 4 * 1024;
}

/// What [`take`] read.
#[derive(Debug)]
pub enum Payload {
    /// An image format's bytes, and the frame count of a `GIF` offered next to it (1: none).
    Image {
        format: Format,
        bytes: Vec<u8>,
        gif_frames: u32,
    },
    /// Local image files to pin (at most [`MAX_FILES`]), and how many more were offered.
    ImageFiles {
        paths: Vec<PathBuf>,
        skipped: usize,
    },
    Color(Rgba),
    /// CF_HTML bytes.
    Html(Vec<u8>),
    Text(String),
    /// File names only (never the folders), and how many more were offered.
    Files {
        names: Vec<String>,
        skipped: usize,
    },
}

/// One read: what was taken, and every kind the clipboard offered (names only).
#[derive(Debug)]
pub struct Taken {
    pub kind: Kind,
    pub offered: Vec<Kind>,
    pub payload: Payload,
}

/// Why nothing was taken.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Refusal {
    /// Nothing Noeul can pin.
    Nothing,
    /// The asked kind is not offered (the others are).
    NotOffered(Kind, Vec<Kind>),
    /// The content is marked private by the program that copied it.
    Private,
    /// The chosen format is larger than its limit (`format`, its size).
    TooLarge(Format, u64),
    /// The chosen format could not be read.
    Unreadable(Format),
}

/// A color with straight alpha.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Rgba {
    pub r: u8,
    pub g: u8,
    pub b: u8,
    pub a: u8,
}

impl Rgba {
    pub const fn rgb(r: u8, g: u8, b: u8) -> Rgba {
        Rgba { r, g, b, a: 255 }
    }

    /// `#RRGGBB`, or `#RRGGBBAA` when not opaque.
    pub fn hex(self) -> String {
        if self.a == 255 {
            format!("#{:02X}{:02X}{:02X}", self.r, self.g, self.b)
        } else {
            format!("#{:02X}{:02X}{:02X}{:02X}", self.r, self.g, self.b, self.a)
        }
    }

    /// `rgb(r, g, b)`, or `rgba(r, g, b, a)` with the alpha as 0–1 when not opaque.
    pub fn css(self) -> String {
        if self.a == 255 {
            format!("rgb({}, {}, {})", self.r, self.g, self.b)
        } else {
            let a = f64::from(self.a) / 255.0;
            format!("rgba({}, {}, {}, {a:.2})", self.r, self.g, self.b)
        }
    }

    pub fn bgra(self) -> [u8; 4] {
        [self.b, self.g, self.r, self.a]
    }
}

/// Text that is exactly one color value: `#RGB`, `#RGBA`, `#RRGGBB`, `#RRGGBBAA`, `rgb(…)` and
/// `rgba(…)` (commas or spaces, integers 0–255 or percentages, an optional alpha 0–1 or %), or
/// three integers 0–255 (`255, 128, 0`, `255 128 0`, `(255;128;0)`), or three numbers 0–1 of
/// which at least one has a decimal point (`1.0, 0.5, 0`). Anything else is not a color.
pub fn parse_color(text: &str) -> Option<Rgba> {
    let t = text.trim();
    if t.is_empty() || t.len() > 64 || t.contains('\n') {
        return None;
    }
    if let Some(hex) = t.strip_prefix('#') {
        return parse_hex(hex);
    }
    let lower = t.to_ascii_lowercase();
    for (prefix, alpha) in [("rgba(", true), ("rgb(", false)] {
        if let Some(inner) = lower.strip_prefix(prefix).and_then(|s| s.strip_suffix(')')) {
            return parse_css_rgb(inner, alpha);
        }
    }
    let inner = t
        .strip_prefix('(')
        .and_then(|s| s.strip_suffix(')'))
        .unwrap_or(t);
    let parts: Vec<&str> = inner
        .split(|c: char| c == ',' || c == ';' || c.is_whitespace())
        .filter(|p| !p.is_empty())
        .collect();
    if parts.len() != 3 {
        return None;
    }
    if parts.iter().any(|p| p.contains('.')) {
        let mut v = [0u8; 3];
        for (i, p) in parts.iter().enumerate() {
            let f: f64 = p.parse().ok().filter(|f: &f64| f.is_finite())?;
            if !(0.0..=1.0).contains(&f) {
                return None;
            }
            v[i] = (f * 255.0).round() as u8;
        }
        return Some(Rgba::rgb(v[0], v[1], v[2]));
    }
    let mut v = [0u8; 3];
    for (i, p) in parts.iter().enumerate() {
        if !p.bytes().all(|b| b.is_ascii_digit()) || p.len() > 3 {
            return None;
        }
        v[i] = p.parse::<u16>().ok().and_then(|n| u8::try_from(n).ok())?;
    }
    Some(Rgba::rgb(v[0], v[1], v[2]))
}

fn parse_hex(hex: &str) -> Option<Rgba> {
    if !hex.bytes().all(|b| b.is_ascii_hexdigit()) {
        return None;
    }
    let d = |i: usize| u8::from_str_radix(&hex[i..=i], 16).ok();
    let dd = |i: usize| u8::from_str_radix(&hex[i..i + 2], 16).ok();
    Some(match hex.len() {
        3 => Rgba::rgb(d(0)? * 17, d(1)? * 17, d(2)? * 17),
        4 => Rgba {
            a: d(3)? * 17,
            ..Rgba::rgb(d(0)? * 17, d(1)? * 17, d(2)? * 17)
        },
        6 => Rgba::rgb(dd(0)?, dd(2)?, dd(4)?),
        8 => Rgba {
            a: dd(6)?,
            ..Rgba::rgb(dd(0)?, dd(2)?, dd(4)?)
        },
        _ => return None,
    })
}

/// The inside of `rgb(…)`/`rgba(…)`: `r, g, b[, a]` or `r g b[ / a]`.
fn parse_css_rgb(inner: &str, comma_alpha: bool) -> Option<Rgba> {
    let (main, slash_alpha) = match inner.split_once('/') {
        Some((m, a)) => (m, Some(a.trim())),
        None => (inner, None),
    };
    let parts: Vec<&str> = main
        .split(|c: char| c == ',' || c.is_whitespace())
        .filter(|p| !p.is_empty())
        .collect();
    let (rgb, alpha) = match (parts.len(), slash_alpha) {
        (3, a) => (&parts[..], a),
        (4, None) if comma_alpha || inner.contains(',') => (&parts[..3], Some(parts[3])),
        _ => return None,
    };
    let channel = |p: &str| -> Option<u8> {
        if let Some(pc) = p.strip_suffix('%') {
            let f: f64 = pc.parse().ok().filter(|f: &f64| f.is_finite())?;
            (0.0..=100.0)
                .contains(&f)
                .then(|| (f / 100.0 * 255.0).round() as u8)
        } else {
            let f: f64 = p.parse().ok().filter(|f: &f64| f.is_finite())?;
            (0.0..=255.0).contains(&f).then(|| f.round() as u8)
        }
    };
    let a = match alpha {
        None => 255,
        Some(p) => {
            let f: f64 = match p.strip_suffix('%') {
                Some(pc) => pc.parse::<f64>().ok()? / 100.0,
                None => p.parse().ok()?,
            };
            if !f.is_finite() || !(0.0..=1.0).contains(&f) {
                return None;
            }
            (f * 255.0).round() as u8
        }
    };
    Some(Rgba {
        r: channel(rgb[0])?,
        g: channel(rgb[1])?,
        b: channel(rgb[2])?,
        a,
    })
}

/// Whether `path` names a file Noeul decodes (by its extension; the bytes decide later).
pub fn is_image_path(path: &Path) -> bool {
    path.extension()
        .and_then(|e| e.to_str())
        .and_then(crate::decode::Format::from_extension)
        .is_some()
}

/// A local drive-letter path (`C:\…`): never a network (`\\server\share`, `\\?\UNC\…`) or device
/// (`\\.\…`, `\\?\…`) path, never relative.
pub fn is_local_path(path: &Path) -> bool {
    let s = path.as_os_str().to_string_lossy();
    let b = s.as_bytes();
    b.len() >= 3
        && b[0].is_ascii_alphabetic()
        && b[1] == b':'
        && (b[2] == b'\\' || b[2] == b'/')
        && !s.contains('\0')
}

/// The text of one copied path to a local image file: trimmed, one line, quotes removed, a
/// `file:///X:/…` URL decoded (no host other than `localhost`). `None` for anything else.
pub fn copied_path(text: &str) -> Option<PathBuf> {
    let t = text.trim();
    if t.is_empty() || t.len() > 1024 || t.contains(['\n', '\r']) {
        return None;
    }
    let t = t
        .strip_prefix('"')
        .and_then(|s| s.strip_suffix('"'))
        .unwrap_or(t);
    let path = if let Some(rest) = strip_prefix_ci(t, "file://") {
        let rest = strip_prefix_ci(rest, "localhost").unwrap_or(rest);
        let rest = rest.strip_prefix('/')?;
        PathBuf::from(percent_decode(rest)?.replace('/', "\\"))
    } else {
        PathBuf::from(t)
    };
    (is_local_path(&path) && is_image_path(&path)).then_some(path)
}

fn strip_prefix_ci<'a>(s: &'a str, prefix: &str) -> Option<&'a str> {
    (s.len() >= prefix.len() && s[..prefix.len()].eq_ignore_ascii_case(prefix))
        .then(|| &s[prefix.len()..])
}

fn percent_decode(s: &str) -> Option<String> {
    let b = s.as_bytes();
    let mut out = Vec::with_capacity(b.len());
    let mut i = 0;
    while i < b.len() {
        if b[i] == b'%' {
            let h = s.get(i + 1..i + 3)?;
            out.push(u8::from_str_radix(h, 16).ok()?);
            i += 3;
        } else {
            out.push(b[i]);
            i += 1;
        }
    }
    String::from_utf8(out).ok()
}

/// CF_UNICODETEXT bytes as text: UTF-16LE up to the first NUL, invalid units replaced.
pub fn utf16_text(bytes: &[u8]) -> String {
    let units: Vec<u16> = bytes
        .as_chunks::<2>()
        .0
        .iter()
        .map(|c| u16::from_le_bytes(*c))
        .take_while(|u| *u != 0)
        .collect();
    String::from_utf16_lossy(&units)
}

/// Text as CF_UNICODETEXT bytes (NUL-terminated UTF-16LE).
pub fn utf16_bytes(text: &str) -> Vec<u8> {
    let mut v: Vec<u8> = text.encode_utf16().flat_map(u16::to_le_bytes).collect();
    v.extend_from_slice(&[0, 0]);
    v
}

/// The short text on the clipboard, if any (for the color and path checks).
fn short_text(clip: &mut dyn Clipboard) -> Option<String> {
    match clip.read(Format::Text, Limits::SHORT_TEXT)? {
        Ok(b) => Some(utf16_text(&b)),
        Err(_) => None,
    }
}

/// Every kind the clipboard offers, in priority order.
pub fn offered(clip: &mut dyn Clipboard, opts: Options) -> Vec<Kind> {
    let mut kinds = Vec::new();
    if IMAGE_FORMATS.iter().any(|f| clip.has(*f)) {
        kinds.push(Kind::Image);
    }
    let files = if clip.has(Format::Files) {
        clip.files(usize::MAX).map(|(p, _)| p).unwrap_or_default()
    } else {
        Vec::new()
    };
    let text = if clip.has(Format::Text) {
        short_text(clip)
    } else {
        None
    };
    let path_text = opts
        .paths
        .then(|| text.as_deref().and_then(copied_path))
        .flatten();
    if files.iter().any(|p| is_image_path(p) && is_local_path(p)) || path_text.is_some() {
        kinds.push(Kind::ImageFile);
    }
    if text.as_deref().and_then(parse_color).is_some() {
        kinds.push(Kind::Color);
    }
    if clip.has(Format::Html) {
        kinds.push(Kind::Html);
    }
    if clip.has(Format::Text) {
        kinds.push(Kind::Text);
    }
    if opts.file_cards && files.iter().any(|p| !is_image_path(p)) {
        kinds.push(Kind::Files);
    }
    kinds
}

/// Reads the clipboard once for a paste: the first offered kind (or `want`), then that kind's
/// data, bounded. Private content is refused before anything is read.
pub fn take(clip: &mut dyn Clipboard, want: Option<Kind>, opts: Options) -> Result<Taken, Refusal> {
    if clip.private() {
        return Err(Refusal::Private);
    }
    let offered = offered(clip, opts);
    let kind = match want {
        Some(k) if offered.contains(&k) => k,
        Some(k) => return Err(Refusal::NotOffered(k, offered)),
        None => *offered.first().ok_or(Refusal::Nothing)?,
    };
    let payload = match kind {
        Kind::Image => {
            let format = *IMAGE_FORMATS
                .iter()
                .find(|f| clip.has(**f))
                .ok_or(Refusal::Nothing)?;
            let max = match format {
                Format::DibV5 | Format::Dib => Limits::DIB,
                _ => Limits::IMAGE,
            };
            let bytes = match clip.read(format, max) {
                Some(Ok(b)) => b,
                Some(Err(size)) => return Err(Refusal::TooLarge(format, size)),
                None => return Err(Refusal::Unreadable(format)),
            };
            // An animation copied next to its first frame: its frame count, from its structure.
            let gif_frames = if format != Format::Gif && clip.has(Format::Gif) {
                match clip.read(Format::Gif, Limits::IMAGE) {
                    Some(Ok(g)) => crate::gif::frame_count(&g).unwrap_or(1).max(1),
                    _ => 1,
                }
            } else {
                1
            };
            Payload::Image {
                format,
                bytes,
                gif_frames,
            }
        }
        Kind::ImageFile => {
            let (files, _) = clip.files(usize::MAX).unwrap_or_default();
            let mut count = files.len();
            let mut images: Vec<PathBuf> = files
                .into_iter()
                .filter(|p| is_image_path(p) && is_local_path(p))
                .collect();
            if images.is_empty()
                && let Some(p) = short_text(clip).as_deref().and_then(copied_path)
            {
                images.push(p);
                count = 1;
            }
            images.truncate(MAX_FILES);
            // Files that are not images, and images beyond the limit.
            Payload::ImageFiles {
                skipped: count.saturating_sub(images.len()),
                paths: images,
            }
        }
        Kind::Color => {
            let c = short_text(clip)
                .as_deref()
                .and_then(parse_color)
                .ok_or(Refusal::Unreadable(Format::Text))?;
            Payload::Color(c)
        }
        Kind::Html => match clip.read(Format::Html, Limits::HTML) {
            Some(Ok(b)) => Payload::Html(b),
            Some(Err(size)) => return Err(Refusal::TooLarge(Format::Html, size)),
            None => return Err(Refusal::Unreadable(Format::Html)),
        },
        Kind::Text => match clip.read(Format::Text, Limits::TEXT) {
            Some(Ok(b)) => Payload::Text(utf16_text(&b)),
            Some(Err(size)) => return Err(Refusal::TooLarge(Format::Text, size)),
            None => return Err(Refusal::Unreadable(Format::Text)),
        },
        Kind::Files => {
            let (files, total) = clip.files(usize::MAX).unwrap_or_default();
            let names: Vec<String> = files
                .iter()
                .take(MAX_NAMES)
                .map(|p| {
                    p.file_name()
                        .map_or_else(|| p.display().to_string(), |n| n.to_string_lossy().into())
                })
                .collect();
            Payload::Files {
                skipped: total.max(files.len()) - names.len(),
                names,
            }
        }
    };
    Ok(Taken {
        kind,
        offered,
        payload,
    })
}

/// A clipboard in memory: the debug-build test hook's stand-in and the tests' clipboard. Every
/// format is synthesized by whoever fills it; the user's clipboard is never involved.
#[derive(Debug, Default, Clone)]
pub struct Snapshot {
    pub formats: BTreeMap<Format, Vec<u8>>,
    pub files: Option<Vec<PathBuf>>,
    /// Marker formats present (their names), with their data.
    pub markers: BTreeMap<String, Vec<u8>>,
    /// How often a format's bytes were read (tests check that only what is needed is read).
    pub reads: BTreeMap<Format, usize>,
}

impl Snapshot {
    pub fn with(mut self, format: Format, bytes: impl Into<Vec<u8>>) -> Snapshot {
        self.formats.insert(format, bytes.into());
        self
    }

    pub fn with_text(self, text: &str) -> Snapshot {
        self.with(Format::Text, utf16_bytes(text))
    }

    pub fn with_files(mut self, files: &[PathBuf]) -> Snapshot {
        self.files = Some(files.to_vec());
        self
    }

    pub fn with_marker(mut self, name: &str, data: &[u8]) -> Snapshot {
        self.markers.insert(name.into(), data.to_vec());
        self
    }

    /// The debug-build test hook's stand-in: one file per format in `dir`, named as Windows
    /// names the format ([`Format::name`]; `CF_UNICODETEXT` holds UTF-8, `CF_HDROP` one path per
    /// line), and one file per marker of [`PRIVATE_MARKERS`]. A missing folder is an empty
    /// clipboard.
    pub fn from_dir(dir: &Path) -> Snapshot {
        let mut s = Snapshot::default();
        for f in Format::ALL {
            let Ok(bytes) = std::fs::read(dir.join(f.name())) else {
                continue;
            };
            match f {
                Format::Text => {
                    s.formats
                        .insert(f, utf16_bytes(&String::from_utf8_lossy(&bytes)));
                }
                Format::Files => {
                    s.files = Some(
                        String::from_utf8_lossy(&bytes)
                            .lines()
                            .filter(|l| !l.trim().is_empty())
                            .map(|l| PathBuf::from(l.trim()))
                            .collect(),
                    );
                }
                _ => {
                    s.formats.insert(f, bytes);
                }
            }
        }
        for m in PRIVATE_MARKERS {
            if let Ok(b) = std::fs::read(dir.join(m)) {
                s.markers.insert(m.into(), b);
            }
        }
        s
    }
}

/// Whether the markers mark the content private: the two presence markers, or
/// `CanIncludeInClipboardHistory` holding a zero DWORD.
pub fn markers_private(present: &dyn Fn(&str) -> Option<Vec<u8>>) -> bool {
    present(PRIVATE_MARKERS[0]).is_some()
        || present(PRIVATE_MARKERS[1]).is_some()
        || present(PRIVATE_MARKERS[2]).is_some_and(|d| {
            let text = String::from_utf8_lossy(&d);
            d.get(..4) == Some(&[0, 0, 0, 0]) || text.trim() == "0"
        })
}

impl Clipboard for Snapshot {
    fn has(&self, format: Format) -> bool {
        if format == Format::Files {
            return self.files.as_ref().is_some_and(|f| !f.is_empty());
        }
        self.formats.contains_key(&format)
    }

    fn read(&mut self, format: Format, max: usize) -> Option<Result<Vec<u8>, u64>> {
        let b = self.formats.get(&format)?;
        *self.reads.entry(format).or_default() += 1;
        Some(if b.len() > max {
            Err(b.len() as u64)
        } else {
            Ok(b.clone())
        })
    }

    fn files(&mut self, max: usize) -> Option<(Vec<PathBuf>, usize)> {
        let f = self.files.as_ref()?;
        Some((f.iter().take(max).cloned().collect(), f.len()))
    }

    fn private(&self) -> bool {
        markers_private(&|name| self.markers.get(name).cloned())
    }
}

/// Whether a format is an image format (for the stand-in and the Win32 reader).
pub fn is_image_format(f: Format) -> bool {
    f.is_image()
}

/// The error a refusal is reported as (NOEUL-FR-074): what happened, never what the clipboard
/// holds.
pub fn refusal_error(r: &Refusal) -> crate::error::NoeulError {
    use crate::error::{Code, NoeulError};
    let names = |kinds: &[Kind]| {
        kinds
            .iter()
            .map(|k| k.as_str())
            .collect::<Vec<_>>()
            .join(", ")
    };
    match r {
        Refusal::Nothing => NoeulError::new(
            Code::NothingToPin,
            "the clipboard holds nothing Noeul can pin",
        )
        .hint("copy an image, image files, text, HTML or a color value first"),
        Refusal::NotOffered(k, offered) => NoeulError::new(
            Code::NothingToPin,
            format!("the clipboard holds no {} to pin", k.as_str()),
        )
        .hint(if offered.is_empty() {
            "the clipboard holds nothing Noeul can pin".to_string()
        } else {
            format!("it offers: {}", names(offered))
        }),
        Refusal::Private => NoeulError::new(
            Code::NothingToPin,
            "the clipboard content is marked private by the program that copied it (for example a password manager); Noeul does not pin it",
        )
        .hint("paste it where you need it instead"),
        Refusal::TooLarge(f, size) => NoeulError::new(
            if f.is_image() {
                Code::ImageInvalid
            } else {
                Code::NothingToPin
            },
            format!(
                "the clipboard's {} data is {} MiB, more than Noeul reads",
                f.name(),
                size.div_ceil(1024 * 1024)
            ),
        )
        .hint("nothing was pinned"),
        Refusal::Unreadable(f) => NoeulError::new(
            Code::NothingToPin,
            format!("the clipboard's {} data could not be read", f.name()),
        ),
    }
}

#[cfg(test)]
mod tests;
