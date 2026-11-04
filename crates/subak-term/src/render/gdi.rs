//! The system's fonts through GDI (ADR 0036): glyphs of the terminal font chain rasterized with
//! grayscale antialiasing into a small DIB section on a memory DC — never a window. Font files
//! stay in the system's font cache; this process holds font handles, one bitmap of a few cells
//! and the glyphs it drew.
//!
//! Fallback is explicit: each face's `cmap` table (read with `GetFontData`) decides which face of
//! the chain draws a character, so Korean goes to Malgun Gothic and emoji to Segoe UI Emoji
//! whatever GDI's own font linking would do. Glyphs are drawn by glyph index, which also covers
//! characters outside the BMP. GDI draws colour emoji fonts in monochrome; colour emoji need a
//! DirectWrite rasterizer behind the same [`Rasterizer`] boundary (docs/TERMINAL_EMULATOR.md §5).

use super::glyph::{CellMetrics, Cmap, FaceInfo, GlyphBitmap, Rasterizer, Style, missing_glyph};
use std::ptr::{null, null_mut};
use windows_sys::Win32::Foundation::SIZE;
use windows_sys::Win32::Graphics::Gdi::{
    ABC, ANTIALIASED_QUALITY, BI_RGB, BITMAPINFO, BITMAPINFOHEADER, CLIP_DEFAULT_PRECIS,
    CreateCompatibleDC, CreateDIBSection, CreateFontIndirectW, DEFAULT_CHARSET, DIB_RGB_COLORS,
    DeleteDC, DeleteObject, ETO_GLYPH_INDEX, ExtTextOutW, FW_BOLD, FW_NORMAL, GDI_ERROR, GdiFlush,
    GetCharABCWidthsI, GetFontData, GetOutlineTextMetricsW, GetTextExtentPoint32W, GetTextFaceW,
    GetTextMetricsW, HBITMAP, HDC, HFONT, HGDIOBJ, LOGFONTW, OUT_TT_PRECIS, OUTLINETEXTMETRICW,
    SelectObject, SetBkMode, SetTextAlign, SetTextColor, TA_BASELINE, TA_LEFT, TEXTMETRICW,
    TRANSPARENT,
};

#[derive(Debug)]
struct Face {
    family: String,
    /// Fonts by [`Style::index`], created on first use.
    fonts: [HFONT; 4],
    cmap: Cmap,
    /// `lfHeight`: negative = em size (the primary face), positive = cell height (fallbacks, so
    /// their ascent and descent fit the cell).
    lf_height: i32,
    ascent: i32,
    descent: i32,
}

/// Rasterizes with the system's fonts through GDI.
#[derive(Debug)]
pub struct GdiRasterizer {
    dc: HDC,
    dib: HBITMAP,
    old_bitmap: HGDIOBJ,
    bits: *mut u32,
    dib_w: i32,
    dib_h: i32,
    families: Vec<String>,
    faces: Vec<Face>,
    metrics: CellMetrics,
}

impl GdiRasterizer {
    /// A rasterizer for the font chain `families` (the installed ones, in order).
    pub fn new(families: &[&str]) -> Result<Self, String> {
        // SAFETY: a memory DC compatible with the screen; no window is involved.
        let dc = unsafe { CreateCompatibleDC(null_mut()) };
        if dc.is_null() {
            return Err("CreateCompatibleDC failed".into());
        }
        let mut r = Self {
            dc,
            dib: null_mut(),
            old_bitmap: null_mut(),
            bits: null_mut(),
            dib_w: 0,
            dib_h: 0,
            families: families.iter().map(|s| (*s).to_string()).collect(),
            faces: Vec::new(),
            metrics: CellMetrics {
                width: 1,
                height: 1,
                baseline: 1,
                underline: 0,
                strike: 0,
                stroke: 1,
            },
        };
        r.configure(13, 16);
        if r.faces.is_empty() {
            return Err(format!("none of the fonts {families:?} is installed"));
        }
        Ok(r)
    }

    /// The installed faces of the chain, in fallback order.
    pub fn faces(&self) -> Vec<String> {
        self.faces.iter().map(|f| f.family.clone()).collect()
    }

    fn create_font(&self, family: &str, height: i32, style: Style) -> HFONT {
        let mut lf = LOGFONTW {
            lfHeight: height,
            lfWeight: if style.bold { FW_BOLD } else { FW_NORMAL } as i32,
            lfItalic: u8::from(style.italic),
            lfCharSet: DEFAULT_CHARSET,
            lfOutPrecision: OUT_TT_PRECIS,
            lfClipPrecision: CLIP_DEFAULT_PRECIS,
            lfQuality: ANTIALIASED_QUALITY,
            ..LOGFONTW::default()
        };
        for (d, s) in lf.lfFaceName.iter_mut().zip(family.encode_utf16().take(31)) {
            *d = s;
        }
        // SAFETY: `lf` is a valid, NUL-terminated LOGFONTW.
        unsafe { CreateFontIndirectW(&raw const lf) }
    }

    /// Loads `family` at `height`, or `None` if it is not installed (GDI silently substitutes
    /// another family for a missing one, so the selected face's name is checked).
    fn load_face(&self, family: &str, height: i32) -> Option<Face> {
        let font = self.create_font(family, height, Style::default());
        if font.is_null() {
            return None;
        }
        let mut name = [0u16; 64];
        let mut tm = TEXTMETRICW::default();
        let mut data = Vec::new();
        // SAFETY: `font` is a live font selected into our own DC and restored before returning;
        // every buffer passed is valid for the size given.
        let selected = unsafe {
            let old = SelectObject(self.dc, font);
            let n = GetTextFaceW(self.dc, name.len() as i32, name.as_mut_ptr());
            GetTextMetricsW(self.dc, &raw mut tm);
            let tag = u32::from_le_bytes(*b"cmap");
            let size = GetFontData(self.dc, tag, 0, null_mut(), 0);
            if size != GDI_ERROR as u32 && size > 0 {
                data = vec![0u8; size as usize];
                GetFontData(self.dc, tag, 0, data.as_mut_ptr().cast(), size);
            }
            SelectObject(self.dc, old);
            n
        };
        let len = name.iter().position(|&c| c == 0).unwrap_or(0);
        let got =
            String::from_utf16_lossy(&name[..len.min(usize::try_from(selected).unwrap_or(0))]);
        if !got.eq_ignore_ascii_case(family) {
            // SAFETY: our own font handle, not selected anywhere.
            unsafe { DeleteObject(font) };
            return None;
        }
        Some(Face {
            family: family.to_string(),
            fonts: [font, null_mut(), null_mut(), null_mut()],
            cmap: Cmap::parse(data),
            lf_height: height,
            ascent: tm.tmAscent,
            descent: tm.tmDescent,
        })
    }

    fn release_fonts(&mut self) {
        for f in self.faces.drain(..) {
            for h in f.fonts {
                if !h.is_null() {
                    // SAFETY: our own font handles; none is selected into the DC between calls.
                    unsafe { DeleteObject(h) };
                }
            }
        }
    }

    /// A DIB of at least `w × h` pixels selected into the DC.
    fn ensure_dib(&mut self, w: i32, h: i32) {
        if self.dib_w >= w && self.dib_h >= h && !self.dib.is_null() {
            return;
        }
        self.release_dib();
        let info = BITMAPINFO {
            bmiHeader: BITMAPINFOHEADER {
                biSize: size_of::<BITMAPINFOHEADER>() as u32,
                biWidth: w,
                biHeight: -h, // top-down
                biPlanes: 1,
                biBitCount: 32,
                biCompression: BI_RGB,
                ..BITMAPINFOHEADER::default()
            },
            ..BITMAPINFO::default()
        };
        let mut bits: *mut core::ffi::c_void = null_mut();
        // SAFETY: a valid BITMAPINFO; the section is owned by this rasterizer and released in
        // `release_dib`.
        let dib = unsafe {
            CreateDIBSection(
                self.dc,
                &raw const info,
                DIB_RGB_COLORS,
                &raw mut bits,
                null_mut(),
                0,
            )
        };
        if dib.is_null() || bits.is_null() {
            return;
        }
        // SAFETY: selecting our own bitmap into our own DC; the previous one is kept to restore.
        self.old_bitmap = unsafe { SelectObject(self.dc, dib) };
        self.dib = dib;
        self.bits = bits.cast();
        self.dib_w = w;
        self.dib_h = h;
    }

    fn release_dib(&mut self) {
        if !self.dib.is_null() {
            // SAFETY: restores the DC's original bitmap, then deletes ours, which is no longer
            // selected.
            unsafe {
                SelectObject(self.dc, self.old_bitmap);
                DeleteObject(self.dib);
            }
            self.dib = null_mut();
            self.bits = null_mut();
            self.dib_w = 0;
            self.dib_h = 0;
        }
    }

    fn font(&mut self, face: usize, style: Style) -> HFONT {
        let i = style.index();
        if self.faces[face].fonts[i].is_null() {
            let f = &self.faces[face];
            let font = self.create_font(&f.family, f.lf_height, style);
            self.faces[face].fonts[i] = if font.is_null() {
                self.faces[face].fonts[0]
            } else {
                font
            };
        }
        self.faces[face].fonts[i]
    }

    fn face_index(&self, c: char) -> Option<usize> {
        self.faces.iter().position(|f| f.cmap.glyph(c) != 0)
    }
}

impl Drop for GdiRasterizer {
    fn drop(&mut self) {
        self.release_dib();
        self.release_fonts();
        // SAFETY: our own memory DC, with its original bitmap restored.
        unsafe { DeleteDC(self.dc) };
    }
}

impl Rasterizer for GdiRasterizer {
    fn name(&self) -> &str {
        "gdi"
    }

    fn configure(&mut self, em_px: u32, cell_height: u32) -> CellMetrics {
        self.release_fonts();
        let families = self.families.clone();
        let em = i32::try_from(em_px.max(4)).unwrap_or(13);
        // The primary face: the first installed family, at the em size.
        let mut rest = families.iter();
        let mut primary = None;
        for f in rest.by_ref() {
            if let Some(face) = self.load_face(f, -em) {
                primary = Some(face);
                break;
            }
        }
        let Some(primary) = primary else {
            return self.metrics;
        };
        let mut advance = SIZE::default();
        let mut otm_size = 0;
        let mut otm = OUTLINETEXTMETRICW::default();
        // SAFETY: the primary font is selected into our own DC and restored; the buffers are
        // valid for the sizes given (GetOutlineTextMetricsW fills at most `size_of` bytes).
        unsafe {
            let old = SelectObject(self.dc, primary.fonts[0]);
            let zero = [u16::from(b'0')];
            GetTextExtentPoint32W(self.dc, zero.as_ptr(), 1, &raw mut advance);
            let need = GetOutlineTextMetricsW(self.dc, 0, null_mut());
            if need as usize >= size_of::<OUTLINETEXTMETRICW>() {
                // The structure is followed by strings; a buffer of the full size is required.
                let mut buf = vec![0u64; (need as usize).div_ceil(8)];
                if GetOutlineTextMetricsW(self.dc, need, buf.as_mut_ptr().cast()) != 0 {
                    otm = std::ptr::read_unaligned(buf.as_ptr().cast());
                    otm_size = need;
                }
            }
            SelectObject(self.dc, old);
        }
        let font_h = primary.ascent + primary.descent;
        let height = i32::try_from(cell_height).unwrap_or(16).max(font_h).max(4);
        let baseline = (height - font_h) / 2 + primary.ascent;
        let width = advance.cx.max(1);
        let (stroke, underline, strike) = if otm_size > 0 {
            let stroke = otm.otmsUnderscoreSize.max(1);
            let under = baseline - otm.otmsUnderscorePosition - stroke / 2;
            let ss = i32::try_from(otm.otmsStrikeoutSize).unwrap_or(1).max(1);
            let strike = baseline - otm.otmsStrikeoutPosition - ss / 2;
            (stroke, under, strike)
        } else {
            let stroke = (em / 12).max(1);
            (stroke, baseline + 1, baseline - primary.ascent / 3)
        };
        let clamp = |v: i32| u32::try_from(v.clamp(0, height - stroke)).unwrap_or(0);
        self.metrics = CellMetrics {
            width: u32::try_from(width).unwrap_or(1),
            height: u32::try_from(height).unwrap_or(1),
            baseline: u32::try_from(baseline).unwrap_or(1),
            underline: clamp(underline),
            strike: clamp(strike),
            stroke: u32::try_from(stroke).unwrap_or(1),
        };
        self.faces.push(primary);
        // Fallback faces at the cell height, so they fit the cell.
        for f in rest {
            if let Some(face) = self.load_face(f, height) {
                self.faces.push(face);
            }
        }
        // Room for a two-cell glyph and a cell of overhang on each side.
        self.ensure_dib(width * 4, height);
        self.metrics
    }

    fn rasterize(
        &mut self,
        cluster: &str,
        style: Style,
        cells: u8,
        m: &CellMetrics,
    ) -> GlyphBitmap {
        let Some(base) = cluster.chars().next() else {
            return GlyphBitmap::default();
        };
        let Some(fi) = self.face_index(base) else {
            return missing_glyph(cells, m);
        };
        if self.bits.is_null() {
            return missing_glyph(cells, m);
        }
        let font = self.font(fi, style);
        let face = &self.faces[fi];
        let glyphs: Vec<u16> = cluster
            .chars()
            .map(|c| face.cmap.glyph(c))
            .filter(|&g| g != 0)
            .collect();
        let cell_w = i32::try_from(m.width).unwrap_or(1);
        let height = i32::try_from(m.height).unwrap_or(1).min(self.dib_h);
        let span = cell_w * i32::from(cells);
        let margin = cell_w;
        let baseline = if fi == 0 {
            i32::try_from(m.baseline).unwrap_or(0)
        } else {
            (height - (face.ascent + face.descent)) / 2 + face.ascent
        };
        let (dib_w, dc, bits) = (self.dib_w, self.dc, self.bits);
        let mut coverage = vec![0u8; (dib_w * height) as usize];
        // SAFETY: our own DC with our own DIB selected; `bits` points at `dib_w × dib_h` pixels
        // (top-down) and only rows below `height <= dib_h` are touched. The glyph and advance
        // arrays outlive the calls that read them.
        unsafe {
            let old = SelectObject(dc, font);
            let mut abc = vec![ABC::default(); glyphs.len()];
            GetCharABCWidthsI(
                dc,
                0,
                glyphs.len() as u32,
                glyphs.as_ptr(),
                abc.as_mut_ptr(),
            );
            let adv = abc
                .first()
                .map_or(cell_w, |a| a.abcA + a.abcB as i32 + a.abcC);
            // Combining marks are drawn at the base's origin, or after its advance when the
            // mark's outline sits left of its own origin (zero-width marks with negative
            // left-side bearing).
            let mut dx = vec![0i32; glyphs.len()];
            if let (Some(d), Some(mark)) = (dx.first_mut(), abc.get(1)) {
                *d = if mark.abcA < 0 { adv } else { 0 };
            }
            let x = margin + (span - adv) / 2;
            std::ptr::write_bytes(self.bits, 0, (dib_w * height) as usize);
            SetTextColor(dc, 0x00FF_FFFF);
            SetBkMode(dc, TRANSPARENT as i32);
            SetTextAlign(dc, TA_BASELINE | TA_LEFT);
            ExtTextOutW(
                dc,
                x,
                baseline,
                ETO_GLYPH_INDEX,
                null(),
                glyphs.as_ptr(),
                glyphs.len() as u32,
                dx.as_ptr(),
            );
            GdiFlush();
            for (i, c) in coverage.iter_mut().enumerate() {
                // Grayscale antialiasing writes the same value to every channel.
                *c = ((*bits.add(i) >> 8) & 0xff) as u8;
            }
            SelectObject(dc, old);
        }
        GlyphBitmap {
            left: -margin,
            top: 0,
            width: u32::try_from(dib_w).unwrap_or(0),
            height: u32::try_from(height).unwrap_or(0),
            coverage,
        }
        .trimmed()
    }

    fn face_for(&mut self, ch: char) -> FaceInfo {
        match self.face_index(ch) {
            Some(i) => FaceInfo {
                family: self.faces[i].family.clone(),
                fallback: i > 0,
                found: true,
            },
            None => FaceInfo {
                family: String::new(),
                fallback: true,
                found: false,
            },
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn raster() -> GdiRasterizer {
        GdiRasterizer::new(&super::super::MONO_FAMILY).expect("the terminal fonts")
    }

    #[test]
    fn the_chain_skips_missing_families_and_falls_back_by_cmap() {
        let mut r = raster();
        let faces = r.faces();
        assert!(!faces.is_empty());
        assert!(!faces.iter().any(|f| f == "No Such Font Family"));
        let primary = &faces[0];
        assert!(
            primary == "Cascadia Mono" || primary == "Consolas",
            "a Windows mono font comes first: {faces:?}"
        );
        assert_eq!(r.face_for('A').family, *primary);
        assert!(!r.face_for('A').fallback);
        if faces.iter().any(|f| f == "Malgun Gothic") {
            let k = r.face_for('한');
            assert_eq!(k.family, "Malgun Gothic", "{k:?}");
            assert!(k.fallback);
        }
        if faces.iter().any(|f| f == "Segoe UI Emoji") {
            assert_eq!(r.face_for('😀').family, "Segoe UI Emoji");
        }
        // A private-use character no face maps.
        assert!(!r.face_for('\u{10FFFD}').found);
        let none = GdiRasterizer::new(&["No Such Font Family"]);
        assert!(none.is_err());
    }

    #[test]
    fn glyphs_have_ink_inside_their_cells_at_every_scale() {
        let mut r = raster();
        for em in [13, 16, 20, 26] {
            let cell_h = (em as f32 * 1.2).ceil() as u32;
            let m = r.configure(em, cell_h);
            assert!(m.width >= em / 2 && m.height >= cell_h, "{m:?}");
            assert!(m.baseline < m.height && m.underline < m.height);
            let a = r.rasterize("A", Style::default(), 1, &m);
            assert!(a.ink() > 0, "A has ink at {em} px");
            assert!(
                a.left >= -1 && a.left + a.width as i32 <= m.width as i32 + 1,
                "{a:?}"
            );
            let bold = r.rasterize(
                "A",
                Style {
                    bold: true,
                    italic: false,
                },
                1,
                &m,
            );
            assert!(bold.ink() >= a.ink(), "bold is not lighter");
            let k = r.rasterize("한", Style::default(), 2, &m);
            assert!(k.ink() > 0);
            // A Hangul syllable is centred in its two cells and wider than one.
            assert!(k.width > m.width, "{k:?} at {m:?}");
            assert!(k.left >= 0 && k.left + k.width as i32 <= 2 * m.width as i32 + 1);
            let e = r.rasterize("😀", Style::default(), 2, &m);
            assert!(e.ink() > 0, "emoji (monochrome) at {em} px");
            let acute = r.rasterize("e\u{301}", Style::default(), 1, &m);
            assert!(acute.ink() > r.rasterize("e", Style::default(), 1, &m).ink());
            let missing = r.rasterize("\u{10FFFD}", Style::default(), 1, &m);
            assert!(missing.ink() > 0, "a missing glyph is a visible box");
        }
    }
}
