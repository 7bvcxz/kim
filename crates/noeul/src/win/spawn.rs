//! Starting the resident instance detached from the terminal (`noeul start`, NOEUL-FR-002).
//!
//! The instance is this same executable running `noeul run`, started without a console
//! (`DETACHED_PROCESS`), in its own process group, with null standard streams, so closing the
//! terminal never ends it and it never shows a console window. When the caller sits in a job
//! that allows breakaway, the instance leaves it (`CREATE_BREAKAWAY_FROM_JOB`); otherwise it
//! stays in the job. Its working folder is the user profile, so it never holds the caller's
//! folder open.

use std::os::windows::process::CommandExt;
use std::path::PathBuf;
use std::process::{Child, Command, Stdio};

const DETACHED_PROCESS: u32 = 0x0000_0008;
const CREATE_NEW_PROCESS_GROUP: u32 = 0x0000_0200;
const CREATE_BREAKAWAY_FROM_JOB: u32 = 0x0100_0000;

fn cwd() -> PathBuf {
    std::env::var_os("USERPROFILE")
        .map(PathBuf::from)
        .filter(|p| p.is_dir())
        .or_else(|| {
            std::env::current_exe()
                .ok()
                .and_then(|p| p.parent().map(PathBuf::from))
        })
        .unwrap_or_else(std::env::temp_dir)
}

/// Starts `<this exe> run <args…>` detached.
pub fn instance(args: &[String]) -> std::io::Result<Child> {
    let exe = std::env::current_exe()?;
    let build = |flags: u32| {
        let mut c = Command::new(&exe);
        c.arg("run")
            .args(args)
            .current_dir(cwd())
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .creation_flags(flags);
        c
    };
    let base = DETACHED_PROCESS | CREATE_NEW_PROCESS_GROUP;
    // A job without JOB_OBJECT_LIMIT_BREAKAWAY_OK refuses the flag (access denied).
    build(base | CREATE_BREAKAWAY_FROM_JOB)
        .spawn()
        .or_else(|_| build(base).spawn())
}
