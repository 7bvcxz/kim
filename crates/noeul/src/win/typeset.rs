//! Text labels of annotations laid out with GDI, with per-character font fallback (Step 56;
//! NOEUL-FR-080, NOEUL-NFR-014; ADR 0038).
//!
//! - **Fallback.** Every character is drawn with the first installed face of [`FACES`] that has
//!   a glyph for it (`GetGlyphIndicesW` with `GGI_MARK_NONEXISTING_GLYPHS`, *documented*): Latin,
//!   Greek, Cyrillic, Hebrew and Arabic in Segoe UI, Hangul in Malgun Gothic, emoji in Segoe UI
//!   Emoji, and so on (GDI matches the face names, so a localized system that reports
//!   "맑은 고딕" for Malgun Gothic works the same). Characters outside the Basic Multilingual Plane (most emoji) cannot be
//!   asked that way; they go to Segoe UI Emoji (emoji blocks) or Segoe UI Symbol. Joiners,
//!   variation selectors, skin tones and combining marks stay with the character before them.
//!   Consecutive characters of one face form a run drawn with one `ExtTextOutW`, which shapes it
//!   (and orders a right-to-left run) through the system's script processing.
//! - **Pixels.** Runs are drawn white on black with grayscale anti-aliasing into a 32-bit DIB
//!   section; one channel is the coverage mask of [`Glyphs`]. Colour emoji are not drawn (GDI
//!   draws their outlines in the label's colour). Nothing here touches a window, the screen or the
//!   clipboard; the text never leaves the process.
//! - **Metrics.** One line height for the whole label (the largest ascent plus the largest descent
//!   of the faces it uses), the baseline at the largest ascent, and the caret before each
//!   character from the run's cumulative extents (`GetTextExtentExPointW`). A right-to-left run's
//!   carets follow its characters' order, not their visual positions (*limitation*).

use crate::annotation::{Glyphs, Typesetter};
use crate::win::wide;
use std::cell::RefCell;
use std::ffi::c_void;
use windows_sys::Win32::Foundation::SIZE;
use windows_sys::Win32::Graphics::Gdi::{
    ANTIALIASED_QUALITY, BI_RGB, BITMAPINFO, BITMAPINFOHEADER, CLIP_DEFAULT_PRECIS,
    CreateCompatibleDC, CreateDIBSection, CreateFontW, DEFAULT_CHARSET, DEFAULT_PITCH,
    DIB_RGB_COLORS, DeleteDC, DeleteObject, ExtTextOutW, FW_SEMIBOLD, GGI_MARK_NONEXISTING_GLYPHS,
    GdiFlush, GetGlyphIndicesW, GetTextExtentExPointW, GetTextMetricsW, HDC, HFONT, OUT_TT_PRECIS,
    SelectObject, SetBkMode, SetTextAlign, SetTextColor, TA_BASELINE, TA_LEFT, TEXTMETRICW,
    TRANSPARENT,
};

/// The faces tried for every character, in order (Windows 10 and 11 ship all of them).
pub const FACES: &[&str] = &[
    "Segoe UI",
    "Malgun Gothic",
    "Segoe UI Emoji",
    "Segoe UI Symbol",
    "Microsoft YaHei UI",
    "Yu Gothic UI",
    "Microsoft JhengHei UI",
    "Nirmala UI",
    "Leelawadee UI",
    "Ebrima",
    "Gadugi",
    "Myanmar Text",
    "Javanese Text",
    "Mongolian Baiti",
    "Microsoft Himalaya",
    "Segoe UI Historic",
];

/// A GDI typesetter: a memory DC and the fonts it made, freed on drop.
pub struct GdiType {
    dc: HDC,
    fonts: RefCell<Vec<(usize, u32, HFONT)>>,
}

impl std::fmt::Debug for GdiType {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("GdiType").finish_non_exhaustive()
    }
}

impl Default for GdiType {
    fn default() -> GdiType {
        GdiType::new()
    }
}

/// Whether `c` belongs with the character before it (it never starts a run of its own face).
fn attaches(c: char) -> bool {
    matches!(c as u32,
        0x200C | 0x200D            // joiners
        | 0xFE00..=0xFE0F          // variation selectors
        | 0x1F3FB..=0x1F3FF        // skin tones
        | 0xE0020..=0xE007F        // tags
        | 0x0300..=0x036F          // combining diacritics
        | 0x20D0..=0x20FF)
}

impl GdiType {
    pub fn new() -> GdiType {
        // SAFETY: a memory DC compatible with the screen; deleted on drop.
        let dc = unsafe { CreateCompatibleDC(std::ptr::null_mut()) };
        if !dc.is_null() {
            // SAFETY: our DC.
            unsafe {
                SetBkMode(dc, TRANSPARENT as i32);
                SetTextAlign(dc, TA_LEFT | TA_BASELINE);
            }
        }
        GdiType {
            dc,
            fonts: RefCell::new(Vec::new()),
        }
    }

    /// The font of face `i` at `size` pixels (em height), cached.
    fn font(&self, i: usize, size: u32) -> HFONT {
        if let Some(f) = self
            .fonts
            .borrow()
            .iter()
            .find(|(fi, s, _)| *fi == i && *s == size)
        {
            return f.2;
        }
        let name = wide(FACES[i]);
        // SAFETY: plain values and a NUL-terminated face name; deleted on drop.
        let f = unsafe {
            CreateFontW(
                -i32::try_from(size).unwrap_or(12),
                0,
                0,
                0,
                FW_SEMIBOLD as i32,
                0,
                0,
                0,
                u32::from(DEFAULT_CHARSET),
                u32::from(OUT_TT_PRECIS),
                u32::from(CLIP_DEFAULT_PRECIS),
                u32::from(ANTIALIASED_QUALITY),
                u32::from(DEFAULT_PITCH),
                name.as_ptr(),
            )
        };
        self.fonts.borrow_mut().push((i, size, f));
        f
    }

    fn has_glyph(&self, face: usize, c: char) -> bool {
        let mut buf = [0u16; 2];
        let units = c.encode_utf16(&mut buf);
        if units.len() != 1 {
            return false;
        }
        let mut idx = [0u16; 1];
        let f = self.font(face, 16);
        // SAFETY: our DC and font; one UTF-16 unit and one output index.
        let n = unsafe {
            let prev = SelectObject(self.dc, f);
            let n = GetGlyphIndicesW(
                self.dc,
                units.as_ptr(),
                1,
                idx.as_mut_ptr(),
                GGI_MARK_NONEXISTING_GLYPHS,
            );
            SelectObject(self.dc, prev);
            n
        };
        n == 1 && idx[0] != 0xFFFF
    }

    /// The face [`FACES`] index that draws `c`: the first whose font has a glyph for it. A face
    /// that is not installed is substituted by GDI; the substitute is asked and, if it has the
    /// glyph, draws it (the same font object is used for both).
    fn face_index(&self, c: char) -> usize {
        let pos = |name: &str| FACES.iter().position(|f| *f == name).unwrap_or(0);
        if (c as u32) > 0xFFFF {
            return if matches!(c as u32, 0x1F000..=0x1FAFF) {
                pos("Segoe UI Emoji")
            } else {
                pos("Segoe UI Historic")
            };
        }
        (0..FACES.len())
            .find(|i| self.has_glyph(*i, c))
            .unwrap_or(0)
    }

    /// The face that draws `c` (tests and the docs' fallback table).
    pub fn face_of(&self, c: char) -> &'static str {
        FACES[self.face_index(c)]
    }

    fn metrics(&self, face: usize, size: u32) -> (i32, i32) {
        let f = self.font(face, size);
        // SAFETY: TEXTMETRICW is plain data filled by GetTextMetricsW for our DC and font.
        unsafe {
            let mut tm: TEXTMETRICW = std::mem::zeroed();
            let prev = SelectObject(self.dc, f);
            GetTextMetricsW(self.dc, &raw mut tm);
            SelectObject(self.dc, prev);
            (tm.tmAscent, tm.tmDescent)
        }
    }

    /// `(cumulative extent after each UTF-16 unit, total)` of `run` in face `face`.
    fn extents(&self, face: usize, size: u32, run: &[u16]) -> Vec<i32> {
        if run.is_empty() {
            return Vec::new();
        }
        let f = self.font(face, size);
        let mut dx = vec![0i32; run.len()];
        let mut sz = SIZE::default();
        let mut fit = 0i32;
        // SAFETY: our DC and font; `run` and `dx` have `run.len()` elements.
        unsafe {
            let prev = SelectObject(self.dc, f);
            GetTextExtentExPointW(
                self.dc,
                run.as_ptr(),
                i32::try_from(run.len()).unwrap_or(0),
                i32::MAX,
                &raw mut fit,
                dx.as_mut_ptr(),
                &raw mut sz,
            );
            SelectObject(self.dc, prev);
        }
        dx
    }
}

/// One run of characters drawn with one face.
struct Run {
    face: usize,
    text: Vec<u16>,
    /// Characters in the run (for carets).
    chars: Vec<char>,
}

impl Typesetter for GdiType {
    fn typeset(&self, text: &str, size: u32) -> Glyphs {
        let size = size.clamp(4, 512);
        let fallback = crate::annotation::BlockType.typeset(text, size);
        if self.dc.is_null() {
            return fallback;
        }
        // Runs per line.
        let mut lines: Vec<Vec<Run>> = Vec::new();
        for line in text.split('\n') {
            let mut runs: Vec<Run> = Vec::new();
            for c in line.chars() {
                let face = match runs.last() {
                    Some(r) if attaches(c) => r.face,
                    _ => self.face_index(c),
                };
                match runs.last_mut() {
                    Some(r) if r.face == face => {
                        let mut b = [0u16; 2];
                        r.text.extend_from_slice(c.encode_utf16(&mut b));
                        r.chars.push(c);
                    }
                    _ => {
                        let mut b = [0u16; 2];
                        runs.push(Run {
                            face,
                            text: c.encode_utf16(&mut b).to_vec(),
                            chars: vec![c],
                        });
                    }
                }
            }
            lines.push(runs);
        }
        // One line height for the label: the faces it uses (or the first).
        let mut used: Vec<usize> = lines.iter().flatten().map(|r| r.face).collect();
        used.push(0);
        used.sort_unstable();
        used.dedup();
        let (mut ascent, mut descent) = (1, 0);
        for f in &used {
            let (a, d) = self.metrics(*f, size);
            ascent = ascent.max(a);
            descent = descent.max(d);
        }
        let lh = u32::try_from(ascent + descent).unwrap_or(size).max(1);
        // Carets and widths.
        let mut carets: Vec<Vec<u32>> = Vec::new();
        let mut placed: Vec<Vec<(i32, usize)>> = Vec::new(); // (x, run index) per line
        for runs in &lines {
            let mut x = 0i32;
            let mut c = vec![0u32];
            let mut at = Vec::new();
            for (ri, r) in runs.iter().enumerate() {
                let dx = self.extents(r.face, size, &r.text);
                at.push((x, ri));
                let mut unit = 0usize;
                for ch in &r.chars {
                    unit += ch.len_utf16();
                    let e = dx.get(unit - 1).copied().unwrap_or(0);
                    c.push(u32::try_from(x + e).unwrap_or(0));
                }
                x += dx.last().copied().unwrap_or(0);
            }
            carets.push(c);
            placed.push(at);
        }
        let advance = carets
            .iter()
            .map(|c| c.last().copied().unwrap_or(0))
            .max()
            .unwrap_or(0);
        // Room for glyphs that reach past their advance (italic-like overhangs, emoji).
        let slack = size / 3 + 2;
        let width = (advance + slack).max(1);
        let height = lh * u32::try_from(lines.len()).unwrap_or(1).max(1);
        let Some(mask) = self.draw(&lines, &placed, size, width, height, lh, ascent) else {
            return fallback;
        };
        Glyphs {
            text: text.to_string(),
            size,
            width,
            height,
            mask,
            line_height: lh,
            carets,
        }
    }
}

impl GdiType {
    #[allow(clippy::too_many_arguments)]
    fn draw(
        &self,
        lines: &[Vec<Run>],
        placed: &[Vec<(i32, usize)>],
        size: u32,
        width: u32,
        height: u32,
        lh: u32,
        ascent: i32,
    ) -> Option<Vec<u8>> {
        if u64::from(width) * u64::from(height) > 64 * 1024 * 1024 {
            return None;
        }
        let info = BITMAPINFO {
            bmiHeader: BITMAPINFOHEADER {
                biSize: u32::try_from(size_of::<BITMAPINFOHEADER>()).expect("small"),
                biWidth: i32::try_from(width).ok()?,
                biHeight: -i32::try_from(height).ok()?,
                biPlanes: 1,
                biBitCount: 32,
                biCompression: BI_RGB,
                ..Default::default()
            },
            ..Default::default()
        };
        let mut bits: *mut c_void = std::ptr::null_mut();
        // SAFETY: `info` describes a top-down 32-bit DIB; freed below.
        let bmp = unsafe {
            CreateDIBSection(
                self.dc,
                &raw const info,
                DIB_RGB_COLORS,
                &raw mut bits,
                std::ptr::null_mut(),
                0,
            )
        };
        if bmp.is_null() || bits.is_null() {
            return None;
        }
        let n = width as usize * height as usize;
        // SAFETY: the DIB holds `n` BGRA pixels at `bits` (zeroed by the system: black).
        unsafe {
            let prev = SelectObject(self.dc, bmp);
            SetTextColor(self.dc, 0x00FF_FFFF);
            for (li, runs) in lines.iter().enumerate() {
                let base = i32::try_from(li as u32 * lh).unwrap_or(0) + ascent;
                for (x, ri) in &placed[li] {
                    let r = &runs[*ri];
                    let f = self.font(r.face, size);
                    let pf = SelectObject(self.dc, f);
                    ExtTextOutW(
                        self.dc,
                        *x,
                        base,
                        0,
                        std::ptr::null(),
                        r.text.as_ptr(),
                        u32::try_from(r.text.len()).unwrap_or(0),
                        std::ptr::null(),
                    );
                    SelectObject(self.dc, pf);
                }
            }
            GdiFlush();
            let px = std::slice::from_raw_parts(bits.cast::<u8>(), n * 4);
            let mask: Vec<u8> = px
                .as_chunks::<4>()
                .0
                .iter()
                .map(|p| p[0].max(p[1]).max(p[2]))
                .collect();
            SelectObject(self.dc, prev);
            DeleteObject(bmp);
            Some(mask)
        }
    }
}

impl Drop for GdiType {
    fn drop(&mut self) {
        // SAFETY: every font and the DC were created by this typesetter and are freed once.
        unsafe {
            for (_, _, f) in self.fonts.borrow_mut().drain(..) {
                if !f.is_null() {
                    DeleteObject(f);
                }
            }
            if !self.dc.is_null() {
                DeleteDC(self.dc);
            }
        }
        self.dc = std::ptr::null_mut();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ink(g: &Glyphs) -> usize {
        g.mask.iter().filter(|v| **v > 64).count()
    }

    /// Korean, emoji, Latin and Hebrew are each drawn by a face that has them (NOEUL-FR-080 font
    /// fallback), off-screen, without a window.
    #[test]
    fn every_script_finds_a_face_and_draws_ink() {
        let t = GdiType::new();
        assert_eq!(t.face_of('A'), "Segoe UI");
        assert_eq!(t.face_of('\u{d55c}'), "Malgun Gothic", "한");
        assert_eq!(t.face_of('\u{1F600}'), "Segoe UI Emoji", "😀");
        assert_eq!(t.face_of('\u{5e9}'), "Segoe UI", "Hebrew shin");
        let g = t.typeset("Ab \u{d55c}\u{ae00} \u{1F600}", 24);
        assert_eq!(g.carets.len(), 1);
        assert_eq!(g.carets[0].len(), 8, "7 characters, 8 caret positions");
        assert!(
            g.carets[0].windows(2).all(|w| w[0] <= w[1]),
            "{:?}",
            g.carets
        );
        assert!(ink(&g) > 200, "ink: {}", ink(&g));
        // The Hangul is real glyphs, not the missing-glyph box of the first face.
        let hangul = t.typeset("\u{d55c}", 24);
        let tofu = t.typeset("\u{0378}", 24);
        assert_ne!(hangul.mask, tofu.mask);
        assert!(ink(&hangul) > 40);
        // Two lines: twice the height; a long label is one long mask.
        let two = t.typeset("a\nb", 18);
        assert_eq!(two.height, 2 * two.line_height);
        let long = t.typeset(&"x".repeat(500), 18);
        assert!(long.width > 2000, "{}", long.width);
    }

    /// A right-to-left run is laid out right to left: the first letter of "שלום" ends up at its
    /// right end (the system's script processing behind `ExtTextOutW`).
    #[test]
    fn hebrew_is_drawn_right_to_left() {
        let t = GdiType::new();
        let one = t.typeset("\u{5e9}", 32);
        let all = t.typeset("\u{5e9}\u{5dc}\u{5d5}\u{5dd}", 32);
        assert!(ink(&all) > ink(&one));
        let adv = one.carets[0][1] as usize;
        let col_ink = |g: &Glyphs, x0: usize, x1: usize| -> Vec<u32> {
            (x0..x1)
                .map(|x| {
                    (0..g.height as usize)
                        .map(|y| u32::from(g.mask[y * g.width as usize + x]))
                        .sum()
                })
                .collect()
        };
        let shin = col_ink(&one, 0, adv);
        let end = all.carets[0][4] as usize;
        let right = col_ink(&all, end - adv, end);
        let left = col_ink(&all, 0, adv);
        let diff = |a: &[u32], b: &[u32]| -> u64 {
            a.iter()
                .zip(b)
                .map(|(x, y)| u64::from(x.abs_diff(*y)))
                .sum()
        };
        assert!(
            diff(&shin, &right) < diff(&shin, &left),
            "shin is at the right end: {} vs {}",
            diff(&shin, &right),
            diff(&shin, &left)
        );
    }
}
