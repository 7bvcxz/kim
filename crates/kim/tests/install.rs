//! Installer integration tests (Step 06): real zip packages served over a loopback HTTP server,
//! real extraction, real self-check (`kim.exe --version` stands in for a tool), real per-user
//! directory layout in a temp sandbox.

use kim::catalog::signature::VerifiedSigner;
use kim::catalog::store::{Loaded, Source};
use kim::catalog::{self, Catalog};
use kim::error::ErrorCode;
use kim::fetch::DefaultFetcher;
use kim::install::{self, Action, Event, Phase, PlanOptions};
use kim::paths::Paths;
use kim::platform::Platform;
use serde_json::json;
use std::path::PathBuf;

mod common;
use common::{Server, package, release};
use std::sync::atomic::{AtomicBool, Ordering};

// --- fixtures --------------------------------------------------------------------------------

struct Sandbox(PathBuf);

impl Sandbox {
    fn new(tag: &str) -> Self {
        let p = std::env::temp_dir().join(format!("kim-install-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&p);
        std::fs::create_dir_all(&p).unwrap();
        Self(p)
    }
    fn paths(&self) -> Paths {
        Paths::under(&self.0)
    }
}

impl Drop for Sandbox {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

#[allow(clippy::needless_pass_by_value)] // test helper: owned `vec![...]` literals read better
fn loaded(releases: Vec<serde_json::Value>, extra_tools: Vec<serde_json::Value>) -> Loaded {
    let mut tools = vec![json!({
        "id": "example-tool", "name": "Example", "summary": "Fixture tool", "description": "d",
        "license": "MIT", "support": "stable", "releases": releases
    })];
    tools.extend(extra_tools);
    let doc = json!({
        "schema": "kim.catalog", "schema_version": 1, "catalog_version": 7,
        "generated_at": "2026-11-01T00:00:00Z", "expires_at": "2099-01-01T00:00:00Z",
        "publisher": "test", "tools": tools
    });
    let catalog: Catalog =
        catalog::parse(&serde_json::to_vec(&doc).unwrap()).expect("valid fixture catalog");
    Loaded {
        catalog,
        source: Source::Cache,
        signer: VerifiedSigner {
            key_id: "test".into(),
            label: "test".into(),
        },
        stale: false,
        warnings: vec![],
    }
}

fn run(
    plan: &install::Plan,
    paths: &Paths,
) -> (Result<install::Outcome, kim::error::KimError>, Vec<Event>) {
    let mut events = Vec::new();
    let cancel = AtomicBool::new(false);
    let r = install::execute(
        plan,
        paths,
        &DefaultFetcher::default(),
        &mut |e| events.push(e),
        &cancel,
    );
    (r, events)
}

fn opts() -> PlanOptions {
    PlanOptions::default()
}

fn staging_is_empty(paths: &Paths) -> bool {
    std::fs::read_dir(paths.staging_dir()).map_or(true, |mut rd| rd.next().is_none())
}

fn installed_version(paths: &Paths) -> Option<String> {
    let rec: serde_json::Value =
        serde_json::from_slice(&std::fs::read(paths.install_record("example-tool")).ok()?).ok()?;
    rec["version"].as_str().map(str::to_string)
}

// --- tests -----------------------------------------------------------------------------------

#[test]
fn fresh_install_verifies_activates_and_reports_progress() {
    let sb = Sandbox::new("fresh");
    let srv = Server::start();
    let body = package(&[("LICENSE", b"MIT")]);
    let url = srv.put("/t-1.0.0.zip", body.clone());
    let l = loaded(vec![release("1.0.0", &url, &body, "tool.exe")], vec![]);
    let p = sb.paths();
    let plan =
        install::plan_install(&l, &p, &Platform::current(), "example-tool", &opts()).unwrap();
    assert_eq!(
        (plan.action, plan.from.as_deref(), plan.to.as_str()),
        (Action::Install, None, "1.0.0")
    );
    assert!(
        !p.tool_dir("example-tool").exists(),
        "planning changes nothing"
    );

    let (r, events) = run(&plan, &p);
    let out = r.expect("install succeeds");
    assert!(out.entrypoint.is_file());
    assert_eq!(installed_version(&p).as_deref(), Some("1.0.0"));
    assert!(staging_is_empty(&p), "staging is removed");
    assert!(
        !p.tool_dir("example-tool").join(".kim.lock").exists(),
        "lock is released"
    );

    let phases: Vec<Phase> = events
        .iter()
        .filter_map(|e| match e {
            Event::Phase { phase } => Some(*phase),
            Event::Progress { .. } => None,
        })
        .collect();
    assert_eq!(
        phases,
        [
            Phase::Locking,
            Phase::Downloading,
            Phase::Verifying,
            Phase::Unpacking,
            Phase::SelfCheck,
            Phase::Activating,
            Phase::CleaningUp,
            Phase::Done
        ]
    );
    let progress: Vec<u64> = events
        .iter()
        .filter_map(|e| match e {
            Event::Progress { done, total } => {
                assert_eq!(*total, body.len() as u64);
                Some(*done)
            }
            Event::Phase { .. } => None,
        })
        .collect();
    assert!(
        progress.windows(2).all(|w| w[0] <= w[1]),
        "progress is monotonic"
    );
    assert_eq!(
        *progress.last().unwrap(),
        body.len() as u64,
        "progress reaches 100 %"
    );

    // The installed program really runs.
    let ok = std::process::Command::new(&out.entrypoint)
        .arg("--version")
        .output()
        .unwrap();
    assert!(ok.status.success());

    // Installing again is a no-op error, not a silent reinstall.
    let again =
        install::plan_install(&l, &p, &Platform::current(), "example-tool", &opts()).unwrap_err();
    assert!(again.message.contains("already installed"));
}

#[test]
fn update_keeps_previous_version_and_prunes_older_ones() {
    let sb = Sandbox::new("update");
    let srv = Server::start();
    let p = sb.paths();
    let platform = Platform::current();
    for (i, v) in ["1.0.0", "1.1.0", "1.2.0"].iter().enumerate() {
        let body = package(&[("VERSION", v.as_bytes())]);
        let url = srv.put(&format!("/t-{v}.zip"), body.clone());
        let l = loaded(vec![release(v, &url, &body, "tool.exe")], vec![]);
        let plan = if i == 0 {
            install::plan_install(&l, &p, &platform, "example-tool", &opts()).unwrap()
        } else {
            install::plan_update(&l, &p, &platform, "example-tool", &opts()).unwrap()
        };
        let out = run(&plan, &p).0.unwrap();
        if i == 2 {
            assert_eq!(out.action, Action::Update);
            assert_eq!(out.removed_versions, ["1.0.0"]);
        }
    }
    let versions: Vec<String> = std::fs::read_dir(p.tool_dir("example-tool").join("versions"))
        .unwrap()
        .flatten()
        .map(|e| e.file_name().to_string_lossy().into_owned())
        .collect();
    assert_eq!(versions.len(), 2, "{versions:?}");
    assert!(versions.contains(&"1.1.0".into()) && versions.contains(&"1.2.0".into()));
    let rec: serde_json::Value =
        serde_json::from_slice(&std::fs::read(p.install_record("example-tool")).unwrap()).unwrap();
    assert_eq!(rec["previous_version"], "1.1.0");
}

#[test]
fn integrity_failures_leave_the_installed_version_untouched() {
    let sb = Sandbox::new("integrity");
    let srv = Server::start();
    let p = sb.paths();
    let platform = Platform::current();
    let good = package(&[]);
    let url = srv.put("/good.zip", good.clone());
    let l = loaded(vec![release("1.0.0", &url, &good, "tool.exe")], vec![]);
    run(
        &install::plan_install(&l, &p, &platform, "example-tool", &opts()).unwrap(),
        &p,
    )
    .0
    .unwrap();
    let before = std::fs::read(p.install_record("example-tool")).unwrap();

    // Wrong hash in the (signed) catalog.
    let newer = package(&[("X", b"y")]);
    let url = srv.put("/newer.zip", newer.clone());
    let mut rel = release("2.0.0", &url, &newer, "tool.exe");
    rel["assets"][0]["sha256"] = json!("0".repeat(64));
    let l = loaded(vec![rel], vec![]);
    let plan = install::plan_update(&l, &p, &platform, "example-tool", &opts()).unwrap();
    let e = run(&plan, &p).0.unwrap_err();
    assert_eq!(e.code, ErrorCode::IntegrityFailed);
    assert_eq!(
        std::fs::read(p.install_record("example-tool")).unwrap(),
        before
    );
    assert!(staging_is_empty(&p));
    assert!(
        !p.tool_dir("example-tool")
            .join("versions")
            .join("2.0.0")
            .exists()
    );

    // Server sends more bytes than the catalog promises.
    let mut rel = release("2.0.0", &url, &newer, "tool.exe");
    rel["assets"][0]["size"] = json!(newer.len() - 100);
    let l = loaded(vec![rel], vec![]);
    let e = run(
        &install::plan_update(&l, &p, &platform, "example-tool", &opts()).unwrap(),
        &p,
    )
    .0
    .unwrap_err();
    assert_eq!(e.code, ErrorCode::IntegrityFailed);
    assert!(e.message.contains("larger than"), "{}", e.message);
    assert_eq!(
        std::fs::read(p.install_record("example-tool")).unwrap(),
        before
    );

    // Retry with a correct catalog succeeds.
    let l = loaded(vec![release("2.0.0", &url, &newer, "tool.exe")], vec![]);
    run(
        &install::plan_update(&l, &p, &platform, "example-tool", &opts()).unwrap(),
        &p,
    )
    .0
    .unwrap();
    assert_eq!(installed_version(&p).as_deref(), Some("2.0.0"));
}

#[test]
fn cancellation_during_download_changes_nothing() {
    let sb = Sandbox::new("cancel");
    let srv = Server::start();
    *srv.slow.lock().unwrap() = Some(std::time::Duration::from_millis(20));
    let p = sb.paths();
    let body = package(&[("pad.bin", &vec![7u8; 600 * 1024])]);
    let url = srv.put("/big.zip", body.clone());
    let l = loaded(vec![release("1.0.0", &url, &body, "tool.exe")], vec![]);
    let plan =
        install::plan_install(&l, &p, &Platform::current(), "example-tool", &opts()).unwrap();
    let cancel = AtomicBool::new(false);
    let mut seen = 0u64;
    let r = install::execute(
        &plan,
        &p,
        &DefaultFetcher::default(),
        &mut |e| {
            if let Event::Progress { done, .. } = e {
                seen = done;
                if done > 0 {
                    cancel.store(true, Ordering::Relaxed);
                }
            }
        },
        &cancel,
    );
    let e = r.unwrap_err();
    assert_eq!(e.code, ErrorCode::Cancelled);
    assert!(seen < body.len() as u64, "cancelled mid-download");
    assert!(installed_version(&p).is_none());
    assert!(staging_is_empty(&p), "partial download removed");
    assert!(!p.tool_dir("example-tool").join(".kim.lock").exists());
}

#[test]
#[cfg(windows)]
fn self_check_failure_and_bad_packages_are_rejected() {
    let sb = Sandbox::new("bad");
    let srv = Server::start();
    let p = sb.paths();
    let platform = Platform::current();

    // Entrypoint that fails `--version` (PING.EXE rejects the option).
    let ping = std::fs::read(r"C:\Windows\System32\PING.EXE").unwrap();
    let body = package(&[("bin/ping.exe", &ping)]);
    let url = srv.put("/ping.zip", body.clone());
    let l = loaded(vec![release("1.0.0", &url, &body, "bin/ping.exe")], vec![]);
    let e = run(
        &install::plan_install(&l, &p, &platform, "example-tool", &opts()).unwrap(),
        &p,
    )
    .0
    .unwrap_err();
    assert_eq!(e.code, ErrorCode::SelfCheckFailed);
    assert!(installed_version(&p).is_none());

    // Missing entrypoint.
    let body = package(&[]);
    let url = srv.put("/noentry.zip", body.clone());
    let l = loaded(vec![release("1.0.0", &url, &body, "missing.exe")], vec![]);
    let e = run(
        &install::plan_install(&l, &p, &platform, "example-tool", &opts()).unwrap(),
        &p,
    )
    .0
    .unwrap_err();
    assert_eq!(e.code, ErrorCode::PackageInvalid);

    // Zip-slip entry.
    let body = package(&[("../../escape.txt", b"pwned")]);
    let url = srv.put("/slip.zip", body.clone());
    let l = loaded(vec![release("1.0.0", &url, &body, "tool.exe")], vec![]);
    let e = run(
        &install::plan_install(&l, &p, &platform, "example-tool", &opts()).unwrap(),
        &p,
    )
    .0
    .unwrap_err();
    assert_eq!(e.code, ErrorCode::PackageInvalid, "{}", e.message);
    assert!(!sb.0.join("escape.txt").exists() && !p.programs.join("escape.txt").exists());

    // Not a zip at all.
    let body = b"definitely not a zip".to_vec();
    let url = srv.put("/junk.zip", body.clone());
    let l = loaded(vec![release("1.0.0", &url, &body, "tool.exe")], vec![]);
    let e = run(
        &install::plan_install(&l, &p, &platform, "example-tool", &opts()).unwrap(),
        &p,
    )
    .0
    .unwrap_err();
    assert_eq!(e.code, ErrorCode::PackageInvalid);
    assert!(staging_is_empty(&p));
}

#[test]
fn network_failure_and_retry() {
    let sb = Sandbox::new("retry");
    let srv = Server::start();
    let p = sb.paths();
    let body = package(&[]);
    let url = format!("http://127.0.0.1:{}/later.zip", srv.port);
    let l = loaded(vec![release("1.0.0", &url, &body, "tool.exe")], vec![]);
    let plan =
        install::plan_install(&l, &p, &Platform::current(), "example-tool", &opts()).unwrap();
    let e = run(&plan, &p).0.unwrap_err();
    assert_eq!(
        e.code,
        ErrorCode::NetworkUnavailable,
        "404 before publishing"
    );
    assert!(installed_version(&p).is_none());
    srv.put("/later.zip", body);
    run(&plan, &p)
        .0
        .expect("retry succeeds once the package is available");
    assert_eq!(installed_version(&p).as_deref(), Some("1.0.0"));
}

#[test]
fn locks_block_concurrent_operations_and_stale_locks_are_recovered() {
    let sb = Sandbox::new("lock");
    let p = sb.paths();
    let held = install::ToolLock::acquire(&p, "example-tool").unwrap();
    drop(held);
    // Another live Kim operation is simulated by a live foreign process holding the lock.
    #[cfg(windows)]
    {
        let lock = p.tool_dir("example-tool").join(".kim.lock");
        let mut other = std::process::Command::new(r"C:\Windows\System32\PING.EXE")
            .args(["-n", "30", "127.0.0.1"])
            .stdout(std::process::Stdio::null())
            .spawn()
            .unwrap();
        std::fs::write(&lock, other.id().to_string()).unwrap();
        let e = install::ToolLock::acquire(&p, "example-tool").unwrap_err();
        assert_eq!(e.code, ErrorCode::Busy);
        let _ = other.kill();
        let _ = other.wait();
        // Its owner is gone now: the stale lock is taken over.
        let l = install::ToolLock::acquire(&p, "example-tool").expect("stale lock is recovered");
        drop(l);
        assert!(!lock.exists());
    }
}

#[test]
fn leftover_staging_from_a_crash_is_cleaned_by_the_next_operation() {
    let sb = Sandbox::new("leftover");
    let srv = Server::start();
    let p = sb.paths();
    let crashed = p.staging_dir().join("example-tool-999-1");
    std::fs::create_dir_all(crashed.join("unpacked")).unwrap();
    std::fs::write(crashed.join("package"), b"half").unwrap();
    let body = package(&[]);
    let url = srv.put("/t.zip", body.clone());
    let l = loaded(vec![release("1.0.0", &url, &body, "tool.exe")], vec![]);
    run(
        &install::plan_install(&l, &p, &Platform::current(), "example-tool", &opts()).unwrap(),
        &p,
    )
    .0
    .unwrap();
    assert!(!crashed.exists());
}

#[test]
fn dependencies_are_never_installed_implicitly() {
    let sb = Sandbox::new("deps");
    let p = sb.paths();
    let body = package(&[]);
    let mut rel = release("1.0.0", "https://example.invalid/t.zip", &body, "tool.exe");
    rel["dependencies"] = json!([{"tool": "helper", "version_req": ">=1.0.0"}]);
    let helper = json!({"id": "helper", "name": "Helper", "summary": "s", "description": "d", "license": "MIT", "support": "stable", "releases": []});
    let l = loaded(vec![rel], vec![helper]);
    let e =
        install::plan_install(&l, &p, &Platform::current(), "example-tool", &opts()).unwrap_err();
    assert_eq!(e.code, ErrorCode::DependencyMissing);
    assert!(!p.programs.exists(), "nothing was created");
}

#[test]
#[cfg(windows)]
fn uninstall_keeps_user_data_refuses_running_tools_and_purges_only_on_request() {
    let sb = Sandbox::new("uninstall");
    let srv = Server::start();
    let p = sb.paths();
    let ping = std::fs::read(r"C:\Windows\System32\PING.EXE").unwrap();
    let body = package(&[("helper.exe", &ping)]);
    let url = srv.put("/t.zip", body.clone());
    let l = loaded(vec![release("1.0.0", &url, &body, "tool.exe")], vec![]);
    let out = run(
        &install::plan_install(&l, &p, &Platform::current(), "example-tool", &opts()).unwrap(),
        &p,
    )
    .0
    .unwrap();
    for d in p.user_data_dirs("example-tool") {
        std::fs::create_dir_all(&d).unwrap();
        std::fs::write(d.join("settings.json"), b"{}").unwrap();
    }

    // A program from the install directory is running: refuse, change nothing, stop nothing.
    let helper = out.entrypoint.parent().unwrap().join("helper.exe");
    let mut child = std::process::Command::new(&helper)
        .args(["-n", "30", "127.0.0.1"])
        .stdout(std::process::Stdio::null())
        .spawn()
        .unwrap();
    std::thread::sleep(std::time::Duration::from_millis(300));
    let plan = install::plan_uninstall(&p, "example-tool", false).unwrap();
    assert_eq!(plan.running.len(), 1);
    let e = install::execute_uninstall(&plan, &p).unwrap_err();
    assert_eq!(e.code, ErrorCode::ToolRunning);
    assert!(
        child.try_wait().unwrap().is_none(),
        "Kim never stops the running program"
    );
    assert!(p.install_record("example-tool").exists());
    let _ = child.kill();
    let _ = child.wait();

    // Default uninstall removes program files but keeps user data.
    let plan = install::plan_uninstall(&p, "example-tool", false).unwrap();
    let out = install::execute_uninstall(&plan, &p).unwrap();
    assert_eq!(out.removed_version.as_deref(), Some("1.0.0"));
    assert_eq!(out.kept_data.len(), 2);
    assert!(!p.tool_dir("example-tool").exists());
    assert!(
        p.user_data_dirs("example-tool")
            .iter()
            .all(|d| d.join("settings.json").exists())
    );
    assert_eq!(
        install::plan_uninstall(&p, "example-tool", false)
            .unwrap_err()
            .code,
        ErrorCode::NotInstalled
    );

    // Purge only when explicitly requested.
    run(
        &install::plan_install(&l, &p, &Platform::current(), "example-tool", &opts()).unwrap(),
        &p,
    )
    .0
    .unwrap();
    let out = install::execute_uninstall(
        &install::plan_uninstall(&p, "example-tool", true).unwrap(),
        &p,
    )
    .unwrap();
    assert_eq!(out.purged_data.len(), 2);
    assert!(p.user_data_dirs("example-tool").iter().all(|d| !d.exists()));
}

#[test]
#[cfg(windows)]
fn update_while_running_never_stops_the_running_version() {
    let sb = Sandbox::new("running-update");
    let srv = Server::start();
    let p = sb.paths();
    let platform = Platform::current();
    let ping = std::fs::read(r"C:\Windows\System32\PING.EXE").unwrap();
    let install_version = |v: &str| {
        let body = package(&[("helper.exe", &ping), ("V", v.as_bytes())]);
        let url = srv.put(&format!("/{v}.zip"), body.clone());
        let l = loaded(vec![release(v, &url, &body, "tool.exe")], vec![]);
        let plan = if installed_version(&p).is_none() {
            install::plan_install(&l, &p, &platform, "example-tool", &opts()).unwrap()
        } else {
            install::plan_update(&l, &p, &platform, "example-tool", &opts()).unwrap()
        };
        (plan.running.len(), run(&plan, &p).0.unwrap())
    };
    let (_, first) = install_version("1.0.0");
    let helper = first.entrypoint.parent().unwrap().join("helper.exe");
    let mut child = std::process::Command::new(&helper)
        .args(["-n", "30", "127.0.0.1"])
        .stdout(std::process::Stdio::null())
        .spawn()
        .unwrap();
    std::thread::sleep(std::time::Duration::from_millis(300));
    let (running_seen, _) = install_version("2.0.0");
    assert_eq!(running_seen, 1, "the plan reports the running program");
    let (_, third) = install_version("3.0.0");
    assert_eq!(
        third.kept_in_use,
        ["1.0.0"],
        "the in-use version is kept, not deleted"
    );
    assert!(
        child.try_wait().unwrap().is_none(),
        "still running after two updates"
    );
    let _ = child.kill();
    let _ = child.wait();
    assert!(helper.exists());
}

#[test]
fn stale_catalogs_need_explicit_consent() {
    let sb = Sandbox::new("stale");
    let p = sb.paths();
    let body = package(&[]);
    let mut l = loaded(
        vec![release(
            "1.0.0",
            "https://example.invalid/t.zip",
            &body,
            "tool.exe",
        )],
        vec![],
    );
    l.stale = true;
    let e =
        install::plan_install(&l, &p, &Platform::current(), "example-tool", &opts()).unwrap_err();
    assert_eq!(e.code, ErrorCode::StaleCatalog);
    let plan = install::plan_install(
        &l,
        &p,
        &Platform::current(),
        "example-tool",
        &PlanOptions {
            allow_stale: true,
            ..PlanOptions::default()
        },
    )
    .unwrap();
    assert!(plan.notes.iter().any(|n| n.contains("stale")));
}

#[test]
fn check_updates_reports_without_changing_anything() {
    let sb = Sandbox::new("check");
    let srv = Server::start();
    let p = sb.paths();
    let platform = Platform::current();
    let b1 = package(&[]);
    let url = srv.put("/1.zip", b1.clone());
    let l1 = loaded(vec![release("1.0.0", &url, &b1, "tool.exe")], vec![]);
    run(
        &install::plan_install(&l1, &p, &platform, "example-tool", &opts()).unwrap(),
        &p,
    )
    .0
    .unwrap();
    let b2 = package(&[("X", b"2")]);
    let l2 = loaded(
        vec![
            release("1.0.0", &url, &b1, "tool.exe"),
            release("2.0.0", "https://example.invalid/2.zip", &b2, "tool.exe"),
        ],
        vec![],
    );
    let ups = install::check_updates(&l2, &p, &platform, catalog::Channel::Stable);
    assert_eq!(ups.len(), 1);
    assert_eq!(
        (ups[0].installed.as_str(), ups[0].available.as_str()),
        ("1.0.0", "2.0.0")
    );
    assert_eq!(
        installed_version(&p).as_deref(),
        Some("1.0.0"),
        "checking never updates"
    );
}
