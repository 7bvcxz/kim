//! Reading an image from the clipboard, once, for `paste.float` (NOEUL-FR-070, NOEUL-FR-074,
//! NOEUL-SEC-002).
//!
//! Noeul has no clipboard listener and never polls: this runs only when the user pastes. The
//! clipboard is opened for the read and closed at once; when another program keeps it open the
//! read is retried for at most 500 ms and then reported (`clipboard_unavailable`). Formats in
//! this build: `CF_DIBV5`, then `CF_DIB` (Windows synthesizes them from bitmaps, so screenshots
//! and images copied from most programs qualify). Nothing read here is logged or stored beyond
//! the pin the user asked for.

use crate::dib;
use crate::error::{Code, NoeulError};
use crate::frame::{Frame, Ledger};
use std::sync::Arc;
use std::time::{Duration, Instant};
use windows_sys::Win32::System::DataExchange::{
    CloseClipboard, GetClipboardData, IsClipboardFormatAvailable, OpenClipboard,
};
use windows_sys::Win32::System::Memory::{GlobalLock, GlobalSize, GlobalUnlock};
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
    let until = Instant::now() + RETRY_FOR;
    loop {
        // SAFETY: opens the clipboard for this thread without an owner window (read only).
        if unsafe { OpenClipboard(std::ptr::null_mut()) } != 0 {
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

/// The clipboard's image, decoded into a frame at (0, 0).
pub fn read_image(ledger: &Arc<Ledger>) -> Result<Frame, NoeulError> {
    let _open = open()?;
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
        return frame;
    }
    Err(
        NoeulError::new(Code::NothingToPin, "the clipboard holds no image")
            .hint("copy an image first; text, HTML, color and file cards are planned (Step 55)"),
    )
}
