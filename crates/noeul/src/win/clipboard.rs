//! The clipboard, only when the user asks: reading an image once for `paste.float`
//! (NOEUL-FR-070, NOEUL-FR-074, NOEUL-SEC-002) and writing a selection for Copy (Step 51,
//! NOEUL-FR-030, NOEUL-FR-034).
//!
//! Noeul has no clipboard listener and never polls. The clipboard is opened for the read or the
//! write and closed at once; when another program keeps it open the attempt is retried for at
//! most 500 ms and then reported (`clipboard_unavailable`). Formats read in this build:
//! `CF_DIBV5`, then `CF_DIB` (Windows synthesizes them from bitmaps, so screenshots and images
//! copied from most programs qualify), and since Step 54 the registered `GIF` format's frame
//! count only, so a pin of an animated GIF's first frame says so (NOEUL-FR-068). A copy writes the registered `PNG` format and `CF_DIBV5`
//! (Windows synthesizes `CF_DIB` and `CF_BITMAP` from it), plus `CanUploadToCloudClipboard` = 0
//! so the picture stays out of cloud clipboard sync (NOEUL-FR-034's default; whether it shows in
//! the local clipboard history stays the user's Windows setting). Nothing read or written here
//! is logged or stored beyond what the user asked for. Pinning never calls this module
//! (NOEUL-FR-025).

use crate::app::Pasted;
use crate::dib;
use crate::error::{Code, NoeulError};
use crate::frame::Ledger;
use crate::win::wide;
use std::sync::Arc;
use std::time::{Duration, Instant};
use windows_sys::Win32::Foundation::{GlobalFree, HGLOBAL, HWND};
use windows_sys::Win32::System::DataExchange::{
    CloseClipboard, EmptyClipboard, GetClipboardData, IsClipboardFormatAvailable, OpenClipboard,
    RegisterClipboardFormatW, SetClipboardData,
};
use windows_sys::Win32::System::Memory::{
    GMEM_MOVEABLE, GlobalAlloc, GlobalLock, GlobalSize, GlobalUnlock,
};
use windows_sys::Win32::System::Ole::{CF_DIB, CF_DIBV5};

/// How long a clipboard held by another program is retried (NOEUL-FR-074).
pub const RETRY_FOR: Duration = Duration::from_millis(500);

struct Open;

impl Drop for Open {
    fn drop(&mut self) {
        // SAFETY: closes the clipboard this thread opened in `open`.
        unsafe { CloseClipboard() };
    }
}

fn open() -> Result<Open, NoeulError> {
    open_for(std::ptr::null_mut())
}

/// Opens the clipboard for this thread, `owner` owning it after `EmptyClipboard` (a write needs
/// an owner window; a read passes none).
fn open_for(owner: HWND) -> Result<Open, NoeulError> {
    let until = Instant::now() + RETRY_FOR;
    loop {
        // SAFETY: opens the clipboard for this thread; `owner` is one of our windows or null.
        if unsafe { OpenClipboard(owner) } != 0 {
            return Ok(Open);
        }
        if Instant::now() >= until {
            return Err(NoeulError::new(
                Code::ClipboardUnavailable,
                "another program keeps the clipboard open",
            )
            .hint("try again in a moment"));
        }
        std::thread::sleep(Duration::from_millis(25));
    }
}

/// The bytes of one clipboard format while the clipboard is open, handed to `f`.
fn with_data<T>(format: u32, f: impl FnOnce(&[u8]) -> T) -> Option<T> {
    // SAFETY: plain query while the clipboard is open.
    if unsafe { IsClipboardFormatAvailable(format) } == 0 {
        return None;
    }
    // SAFETY: the clipboard is open; the handle stays owned by the clipboard.
    let h = unsafe { GetClipboardData(format) };
    if h.is_null() {
        return None;
    }
    // SAFETY: global-memory clipboard data; locking gives a pointer to its bytes, unlocked below
    // before the clipboard closes.
    let p = unsafe { GlobalLock(h) };
    if p.is_null() {
        return None;
    }
    // SAFETY: the size of the same global block.
    let size = unsafe { GlobalSize(h) };
    // SAFETY: `p` points to `size` readable bytes while the block is locked.
    let r = f(unsafe { std::slice::from_raw_parts(p.cast::<u8>(), size) });
    // SAFETY: balances the GlobalLock above.
    unsafe { GlobalUnlock(h) };
    Some(r)
}

/// How many frames the GIF next to the clipboard's image has (the registered `GIF` format that
/// programs copying a GIF offer); 1 when there is none or it cannot be read (Step 54,
/// NOEUL-FR-068). Only the block structure is walked, nothing is decoded.
fn gif_frames() -> u32 {
    // SAFETY: registers (or looks up) the documented format name.
    let format = unsafe { RegisterClipboardFormatW(wide("GIF").as_ptr()) };
    if format == 0 {
        return 1;
    }
    with_data(format, |b| crate::gif::frame_count(b).unwrap_or(1))
        .unwrap_or(1)
        .max(1)
}

/// The clipboard's image, decoded into a frame at (0, 0), and the frame count of an animated
/// GIF offered next to it (its first frame is the image; NOEUL-FR-068).
pub fn read_image(ledger: &Arc<Ledger>) -> Result<Pasted, NoeulError> {
    let _open = open()?;
    let frames = gif_frames();
    for format in [CF_DIBV5, CF_DIB] {
        // SAFETY: plain query while the clipboard is open.
        if unsafe { IsClipboardFormatAvailable(u32::from(format)) } == 0 {
            continue;
        }
        // SAFETY: the clipboard is open; the handle stays owned by the clipboard.
        let h = unsafe { GetClipboardData(u32::from(format)) };
        if h.is_null() {
            continue;
        }
        // SAFETY: CF_DIB/CF_DIBV5 data is an HGLOBAL; locking gives a pointer to its bytes,
        // unlocked below before the clipboard closes.
        let p = unsafe { GlobalLock(h) };
        if p.is_null() {
            continue;
        }
        // SAFETY: the size of the same global block.
        let size = unsafe { GlobalSize(h) };
        // SAFETY: `p` points to `size` readable bytes while the block is locked.
        let bytes = unsafe { std::slice::from_raw_parts(p.cast::<u8>(), size) };
        let frame = dib::decode(ledger, bytes);
        // SAFETY: balances the GlobalLock above.
        unsafe { GlobalUnlock(h) };
        return frame.map(|frame| Pasted { frame, frames });
    }
    Err(
        NoeulError::new(Code::NothingToPin, "the clipboard holds no image").hint(if frames > 1 {
            "the clipboard holds only an animated GIF; pins of GIF files are not supported yet"
        } else {
            "copy an image first; text, HTML, color and file cards are planned (Step 55)"
        }),
    )
}

/// A movable global block holding some bytes (what `SetClipboardData` takes).
struct Block(HGLOBAL);

impl Block {
    fn new(bytes: &[u8]) -> Result<Block, NoeulError> {
        let fail = || NoeulError::new(Code::Internal, "out of memory for the clipboard");
        // SAFETY: a movable block of the bytes' size (at least 1 byte).
        let h = unsafe { GlobalAlloc(GMEM_MOVEABLE, bytes.len().max(1)) };
        if h.is_null() {
            return Err(fail());
        }
        let block = Block(h);
        // SAFETY: locking our own new block gives a pointer to at least `bytes.len()` bytes.
        let p = unsafe { GlobalLock(h) };
        if p.is_null() {
            return Err(fail());
        }
        // SAFETY: `p` is writable for `bytes.len()` bytes; the regions do not overlap.
        unsafe {
            std::ptr::copy_nonoverlapping(bytes.as_ptr(), p.cast::<u8>(), bytes.len());
            GlobalUnlock(h);
        }
        Ok(block)
    }

    /// Hands the block to the clipboard; on success the system owns it.
    fn set(self, format: u32) -> bool {
        // SAFETY: the clipboard is open and emptied by us; `self.0` is a movable global block.
        let ok = !unsafe { SetClipboardData(format, self.0) }.is_null();
        if ok {
            std::mem::forget(self);
        }
        ok
    }
}

impl Drop for Block {
    fn drop(&mut self) {
        // SAFETY: a block we allocated and still own.
        unsafe { GlobalFree(self.0) };
    }
}

/// Copy (NOEUL-FR-030): puts `png` (registered `PNG` format) and `dib` (`CF_DIBV5`) on the
/// clipboard, owned by `owner` (Noeul's control window), with `CanUploadToCloudClipboard` = 0.
/// The memory is prepared before the clipboard is opened, so it stays open only for the
/// handover; a clipboard held by another program is retried for 500 ms
/// (`clipboard_unavailable`). Not run by automated tests (they never touch the user's
/// clipboard); the encodings are tested, this handover is in the manual script.
pub fn write_image(owner: HWND, png: &[u8], dib: &[u8]) -> Result<(), NoeulError> {
    let png_block = Block::new(png)?;
    let dib_block = Block::new(dib)?;
    let cloud_block = Block::new(&0u32.to_le_bytes())?;
    // SAFETY: registers (or looks up) the documented format names.
    let (png_format, cloud_format) = unsafe {
        (
            RegisterClipboardFormatW(wide("PNG").as_ptr()),
            RegisterClipboardFormatW(wide("CanUploadToCloudClipboard").as_ptr()),
        )
    };
    let _open = open_for(owner)?;
    // SAFETY: the clipboard is open for this thread.
    if unsafe { EmptyClipboard() } == 0 {
        return Err(NoeulError::new(
            Code::ClipboardUnavailable,
            "the clipboard could not be emptied",
        ));
    }
    let dib_ok = dib_block.set(u32::from(CF_DIBV5));
    let png_ok = png_format != 0 && png_block.set(png_format);
    if cloud_format != 0 {
        cloud_block.set(cloud_format);
    }
    if dib_ok || png_ok {
        Ok(())
    } else {
        Err(NoeulError::new(
            Code::ClipboardUnavailable,
            "the clipboard refused the picture",
        ))
    }
}
