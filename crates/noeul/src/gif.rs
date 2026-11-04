//! Animated GIFs (NOEUL-FR-068): only what this build needs to be honest about them.
//!
//! Noeul has no GIF decoder yet and no animated pins: frame navigation, pause and playback speed
//! are not in this build. What it does is recognise an animation on the clipboard — programs that
//! copy a GIF offer it in the registered `GIF` format next to a still bitmap of its first frame —
//! so a pin made from that bitmap says it shows only the first frame instead of flattening the
//! animation silently ([`frame_count`] walks the block structure and counts image descriptors; it
//! decodes no pixels). Untrusted input: every length is checked and a malformed file is an error,
//! never a panic.

/// Why a GIF could not be read.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GifError {
    NotGif,
    Truncated,
    Malformed,
}

/// The most frames counted (a bound on the work for hostile input).
pub const MAX_FRAMES: u32 = 100_000;

/// How many frames (image descriptors) the GIF `data` holds.
pub fn frame_count(data: &[u8]) -> Result<u32, GifError> {
    if data.len() < 13 || !(data.starts_with(b"GIF87a") || data.starts_with(b"GIF89a")) {
        return Err(GifError::NotGif);
    }
    let mut at = 13usize;
    let packed = data[10];
    if packed & 0x80 != 0 {
        at += 3 * (1usize << ((packed & 0x07) + 1));
    }
    let mut frames = 0u32;
    // Skips data sub-blocks (a length byte, that many bytes, … a zero length).
    let skip_blocks = |mut at: usize| -> Result<usize, GifError> {
        loop {
            let n = *data.get(at).ok_or(GifError::Truncated)? as usize;
            at += 1;
            if n == 0 {
                return Ok(at);
            }
            at = at.checked_add(n).ok_or(GifError::Malformed)?;
            if at > data.len() {
                return Err(GifError::Truncated);
            }
        }
    };
    loop {
        match *data.get(at).ok_or(GifError::Truncated)? {
            // Trailer.
            0x3B => return Ok(frames),
            // Extension: label, then sub-blocks.
            0x21 => {
                data.get(at + 1).ok_or(GifError::Truncated)?;
                at = skip_blocks(at + 2)?;
            }
            // Image descriptor: 9 bytes, an optional local colour table, the LZW code size,
            // then the image data's sub-blocks.
            0x2C => {
                let desc = data.get(at + 1..at + 10).ok_or(GifError::Truncated)?;
                at += 10;
                if desc[8] & 0x80 != 0 {
                    at += 3 * (1usize << ((desc[8] & 0x07) + 1));
                }
                data.get(at).ok_or(GifError::Truncated)?;
                at = skip_blocks(at + 1)?;
                frames += 1;
                if frames >= MAX_FRAMES {
                    return Ok(frames);
                }
            }
            _ => return Err(GifError::Malformed),
        }
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;

    /// A minimal well-formed GIF89a of 2×2 pixels with `frames` frames (each a tiny image
    /// descriptor with LZW data that is never decoded here).
    pub fn sample(frames: u32) -> Vec<u8> {
        let mut g = b"GIF89a".to_vec();
        g.extend_from_slice(&[2, 0, 2, 0, 0x80, 0, 0]); // 2×2, a global table of 2 colours
        g.extend_from_slice(&[0, 0, 0, 255, 255, 255]);
        // The NETSCAPE looping extension, as animations carry it.
        g.extend_from_slice(&[0x21, 0xFF, 11]);
        g.extend_from_slice(b"NETSCAPE2.0");
        g.extend_from_slice(&[3, 1, 0, 0, 0]);
        for _ in 0..frames {
            // Graphic control extension (delay 10 cs), then the image.
            g.extend_from_slice(&[0x21, 0xF9, 4, 0, 10, 0, 0, 0]);
            g.extend_from_slice(&[0x2C, 0, 0, 0, 0, 2, 0, 2, 0, 0]);
            g.extend_from_slice(&[2, 2, 0x44, 0x01, 0]);
        }
        g.push(0x3B);
        g
    }

    #[test]
    fn frames_are_counted_without_decoding_anything() {
        assert_eq!(frame_count(&sample(1)), Ok(1));
        assert_eq!(frame_count(&sample(12)), Ok(12));
        assert_eq!(frame_count(b"GIF87a"), Err(GifError::NotGif));
        assert_eq!(frame_count(b"\x89PNG\r\n\x1a\n...."), Err(GifError::NotGif));
        let g = sample(3);
        for cut in [13, 20, g.len() - 1] {
            assert!(frame_count(&g[..cut]).is_err(), "cut at {cut}");
        }
        let mut bad = sample(1);
        let n = bad.len();
        bad[n - 1] = 0x99;
        assert_eq!(frame_count(&bad), Err(GifError::Malformed));
        // A sub-block length running past the end is refused, not read.
        let mut long = sample(1);
        let i = long.len() - 5;
        assert_eq!(long[i], 2, "the image data's sub-block length");
        long[i] = 0xFF;
        assert_eq!(frame_count(&long), Err(GifError::Truncated));
    }
}
