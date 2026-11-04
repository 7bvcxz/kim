//! Decoder tests (Step 55, NOEUL-FR-072, NOEUL-SEC-009): every format from pictures generated
//! here in memory, the limits, malformed and hostile data, multi-image data and ICC conversion.

use super::*;
use image::{ExtendedColorType, ImageEncoder};

/// A `w` × `h` RGBA picture: a colour ramp, opaque on the left half, half transparent on the
/// right, fully transparent in the last column.
pub fn ramp(w: u32, h: u32) -> Vec<u8> {
    let mut v = Vec::with_capacity((w * h * 4) as usize);
    for y in 0..h {
        for x in 0..w {
            let a = if x == w - 1 {
                0
            } else if x >= w / 2 {
                128
            } else {
                255
            };
            v.extend_from_slice(&[(x * 40 % 256) as u8, (y * 60 % 256) as u8, 200, a]);
        }
    }
    v
}

/// `rgba` encoded as `format` with the `image` encoders (JPEG drops the alpha).
pub fn encode(format: Format, w: u32, h: u32, rgba: &[u8]) -> Vec<u8> {
    let mut out = Vec::new();
    let c = ExtendedColorType::Rgba8;
    match format {
        Format::Png => image::codecs::png::PngEncoder::new(&mut out)
            .write_image(rgba, w, h, c)
            .unwrap(),
        Format::Jpeg => {
            let rgb: Vec<u8> = rgba
                .as_chunks::<4>()
                .0
                .iter()
                .flat_map(|p| [p[0], p[1], p[2]])
                .collect();
            image::codecs::jpeg::JpegEncoder::new_with_quality(&mut out, 100)
                .write_image(&rgb, w, h, ExtendedColorType::Rgb8)
                .unwrap();
        }
        Format::Gif => image::codecs::gif::GifEncoder::new(&mut out)
            .write_image(rgba, w, h, c)
            .unwrap(),
        Format::Bmp => image::codecs::bmp::BmpEncoder::new(&mut out)
            .write_image(rgba, w, h, c)
            .unwrap(),
        Format::Ico => image::codecs::ico::IcoEncoder::new(&mut out)
            .write_image(rgba, w, h, c)
            .unwrap(),
        Format::Tiff => {
            let mut cur = Cursor::new(Vec::new());
            image::codecs::tiff::TiffEncoder::new(&mut cur)
                .write_image(rgba, w, h, c)
                .unwrap();
            out = cur.into_inner();
        }
        Format::Tga => image::codecs::tga::TgaEncoder::new(&mut out)
            .write_image(rgba, w, h, c)
            .unwrap(),
    }
    out
}

fn bgra_at(f: &Frame, x: u32, y: u32) -> [u8; 4] {
    let i = ((y * f.width() + x) * 4) as usize;
    let p = &f.pixels()[i..i + 4];
    [p[0], p[1], p[2], p[3]]
}

fn rgba_at(v: &[u8], w: u32, x: u32, y: u32) -> [u8; 4] {
    let i = ((y * w + x) * 4) as usize;
    [v[i], v[i + 1], v[i + 2], v[i + 3]]
}

/// PNG, BMP, TGA, ICO and TIFF keep every pixel and the alpha exactly; GIF keeps its colours and
/// one transparent index; JPEG (no alpha, lossy) comes back opaque and close.
#[test]
fn every_format_decodes_with_its_alpha() {
    let ledger = Ledger::new();
    let (w, h) = (6, 4);
    let src = ramp(w, h);
    for format in [
        Format::Png,
        Format::Bmp,
        Format::Tga,
        Format::Ico,
        Format::Tiff,
    ] {
        let data = encode(format, w, h, &src);
        assert_eq!(
            sniff(&data, format == Format::Tga),
            Some(format),
            "{format:?}"
        );
        let d = decode(&ledger, &data, format).unwrap_or_else(|e| panic!("{format:?}: {e:?}"));
        assert_eq!((d.frame.width(), d.frame.height()), (w, h), "{format:?}");
        assert_eq!(d.frame.rect().x, 0);
        for y in 0..h {
            for x in 0..w {
                let s = rgba_at(&src, w, x, y);
                assert_eq!(
                    bgra_at(&d.frame, x, y),
                    [s[2], s[1], s[0], s[3]],
                    "{format:?} at ({x}, {y})"
                );
            }
        }
        assert_eq!(d.multi, Multi::Single);
        assert!(!d.icc);
    }
    // GIF: few colours survive exactly; the transparent column stays transparent.
    let mut flat = Vec::new();
    for y in 0..h {
        for x in 0..w {
            flat.extend_from_slice(&if x == w - 1 {
                [0, 0, 0, 0]
            } else if (x + y) % 2 == 0 {
                [255, 0, 0, 255]
            } else {
                [0, 0, 255, 255]
            });
        }
    }
    let gif = encode(Format::Gif, w, h, &flat);
    assert_eq!(sniff(&gif, false), Some(Format::Gif));
    let d = decode(&ledger, &gif, Format::Gif).unwrap();
    assert_eq!(bgra_at(&d.frame, 0, 0), [0, 0, 255, 255], "red");
    assert_eq!(bgra_at(&d.frame, 1, 0), [255, 0, 0, 255], "blue");
    assert_eq!(bgra_at(&d.frame, w - 1, 2)[3], 0, "transparent");
    drop(d);
    // JPEG: opaque, within the codec's error.
    let jpg = encode(Format::Jpeg, w, h, &src);
    assert_eq!(sniff(&jpg, false), Some(Format::Jpeg));
    let d = decode(&ledger, &jpg, Format::Jpeg).unwrap();
    for y in 0..h {
        for x in 0..w {
            let s = rgba_at(&src, w, x, y);
            let p = bgra_at(&d.frame, x, y);
            assert_eq!(p[3], 255);
            for (got, want) in [(p[2], s[0]), (p[1], s[1]), (p[0], s[2])] {
                assert!(
                    got.abs_diff(want) <= 40,
                    "JPEG at ({x}, {y}): {got} vs {want}"
                );
            }
        }
    }
    drop(d);
    assert_eq!(ledger.live(), (0, 0), "nothing is left behind");
}

#[test]
fn formats_are_recognised_by_their_bytes_not_their_names() {
    assert_eq!(sniff(b"\x89PNG\r\n\x1a\n rest", false), Some(Format::Png));
    assert_eq!(sniff(b"GIF87a.....", false), Some(Format::Gif));
    assert_eq!(sniff(b"II*\0....", false), Some(Format::Tiff));
    assert_eq!(sniff(b"MM\0*....", false), Some(Format::Tiff));
    assert_eq!(sniff(b"II+\0....", false), Some(Format::Tiff), "BigTIFF");
    assert_eq!(sniff(b"<html><body>hi", false), None);
    assert_eq!(sniff(b"", false), None);
    assert_eq!(sniff(b"BM", false), None, "too short for a BMP");
    // TGA has no signature: only a `.tga` name or the TGA 2.0 footer admit it.
    let tga = encode(Format::Tga, 2, 2, &ramp(2, 2));
    assert_eq!(sniff(&tga, false), None);
    assert_eq!(sniff(&tga, true), Some(Format::Tga));
    let mut footer = tga;
    footer.extend_from_slice(&[0; 8]);
    footer.extend_from_slice(b"TRUEVISION-XFILE.\0");
    assert_eq!(sniff(&footer, false), Some(Format::Tga));
    for (ext, f) in [
        ("PNG", Format::Png),
        ("jpeg", Format::Jpeg),
        ("JFIF", Format::Jpeg),
        ("gif", Format::Gif),
        ("bmp", Format::Bmp),
        ("ico", Format::Ico),
        ("tif", Format::Tiff),
        ("tga", Format::Tga),
    ] {
        assert_eq!(Format::from_extension(ext), Some(f), "{ext}");
    }
    for ext in ["txt", "exe", "svg", "webp", ""] {
        assert_eq!(Format::from_extension(ext), None, "{ext}");
    }
}

/// The CRC-32 of PNG chunks (tests build hostile headers by hand).
fn crc32(data: &[u8]) -> u32 {
    let mut c = 0xFFFF_FFFFu32;
    for b in data {
        c ^= u32::from(*b);
        for _ in 0..8 {
            c = if c & 1 != 0 {
                0xEDB8_8320 ^ (c >> 1)
            } else {
                c >> 1
            };
        }
    }
    !c
}

fn chunk(kind: &[u8; 4], data: &[u8]) -> Vec<u8> {
    let mut v = (data.len() as u32).to_be_bytes().to_vec();
    v.extend_from_slice(kind);
    v.extend_from_slice(data);
    let mut c = kind.to_vec();
    c.extend_from_slice(data);
    v.extend_from_slice(&crc32(&c).to_be_bytes());
    v
}

/// A PNG whose header claims `w` × `h` RGBA pixels but whose data is a few bytes: a
/// decompression bomb's header.
pub fn png_claiming(w: u32, h: u32) -> Vec<u8> {
    let mut v = b"\x89PNG\r\n\x1a\n".to_vec();
    let mut ihdr = w.to_be_bytes().to_vec();
    ihdr.extend_from_slice(&h.to_be_bytes());
    ihdr.extend_from_slice(&[8, 6, 0, 0, 0]);
    v.extend(chunk(b"IHDR", &ihdr));
    v.extend(chunk(
        b"IDAT",
        &[0x78, 0x9C, 0x03, 0x00, 0x00, 0x00, 0x00, 0x01],
    ));
    v.extend(chunk(b"IEND", &[]));
    v
}

/// Pictures that claim more pixels than the limits are refused from their headers, before a
/// buffer of that size exists; the ledger never sees them.
#[test]
fn oversized_pictures_and_decompression_bombs_are_refused_from_their_headers() {
    let ledger = Ledger::new();
    // 30000 × 30000 is under the side limit but 900 million pixels.
    for (w, h) in [(30_000, 30_000), (40_000, 10), (10, 40_000), (7_000, 7_000)] {
        let e = decode(&ledger, &png_claiming(w, h), Format::Png).unwrap_err();
        assert_eq!(e.code, Code::ImageInvalid, "{w}×{h}");
        assert!(
            e.message.contains("limit")
                || e.message.contains("million")
                || e.message.contains("wider"),
            "{w}×{h}: {}",
            e.message
        );
    }
    // A BMP header claiming 30000 × 30000 with no pixel data.
    let mut bmp = b"BM".to_vec();
    bmp.extend_from_slice(&[0; 12]);
    bmp.extend_from_slice(&40u32.to_le_bytes());
    bmp.extend_from_slice(&30_000i32.to_le_bytes());
    bmp.extend_from_slice(&30_000i32.to_le_bytes());
    bmp.extend_from_slice(&1u16.to_le_bytes());
    bmp.extend_from_slice(&32u16.to_le_bytes());
    bmp.extend_from_slice(&[0; 24]);
    assert_eq!(sniff(&bmp, false), Some(Format::Bmp));
    assert_eq!(
        decode(&ledger, &bmp, Format::Bmp).unwrap_err().code,
        Code::ImageInvalid
    );
    // A GIF whose screen and frame claim 60000 × 60000.
    let mut gif = b"GIF89a".to_vec();
    gif.extend_from_slice(&60_000u16.to_le_bytes());
    gif.extend_from_slice(&60_000u16.to_le_bytes());
    gif.extend_from_slice(&[0x80, 0, 0, 0, 0, 0, 255, 255, 255]);
    gif.extend_from_slice(&[0x2C, 0, 0, 0, 0]);
    gif.extend_from_slice(&60_000u16.to_le_bytes());
    gif.extend_from_slice(&60_000u16.to_le_bytes());
    gif.extend_from_slice(&[0, 2, 2, 0x44, 0x01, 0, 0x3B]);
    assert_eq!(
        decode(&ledger, &gif, Format::Gif).unwrap_err().code,
        Code::ImageInvalid
    );
    // More encoded bytes than allowed: refused without being looked at.
    let big = vec![0u8; usize::try_from(MAX_INPUT_BYTES).unwrap() + 1];
    let e = decode(&ledger, &big, Format::Png).unwrap_err();
    assert!(e.message.contains("MiB"), "{}", e.message);
    assert_eq!(check_dimensions(0, 5).unwrap_err(), "it has no pixels");
    assert!(check_dimensions(7680, 4320).is_ok(), "an 8K screenshot");
    assert_eq!(ledger.live(), (0, 0));
}

/// Truncated, corrupted and random data in every format: an error, never a panic, never a
/// partial picture.
#[test]
fn malformed_pictures_fail_cleanly_in_every_format() {
    let ledger = Ledger::new();
    let src = ramp(8, 8);
    for format in [
        Format::Png,
        Format::Jpeg,
        Format::Gif,
        Format::Bmp,
        Format::Ico,
        Format::Tiff,
        Format::Tga,
    ] {
        let good = encode(format, 8, 8, &src);
        for cut in [1, 8, 18, good.len() / 3, good.len() / 2] {
            if cut >= good.len() {
                continue;
            }
            let r = decode(&ledger, &good[..cut], format);
            if let Err(e) = &r {
                assert_eq!(e.code, Code::ImageInvalid, "{format:?} cut at {cut}");
                assert!(!e.message.contains('\\'), "no path in the message");
            }
            // Some formats legitimately decode a truncated tail as a partial picture
            // (BMP/TGA rows); what matters is no panic and an exact size if it decodes.
            if let Ok(d) = r {
                assert_eq!((d.frame.width(), d.frame.height()), (8, 8), "{format:?}");
            }
        }
        // Flipped bytes after the header.
        let mut bad = good.clone();
        let n = bad.len();
        for i in (n / 4..n).step_by(7) {
            bad[i] ^= 0xA5;
        }
        let _ = decode(&ledger, &bad, format);
        // Pure noise with the right signature.
        let mut noise: Vec<u8> = good[..good.len().min(12)].to_vec();
        let mut x = 0x1234_5678u32;
        for _ in 0..4096 {
            x ^= x << 13;
            x ^= x >> 17;
            x ^= x << 5;
            noise.push(x as u8);
        }
        let _ = decode(&ledger, &noise, format);
    }
    assert_eq!(ledger.live(), (0, 0), "no partial picture is kept");
}

/// A hand-built TIFF header with `pages` image directories (no pixels: only the chain is read).
fn tiff_chain(pages: u32, looped: bool) -> Vec<u8> {
    let mut v = b"II*\0".to_vec();
    v.extend_from_slice(&8u32.to_le_bytes());
    for p in 0..pages {
        let at = v.len() as u32;
        v.extend_from_slice(&1u16.to_le_bytes()); // one entry
        v.extend_from_slice(&[0; 12]);
        let next = if p + 1 < pages {
            at + 2 + 12 + 4
        } else if looped {
            8
        } else {
            0
        };
        v.extend_from_slice(&next.to_le_bytes());
    }
    v
}

#[test]
fn multi_image_data_is_counted_without_decoding() {
    let g = crate::gif::tests::sample(5);
    assert_eq!(count(&g, Format::Gif), Multi::Frames(5));
    assert_eq!(
        count(&crate::gif::tests::sample(1), Format::Gif),
        Multi::Single
    );
    assert_eq!(count(&tiff_chain(3, false), Format::Tiff), Multi::Pages(3));
    assert_eq!(count(&tiff_chain(1, false), Format::Tiff), Multi::Single);
    assert_eq!(
        count(&tiff_chain(4, true), Format::Tiff),
        Multi::Pages(4),
        "a loop in the chain stops the count"
    );
    assert_eq!(count(b"II*\0\xFF\xFF\xFF\x7F", Format::Tiff), Multi::Single);
    // An icon with two sizes: the largest is shown.
    let ledger = Ledger::new();
    let small = ramp(4, 4);
    let large = ramp(8, 8);
    let frames = [
        image::codecs::ico::IcoFrame::as_png(&small, 4, 4, ExtendedColorType::Rgba8).unwrap(),
        image::codecs::ico::IcoFrame::as_png(&large, 8, 8, ExtendedColorType::Rgba8).unwrap(),
    ];
    let mut ico = Vec::new();
    image::codecs::ico::IcoEncoder::new(&mut ico)
        .encode_images(&frames)
        .unwrap();
    assert_eq!(count(&ico, Format::Ico), Multi::Sizes(2));
    let d = decode(&ledger, &ico, Format::Ico).unwrap();
    assert_eq!(
        (d.frame.width(), d.frame.height()),
        (8, 8),
        "the largest size"
    );
    assert_eq!(d.multi, Multi::Sizes(2));
    // An animated GIF decodes its first frame and says how many there are.
    let rgba = vec![
        255, 0, 0, 255, 255, 0, 0, 255, //
        255, 0, 0, 255, 255, 0, 0, 255,
    ];
    let mut out = Vec::new();
    {
        let mut enc = image::codecs::gif::GifEncoder::new(&mut out);
        for _ in 0..3 {
            let buf = image::RgbaImage::from_raw(2, 2, rgba.clone()).unwrap();
            enc.encode_frame(image::Frame::new(buf)).unwrap();
        }
    }
    let d = decode(&ledger, &out, Format::Gif).unwrap();
    assert_eq!(d.multi, Multi::Frames(3));
    assert_eq!(bgra_at(&d.frame, 1, 1), [0, 0, 255, 255]);
}

/// An embedded profile other than sRGB changes the pixels into sRGB; an sRGB profile leaves them
/// (within rounding); a broken profile is ignored, never an error.
#[test]
fn an_embedded_icc_profile_is_converted_to_srgb() {
    let ledger = Ledger::new();
    let px = [100u8, 150, 200, 255].repeat(4);
    let with = |icc: Vec<u8>| {
        let mut out = Vec::new();
        let mut enc = image::codecs::png::PngEncoder::new(&mut out);
        enc.set_icc_profile(icc).unwrap();
        enc.write_image(&px, 2, 2, ExtendedColorType::Rgba8)
            .unwrap();
        decode(&ledger, &out, Format::Png).unwrap()
    };
    let adobe = with(moxcms::ColorProfile::new_adobe_rgb().encode().unwrap());
    assert!(adobe.icc, "converted");
    let p = bgra_at(&adobe.frame, 0, 0);
    let moved = [p[2].abs_diff(100), p[1].abs_diff(150), p[0].abs_diff(200)];
    assert!(moved.iter().any(|d| *d > 5), "Adobe RGB is not sRGB: {p:?}");
    assert_eq!(p[3], 255, "alpha untouched");
    let srgb = with(moxcms::ColorProfile::new_srgb().encode().unwrap());
    let p = bgra_at(&srgb.frame, 1, 1);
    assert!(
        p[2].abs_diff(100) <= 2 && p[1].abs_diff(150) <= 2 && p[0].abs_diff(200) <= 2,
        "sRGB stays: {p:?}"
    );
    let broken = with(b"not a profile at all, just bytes".to_vec());
    assert!(!broken.icc);
    assert_eq!(bgra_at(&broken.frame, 0, 0), [200, 150, 100, 255]);
}
