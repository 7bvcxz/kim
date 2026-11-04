//! Capture-image buffers and their ownership (ADR 0023 §3).
//!
//! A [`Frame`] is the only owner of a captured or pasted picture: BGRA, 8 bits per channel,
//! top-down rows without padding (`stride = width × 4`), positioned in virtual-desktop device
//! pixels. It is deliberately **not `Clone`**: a picture moves from the capture backend into a
//! capture session, from the session into a pin (cropped to the selection), and from a closed pin
//! into the reopen memory. Dropping the last owner frees it. Every frame is counted in a
//! [`Ledger`], so the instance can report how much image memory it holds (`noeul status`) and
//! tests can prove that cancelling, pinning, closing and quitting leave nothing behind
//! (NOEUL-NFR-002). Frames live in memory only; nothing here writes to disk (NOEUL-SEC-005), and
//! `Debug` never prints pixels (NOEUL-SEC-008).

use serde_json::{Value, json};
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

/// Largest width or height Noeul accepts for one picture (device pixels).
pub const MAX_SIDE: u32 = 32_767;
/// Largest picture in bytes (BGRA): 512 MiB, about seven 4K monitors.
pub const MAX_BYTES: u64 = 512 * 1024 * 1024;

/// A rectangle in device pixels (`x`/`y` may be negative on a multi-monitor desktop).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct Rect {
    pub x: i32,
    pub y: i32,
    pub width: u32,
    pub height: u32,
}

impl Rect {
    pub const fn new(x: i32, y: i32, width: u32, height: u32) -> Rect {
        Rect {
            x,
            y,
            width,
            height,
        }
    }

    /// `x,y,w,h` as typed on the command line.
    pub fn parse(s: &str) -> Result<Rect, String> {
        let parts: Vec<&str> = s.split(',').map(str::trim).collect();
        let [x, y, w, h] = parts.as_slice() else {
            return Err(format!("`{s}` is not a region; use x,y,width,height"));
        };
        let int = |v: &str, what: &str| {
            v.parse::<i64>()
                .map_err(|_| format!("`{v}` is not a whole number ({what})"))
        };
        let (x, y, w, h) = (
            int(x, "x")?,
            int(y, "y")?,
            int(w, "width")?,
            int(h, "height")?,
        );
        let side = |v: i64, what: &str| {
            u32::try_from(v)
                .ok()
                .filter(|v| (1..=MAX_SIDE).contains(v))
                .ok_or_else(|| format!("{what} must be between 1 and {MAX_SIDE}"))
        };
        let coord = |v: i64, what: &str| {
            i32::try_from(v)
                .ok()
                .filter(|v| v.unsigned_abs() <= 1_000_000)
                .ok_or_else(|| format!("{what} is out of range"))
        };
        Ok(Rect::new(
            coord(x, "x")?,
            coord(y, "y")?,
            side(w, "width")?,
            side(h, "height")?,
        ))
    }

    pub fn right(&self) -> i64 {
        i64::from(self.x) + i64::from(self.width)
    }

    pub fn bottom(&self) -> i64 {
        i64::from(self.y) + i64::from(self.height)
    }

    pub fn is_empty(&self) -> bool {
        self.width == 0 || self.height == 0
    }

    pub fn contains(&self, o: &Rect) -> bool {
        o.x >= self.x && o.y >= self.y && o.right() <= self.right() && o.bottom() <= self.bottom()
    }

    pub fn intersects(&self, o: &Rect) -> bool {
        i64::from(o.x) < self.right()
            && i64::from(self.x) < o.right()
            && i64::from(o.y) < self.bottom()
            && i64::from(self.y) < o.bottom()
    }

    /// Bytes of a BGRA picture of this size.
    pub fn bgra_bytes(&self) -> u64 {
        u64::from(self.width) * u64::from(self.height) * 4
    }

    pub fn to_json(&self) -> Value {
        json!({"x": self.x, "y": self.y, "width": self.width, "height": self.height})
    }

    /// Reads `{"x","y","width","height"}` (IPC parameters).
    pub fn from_json(v: &Value) -> Option<Rect> {
        let o = v.as_object()?;
        if o.len() != 4 {
            return None;
        }
        let x = i32::try_from(o.get("x")?.as_i64()?).ok()?;
        let y = i32::try_from(o.get("y")?.as_i64()?).ok()?;
        let w = u32::try_from(o.get("width")?.as_u64()?).ok()?;
        let h = u32::try_from(o.get("height")?.as_u64()?).ok()?;
        Rect::parse(&format!("{x},{y},{w},{h}")).ok()
    }
}

impl std::fmt::Display for Rect {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{},{},{},{}", self.x, self.y, self.width, self.height)
    }
}

/// Counts the frames and bytes alive in one process (or one test). Shared by `Arc`.
#[derive(Debug, Default)]
pub struct Ledger {
    frames: AtomicU64,
    bytes: AtomicU64,
    peak_bytes: AtomicU64,
}

impl Ledger {
    pub fn new() -> Arc<Ledger> {
        Arc::new(Ledger::default())
    }

    /// `(frames, bytes)` alive now.
    pub fn live(&self) -> (u64, u64) {
        (
            self.frames.load(Ordering::SeqCst),
            self.bytes.load(Ordering::SeqCst),
        )
    }

    pub fn peak_bytes(&self) -> u64 {
        self.peak_bytes.load(Ordering::SeqCst)
    }

    pub fn to_json(&self) -> Value {
        let (frames, bytes) = self.live();
        json!({"frames": frames, "bytes": bytes, "peak_bytes": self.peak_bytes()})
    }

    fn add(&self, bytes: u64) {
        self.frames.fetch_add(1, Ordering::SeqCst);
        let now = self.bytes.fetch_add(bytes, Ordering::SeqCst) + bytes;
        self.peak_bytes.fetch_max(now, Ordering::SeqCst);
    }

    fn remove(&self, bytes: u64) {
        self.frames.fetch_sub(1, Ordering::SeqCst);
        self.bytes.fetch_sub(bytes, Ordering::SeqCst);
    }
}

/// Why a picture was refused.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FrameError {
    /// Zero or more than [`MAX_SIDE`] pixels wide or high, or more than [`MAX_BYTES`].
    TooLarge,
    /// The pixel buffer does not have `width × height × 4` bytes.
    BadLength,
}

impl std::fmt::Display for FrameError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            FrameError::TooLarge => write!(
                f,
                "the picture must be 1 to {MAX_SIDE} pixels on each side and at most {} MiB",
                MAX_BYTES / (1024 * 1024)
            ),
            FrameError::BadLength => f.write_str("the pixel buffer has the wrong length"),
        }
    }
}

/// An owned BGRA picture. See the module documentation.
pub struct Frame {
    rect: Rect,
    pixels: Vec<u8>,
    ledger: Arc<Ledger>,
}

impl std::fmt::Debug for Frame {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        // Never the pixels (NOEUL-SEC-008).
        f.debug_struct("Frame")
            .field("rect", &self.rect)
            .field("bytes", &self.pixels.len())
            .finish_non_exhaustive()
    }
}

/// Checks a size before anything is allocated.
pub fn check_size(width: u32, height: u32) -> Result<u64, FrameError> {
    let r = Rect::new(0, 0, width, height);
    if width == 0
        || height == 0
        || width > MAX_SIDE
        || height > MAX_SIDE
        || r.bgra_bytes() > MAX_BYTES
    {
        return Err(FrameError::TooLarge);
    }
    Ok(r.bgra_bytes())
}

impl Frame {
    /// Takes ownership of `pixels` (BGRA, top-down, `width × 4` bytes per row).
    pub fn new(ledger: &Arc<Ledger>, rect: Rect, pixels: Vec<u8>) -> Result<Frame, FrameError> {
        let bytes = check_size(rect.width, rect.height)?;
        if pixels.len() as u64 != bytes {
            return Err(FrameError::BadLength);
        }
        ledger.add(bytes);
        Ok(Frame {
            rect,
            pixels,
            ledger: Arc::clone(ledger),
        })
    }

    /// A picture of one color (tests and synthetic pins).
    pub fn solid(ledger: &Arc<Ledger>, rect: Rect, bgra: [u8; 4]) -> Result<Frame, FrameError> {
        let bytes = check_size(rect.width, rect.height)?;
        let mut pixels = Vec::with_capacity(bytes as usize);
        for _ in 0..u64::from(rect.width) * u64::from(rect.height) {
            pixels.extend_from_slice(&bgra);
        }
        Frame::new(ledger, rect, pixels)
    }

    /// Where the picture is, in virtual-desktop device pixels.
    pub fn rect(&self) -> Rect {
        self.rect
    }

    pub fn width(&self) -> u32 {
        self.rect.width
    }

    pub fn height(&self) -> u32 {
        self.rect.height
    }

    pub fn pixels(&self) -> &[u8] {
        &self.pixels
    }

    /// The pixels to draw on (annotations are rasterized into a copy, Step 56); the size stays.
    pub fn pixels_mut(&mut self) -> &mut [u8] {
        &mut self.pixels
    }

    pub fn byte_len(&self) -> u64 {
        self.pixels.len() as u64
    }

    /// The same picture at another position (a pasted image placed at the pointer).
    #[must_use]
    pub fn moved_to(mut self, x: i32, y: i32) -> Frame {
        self.set_origin(x, y);
        self
    }

    /// Moves the picture (its pin window was moved); the pixels stay.
    pub fn set_origin(&mut self, x: i32, y: i32) {
        self.rect.x = x;
        self.rect.y = y;
    }

    /// A new frame made from this one (e.g. its shown orientation, Step 54), accounted to the
    /// same ledger.
    pub fn derived(&self, rect: Rect, pixels: Vec<u8>) -> Result<Frame, FrameError> {
        Frame::new(&self.ledger, rect, pixels)
    }

    /// A new frame with the part of this one inside `area` (absolute device pixels). `None` when
    /// `area` is empty or not inside this frame. The original stays untouched.
    pub fn crop(&self, area: Rect) -> Option<Frame> {
        if area.is_empty() || !self.rect.contains(&area) {
            return None;
        }
        let dx = (i64::from(area.x) - i64::from(self.rect.x)) as usize;
        let dy = (i64::from(area.y) - i64::from(self.rect.y)) as usize;
        let src_stride = self.rect.width as usize * 4;
        let row = area.width as usize * 4;
        let mut out = Vec::with_capacity(row * area.height as usize);
        for r in 0..area.height as usize {
            let start = (dy + r) * src_stride + dx * 4;
            out.extend_from_slice(&self.pixels[start..start + row]);
        }
        Frame::new(&self.ledger, area, out).ok()
    }
}

impl Drop for Frame {
    fn drop(&mut self) {
        self.ledger.remove(self.pixels.len() as u64);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn regions_parse_and_reject_nonsense() {
        assert_eq!(
            Rect::parse("-10, 20,30,40").unwrap(),
            Rect::new(-10, 20, 30, 40)
        );
        for bad in [
            "1,2,3",
            "a,b,c,d",
            "0,0,0,5",
            "0,0,5,-1",
            "0,0,40000,1",
            "1,2,3,4,5",
        ] {
            assert!(Rect::parse(bad).is_err(), "{bad}");
        }
        let r = Rect::new(-5, -5, 10, 10);
        assert!(r.contains(&Rect::new(-5, -5, 10, 10)));
        assert!(!r.contains(&Rect::new(-5, -5, 11, 10)));
        assert!(r.intersects(&Rect::new(4, 4, 3, 3)));
        assert!(!r.intersects(&Rect::new(5, 5, 3, 3)));
        assert_eq!(Rect::from_json(&r.to_json()), Some(r));
        assert_eq!(
            Rect::from_json(&json!({"x": 1, "y": 2, "width": 0, "height": 3})),
            None
        );
        assert_eq!(Rect::from_json(&json!({"x": 1, "y": 2, "width": 1})), None);
    }

    #[test]
    fn ownership_is_counted_and_released() {
        let ledger = Ledger::new();
        let a = Frame::solid(&ledger, Rect::new(0, 0, 4, 3), [1, 2, 3, 255]).unwrap();
        assert_eq!(ledger.live(), (1, 48));
        let b = a.crop(Rect::new(1, 1, 2, 2)).unwrap();
        assert_eq!(b.pixels().len(), 16);
        assert_eq!(ledger.live(), (2, 64));
        drop(a);
        assert_eq!(ledger.live(), (1, 16), "the crop owns its own pixels");
        let b = b.moved_to(-7, 9);
        assert_eq!(b.rect(), Rect::new(-7, 9, 2, 2));
        drop(b);
        assert_eq!(ledger.live(), (0, 0));
        assert_eq!(ledger.peak_bytes(), 64);
    }

    #[test]
    fn crop_copies_the_right_pixels() {
        let ledger = Ledger::new();
        // 3×2 picture whose blue channel is the pixel index.
        let pixels: Vec<u8> = (0..6u8).flat_map(|i| [i, 0, 0, 255]).collect();
        let f = Frame::new(&ledger, Rect::new(10, 20, 3, 2), pixels).unwrap();
        let c = f.crop(Rect::new(11, 20, 2, 2)).unwrap();
        let blues: Vec<u8> = c.pixels().chunks(4).map(|p| p[0]).collect();
        assert_eq!(blues, [1, 2, 4, 5]);
        assert!(f.crop(Rect::new(9, 20, 2, 2)).is_none(), "outside");
        assert!(f.crop(Rect::new(10, 20, 0, 2)).is_none(), "empty");
    }

    #[test]
    fn sizes_are_bounded_before_allocation() {
        let ledger = Ledger::new();
        assert_eq!(check_size(0, 1), Err(FrameError::TooLarge));
        assert_eq!(check_size(MAX_SIDE, MAX_SIDE), Err(FrameError::TooLarge));
        assert_eq!(check_size(3840, 2160), Ok(3840 * 2160 * 4));
        assert_eq!(
            Frame::new(&ledger, Rect::new(0, 0, 2, 2), vec![0; 15]).unwrap_err(),
            FrameError::BadLength
        );
        assert_eq!(ledger.live(), (0, 0));
        let f = Frame::solid(&ledger, Rect::new(0, 0, 1, 1), [0; 4]).unwrap();
        assert!(!format!("{f:?}").contains('['), "Debug shows no pixel data");
    }
}
