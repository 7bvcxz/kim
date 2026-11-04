//! Pasting (Step 55; NOEUL-FR-070…NOEUL-FR-074, NOEUL-SEC-002, NOEUL-SEC-009; ADR 0047): what a
//! paste took from the clipboard ([`crate::paste`]), or image files (`noeul pin <file>`, a drop on
//! a pin), become pins: pictures decoded locally ([`crate::decode`], [`crate::dib`]) or cards
//! ([`crate::card`]). Every source goes through [`App::paste_clipboard`] or
//! [`App::paste_files`], so `F3`, "Pin clipboard as…", the CLI and a drop behave the same.
//!
//! - **Placement.** The first pin's top-left goes to the pointer (or the CLI's `at`), inside the
//!   work area of the monitor there and zoomed out when larger (ADR 0029); several files are
//!   placed down and to the right of each other, 24 logical pixels apart.
//! - **Reports.** Notices name what happened (pinned, nothing to pin, private content, a file that
//!   could not be read, the first frame of a GIF, the first page of a TIFF, a long card, images of
//!   the HTML shown as placeholders) but never the content, a path or a name (NOEUL-SEC-008).

use super::*;
use crate::card;
use crate::decode::{self, Multi};
use crate::paste::{self, Kind, Payload};
use crate::pins::{Content, ContentKind};
use std::path::Path;

/// What one paste made.
#[derive(Debug, Default)]
pub struct Made {
    /// The pins, the first first.
    pub ids: Vec<PinId>,
    pub kind: Option<Kind>,
    /// Every kind the clipboard offered (names only).
    pub offered: Vec<Kind>,
    /// Offered files that were not pinned (not images, or beyond the limit).
    pub skipped: usize,
    /// Files that could not be read or decoded.
    pub failed: usize,
}

impl Made {
    pub fn to_json(&self, refs: &[Value]) -> Value {
        json!({
            "kind": self.kind.map(Kind::as_str),
            "offered": self.offered.iter().map(|k| k.as_str()).collect::<Vec<_>>(),
            "pins": refs,
            "skipped": self.skipped,
            "failed": self.failed,
        })
    }
}

/// One picture ready to become a pin.
struct Ready {
    frame: Frame,
    content: Content,
    multi: Multi,
    /// A notice the pin deserves (a long card, placeholders), without content.
    note: Option<&'static str>,
}

/// Steps between several pins of one paste, in logical pixels.
const CASCADE: u32 = 24;

impl<S: Shell> App<S> {
    fn paste_options(&self) -> paste::Options {
        self.settings.settings.paste
    }

    /// Reads the clipboard once and pins what it holds (the first kind, or `want`) with its
    /// top-left at `at`. Nothing else reads the clipboard (NOEUL-SEC-002).
    pub(super) fn paste_clipboard(
        &mut self,
        want: Option<Kind>,
        at: (i32, i32),
        focus: bool,
    ) -> Result<Made> {
        let opts = self.paste_options();
        let taken = self.shell.clipboard_take(want, opts)?;
        let kind = taken.kind;
        let offered = taken.offered;
        let (ready, skipped, failed) = self.prepare(taken.payload, at, kind, &offered)?;
        self.place(
            ready,
            at,
            focus,
            Source::Clipboard,
            kind,
            offered,
            skipped,
            failed,
        )
    }

    /// Pins image files: `noeul pin <file>` and files dropped on a pin (NOEUL-FR-073).
    pub(super) fn paste_files(
        &mut self,
        mut paths: Vec<PathBuf>,
        at: (i32, i32),
        focus: bool,
    ) -> Result<Made> {
        let total = paths.len();
        paths.truncate(paste::MAX_FILES);
        let skipped = total - paths.len();
        let payload = Payload::ImageFiles { paths, skipped };
        let (ready, skipped, failed) = self.prepare(payload, at, Kind::ImageFile, &[])?;
        self.place(
            ready,
            at,
            focus,
            Source::File,
            Kind::ImageFile,
            Vec::new(),
            skipped,
            failed,
        )
    }

    /// A clipboard image format's bytes as a frame.
    fn decode_clipboard_image(
        &self,
        format: paste::Format,
        bytes: &[u8],
    ) -> Result<(Frame, Multi)> {
        match format {
            paste::Format::DibV5 | paste::Format::Dib => {
                Ok((crate::dib::decode(&self.ledger, bytes)?, Multi::Single))
            }
            _ => {
                let f = decode::sniff(bytes, false).ok_or_else(|| {
                    NoeulError::new(
                        Code::ImageInvalid,
                        format!(
                            "the clipboard's {} data is not a picture Noeul can read",
                            format.name()
                        ),
                    )
                })?;
                let d = decode::decode(&self.ledger, bytes, f)?;
                Ok((d.frame, d.multi))
            }
        }
    }

    /// An image file as a frame: read through the shell (local files only), recognised by its
    /// bytes, decoded with the limits.
    fn decode_file(&mut self, path: &Path) -> Result<(Frame, Multi)> {
        let bytes = self.shell.read_image_file(path, decode::MAX_INPUT_BYTES)?;
        let tga = path
            .extension()
            .and_then(|e| e.to_str())
            .and_then(decode::Format::from_extension)
            == Some(decode::Format::Tga);
        let f = decode::sniff(&bytes, tga).ok_or_else(|| {
            NoeulError::new(
                Code::NothingToPin,
                "the file is not a picture Noeul can read",
            )
            .hint("PNG, JPEG, GIF, BMP, ICO, TIFF and TGA files can be pinned")
        })?;
        let d = decode::decode(&self.ledger, &bytes, f)?;
        Ok((d.frame, d.multi))
    }

    /// A card of `doc` at `dpi`.
    fn card(
        &mut self,
        doc: &card::Doc,
        dpi: u32,
        kind: ContentKind,
        text: String,
        offered: &[Kind],
        pasted: Kind,
    ) -> Result<Ready> {
        if doc.blocks.is_empty() {
            return Err(NoeulError::new(
                Code::NothingToPin,
                "the clipboard's text is empty",
            ));
        }
        let frame = card::render(&self.ledger, doc, dpi, self.shell.text_engine())
            .map_err(|e| NoeulError::new(Code::Internal, format!("the card: {e}")))?;
        let note = if doc.placeholders > 0 {
            Some(
                "Images in the copied HTML are shown as placeholders: Noeul never downloads anything.",
            )
        } else if doc.truncated {
            Some("The text is long: the card shows its beginning. Copy text gives you all of it.")
        } else {
            None
        };
        Ok(Ready {
            frame,
            content: Content {
                kind,
                text: Some(text),
                offered: offered.to_vec(),
                pasted: Some(pasted),
            },
            multi: Multi::Single,
            note,
        })
    }

    /// What a payload becomes: pictures or a card. `(ready, skipped, failed)`; an error when
    /// nothing could be made.
    fn prepare(
        &mut self,
        payload: Payload,
        at: (i32, i32),
        kind: Kind,
        offered: &[Kind],
    ) -> Result<(Vec<Ready>, usize, usize)> {
        let dpi = crate::pins::dpi_at(&self.capture.desktop(), Rect::new(at.0, at.1, 1, 1));
        let picture = |frame: Frame, multi: Multi| Ready {
            frame,
            content: Content {
                kind: ContentKind::Image,
                text: None,
                offered: offered.to_vec(),
                pasted: Some(kind),
            },
            multi,
            note: None,
        };
        match payload {
            Payload::Image {
                format,
                bytes,
                gif_frames,
            } => {
                let (frame, multi) = self.decode_clipboard_image(format, &bytes)?;
                let multi = if gif_frames > 1 {
                    Multi::Frames(gif_frames)
                } else {
                    multi
                };
                Ok((vec![picture(frame, multi)], 0, 0))
            }
            Payload::ImageFiles { paths, skipped } => {
                let mut ready = Vec::new();
                let mut first_error = None;
                let mut failed = 0;
                for p in &paths {
                    match self.decode_file(p) {
                        Ok((frame, multi)) => ready.push(picture(frame, multi)),
                        Err(e) => {
                            failed += 1;
                            first_error.get_or_insert(e);
                        }
                    }
                }
                if ready.is_empty() {
                    return Err(first_error.unwrap_or_else(|| {
                        NoeulError::new(Code::NothingToPin, "no image file to pin")
                    }));
                }
                Ok((ready, skipped, failed))
            }
            Payload::Color(c) => {
                let frame = card::swatch(&self.ledger, c, dpi, self.shell.text_engine())
                    .map_err(|e| NoeulError::new(Code::Internal, format!("the card: {e}")))?;
                Ok((
                    vec![Ready {
                        frame,
                        content: Content {
                            kind: ContentKind::Color,
                            text: Some(c.hex()),
                            offered: offered.to_vec(),
                            pasted: Some(kind),
                        },
                        multi: Multi::Single,
                        note: None,
                    }],
                    0,
                    0,
                ))
            }
            Payload::Html(bytes) => {
                let doc = crate::html::parse_cf_html(&bytes);
                let text = doc.plain_text();
                let r = self.card(&doc, dpi, ContentKind::Html, text, offered, kind)?;
                Ok((vec![r], 0, 0))
            }
            Payload::Text(text) => {
                let doc = card::Doc::from_text(&text);
                let r = self.card(&doc, dpi, ContentKind::Text, text, offered, kind)?;
                Ok((vec![r], 0, 0))
            }
            Payload::Files { names, skipped } => {
                let doc = card::Doc::files(&names, skipped);
                let text = names.join("\n");
                let r = self.card(&doc, dpi, ContentKind::Files, text, offered, kind)?;
                Ok((vec![r], 0, 0))
            }
        }
    }

    /// Creates the pins of one paste, cascading from `at`.
    #[allow(clippy::too_many_arguments)]
    fn place(
        &mut self,
        ready: Vec<Ready>,
        at: (i32, i32),
        focus: bool,
        source: Source,
        kind: Kind,
        offered: Vec<Kind>,
        skipped: usize,
        mut failed: usize,
    ) -> Result<Made> {
        let desktop = self.capture.desktop();
        let dpi = crate::pins::dpi_at(&desktop, Rect::new(at.0, at.1, 1, 1));
        let step = i32::try_from((CASCADE * dpi + 48) / 96).unwrap_or(24);
        let n = ready.len();
        let mut made = Made {
            kind: Some(kind),
            offered,
            skipped,
            ..Made::default()
        };
        let mut notes: Vec<&'static str> = Vec::new();
        for (i, r) in ready.into_iter().enumerate() {
            let k = i32::try_from(i).unwrap_or(0);
            let new = NewPin {
                frame: r.frame,
                source,
                placement: Placement::At(at.0 + k * step, at.1 + k * step),
                // Only the last of several pins takes the focus (the user's own gesture).
                focus: focus && i + 1 == n,
                guard: None,
                annotations: Vec::new(),
            };
            match self.pins.create(self.shell.pin_windows(), &desktop, new) {
                Ok(id) => {
                    self.pins.set_content(id, r.content);
                    match r.multi {
                        Multi::Frames(f) => self.first_frame_only(id, f),
                        Multi::Pages(p) => self.shell.notice(&format!(
                            "This TIFF has {p} pages; Noeul shows only the first."
                        )),
                        Multi::Single | Multi::Sizes(_) => {}
                    }
                    if let Some(note) = r.note {
                        notes.push(note);
                    }
                    made.ids.push(id);
                }
                Err((frame, why)) => {
                    drop(frame);
                    if made.ids.is_empty() && i + 1 == n {
                        return Err(NoeulError::new(
                            Code::Internal,
                            format!("the pin window could not be created: {why}"),
                        )
                        .hint("nothing was pinned"));
                    }
                    failed += 1;
                }
            }
        }
        if made.ids.is_empty() {
            return Err(NoeulError::new(
                Code::Internal,
                "the pin windows could not be created",
            ));
        }
        made.failed = failed;
        for note in notes {
            self.shell.notice(note);
        }
        if skipped + failed > 0 {
            let pinned = made.ids.len();
            self.shell.notice(&format!(
                "Pinned {pinned} of {} files; the others are not images Noeul can read{}.",
                pinned + skipped + failed,
                if skipped > 0 && pinned + skipped > paste::MAX_FILES {
                    format!(" or beyond {} at once", paste::MAX_FILES)
                } else {
                    String::new()
                }
            ));
        }
        Ok(made)
    }

    /// The notice of a paste that pinned nothing (NOEUL-FR-074): short, without content.
    pub(super) fn paste_failed_notice(&mut self, e: &NoeulError) {
        let text = match e.code {
            Code::NothingToPin if e.message.contains("private") => {
                "Nothing pinned: the copied content is marked private (for example by a password manager)."
            }
            Code::NothingToPin => "Nothing to pin.",
            Code::ClipboardUnavailable => "The clipboard is in use by another program; try again.",
            Code::ImageInvalid => "The picture could not be read (malformed or too large).",
            Code::PermissionDenied => "That file is not on this PC; Noeul pins local files only.",
            _ => "The clipboard could not be pinned.",
        };
        self.shell.notice(text);
    }

    /// `pin.copy-text` (`Ctrl+Shift+C`, the menu): a card's text to the clipboard (as plain text,
    /// kept out of cloud sync like every Noeul copy, NOEUL-FR-034).
    pub(super) fn copy_text(&mut self, id: PinId) -> &'static str {
        let Some(content) = self.pins.content(id) else {
            return "not_found";
        };
        let Some(text) = content.text.clone() else {
            return "no_text";
        };
        match self.shell.clipboard_write_text(&text) {
            Ok(()) => "copied",
            Err(e) if e.code == Code::ClipboardUnavailable => {
                self.shell
                    .notice("The clipboard is in use by another program; nothing was copied.");
                "clipboard_unavailable"
            }
            Err(_) => {
                self.shell.notice("The text could not be copied.");
                "failed"
            }
        }
    }
}
