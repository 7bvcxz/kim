//! The clipboard, only when the user asks: reading it once for a paste (`paste.float`, "Pin
//! clipboard as…", `noeul pin --clipboard`; NOEUL-FR-070, NOEUL-FR-071, NOEUL-FR-074,
//! NOEUL-SEC-002) and writing a picture for Copy or a card's text for `pin.copy-text` (Step 51,
//! Step 55; NOEUL-FR-030, NOEUL-FR-034).
//!
//! Noeul has no clipboard listener and never polls. The clipboard is opened for the read or the
//! write and closed at once; when another program keeps it open the attempt is retried for at
//! most 500 ms and then reported (`clipboard_unavailable`). A read goes through
//! [`crate::paste::take`] on the open clipboard ([`WinClipboard`]): the formats offered are
//! looked at, private content is refused before anything is read, and only the chosen format is
//! copied out, bounded by its limit; decoding happens after the clipboard is closed. Formats read:
//! the registered `PNG` (and `image/png`), `CF_DIBV5`, `CF_DIB` (Windows synthesizes them from
//! bitmaps), the registered `GIF`, `JFIF` (and `image/jpeg`), `CF_HDROP`, the registered
//! `HTML Format` and `CF_UNICODETEXT`, plus the private markers
//! `ExcludeClipboardContentFromMonitorProcessing`, `Clipboard Viewer Ignore` and
//! `CanIncludeInClipboardHistory`.
//!
//! A copy writes the registered `PNG` format and `CF_DIBV5` (Windows synthesizes `CF_DIB` and
//! `CF_BITMAP` from it), or `CF_UNICODETEXT`, plus `CanUploadToCloudClipboard` = 0 so it stays
//! out of cloud clipboard sync (NOEUL-FR-034's default; whether it shows in the local clipboard
//! history stays the user's Windows setting). Nothing read or written here is logged or stored
//! beyond what the user asked for. Pinning never calls this module (NOEUL-FR-025).

use crate::error::{Code, NoeulError};
use crate::paste::{self, Clipboard, Format, Kind, Options, Taken};
use crate::win::wide;
use std::path::PathBuf;
use std::time::{Duration, Instant};
use windows_sys::Win32::Foundation::{GlobalFree, HGLOBAL, HWND};
use windows_sys::Win32::System::DataExchange::{
    CloseClipboard, EmptyClipboard, GetClipboardData, IsClipboardFormatAvailable, OpenClipboard,
    RegisterClipboardFormatW, SetClipboardData,
};
use windows_sys::Win32::System::Memory::{
    GMEM_MOVEABLE, GlobalAlloc, GlobalLock, GlobalSize, GlobalUnlock,
};
use windows_sys::Win32::System::Ole::{CF_DIB, CF_DIBV5, CF_HDROP, CF_UNICODETEXT};
use windows_sys::Win32::UI::Shell::{DragQueryFileW, HDROP};

/// How long a clipboard held by another program is retried (NOEUL-FR-074).
pub const RETRY_FOR: Duration = Duration::from_millis(500);

#[derive(Debug)]
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

/// A registered format's id (0 when it cannot be registered).
fn registered(name: &str) -> u32 {
    // SAFETY: registers (or looks up) a format name; the string is NUL-terminated.
    unsafe { RegisterClipboardFormatW(wide(name).as_ptr()) }
}

/// The bytes of one clipboard format while the clipboard is open, handed to `f` with the block's
/// size; `None` when it is not offered or cannot be locked.
fn with_data<T>(format: u32, f: impl FnOnce(&[u8], usize) -> T) -> Option<T> {
    // SAFETY: plain query while the clipboard is open.
    if format == 0 || unsafe { IsClipboardFormatAvailable(format) } == 0 {
        return None;
    }
    // SAFETY: the clipboard is open; the handle stays owned by the clipboard.
    let h = unsafe { GetClipboardData(format) };
    if h.is_null() {
        return None;
    }
    // SAFETY: the size of the clipboard's global block (nothing is read yet).
    let size = unsafe { GlobalSize(h) };
    // SAFETY: global-memory clipboard data; locking gives a pointer to its bytes, unlocked below
    // before the clipboard closes.
    let p = unsafe { GlobalLock(h) };
    if p.is_null() {
        return None;
    }
    // SAFETY: `p` points to `size` readable bytes while the block is locked.
    let r = f(
        unsafe { std::slice::from_raw_parts(p.cast::<u8>(), size) },
        size,
    );
    // SAFETY: balances the GlobalLock above.
    unsafe { GlobalUnlock(h) };
    Some(r)
}

/// The open clipboard as a paste source.
#[derive(Debug)]
pub struct WinClipboard {
    _open: Open,
    /// The ids each format may be offered under, in preference order.
    ids: Vec<(Format, Vec<u32>)>,
}

impl WinClipboard {
    fn new(open: Open) -> WinClipboard {
        let ids = vec![
            (
                Format::Png,
                vec![registered("PNG"), registered("image/png")],
            ),
            (Format::DibV5, vec![u32::from(CF_DIBV5)]),
            (Format::Dib, vec![u32::from(CF_DIB)]),
            (Format::Gif, vec![registered("GIF")]),
            (
                Format::Jfif,
                vec![registered("JFIF"), registered("image/jpeg")],
            ),
            (Format::Files, vec![u32::from(CF_HDROP)]),
            (Format::Html, vec![registered("HTML Format")]),
            (Format::Text, vec![u32::from(CF_UNICODETEXT)]),
        ];
        WinClipboard { _open: open, ids }
    }

    /// The first id under which `format` is offered now.
    fn id(&self, format: Format) -> Option<u32> {
        self.ids
            .iter()
            .find(|(f, _)| *f == format)?
            .1
            .iter()
            .copied()
            // SAFETY: plain query while the clipboard is open.
            .find(|id| *id != 0 && unsafe { IsClipboardFormatAvailable(*id) } != 0)
    }
}

impl Clipboard for WinClipboard {
    fn has(&self, format: Format) -> bool {
        self.id(format).is_some()
    }

    fn read(&mut self, format: Format, max: usize) -> Option<Result<Vec<u8>, u64>> {
        let id = self.id(format)?;
        with_data(id, |bytes, size| {
            if size > max {
                Err(size as u64)
            } else {
                Ok(bytes.to_vec())
            }
        })
    }

    fn files(&mut self, max: usize) -> Option<(Vec<PathBuf>, usize)> {
        let id = self.id(Format::Files)?;
        // SAFETY: plain query while the clipboard is open.
        let h = unsafe { GetClipboardData(id) };
        if h.is_null() {
            return None;
        }
        let drop = h as HDROP;
        // SAFETY: CF_HDROP data is an HDROP owned by the clipboard; 0xFFFFFFFF asks the count.
        let count = unsafe { DragQueryFileW(drop, u32::MAX, std::ptr::null_mut(), 0) } as usize;
        let mut out = Vec::new();
        for i in 0..count.min(max).min(4096) {
            let i = u32::try_from(i).unwrap_or(0);
            // SAFETY: the length (in characters, without the NUL) of file `i`.
            let len = unsafe { DragQueryFileW(drop, i, std::ptr::null_mut(), 0) } as usize;
            if len == 0 || len > 32_767 {
                continue;
            }
            let mut buf = vec![0u16; len + 1];
            // SAFETY: `buf` holds `len + 1` characters.
            let n = unsafe {
                DragQueryFileW(
                    drop,
                    i,
                    buf.as_mut_ptr(),
                    u32::try_from(len + 1).unwrap_or(0),
                )
            } as usize;
            out.push(PathBuf::from(String::from_utf16_lossy(&buf[..n.min(len)])));
        }
        Some((out, count))
    }

    fn private(&self) -> bool {
        paste::markers_private(&|name| {
            let id = registered(name);
            // SAFETY: plain query while the clipboard is open.
            if id == 0 || unsafe { IsClipboardFormatAvailable(id) } == 0 {
                return None;
            }
            // Present: its data only matters for `CanIncludeInClipboardHistory` (a DWORD).
            Some(
                with_data(id, |b, _| {
                    b.get(..4).map(<[u8]>::to_vec).unwrap_or_default()
                })
                .unwrap_or_default(),
            )
        })
    }
}

/// Reads the clipboard once for a paste ([`paste::take`]): opened (retried for 500 ms when held),
/// read, closed before anything is decoded.
pub fn take(want: Option<Kind>, opts: Options) -> Result<Taken, NoeulError> {
    let mut clip = WinClipboard::new(open()?);
    paste::take(&mut clip, want, opts).map_err(|r| paste::refusal_error(&r))
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

/// Puts `blocks` (format, bytes) on the clipboard owned by `owner`, with
/// `CanUploadToCloudClipboard` = 0. The memory is prepared before the clipboard is opened, so it
/// stays open only for the handover.
fn write(owner: HWND, blocks: Vec<(u32, &[u8])>) -> Result<(), NoeulError> {
    let mut prepared = Vec::new();
    for (format, bytes) in blocks {
        prepared.push((format, Block::new(bytes)?));
    }
    let cloud_block = Block::new(&0u32.to_le_bytes())?;
    let cloud_format = registered("CanUploadToCloudClipboard");
    let _open = open_for(owner)?;
    // SAFETY: the clipboard is open for this thread.
    if unsafe { EmptyClipboard() } == 0 {
        return Err(NoeulError::new(
            Code::ClipboardUnavailable,
            "the clipboard could not be emptied",
        ));
    }
    let mut any = false;
    for (format, block) in prepared {
        any |= format != 0 && block.set(format);
    }
    if cloud_format != 0 {
        cloud_block.set(cloud_format);
    }
    if any {
        Ok(())
    } else {
        Err(NoeulError::new(
            Code::ClipboardUnavailable,
            "the clipboard refused the data",
        ))
    }
}

/// Copy (NOEUL-FR-030): puts `png` (registered `PNG` format) and `dib` (`CF_DIBV5`) on the
/// clipboard, owned by `owner` (Noeul's control window), with `CanUploadToCloudClipboard` = 0;
/// a clipboard held by another program is retried for 500 ms (`clipboard_unavailable`). Not run
/// by automated tests (they never touch the user's clipboard); the encodings are tested, this
/// handover is in the manual script.
pub fn write_image(owner: HWND, png: &[u8], dib: &[u8]) -> Result<(), NoeulError> {
    write(
        owner,
        vec![(u32::from(CF_DIBV5), dib), (registered("PNG"), png)],
    )
}

/// `pin.copy-text` (Step 55): `text` as `CF_UNICODETEXT`, owned by `owner`, with
/// `CanUploadToCloudClipboard` = 0. Not run by automated tests (the manual script checks it).
pub fn write_text(owner: HWND, text: &str) -> Result<(), NoeulError> {
    let bytes = paste::utf16_bytes(text);
    write(owner, vec![(u32::from(CF_UNICODETEXT), &bytes)])
}
