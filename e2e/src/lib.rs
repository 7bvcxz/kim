//! Shared harness for the e2e suite.
//!
//! Every e2e test is `#[ignore]`d and additionally calls [`require_e2e`], so it can only run
//! through `cargo xtask e2e`, which enforces the "once per 15 Step issues" cadence and points
//! the tests at freshly built release binaries.

use std::path::{Path, PathBuf};
use std::process::{Command, Output};

/// Panics unless the suite was started by `cargo xtask e2e`.
pub fn require_e2e() {
    assert_eq!(
        std::env::var("KIM_E2E").as_deref(),
        Ok("1"),
        "e2e tests run only via `cargo xtask e2e` (AGENTS.md §4)"
    );
}

/// Path of a release binary built by `cargo xtask e2e`.
pub fn bin(name: &str) -> PathBuf {
    require_e2e();
    let dir = std::env::var_os("KIM_E2E_BIN_DIR").expect("KIM_E2E_BIN_DIR is set by xtask");
    let path = Path::new(&dir).join(format!("{name}{}", std::env::consts::EXE_SUFFIX));
    assert!(path.exists(), "missing e2e binary {}", path.display());
    path
}

/// A unique, empty directory under the system temp dir, removed on drop.
#[derive(Debug)]
pub struct TempDir(PathBuf);

impl TempDir {
    pub fn new(tag: &str) -> Self {
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or_default();
        let path =
            std::env::temp_dir().join(format!("kim-e2e-{tag}-{}-{nanos}", std::process::id()));
        std::fs::create_dir_all(&path).expect("create temp dir");
        Self(path)
    }

    pub fn path(&self) -> &Path {
        &self.0
    }

    /// Number of entries (recursively) below the directory.
    pub fn entry_count(&self) -> usize {
        fn count(dir: &Path) -> usize {
            std::fs::read_dir(dir).map_or(0, |entries| {
                entries
                    .flatten()
                    .map(|e| {
                        1 + if e.path().is_dir() {
                            count(&e.path())
                        } else {
                            0
                        }
                    })
                    .sum()
            })
        }
        count(&self.0)
    }
}

impl Drop for TempDir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

/// Runs a binary with its per-user data locations redirected into `sandbox`, so a test can
/// prove the command wrote nothing there.
pub fn run_sandboxed(binary: &Path, args: &[&str], sandbox: &TempDir) -> Output {
    let p = sandbox.path();
    Command::new(binary)
        .args(args)
        .env("APPDATA", p.join("Roaming"))
        .env("LOCALAPPDATA", p.join("Local"))
        .env("USERPROFILE", p)
        .env("HOME", p)
        .env("XDG_CONFIG_HOME", p.join(".config"))
        .env("XDG_DATA_HOME", p.join(".local/share"))
        .output()
        .expect("run e2e binary")
}
