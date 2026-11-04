//! Decoding a device-independent bitmap from the clipboard (`CF_DIBV5`, `CF_DIB`) into a
//! [`Frame`] (NOEUL-FR-070, NOEUL-FR-072, NOEUL-SEC-009).
//!
//! Untrusted input: every size is checked before anything is allocated, and malformed data is
//! refused with `image_invalid` instead of producing a partial picture. Supported in this build:
//! uncompressed 24- and 32-bit pictures (`BI_RGB`, and `BI_BITFIELDS` with the usual BGRA masks),
//! bottom-up or top-down, with a `BITMAPINFOHEADER`, `BITMAPV4HEADER` or `BITMAPV5HEADER`. A
//! 32-bit picture whose alpha is all zero (common for `CF_DIB`) is treated as opaque. PNG on the
//! clipboard and the other formats of NOEUL-FR-071/072 are Step 55.

use crate::error::{Code, NoeulError};
use crate::frame::{Frame, Ledger, Rect, check_size};
use std::sync::Arc;

const BI_RGB: u32 = 0;
const BI_BITFIELDS: u32 = 3;

fn invalid(why: &str) -> NoeulError {
    NoeulError::new(
        Code::ImageInvalid,
        format!("the clipboard image cannot be read: {why}"),
    )
}

fn u16_at(b: &[u8], at: usize) -> u16 {
    u16::from_le_bytes([b[at], b[at + 1]])
}

fn u32_at(b: &[u8], at: usize) -> u32 {
    u32::from_le_bytes([b[at], b[at + 1], b[at + 2], b[at + 3]])
}

/// Decodes a packed DIB (header, optional masks or palette, pixels).
pub fn decode(ledger: &Arc<Ledger>, data: &[u8]) -> Result<Frame, NoeulError> {
    if data.len() < 40 {
        return Err(invalid("too short"));
    }
    let header = u32_at(data, 0) as usize;
    if !(header == 40 || header == 108 || header == 124) || data.len() < header {
        return Err(invalid("unknown header"));
    }
    let width = u32_at(data, 4) as i32;
    let raw_height = u32_at(data, 8) as i32;
    let planes = u16_at(data, 12);
    let bits = u16_at(data, 14);
    let compression = u32_at(data, 16);
    let colors_used = u32_at(data, 32);
    if planes != 1 || width <= 0 || raw_height == 0 || raw_height == i32::MIN {
        return Err(invalid("bad dimensions"));
    }
    let top_down = raw_height < 0;
    let height = raw_height.unsigned_abs();
    let width = width as u32;
    check_size(width, height).map_err(|e| invalid(&e.to_string()))?;
    if !(bits == 24 || bits == 32) {
        return Err(invalid(
            "only 24- and 32-bit pictures are supported in this build",
        ));
    }
    let mut offset = header;
    // Color masks follow a 40-byte header, or sit inside a V4/V5 header (which also has an
    // alpha mask).
    let masks = match compression {
        BI_RGB => None,
        BI_BITFIELDS if bits == 32 => {
            if header == 40 {
                offset += 12;
            }
            if data.len() < 52 {
                return Err(invalid("missing color masks"));
            }
            let alpha = (header >= 108).then(|| u32_at(data, 52));
            Some((u32_at(data, 40), u32_at(data, 44), u32_at(data, 48), alpha))
        }
        _ => return Err(invalid("compressed pictures are not supported")),
    };
    if let Some((r, g, b, a)) = masks
        && ((r, g, b) != (0x00FF_0000, 0x0000_FF00, 0x0000_00FF)
            || !matches!(a, None | Some(0) | Some(0xFF00_0000)))
    {
        return Err(invalid("unusual color masks are not supported"));
    }
    // A palette is not used by 24/32-bit pictures but may be present.
    offset += colors_used.min(256) as usize * 4;
    let stride = (u64::from(width) * u64::from(bits)).div_ceil(32) * 4;
    let need = stride * u64::from(height);
    let pixels = data
        .get(offset..)
        .filter(|p| p.len() as u64 >= need)
        .ok_or_else(|| invalid("truncated pixel data"))?;
    let stride = stride as usize;
    let mut out = Vec::with_capacity(width as usize * height as usize * 4);
    let mut any_alpha = false;
    for row in 0..height as usize {
        let src_row = if top_down {
            row
        } else {
            height as usize - 1 - row
        };
        let line = &pixels[src_row * stride..src_row * stride + stride];
        for x in 0..width as usize {
            if bits == 24 {
                let p = &line[x * 3..x * 3 + 3];
                out.extend_from_slice(&[p[0], p[1], p[2], 255]);
            } else {
                let p = &line[x * 4..x * 4 + 4];
                any_alpha |= p[3] != 0;
                out.extend_from_slice(p);
            }
        }
    }
    // A V4/V5 header that declares no alpha mask means opaque, whatever the fourth byte holds.
    let no_alpha_declared = matches!(masks, Some((_, _, _, Some(0))));
    if bits == 32 && (!any_alpha || no_alpha_declared) {
        for p in out.as_chunks_mut::<4>().0 {
            p[3] = 255;
        }
    }
    Frame::new(ledger, Rect::new(0, 0, width, height), out).map_err(|e| invalid(&e.to_string()))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn header(width: i32, height: i32, bits: u16, compression: u32) -> Vec<u8> {
        let mut h = Vec::new();
        h.extend_from_slice(&40u32.to_le_bytes());
        h.extend_from_slice(&width.to_le_bytes());
        h.extend_from_slice(&height.to_le_bytes());
        h.extend_from_slice(&1u16.to_le_bytes());
        h.extend_from_slice(&bits.to_le_bytes());
        h.extend_from_slice(&compression.to_le_bytes());
        h.extend_from_slice(&[0; 20]);
        h
    }

    #[test]
    fn bottom_up_24_bit_pictures_are_flipped_and_opaque() {
        let ledger = Ledger::new();
        // 2×2, rows padded to 8 bytes; bottom row first: blue, green / then red, white.
        let mut d = header(2, 2, 24, BI_RGB);
        d.extend_from_slice(&[255, 0, 0, 0, 255, 0, 0, 0]);
        d.extend_from_slice(&[0, 0, 255, 255, 255, 255, 0, 0]);
        let f = decode(&ledger, &d).unwrap();
        assert_eq!(f.rect(), Rect::new(0, 0, 2, 2));
        assert_eq!(
            f.pixels(),
            [
                0, 0, 255, 255, 255, 255, 255, 255, 255, 0, 0, 255, 0, 255, 0, 255
            ]
        );
    }

    #[test]
    fn thirty_two_bit_alpha_rules() {
        let ledger = Ledger::new();
        let mut d = header(1, -2, 32, BI_RGB);
        d.extend_from_slice(&[1, 2, 3, 0, 4, 5, 6, 0]);
        let f = decode(&ledger, &d).unwrap();
        assert_eq!(
            f.pixels(),
            [1, 2, 3, 255, 4, 5, 6, 255],
            "all-zero alpha means opaque"
        );
        let mut d = header(1, -1, 32, BI_BITFIELDS);
        d.extend_from_slice(&0x00FF_0000u32.to_le_bytes());
        d.extend_from_slice(&0x0000_FF00u32.to_le_bytes());
        d.extend_from_slice(&0x0000_00FFu32.to_le_bytes());
        d.extend_from_slice(&[9, 8, 7, 128]);
        assert_eq!(decode(&ledger, &d).unwrap().pixels(), [9, 8, 7, 128]);
    }

    #[test]
    fn malformed_pictures_are_refused_without_allocation() {
        let ledger = Ledger::new();
        let cases: Vec<Vec<u8>> = vec![
            vec![0; 10],
            header(0, 1, 24, BI_RGB),
            header(1, 0, 24, BI_RGB),
            header(1, 1, 8, BI_RGB),
            header(1, 1, 24, 1),
            header(100_000, 1, 24, BI_RGB),
            header(4, 4, 32, BI_RGB), // no pixel data
            {
                let mut d = header(1, 1, 32, BI_BITFIELDS);
                d.extend_from_slice(&[0xFF, 0, 0, 0, 0, 0xFF, 0, 0, 0, 0, 0xFF, 0, 1, 2, 3, 4]);
                d
            },
        ];
        for (i, d) in cases.iter().enumerate() {
            let e = decode(&ledger, d).unwrap_err();
            assert_eq!(e.code, Code::ImageInvalid, "case {i}");
        }
        assert_eq!(ledger.live(), (0, 0));
    }
}
