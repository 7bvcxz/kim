//! The cards' text engine on Windows (Step 55; NOEUL-FR-071, ADR 0047): GDI glyphs with
//! per-character font fallback, as coverage masks for [`crate::card`] to blend.
//!
//! - **Faces.** Proportional text uses Segoe UI, monospace text Consolas; every character that face
//!   lacks is drawn with the first face of [`typeset::FACES`] that has it (Malgun Gothic for
//!   Hangul, Segoe UI Emoji, …; `GetGlyphIndicesW` with `GGI_MARK_NONEXISTING_GLYPHS`, as the
//!   annotation labels do). Characters outside the Basic Multilingual Plane go to Segoe UI Emoji
//!   or Segoe UI Historic.
//! - **Pixels.** Each run is drawn white on black with grayscale anti-aliasing into a 32-bit DIB
//!   section of a memory DC; its brightest channel is the coverage. Nothing here touches a
//!   window, the screen or the clipboard, and the text never leaves the process.
//! - **Metrics.** A line's ascent and descent are the larger of the face's and of the fallback
//!   faces most text needs (Malgun Gothic, Segoe UI Emoji), so lines of mixed scripts keep one
//!   height.

use crate::card::{Font, Mask, TextEngine};
use crate::win::typeset::FACES;
use crate::win::wide;
use std::collections::HashMap;
use std::ffi::c_void;
use windows_sys::Win32::Foundation::SIZE;
use windows_sys::Win32::Graphics::Gdi::{
    ANTIALIASED_QUALITY, BI_RGB, BITMAPINFO, BITMAPINFOHEADER, CLIP_DEFAULT_PRECIS,
    CreateCompatibleDC, CreateDIBSection, CreateFontW, DEFAULT_CHARSET, DEFAULT_PITCH,
    DIB_RGB_COLORS, DeleteDC, DeleteObject, ExtTextOutW, FW_BOLD, FW_NORMAL,
    GGI_MARK_NONEXISTING_GLYPHS, GdiFlush, GetGlyphIndicesW, GetTextExtentPoint32W,
    GetTextMetricsW, HDC, HFONT, OUT_TT_PRECIS, SelectObject, SetBkMode, SetTextAlign,
    SetTextColor, TA_BASELINE, TA_LEFT, TEXTMETRICW, TRANSPARENT,
};

/// The first face of monospace text.
const MONO: &str = "Consolas";

/// The face list index: 0 is the font's own face (Segoe UI or Consolas), then [`FACES`] after
/// Segoe UI.
fn face_name(mono: bool, i: usize) -> &'static str {
    match (mono, i) {
        (true, 0) => MONO,
        (false, 0) => FACES[0],
        (_, i) => FACES.get(i).copied().unwrap_or(FACES[0]),
    }
}

/// The GDI text engine: a memory DC, its fonts and the faces chosen per character.
pub struct GdiText {
    dc: HDC,
    fonts: HashMap<(usize, Font), HFONT>,
    faces: HashMap<(bool, char), usize>,
}

impl std::fmt::Debug for GdiText {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("GdiText").finish_non_exhaustive()
    }
}

impl Default for GdiText {
    fn default() -> GdiText {
        GdiText::new()
    }
}

impl GdiText {
    pub fn new() -> GdiText {
        // SAFETY: a memory DC compatible with the screen; deleted on drop.
        let dc = unsafe { CreateCompatibleDC(std::ptr::null_mut()) };
        if !dc.is_null() {
            // SAFETY: our DC.
            unsafe {
                SetBkMode(dc, TRANSPARENT as i32);
                SetTextAlign(dc, TA_LEFT | TA_BASELINE);
            }
        }
        GdiText {
            dc,
            fonts: HashMap::new(),
            faces: HashMap::new(),
        }
    }

    /// Whether GDI could make its memory DC.
    pub fn ok(&self) -> bool {
        !self.dc.is_null()
    }

    fn font(&mut self, face: usize, font: Font) -> HFONT {
        if let Some(f) = self.fonts.get(&(face, font)) {
            return *f;
        }
        let name = wide(face_name(font.mono, face));
        // SAFETY: plain values and a NUL-terminated face name; deleted on drop.
        let f = unsafe {
            CreateFontW(
                -i32::try_from(font.px.clamp(4, 512)).unwrap_or(14),
                0,
                0,
                0,
                if font.bold { FW_BOLD } else { FW_NORMAL } as i32,
                u32::from(font.italic),
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
        self.fonts.insert((face, font), f);
        f
    }

    fn has_glyph(&mut self, face: usize, mono: bool, c: char) -> bool {
        let mut buf = [0u16; 2];
        let units = c.encode_utf16(&mut buf);
        if units.len() != 1 {
            return false;
        }
        let probe = Font {
            px: 16,
            bold: false,
            italic: false,
            mono,
        };
        let f = self.font(face, probe);
        let mut idx = [0u16; 1];
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

    /// The face index that draws `c`.
    fn face_of(&mut self, mono: bool, c: char) -> usize {
        if let Some(i) = self.faces.get(&(mono, c)) {
            return *i;
        }
        let pos = |name: &str| FACES.iter().position(|f| *f == name).unwrap_or(0);
        let i = if (c as u32) > 0xFFFF {
            if matches!(c as u32, 0x1F000..=0x1FAFF) {
                pos("Segoe UI Emoji")
            } else {
                pos("Segoe UI Historic")
            }
        } else if c.is_whitespace() || self.has_glyph(0, mono, c) {
            0
        } else {
            (1..FACES.len())
                .find(|i| self.has_glyph(*i, mono, c))
                .unwrap_or(0)
        };
        if self.faces.len() < 4096 {
            self.faces.insert((mono, c), i);
        }
        i
    }

    /// `text` cut into runs of one face (joiners and marks stay with the character before).
    fn runs(&mut self, text: &str, mono: bool) -> Vec<(usize, Vec<u16>)> {
        let mut runs: Vec<(usize, Vec<u16>)> = Vec::new();
        for c in text.chars() {
            let attaches = matches!(c as u32,
                0x200C | 0x200D | 0xFE00..=0xFE0F | 0x1F3FB..=0x1F3FF | 0xE0020..=0xE007F
                | 0x0300..=0x036F | 0x1AB0..=0x1AFF | 0x20D0..=0x20FF);
            let face = match runs.last() {
                Some(r) if attaches => r.0,
                _ => self.face_of(mono, c),
            };
            let mut b = [0u16; 2];
            let units = c.encode_utf16(&mut b);
            match runs.last_mut() {
                Some(r) if r.0 == face => r.1.extend_from_slice(units),
                _ => runs.push((face, units.to_vec())),
            }
        }
        runs
    }

    fn face_metrics(&mut self, face: usize, font: Font) -> (u32, u32) {
        let f = self.font(face, font);
        // SAFETY: TEXTMETRICW is plain data filled by GetTextMetricsW for our DC and font.
        unsafe {
            let mut tm: TEXTMETRICW = std::mem::zeroed();
            let prev = SelectObject(self.dc, f);
            GetTextMetricsW(self.dc, &raw mut tm);
            SelectObject(self.dc, prev);
            (
                u32::try_from(tm.tmAscent).unwrap_or(0),
                u32::try_from(tm.tmDescent).unwrap_or(0),
            )
        }
    }

    fn run_width(&mut self, face: usize, font: Font, run: &[u16]) -> u32 {
        if run.is_empty() {
            return 0;
        }
        let f = self.font(face, font);
        let mut sz = SIZE::default();
        // SAFETY: our DC and font; `run` has `run.len()` units.
        unsafe {
            let prev = SelectObject(self.dc, f);
            GetTextExtentPoint32W(
                self.dc,
                run.as_ptr(),
                i32::try_from(run.len()).unwrap_or(0),
                &raw mut sz,
            );
            SelectObject(self.dc, prev);
        }
        u32::try_from(sz.cx).unwrap_or(0)
    }
}

impl TextEngine for GdiText {
    fn metrics(&mut self, font: Font) -> (u32, u32) {
        if self.dc.is_null() {
            return crate::card::BlockText.metrics(font);
        }
        let mut a = 1;
        let mut d = 0;
        let pos = |name: &str| FACES.iter().position(|f| *f == name).unwrap_or(0);
        for face in [0, pos("Malgun Gothic"), pos("Segoe UI Emoji")] {
            let (fa, fd) = self.face_metrics(face, font);
            a = a.max(fa);
            d = d.max(fd);
        }
        (a, d)
    }

    fn width(&mut self, text: &str, font: Font) -> u32 {
        if self.dc.is_null() {
            return crate::card::BlockText.width(text, font);
        }
        let runs = self.runs(text, font.mono);
        runs.iter()
            .map(|(face, units)| self.run_width(*face, font, units))
            .sum()
    }

    fn coverage(&mut self, text: &str, font: Font) -> Mask {
        let fallback = |t: &str| crate::card::BlockText.coverage(t, font);
        if self.dc.is_null() {
            return fallback(text);
        }
        let (ascent, descent) = self.metrics(font);
        let runs = self.runs(text, font.mono);
        let mut placed = Vec::with_capacity(runs.len());
        let mut x = 0u32;
        for (face, units) in &runs {
            placed.push(x);
            x += self.run_width(*face, font, units);
        }
        // Room for glyphs that reach past their advance (italics, emoji).
        let width = (x + font.px / 3 + 2).max(1);
        let height = (ascent + descent).max(1);
        if u64::from(width) * u64::from(height) > 64 * 1024 * 1024 {
            return fallback(text);
        }
        let info = BITMAPINFO {
            bmiHeader: BITMAPINFOHEADER {
                biSize: u32::try_from(size_of::<BITMAPINFOHEADER>()).expect("small"),
                biWidth: i32::try_from(width).unwrap_or(1),
                biHeight: -i32::try_from(height).unwrap_or(1),
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
            return fallback(text);
        }
        let fonts: Vec<HFONT> = runs
            .iter()
            .map(|(face, _)| self.font(*face, font))
            .collect();
        let n = width as usize * height as usize;
        // SAFETY: the DIB holds `n` BGRA pixels at `bits` (zeroed by the system: black); the
        // fonts and the DC are ours; every selection is restored.
        let data = unsafe {
            let prev = SelectObject(self.dc, bmp);
            SetTextColor(self.dc, 0x00FF_FFFF);
            for (((_, units), x), f) in runs.iter().zip(&placed).zip(&fonts) {
                let pf = SelectObject(self.dc, *f);
                ExtTextOutW(
                    self.dc,
                    i32::try_from(*x).unwrap_or(0),
                    i32::try_from(ascent).unwrap_or(0),
                    0,
                    std::ptr::null(),
                    units.as_ptr(),
                    u32::try_from(units.len()).unwrap_or(0),
                    std::ptr::null(),
                );
                SelectObject(self.dc, pf);
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
            mask
        };
        Mask {
            width,
            height,
            data,
        }
    }
}

impl Drop for GdiText {
    fn drop(&mut self) {
        // SAFETY: every font and the DC were created here and are freed once.
        unsafe {
            for (_, f) in self.fonts.drain() {
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
    use crate::card::{Doc, render};
    use crate::frame::Ledger;

    fn ink(m: &Mask) -> usize {
        m.data.iter().filter(|v| **v > 64).count()
    }

    /// Real GDI glyphs in memory (no window): Latin, Hangul, monospace, bold and italic all draw
    /// ink inside their box; widths grow with the text.
    #[test]
    fn gdi_draws_every_script_with_fallback() {
        let mut t = GdiText::new();
        assert!(t.ok());
        let f = Font {
            px: 18,
            bold: false,
            italic: false,
            mono: false,
        };
        let (a, d) = t.metrics(f);
        assert!(a >= 12 && d >= 2, "{a} {d}");
        for text in ["Hello", "한글 카드", "Ünïcödé", "x = 1;", "\u{1F600}"] {
            let m = t.coverage(text, f);
            assert_eq!(m.height, a + d);
            assert!(ink(&m) > 10, "{text}: ink");
            assert!(t.width(text, f) > 0);
        }
        assert!(t.width("Hello world", f) > t.width("Hello", f));
        let mono = Font { mono: true, ..f };
        assert_eq!(
            t.width("iiii", mono),
            t.width("WWWW", mono),
            "Consolas is monospaced"
        );
        let bold = Font { bold: true, ..f };
        assert!(ink(&t.coverage("Bold", bold)) > ink(&t.coverage("Bold", f)));
        assert_eq!(t.coverage("", f).data.iter().filter(|v| **v > 0).count(), 0);
    }

    /// A whole card through GDI: opaque, bounded, with ink.
    #[test]
    fn a_card_renders_with_gdi() {
        let ledger = Ledger::new();
        let mut t = GdiText::new();
        let doc = Doc::from_text("Noeul 노을\nfloating text card");
        let card = render(&ledger, &doc, 96, &mut t).unwrap();
        assert!(card.width() > 100 && card.width() < 700, "{}", card.width());
        assert!(
            card.height() > 30 && card.height() < 200,
            "{}",
            card.height()
        );
        let bg = crate::card::SURFACE;
        let differs = card
            .pixels()
            .as_chunks::<4>()
            .0
            .iter()
            .filter(|p| p[0..3] != [bg[2], bg[1], bg[0]])
            .count();
        assert!(differs > 200, "text was drawn: {differs}");
        let html = crate::html::parse("<h1>Title</h1><p>Body <b>bold</b> <i>italic</i></p>");
        assert!(render(&ledger, &html, 144, &mut t).is_ok());
    }
}
