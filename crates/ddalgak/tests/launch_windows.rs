//! Real launches on Windows (Step 70, `DDALGAK-FR-013`, `DDALGAK-SEC-008`).
//!
//! The maintainer uses this machine while tests run: the only program started here is the test's
//! own console marker program (`common/marker.rs`), from a sandbox folder whose path has spaces and
//! Korean characters, always hidden (the test-host launcher forces `CREATE_NO_WINDOW` and
//! `SW_HIDE`), and it is killed if it is still running. No browser, no shell hand-off, no window
//! of another program is involved; websites and packaged apps are tested with fakes (`launch`
//! unit tests).

#![cfg(windows)]

#[path = "common/marker.rs"]
mod marker;

use ddalgak::launch::{self, Exec, Plan, ProgramSpec, Show, Target, TestHostLauncher};
use ddalgak::win::launch::WinLauncher;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, Instant};

struct Root(PathBuf);

impl Root {
    fn new(tag: &str) -> Root {
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let p = std::env::temp_dir().join(format!(
            "ddalgak-launch-{tag}-{}-{nanos}",
            std::process::id()
        ));
        std::fs::create_dir_all(&p).unwrap();
        Root(p)
    }
}

impl Drop for Root {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

fn same_dir(a: &str, b: &Path) -> bool {
    match (std::fs::canonicalize(a), std::fs::canonicalize(b)) {
        (Ok(x), Ok(y)) => x == y,
        _ => false,
    }
}

fn run(plan: &Plan, launcher: &dyn launch::Launcher) -> &'static str {
    launch::execute(
        plan,
        &Exec {
            launcher,
            var: &launch::process_var,
            cancelled: &|| false,
        },
    )
}

#[test]
fn a_real_hidden_launch_passes_arguments_folder_and_environment_exactly() {
    let root = Root::new("real");
    let built = marker::build(&root.0.join("build"));
    let sandbox = root.0.join("런치 sandbox with spaces");
    let programs = sandbox.join("테스트 폴더");
    let work = sandbox.join("작업 폴더 (work)");
    std::fs::create_dir_all(&programs).unwrap();
    std::fs::create_dir_all(&work).unwrap();
    let exe = programs.join("마커 app.exe");
    std::fs::copy(&built, &exe).unwrap();
    let marker_file = sandbox.join("결과 marker.txt");
    let args: Vec<String> = vec![
        marker_file.display().to_string(),
        "첫 번째 인자".into(),
        r#"say "hi""#.into(),
        r"C:\trailing\".into(),
        String::new(),
        "emoji 👍 %PATH% & | <>".into(),
    ];
    let plan = Plan {
        targets: vec![Target::Program(ProgramSpec {
            path: exe.display().to_string(),
            args: args.clone(),
            working_dir: Some(work.display().to_string()),
            show: Show::Normal,
            activate: false,
            env: vec![("DDALGAK_TEST_VAR".into(), "값 value".into())],
        })],
        stop_on_error: true,
    };
    // The test host's launcher: only programs inside `sandbox`, always hidden.
    let host = TestHostLauncher::new(Arc::new(WinLauncher), Some(sandbox));
    let t = Instant::now();
    assert_eq!(run(&plan, &host), "ok");
    let started = t.elapsed();
    let m = marker::wait(&marker_file, Duration::from_secs(20)).expect("the marker program ran");
    let ran = t.elapsed();
    marker::kill_if_running(m.pid, &exe);
    assert_eq!(
        m.args,
        args[1..],
        "every argument arrived exactly as written"
    );
    assert!(same_dir(&m.cwd, &work), "working folder: {}", m.cwd);
    assert_eq!(
        m.env, "값 value",
        "the explicit override reached the program"
    );
    eprintln!(
        "real hidden launch: CreateProcessW returned after {started:?}, the program had written its marker after {ran:?}"
    );
}

#[test]
fn the_test_host_never_starts_a_program_outside_its_folder() {
    let root = Root::new("outside");
    let built = marker::build(&root.0.join("build"));
    let allowed = root.0.join("allowed");
    std::fs::create_dir_all(&allowed).unwrap();
    let outside = root.0.join("outside dir");
    std::fs::create_dir_all(&outside).unwrap();
    let exe = outside.join("marker.exe");
    std::fs::copy(&built, &exe).unwrap();
    let marker_file = outside.join("marker.txt");
    let plan = Plan {
        targets: vec![Target::Program(ProgramSpec {
            path: exe.display().to_string(),
            args: vec![marker_file.display().to_string()],
            working_dir: None,
            show: Show::Normal,
            activate: false,
            env: Vec::new(),
        })],
        stop_on_error: true,
    };
    let host = TestHostLauncher::new(Arc::new(WinLauncher), Some(allowed));
    assert_eq!(run(&plan, &host), "launch_failed");
    assert!(
        marker::wait(&marker_file, Duration::from_millis(500)).is_none(),
        "nothing ran"
    );
    // Moved away: never searched for.
    let moved = Plan {
        targets: vec![Target::Program(ProgramSpec {
            path: root.0.join("gone").join("marker.exe").display().to_string(),
            ..match &plan.targets[0] {
                Target::Program(s) => s.clone(),
                _ => unreachable!(),
            }
        })],
        stop_on_error: true,
    };
    assert_eq!(run(&moved, &host), "program_not_found");
}
