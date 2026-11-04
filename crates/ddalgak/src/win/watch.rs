//! Change notifications for Ddalgak's configuration folder (`DDALGAK-FR-009`,
//! HOTKEY_CONTRACT §8.3 5).
//!
//! One thread waits in `WaitForMultipleObjects` on a `FindFirstChangeNotificationW` handle for
//! the folder (file names, sizes and write times, not subfolders) and a stop event. Nothing
//! polls: the thread wakes only when a file in the folder changes or when it is stopped. Each
//! notification calls `on_change`, which asks the app to reload; the app compares the file's
//! bytes, so the several notifications of one atomic save apply the change once.

use super::Event;
use std::io;
use std::path::Path;
use std::sync::Arc;
use windows_sys::Win32::Foundation::{HANDLE, INVALID_HANDLE_VALUE, WAIT_OBJECT_0};
use windows_sys::Win32::Storage::FileSystem::{
    FILE_NOTIFY_CHANGE_FILE_NAME, FILE_NOTIFY_CHANGE_LAST_WRITE, FILE_NOTIFY_CHANGE_SIZE,
    FindCloseChangeNotification, FindFirstChangeNotificationW, FindNextChangeNotification,
};
use windows_sys::Win32::System::Threading::{INFINITE, WaitForMultipleObjects};

/// A running folder watcher; stopped and joined on drop.
#[derive(Debug)]
pub struct FolderWatcher {
    stop: Arc<Event>,
    join: Option<std::thread::JoinHandle<()>>,
}

struct Notification(HANDLE);

// SAFETY: a change-notification handle is a kernel handle, usable from any thread; it is moved
// into the watcher thread and closed there exactly once.
unsafe impl Send for Notification {}

impl FolderWatcher {
    /// Starts watching `dir` (which must exist).
    pub fn start(dir: &Path, on_change: Box<dyn Fn() + Send>) -> io::Result<FolderWatcher> {
        let w = super::wide(dir);
        // SAFETY: NUL-terminated folder name; the handle is closed by the watcher thread.
        let h = unsafe {
            FindFirstChangeNotificationW(
                w.as_ptr(),
                0,
                FILE_NOTIFY_CHANGE_FILE_NAME
                    | FILE_NOTIFY_CHANGE_LAST_WRITE
                    | FILE_NOTIFY_CHANGE_SIZE,
            )
        };
        if h.is_null() || h == INVALID_HANDLE_VALUE {
            return Err(io::Error::last_os_error());
        }
        let n = Notification(h);
        let stop = Arc::new(Event::new()?);
        let stop2 = Arc::clone(&stop);
        let join = std::thread::Builder::new()
            .name("ddalgak-watch".into())
            .spawn(move || {
                let n = n;
                loop {
                    let handles = [n.0, stop2.raw()];
                    // SAFETY: two valid handles; blocks without polling.
                    let r = unsafe { WaitForMultipleObjects(2, handles.as_ptr(), 0, INFINITE) };
                    if r != WAIT_OBJECT_0 {
                        break; // stopped, or the wait failed
                    }
                    on_change();
                    // SAFETY: re-arms the notification handle created above.
                    if unsafe { FindNextChangeNotification(n.0) } == 0 {
                        break;
                    }
                }
                // SAFETY: closes the notification handle created above, exactly once.
                unsafe { FindCloseChangeNotification(n.0) };
            })?;
        Ok(FolderWatcher {
            stop,
            join: Some(join),
        })
    }
}

impl Drop for FolderWatcher {
    fn drop(&mut self) {
        self.stop.set();
        if let Some(j) = self.join.take() {
            let _ = j.join();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::mpsc::channel;
    use std::time::Duration;

    #[test]
    fn a_change_in_the_folder_is_reported_and_drop_stops_the_thread() {
        let dir = std::env::temp_dir().join(format!("ddalgak-watch-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let (tx, rx) = channel();
        let w = FolderWatcher::start(
            &dir,
            Box::new(move || {
                let _ = tx.send(());
            }),
        )
        .unwrap();
        assert!(
            rx.recv_timeout(Duration::from_millis(200)).is_err(),
            "quiet while nothing changes"
        );
        std::fs::write(dir.join("rules.json"), b"{}").unwrap();
        assert!(
            rx.recv_timeout(Duration::from_secs(5)).is_ok(),
            "a write is noticed"
        );
        drop(w);
        let _ = std::fs::remove_dir_all(&dir);
    }
}
