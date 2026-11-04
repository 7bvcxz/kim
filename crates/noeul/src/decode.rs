//! Local image decoding for pins (Step 55; NOEUL-FR-072, NOEUL-SEC-009; ADR 0047).
//!
//! Pasted and dropped pictures are untrusted input. Every decode here:
//!
//! - **recognises the format by its first bytes** (PNG, JPEG, GIF, BMP, ICO/CUR, TIFF), never by a
//!   file name alone; TGA, which has no signature, is accepted only when the caller names it
//!   (a `.tga` file) or the data carries the TGA 2.0 footer;
//! - **is bounded before anything large is allocated**: at most [`MAX_INPUT_BYTES`] of encoded
//!   data, at most [`frame::MAX_SIDE`] pixels on a side and [`MAX_PIXELS`] in all, and at most
//!   [`MAX_DECODE_BYTES`] for the decoder's own buffers (`image::Limits`), so a small file that
//!   claims a huge picture (a decompression bomb) is refused from its header;
//! - **never panics the instance**: the decoders are pure Rust (`image` with the `png`, `jpeg`,
//!   `gif`, `bmp`, `ico`, `tga` and `tiff` features), and a decoder that panics on hostile input is
//!   caught and reported as `image_invalid` like any malformed picture;
//! - **keeps alpha** (straight BGRA in a [`Frame`]) and **converts an embedded ICC profile to
//!   sRGB** (moxcms, the colour engine `image` already uses), the space pins are shown in; EXIF
//!   orientation is applied so a photo stands the right way up. Nothing else of the file's
//!   metadata (EXIF, XMP, text chunks, the ICC profile itself) is kept: a pin's copy and save
//!   write only the pixels (IHDR, IDAT, IEND; the Step 57 rule), so no metadata reaches an export;
//! - **distinguishes multi-image data**: an animated GIF's frames, a TIFF's pages and an ICO's
//!   sizes are counted ([`Multi`]); the pin shows the first frame or page (the largest icon) and
//!   the caller says so.
//!
//! Errors name the format and the limit, never a path or a pixel (NOEUL-SEC-008).

use crate::error::{Code, NoeulError};
use crate::frame::{self, Frame, Ledger, Rect};
use std::io::Cursor;
use std::sync::Arc;

/// The most encoded bytes one picture may have (a file or one clipboard format).
pub const MAX_INPUT_BYTES: u64 = 128 * 1024 * 1024;
/// The most pixels one picture may have: an 8K screenshot (7680 × 4320) fits.
pub const MAX_PIXELS: u64 = 40_000_000;
/// The most memory a decoder may allocate for one picture (16-bit RGBA of [`MAX_PIXELS`]).
pub const MAX_DECODE_BYTES: u64 = MAX_PIXELS * 8;

/// An image format Noeul decodes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Format {
    Png,
    Jpeg,
    Gif,
    Bmp,
    Ico,
    Tiff,
    Tga,
}

impl Format {
    pub fn as_str(self) -> &'static str {
        match self {
            Format::Png => "PNG",
            Format::Jpeg => "JPEG",
            Format::Gif => "GIF",
            Format::Bmp => "BMP",
            Format::Ico => "ICO",
            Format::Tiff => "TIFF",
            Format::Tga => "TGA",
        }
    }

    /// The format a file name's extension names (ASCII, any case), if Noeul decodes it.
    pub fn from_extension(ext: &str) -> Option<Format> {
        Some(match ext.to_ascii_lowercase().as_str() {
            "png" => Format::Png,
            "jpg" | "jpeg" | "jpe" | "jfif" => Format::Jpeg,
            "gif" => Format::Gif,
            "bmp" | "dib" => Format::Bmp,
            "ico" | "cur" => Format::Ico,
            "tif" | "tiff" => Format::Tiff,
            "tga" | "icb" | "vda" | "vst" => Format::Tga,
            _ => return None,
        })
    }

    fn image(self) -> image::ImageFormat {
        match self {
            Format::Png => image::ImageFormat::Png,
            Format::Jpeg => image::ImageFormat::Jpeg,
            Format::Gif => image::ImageFormat::Gif,
            Format::Bmp => image::ImageFormat::Bmp,
            Format::Ico => image::ImageFormat::Ico,
            Format::Tiff => image::ImageFormat::Tiff,
            Format::Tga => image::ImageFormat::Tga,
        }
    }
}

/// The format of `data` from its signature; `tga_hint` (the caller knows it is a TGA file)
/// admits TGA, which has none.
pub fn sniff(data: &[u8], tga_hint: bool) -> Option<Format> {
    let starts = |s: &[u8]| data.starts_with(s);
    if starts(b"\x89PNG\r\n\x1a\n") {
        Some(Format::Png)
    } else if starts(&[0xFF, 0xD8, 0xFF]) {
        Some(Format::Jpeg)
    } else if starts(b"GIF87a") || starts(b"GIF89a") {
        Some(Format::Gif)
    } else if starts(b"BM") && data.len() >= 26 {
        Some(Format::Bmp)
    } else if (starts(&[0, 0, 1, 0]) || starts(&[0, 0, 2, 0])) && data.len() >= 6 {
        Some(Format::Ico)
    } else if starts(b"II*\0") || starts(b"MM\0*") || starts(b"II+\0") || starts(b"MM\0+") {
        Some(Format::Tiff)
    } else if tga_hint || data.ends_with(b"TRUEVISION-XFILE.\0") {
        (data.len() >= 18).then_some(Format::Tga)
    } else {
        None
    }
}

/// Several pictures in one: the pin shows the first (the largest icon) and says so.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Multi {
    /// One picture.
    Single,
    /// An animated GIF of this many frames.
    Frames(u32),
    /// A TIFF of this many pages.
    Pages(u32),
    /// An icon file holding this many sizes (the largest is shown, as icon viewers do).
    Sizes(u32),
}

/// A decoded picture: at (0, 0), straight-alpha BGRA.
#[derive(Debug)]
pub struct Decoded {
    pub frame: Frame,
    pub format: Format,
    pub multi: Multi,
    /// An embedded ICC profile was converted to sRGB.
    pub icc: bool,
}

fn invalid(format: Option<Format>, why: impl std::fmt::Display) -> NoeulError {
    let what = format.map_or("image", Format::as_str);
    NoeulError::new(
        Code::ImageInvalid,
        format!("the {what} picture cannot be read: {why}"),
    )
    .hint("nothing was pinned; the picture is malformed or larger than Noeul's limits")
}

/// The limit a picture of `w` × `h` breaks, if any.
pub fn check_dimensions(w: u32, h: u32) -> Result<(), String> {
    if w == 0 || h == 0 {
        return Err("it has no pixels".into());
    }
    if w > frame::MAX_SIDE || h > frame::MAX_SIDE {
        return Err(format!(
            "{w} × {h} pixels is wider or higher than {} pixels",
            frame::MAX_SIDE
        ));
    }
    if u64::from(w) * u64::from(h) > MAX_PIXELS {
        return Err(format!(
            "{w} × {h} pixels is more than {} million pixels",
            MAX_PIXELS / 1_000_000
        ));
    }
    Ok(())
}

/// Decodes `data` (`format` from [`sniff`]) into a frame at (0, 0).
pub fn decode(ledger: &Arc<Ledger>, data: &[u8], format: Format) -> Result<Decoded, NoeulError> {
    if data.len() as u64 > MAX_INPUT_BYTES {
        return Err(invalid(
            Some(format),
            format!("it is larger than {} MiB", MAX_INPUT_BYTES / (1024 * 1024)),
        ));
    }
    let multi = count(data, format);
    if let Multi::Frames(n) = multi
        && n == 0
    {
        return Err(invalid(Some(format), "the GIF has no frame"));
    }
    // A hostile file may make a decoder panic: report it like any malformed picture (the
    // release profile unwinds, so the instance survives; CONTRIBUTING, build profiles).
    let run = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| pixels(data, format)));
    let (w, h, rgba, icc) = match run {
        Ok(r) => r.map_err(|why| invalid(Some(format), why))?,
        Err(_) => return Err(invalid(Some(format), "the decoder refused the data")),
    };
    let mut bgra = rgba;
    for p in bgra.as_chunks_mut::<4>().0 {
        p.swap(0, 2);
    }
    let frame =
        Frame::new(ledger, Rect::new(0, 0, w, h), bgra).map_err(|e| invalid(Some(format), e))?;
    Ok(Decoded {
        frame,
        format,
        multi,
        icc,
    })
}

/// `(width, height, RGBA8, ICC converted)`.
fn pixels(data: &[u8], format: Format) -> Result<(u32, u32, Vec<u8>, bool), String> {
    use image::ImageDecoder;
    let mut reader = image::ImageReader::with_format(Cursor::new(data), format.image());
    let mut limits = image::Limits::default();
    limits.max_image_width = Some(frame::MAX_SIDE);
    limits.max_image_height = Some(frame::MAX_SIDE);
    limits.max_alloc = Some(MAX_DECODE_BYTES);
    reader.limits(limits);
    let mut decoder = reader.into_decoder().map_err(describe)?;
    let (w, h) = decoder.dimensions();
    check_dimensions(w, h)?;
    if decoder.total_bytes() > MAX_DECODE_BYTES {
        return Err("it needs more memory than Noeul allows for one picture".into());
    }
    let icc = decoder.icc_profile().ok().flatten();
    let orientation = decoder
        .orientation()
        .unwrap_or(image::metadata::Orientation::NoTransforms);
    let mut img = image::DynamicImage::from_decoder(decoder).map_err(describe)?;
    img.apply_orientation(orientation);
    let (w, h) = (img.width(), img.height());
    check_dimensions(w, h)?;
    let mut rgba = img.into_rgba8().into_raw();
    let converted = icc.is_some_and(|p| to_srgb(&p, &mut rgba));
    Ok((w, h, rgba, converted))
}

/// Converts RGBA8 pixels from the embedded profile `icc` to sRGB in place; `false` (pixels kept
/// as they are, shown as sRGB) when the profile cannot be read or connected.
fn to_srgb(icc: &[u8], rgba: &mut [u8]) -> bool {
    use moxcms::{ColorProfile, Layout, TransformOptions};
    if icc.len() > 4 * 1024 * 1024 {
        return false;
    }
    let Ok(src) = ColorProfile::new_from_slice(icc) else {
        return false;
    };
    let Ok(t) = src.create_transform_8bit(
        Layout::Rgba,
        &ColorProfile::new_srgb(),
        Layout::Rgba,
        TransformOptions::default(),
    ) else {
        return false;
    };
    let mut out = vec![0u8; rgba.len()];
    if t.transform(rgba, &mut out).is_err() {
        return false;
    }
    rgba.copy_from_slice(&out);
    true
}

/// A decoder error in words (the decoders' own messages name no path: they read from memory).
fn describe(e: image::ImageError) -> String {
    match e {
        image::ImageError::Limits(_) => "it is larger than Noeul's limits".into(),
        image::ImageError::Unsupported(_) => "this variant of the format is not supported".into(),
        other => {
            let s = other.to_string();
            // Keep messages short; they are shown in a notice and returned in JSON.
            s.chars().take(160).collect()
        }
    }
}

/// How many pictures `data` holds, from its structure only (nothing is decoded).
pub fn count(data: &[u8], format: Format) -> Multi {
    match format {
        Format::Gif => match crate::gif::frame_count(data) {
            Ok(n) if n > 1 => Multi::Frames(n),
            Ok(0) => Multi::Frames(0),
            _ => Multi::Single,
        },
        Format::Tiff => match tiff_pages(data) {
            n if n > 1 => Multi::Pages(n),
            _ => Multi::Single,
        },
        Format::Ico => match data.get(4..6).map(|b| u16::from_le_bytes([b[0], b[1]])) {
            Some(n) if n > 1 => Multi::Sizes(u32::from(n)),
            _ => Multi::Single,
        },
        _ => Multi::Single,
    }
}

/// The most TIFF pages counted (a bound on the work for hostile input).
pub const MAX_PAGES: u32 = 10_000;

/// How many image file directories (pages) a TIFF chains; 0 when the chain is unreadable. A loop
/// in the chain stops the count.
fn tiff_pages(data: &[u8]) -> u32 {
    let le = data.starts_with(b"II");
    let big = data.get(2..4) == Some(if le { &[43, 0] } else { &[0, 43] });
    let rd = |at: usize, n: usize| -> Option<u64> {
        let b = data.get(at..at.checked_add(n)?)?;
        let mut v = 0u64;
        for i in 0..n {
            let byte = u64::from(if le { b[n - 1 - i] } else { b[i] });
            v = (v << 8) | byte;
        }
        Some(v)
    };
    let (mut next, entry, count_len, off_len) = if big {
        (rd(8, 8), 20usize, 8usize, 8usize)
    } else {
        (rd(4, 4), 12, 2, 4)
    };
    let mut seen = std::collections::BTreeSet::new();
    let mut pages = 0u32;
    while let Some(at) = next.and_then(|v| usize::try_from(v).ok()) {
        if at == 0 || !seen.insert(at) || pages >= MAX_PAGES {
            break;
        }
        let Some(n) = rd(at, count_len).and_then(|v| usize::try_from(v).ok()) else {
            break;
        };
        let Some(after) = n
            .checked_mul(entry)
            .and_then(|e| e.checked_add(at + count_len))
        else {
            break;
        };
        pages += 1;
        next = rd(after, off_len);
    }
    pages
}

#[cfg(test)]
pub(crate) mod tests;
