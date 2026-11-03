//! KIM-FR-012 (NON-NEGOTIABLE): headless paths never launch a GUI.
//!
//! Each `kim` command runs inside a fresh Windows job object. The process is created suspended,
//! put into the job, and only then resumed, so every process it could start (Kim Manager, a
//! browser, a shell, anything) is counted by the job's accounting. Read-only commands, dry runs
//! and failures must start nothing at all; as a positive control, a real install must show
//! exactly one extra program (the staged tool's `--version` self-check). On systems where every
//! console process gets a console host (conhost.exe) inside the job, the baseline is 2 and each
//! program counts twice; the test measures the baseline instead of assuming it.
#![cfg(windows)]
#![allow(unsafe_code)]

mod common;

use common::{Sandbox, Server, package, release};
use std::os::windows::io::AsRawHandle;
use std::os::windows::process::CommandExt;
use std::process::{Output, Stdio};
use windows_sys::Win32::Foundation::{CloseHandle, HANDLE};
use windows_sys::Win32::System::Diagnostics::ToolHelp::{
    CreateToolhelp32Snapshot, TH32CS_SNAPTHREAD, THREADENTRY32, Thread32First, Thread32Next,
};
use windows_sys::Win32::System::JobObjects::{
    AssignProcessToJobObject, CreateJobObjectW, JOBOBJECT_BASIC_ACCOUNTING_INFORMATION,
    JobObjectBasicAccountingInformation, QueryInformationJobObject,
};
use windows_sys::Win32::System::Threading::{OpenThread, ResumeThread, THREAD_SUSPEND_RESUME};

const CREATE_SUSPENDED: u32 = 0x0000_0004;
const CREATE_NO_WINDOW: u32 = 0x0800_0000;

/// Resumes every thread of a suspended process; returns how many were resumed.
fn resume(pid: u32) -> usize {
    let mut resumed = 0;
    // SAFETY: plain Toolhelp snapshot; the handle is closed below.
    let snap = unsafe { CreateToolhelp32Snapshot(TH32CS_SNAPTHREAD, 0) };
    assert!(!snap.is_null() && snap as isize != -1, "thread snapshot");
    let mut entry = THREADENTRY32 {
        dwSize: u32::try_from(size_of::<THREADENTRY32>()).unwrap(),
        ..Default::default()
    };
    // SAFETY: `entry` has the correct dwSize as the API requires.
    let mut ok = unsafe { Thread32First(snap, &raw mut entry) } != 0;
    while ok {
        if entry.th32OwnerProcessID == pid {
            // SAFETY: plain thread open with resume rights; null is checked; closed below.
            let thread = unsafe { OpenThread(THREAD_SUSPEND_RESUME, 0, entry.th32ThreadID) };
            if !thread.is_null() {
                // SAFETY: `thread` is a valid handle with THREAD_SUSPEND_RESUME access.
                unsafe { ResumeThread(thread) };
                // SAFETY: opened above.
                unsafe { CloseHandle(thread) };
                resumed += 1;
            }
        }
        // SAFETY: same snapshot and initialized entry.
        ok = unsafe { Thread32Next(snap, &raw mut entry) } != 0;
    }
    // SAFETY: `snap` is a valid snapshot handle.
    unsafe { CloseHandle(snap) };
    resumed
}

/// Runs `kim <args>` inside a new job; returns how many processes the job ever contained.
fn processes_in_job(sb: &Sandbox, args: &[&str]) -> (u32, Output) {
    // SAFETY: anonymous job with default security; checked for null; closed below.
    let job: HANDLE = unsafe { CreateJobObjectW(std::ptr::null(), std::ptr::null()) };
    assert!(!job.is_null(), "CreateJobObjectW");
    let mut child = sb
        .cmd(args)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .creation_flags(CREATE_SUSPENDED | CREATE_NO_WINDOW)
        .spawn()
        .expect("spawn kim suspended");
    // SAFETY: both handles are valid; the child has not run a single instruction yet.
    let assigned = unsafe { AssignProcessToJobObject(job, child.as_raw_handle() as HANDLE) };
    if assigned == 0 || resume(child.id()) == 0 {
        let _ = child.kill();
        panic!("could not place kim in a job or resume it");
    }
    let out = child.wait_with_output().expect("wait for kim");
    let mut info = JOBOBJECT_BASIC_ACCOUNTING_INFORMATION::default();
    // SAFETY: `info` is a writable struct of exactly the size passed.
    let ok = unsafe {
        QueryInformationJobObject(
            job,
            JobObjectBasicAccountingInformation,
            (&raw mut info).cast(),
            u32::try_from(size_of::<JOBOBJECT_BASIC_ACCOUNTING_INFORMATION>()).unwrap(),
            std::ptr::null_mut(),
        )
    };
    // SAFETY: `job` was created above.
    unsafe { CloseHandle(job) };
    assert_ne!(ok, 0, "QueryInformationJobObject");
    (info.TotalProcesses, out)
}

#[test]
fn headless_commands_start_no_process_and_never_a_gui() {
    let sb = Sandbox::new("headless");
    let srv = Server::start();
    let body = package(&[]);
    let url = srv.put("/1.zip", body.clone());
    sb.publish(100, &[release("1.0.0", &url, &body, "tool.exe")]);

    // Baseline: what the job counts for a run of kim that certainly starts nothing.
    let (baseline, out) = processes_in_job(&sb, &["--version"]);
    assert!(out.status.success());
    assert!(
        baseline == 1 || baseline == 2,
        "unexpected baseline {baseline} (kim, plus a console host on some systems)"
    );

    let catalog =
        std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../catalog/catalog.json");
    let catalog = catalog.to_str().unwrap();
    let read_only: &[&[&str]] = &[
        &[],
        &["--help"],
        &["capabilities"],
        &["version", "--json"],
        &["status"],
        &["tools", "list", "--json"],
        &["tools", "describe", "kim"],
        &["catalog", "list"],
        &["catalog", "list", "--installed", "--json"],
        &["catalog", "show", "example-tool"],
        &["catalog", "status"],
        &["catalog", "verify", catalog],
        &["update", "check", "--json"],
        &["settings", "show"],
        &["settings", "set", "offline_mode", "true", "--dry-run"],
        &["install", "example-tool", "--dry-run", "--json"],
        &["uninstall", "example-tool", "--dry-run"],
        &["install", "nope", "--json"],
        &["frobnicate"],
        // KIM-FR-045: the hotkeys commands never open a window, register a hotkey, install a
        // hook or start a tool (including the cross-tool check and real writes).
        &["hotkeys", "list"],
        &["hotkeys", "check", "--all-tools", "--json"],
        &["hotkeys", "set", "catalog.refresh", "Ctrl+R"],
        &["hotkeys", "disable", "--all", "--json"],
        &["hotkeys", "reset", "--all", "--yes"],
    ];
    for args in read_only {
        let (n, out) = processes_in_job(&sb, args);
        assert_eq!(
            n,
            baseline,
            "`kim {}` started {} extra process(es); stderr: {}",
            args.join(" "),
            n - baseline,
            String::from_utf8_lossy(&out.stderr)
        );
    }

    // Positive control: the job really sees children. An explicit install runs exactly one
    // program (the staged tool's `--version` self-check, without a window). A console program
    // brings its own console host when the baseline shows one, so the count doubles.
    let (n, out) = processes_in_job(&sb, &["install", "example-tool", "--json"]);
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stdout)
    );
    assert_eq!(n, 2 * baseline, "install = kim + one self-check");
    let (n, _) = processes_in_job(&sb, &["tools", "describe", "example-tool", "--json"]);
    assert_eq!(
        n, baseline,
        "describe reads the manifest; it starts nothing"
    );
    let (n, out) = processes_in_job(&sb, &["uninstall", "example-tool", "--yes", "--json"]);
    assert!(out.status.success());
    assert_eq!(n, baseline, "uninstall starts nothing");
}
