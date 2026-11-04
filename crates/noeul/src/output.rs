//! The copy and save outputs of a capture selection (Step 51; NOEUL-FR-023, NOEUL-FR-030…032,
//! NOEUL-SEC-005, NOEUL-SEC-007).
//!
//! Everything here is platform-independent: the pixels of a [`Frame`] are encoded in memory —
//! PNG for files and for the clipboard's registered `PNG` format, a packed `CF_DIBV5` bitmap for
//! the clipboard — and written only where the user asked:
//!
//! - **Copy** hands both encodings to the platform's clipboard writer; nothing reaches the disk.
//! - **Save** writes the file the user chose in the save dialog (the dialog asked before
//!   replacing an existing file).
//! - **Quick save** writes `noeul-YYYYMMDD-HHMMSS.png` into the quick-save folder and never
//!   overwrites: a name that exists gets `-2`, `-3`, … (NOEUL-FR-032).
//!
//! A failed write never leaves a half-written picture behind (see [`write_file`]). Saved
//! pictures carry no
//! metadata beyond what PNG needs (no text chunks, no window titles, no program names,
//! NOEUL-FR-031, NOEUL-SEC-007); a fully opaque picture is saved without an alpha channel.

use crate::frame::Frame;
use std::io::Write;
use std::path::{Path, PathBuf};

/// Whether every pixel is fully opaque.
pub fn opaque(frame: &Frame) -> bool {
    frame
        .pixels()
        .as_chunks::<4>()
        .0
        .iter()
        .all(|p| p[3] == 255)
}

/// The picture as PNG: 8-bit RGB when fully opaque, otherwise 8-bit RGBA with straight alpha.
pub fn png(frame: &Frame) -> Result<Vec<u8>, String> {
    let (w, h) = (frame.width(), frame.height());
    let rgb = opaque(frame);
    let mut data = Vec::with_capacity(frame.pixels().len());
    for p in frame.pixels().as_chunks::<4>().0 {
        data.extend_from_slice(&[p[2], p[1], p[0]]);
        if !rgb {
            data.push(p[3]);
        }
    }
    let mut out = Vec::new();
    {
        let mut enc = png::Encoder::new(&mut out, w, h);
        enc.set_color(if rgb {
            png::ColorType::Rgb
        } else {
            png::ColorType::Rgba
        });
        enc.set_depth(png::BitDepth::Eight);
        enc.set_compression(png::Compression::Fast);
        let mut writer = enc.write_header().map_err(|e| e.to_string())?;
        writer.write_image_data(&data).map_err(|e| e.to_string())?;
        writer.finish().map_err(|e| e.to_string())?;
    }
    Ok(out)
}

/// The picture as a packed `CF_DIBV5`: a `BITMAPV5HEADER` (124 bytes, `BI_BITFIELDS` with the
/// BGRA masks, sRGB) followed by bottom-up 32-bit rows — the layout browsers, office suites and
/// image editors read most reliably.
pub fn dib_v5(frame: &Frame) -> Vec<u8> {
    let (w, h) = (frame.width(), frame.height());
    let size = u32::try_from(frame.pixels().len()).unwrap_or(u32::MAX);
    let mut d = Vec::with_capacity(124 + frame.pixels().len());
    let u32le = |d: &mut Vec<u8>, v: u32| d.extend_from_slice(&v.to_le_bytes());
    u32le(&mut d, 124); // bV5Size
    d.extend_from_slice(&i32::try_from(w).unwrap_or(i32::MAX).to_le_bytes());
    d.extend_from_slice(&i32::try_from(h).unwrap_or(i32::MAX).to_le_bytes()); // bottom-up
    d.extend_from_slice(&1u16.to_le_bytes()); // planes
    d.extend_from_slice(&32u16.to_le_bytes()); // bits
    u32le(&mut d, 3); // BI_BITFIELDS
    u32le(&mut d, size); // bV5SizeImage
    u32le(&mut d, 0); // x pixels per meter
    u32le(&mut d, 0); // y pixels per meter
    u32le(&mut d, 0); // colors used
    u32le(&mut d, 0); // colors important
    u32le(&mut d, 0x00FF_0000); // red mask
    u32le(&mut d, 0x0000_FF00); // green mask
    u32le(&mut d, 0x0000_00FF); // blue mask
    u32le(&mut d, 0xFF00_0000); // alpha mask
    u32le(&mut d, 0x7352_4742); // LCS_sRGB ('sRGB')
    d.extend_from_slice(&[0; 36]); // endpoints (unused for sRGB)
    d.extend_from_slice(&[0; 12]); // gamma red, green, blue
    u32le(&mut d, 4); // LCS_GM_IMAGES
    u32le(&mut d, 0); // profile data
    u32le(&mut d, 0); // profile size
    u32le(&mut d, 0); // reserved
    debug_assert_eq!(d.len(), 124);
    let stride = w as usize * 4;
    for row in (0..h as usize).rev() {
        d.extend_from_slice(&frame.pixels()[row * stride..row * stride + stride]);
    }
    d
}

/// `noeul-YYYYMMDD-HHMMSS.png` for a local time `[year, month, day, hour, minute, second]`: no
/// window title, program name or user name (NOEUL-FR-031).
pub fn file_name(t: [u16; 6]) -> String {
    format!(
        "noeul-{:04}{:02}{:02}-{:02}{:02}{:02}.png",
        t[0], t[1], t[2], t[3], t[4], t[5]
    )
}

fn temp_beside(path: &Path) -> PathBuf {
    let name = path
        .file_name()
        .map_or_else(|| "noeul".into(), |n| n.to_string_lossy().into_owned());
    path.with_file_name(format!(".{name}.{}.tmp", std::process::id()))
}

fn write_new(path: &Path, bytes: &[u8]) -> std::io::Result<()> {
    let mut f = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(path)?;
    let done = f.write_all(bytes).and_then(|()| f.sync_all());
    if done.is_err() {
        drop(f);
        let _ = std::fs::remove_file(path);
    }
    done
}

/// Writes `bytes` to `path`. With `replace` (the save dialog asked before replacing) the
/// picture goes to a temporary file in the same folder and is renamed over `path`, so a failure
/// never leaves a half-written file under that name. Without it the name is claimed atomically
/// (`create_new`): an existing file is an error (`AlreadyExists`) and stays untouched, and a
/// failed write removes what it created.
pub fn write_file(path: &Path, bytes: &[u8], replace: bool) -> std::io::Result<()> {
    if !replace {
        return write_new(path, bytes);
    }
    let tmp = temp_beside(path);
    if let Err(e) = write_new(&tmp, bytes) {
        let _ = std::fs::remove_file(&tmp);
        return Err(e);
    }
    let placed = std::fs::rename(&tmp, path);
    if placed.is_err() {
        let _ = std::fs::remove_file(&tmp);
    }
    placed
}

/// Saves `bytes` into `folder` (created if needed) under `name`, or `name-2.png`, `name-3.png`,
/// … when taken: never overwrites (NOEUL-FR-032). Returns the path written.
pub fn quick_save(folder: &Path, name: &str, bytes: &[u8]) -> std::io::Result<PathBuf> {
    std::fs::create_dir_all(folder)?;
    let stem = name.strip_suffix(".png").unwrap_or(name);
    for n in 1..=1000u32 {
        let candidate = if n == 1 {
            folder.join(format!("{stem}.png"))
        } else {
            folder.join(format!("{stem}-{n}.png"))
        };
        match write_file(&candidate, bytes, false) {
            Ok(()) => return Ok(candidate),
            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => {}
            Err(e) => return Err(e),
        }
    }
    Err(std::io::ErrorKind::AlreadyExists.into())
}

/// A free name for `name` in `folder` (as [`quick_save`] would pick), without writing anything.
pub fn free_name(folder: &Path, name: &str) -> PathBuf {
    let stem = name.strip_suffix(".png").unwrap_or(name);
    (1..=1000u32)
        .map(|n| {
            if n == 1 {
                folder.join(format!("{stem}.png"))
            } else {
                folder.join(format!("{stem}-{n}.png"))
            }
        })
        .find(|p| !p.exists())
        .unwrap_or_else(|| folder.join(format!("{stem}.png")))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::frame::{Ledger, Rect};

    fn picture(ledger: &std::sync::Arc<Ledger>, alpha: u8) -> Frame {
        let mut px = Vec::new();
        for y in 0..3u8 {
            for x in 0..4u8 {
                px.extend_from_slice(&[x * 10, y * 20, 200, alpha]);
            }
        }
        Frame::new(ledger, Rect::new(-5, 7, 4, 3), px).unwrap()
    }

    #[test]
    fn png_is_rgb_when_opaque_and_rgba_otherwise_and_carries_no_text() {
        let ledger = Ledger::new();
        let f = picture(&ledger, 255);
        let bytes = png(&f).unwrap();
        assert_eq!(&bytes[..8], b"\x89PNG\r\n\x1a\n");
        let decoder = png::Decoder::new(std::io::Cursor::new(bytes.clone()));
        let mut reader = decoder.read_info().unwrap();
        let mut buf = vec![0; reader.output_buffer_size().unwrap()];
        let info = reader.next_frame(&mut buf).unwrap();
        assert_eq!((info.width, info.height), (4, 3));
        assert_eq!(
            info.color_type,
            png::ColorType::Rgb,
            "opaque: no alpha channel"
        );
        assert_eq!(&buf[..6], &[200, 0, 0, 200, 0, 10], "BGRA became RGB");
        assert!(reader.info().uncompressed_latin1_text.is_empty());
        assert!(reader.info().utf8_text.is_empty());
        let chunks: Vec<&[u8]> = {
            let mut v = Vec::new();
            let mut i = 8;
            while i + 8 <= bytes.len() {
                let len = u32::from_be_bytes(bytes[i..i + 4].try_into().unwrap()) as usize;
                v.push(&bytes[i + 4..i + 8]);
                i += 12 + len;
            }
            v
        };
        assert_eq!(
            chunks,
            [b"IHDR".as_slice(), b"IDAT", b"IEND"],
            "no metadata chunks"
        );

        let t = picture(&ledger, 128);
        let decoder = png::Decoder::new(std::io::Cursor::new(png(&t).unwrap()));
        let mut reader = decoder.read_info().unwrap();
        let mut buf = vec![0; reader.output_buffer_size().unwrap()];
        let info = reader.next_frame(&mut buf).unwrap();
        assert_eq!(info.color_type, png::ColorType::Rgba, "alpha kept");
        assert_eq!(&buf[..4], &[200, 0, 0, 128]);
    }

    #[test]
    fn the_dib_v5_round_trips_through_the_clipboard_decoder() {
        let ledger = Ledger::new();
        let f = picture(&ledger, 255);
        let d = dib_v5(&f);
        assert_eq!(d.len(), 124 + 4 * 3 * 4);
        assert_eq!(u32::from_le_bytes(d[0..4].try_into().unwrap()), 124);
        let back = crate::dib::decode(&ledger, &d).unwrap();
        assert_eq!(back.pixels(), f.pixels(), "same pixels, same row order");
        let t = picture(&ledger, 7);
        let back = crate::dib::decode(&ledger, &dib_v5(&t)).unwrap();
        assert_eq!(back.pixels(), t.pixels(), "alpha kept");
    }

    #[test]
    fn file_names_carry_only_the_time() {
        assert_eq!(
            file_name([2026, 11, 4, 9, 5, 7]),
            "noeul-20261104-090507.png"
        );
    }

    #[test]
    fn quick_save_never_overwrites_and_leaves_no_temporary_file() {
        let dir = std::env::temp_dir().join(format!("noeul-output-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let folder = dir.join("Pictures").join("Noeul");
        let a = quick_save(&folder, "noeul-20261104-090507.png", b"one").unwrap();
        let b = quick_save(&folder, "noeul-20261104-090507.png", b"two").unwrap();
        assert_eq!(a, folder.join("noeul-20261104-090507.png"));
        assert_eq!(b, folder.join("noeul-20261104-090507-2.png"));
        assert_eq!(std::fs::read(&a).unwrap(), b"one", "never overwritten");
        assert_eq!(
            free_name(&folder, "noeul-20261104-090507.png"),
            folder.join("noeul-20261104-090507-3.png")
        );
        // A save the dialog confirmed replaces the file.
        write_file(&a, b"three", true).unwrap();
        assert_eq!(std::fs::read(&a).unwrap(), b"three");
        assert!(write_file(&a, b"four", false).is_err());
        let names: Vec<String> = std::fs::read_dir(&folder)
            .unwrap()
            .flatten()
            .map(|e| e.file_name().to_string_lossy().into_owned())
            .collect();
        assert_eq!(names.len(), 2, "no temporary file is left: {names:?}");
        let _ = std::fs::remove_dir_all(&dir);
    }
}
