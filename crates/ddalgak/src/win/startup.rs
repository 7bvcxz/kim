//! The real sign-in store: one string value under `HKEY_CURRENT_USER\<key>` (ADR 0024 §6).
//!
//! Production uses [`crate::startup::RUN_KEY`]. Tests use a private key below
//! `HKCU\Software\mouv-test\` that they delete afterwards; debug builds of the CLI accept such a
//! key in `DDALGAK_DEV_STARTUP_KEY` (compiled out of release builds), so no test ever writes the
//! real Run key.

use super::wide;
use crate::startup::{RUN_KEY, Store, VALUE_NAME};
use std::io;
use windows_sys::Win32::Foundation::{ERROR_FILE_NOT_FOUND, ERROR_SUCCESS};
use windows_sys::Win32::System::Registry::{
    HKEY, HKEY_CURRENT_USER, KEY_QUERY_VALUE, KEY_SET_VALUE, REG_OPTION_NON_VOLATILE, REG_SZ,
    RRF_RT_REG_SZ, RegCloseKey, RegCreateKeyExW, RegDeleteTreeW, RegDeleteValueW, RegGetValueW,
    RegOpenKeyExW, RegSetValueExW,
};

/// Prefix every test key must have.
pub const TEST_PREFIX: &str = "Software\\mouv-test\\";

/// A value in a key under `HKEY_CURRENT_USER`.
#[derive(Clone, Debug)]
pub struct Registry {
    key: String,
}

fn err(code: u32) -> io::Error {
    io::Error::from_raw_os_error(i32::try_from(code).unwrap_or(-1))
}

struct Key(HKEY);

impl Drop for Key {
    fn drop(&mut self) {
        // SAFETY: a key handle opened below, closed once.
        unsafe { RegCloseKey(self.0) };
    }
}

impl Registry {
    /// The user's real Run key.
    pub fn run_key() -> Registry {
        Registry {
            key: RUN_KEY.to_string(),
        }
    }

    /// A private test key (must start with [`TEST_PREFIX`]).
    pub fn test_key(key: &str) -> Option<Registry> {
        key.starts_with(TEST_PREFIX).then(|| Registry {
            key: key.to_string(),
        })
    }

    /// The store the CLI uses: the Run key, or in debug builds a test key from
    /// `DDALGAK_DEV_STARTUP_KEY`.
    pub fn for_cli() -> Registry {
        #[cfg(debug_assertions)]
        if let Some(r) = std::env::var("DDALGAK_DEV_STARTUP_KEY")
            .ok()
            .and_then(|k| Registry::test_key(&k))
        {
            return r;
        }
        Registry::run_key()
    }

    fn open(&self, access: u32, create: bool) -> io::Result<Option<Key>> {
        let path = wide(&self.key);
        let mut h: HKEY = std::ptr::null_mut();
        // SAFETY: NUL-terminated path; `h` receives a key handle on success.
        let rc = unsafe {
            if create {
                RegCreateKeyExW(
                    HKEY_CURRENT_USER,
                    path.as_ptr(),
                    0,
                    std::ptr::null(),
                    REG_OPTION_NON_VOLATILE,
                    access,
                    std::ptr::null(),
                    &raw mut h,
                    std::ptr::null_mut(),
                )
            } else {
                RegOpenKeyExW(HKEY_CURRENT_USER, path.as_ptr(), 0, access, &raw mut h)
            }
        };
        match rc {
            ERROR_SUCCESS => Ok(Some(Key(h))),
            ERROR_FILE_NOT_FOUND => Ok(None),
            e => Err(err(e)),
        }
    }

    /// Deletes the whole test key (tests only; refuses anything else).
    pub fn delete_test_key(&self) -> io::Result<()> {
        if !self.key.starts_with(TEST_PREFIX) {
            return Err(io::Error::other(
                "refusing to delete a key outside the test prefix",
            ));
        }
        let path = wide(&self.key);
        // SAFETY: NUL-terminated path below HKCU\Software\mouv-test.
        match unsafe { RegDeleteTreeW(HKEY_CURRENT_USER, path.as_ptr()) } {
            ERROR_SUCCESS | ERROR_FILE_NOT_FOUND => {}
            e => return Err(err(e)),
        }
        Ok(())
    }
}

impl Store for Registry {
    fn get(&self) -> io::Result<Option<String>> {
        let Some(k) = self.open(KEY_QUERY_VALUE, false)? else {
            return Ok(None);
        };
        let name = wide(VALUE_NAME);
        let mut buf = vec![0u16; 2048];
        let mut bytes = u32::try_from(buf.len() * 2).expect("small");
        // SAFETY: valid key; `buf` has `bytes` writable bytes; RRF_RT_REG_SZ only.
        let rc = unsafe {
            RegGetValueW(
                k.0,
                std::ptr::null(),
                name.as_ptr(),
                RRF_RT_REG_SZ,
                std::ptr::null_mut(),
                buf.as_mut_ptr().cast(),
                &raw mut bytes,
            )
        };
        match rc {
            ERROR_SUCCESS => {
                let len = (bytes as usize / 2).min(buf.len());
                buf.truncate(len);
                while buf.last() == Some(&0) {
                    buf.pop();
                }
                Ok(Some(String::from_utf16_lossy(&buf)))
            }
            ERROR_FILE_NOT_FOUND => Ok(None),
            e => Err(err(e)),
        }
    }

    fn set(&mut self, command: &str) -> io::Result<()> {
        let k = self
            .open(KEY_SET_VALUE, true)?
            .ok_or_else(|| io::Error::other("the key could not be created"))?;
        let name = wide(VALUE_NAME);
        let data = wide(command);
        // SAFETY: valid key; `data` is a NUL-terminated UTF-16 string of the given byte length.
        let rc = unsafe {
            RegSetValueExW(
                k.0,
                name.as_ptr(),
                0,
                REG_SZ,
                data.as_ptr().cast(),
                u32::try_from(data.len() * 2).expect("small"),
            )
        };
        if rc == ERROR_SUCCESS {
            Ok(())
        } else {
            Err(err(rc))
        }
    }

    fn remove(&mut self) -> io::Result<bool> {
        let Some(k) = self.open(KEY_SET_VALUE, false)? else {
            return Ok(false);
        };
        let name = wide(VALUE_NAME);
        // SAFETY: valid key; NUL-terminated value name.
        match unsafe { RegDeleteValueW(k.0, name.as_ptr()) } {
            ERROR_SUCCESS => Ok(true),
            ERROR_FILE_NOT_FOUND => Ok(false),
            e => Err(err(e)),
        }
    }

    fn location(&self) -> String {
        format!("HKCU\\{}\\{VALUE_NAME}", self.key)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_private_test_key_round_trips_and_is_deleted() {
        let key = format!("{TEST_PREFIX}ddalgak-unit-{}\\Run", std::process::id());
        let mut r = Registry::test_key(&key).unwrap();
        assert!(
            Registry::test_key(RUN_KEY).is_none(),
            "the real Run key is never a test key"
        );
        assert_eq!(r.get().unwrap(), None);
        r.set("\"C:\\x\\ddalgak-gui.exe\" --startup").unwrap();
        assert_eq!(
            r.get().unwrap().as_deref(),
            Some("\"C:\\x\\ddalgak-gui.exe\" --startup")
        );
        assert!(r.location().ends_with("\\Run\\mouv.ddalgak"));
        assert!(r.remove().unwrap());
        assert!(!r.remove().unwrap());
        let parent =
            Registry::test_key(&format!("{TEST_PREFIX}ddalgak-unit-{}", std::process::id()))
                .unwrap();
        parent.delete_test_key().unwrap();
        assert_eq!(r.get().unwrap(), None);
        assert!(
            Registry::run_key().delete_test_key().is_err(),
            "refuses the real key"
        );
    }
}
