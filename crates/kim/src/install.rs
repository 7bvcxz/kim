//! Explicit, transactional install / update / uninstall (ADR 0013, docs/RELEASE_POLICY.md).
//!
//! Nothing here runs unless a user asked for that specific operation. Every operation:
//! * takes a per-tool lock (no two Kim operations on one tool at once);
//! * downloads into a private staging directory, verifies size and SHA-256 against the
//!   **signed** catalog, unpacks with path and size limits, and runs the program's own
//!   `--version` self-check — all before touching the installed version;
//! * activates by atomically replacing `current.json` (the single commit point);
//! * on failure or cancellation before the commit point, removes its staging data and leaves the
//!   previously installed version exactly as it was;
//! * never stops a running program and never elevates privileges.

use crate::catalog::status::{self, Installed, incompatibility, matching_asset};
use crate::catalog::store::Loaded;
use crate::catalog::{Asset, AssetKind, Channel, Release};
use crate::error::{ErrorCode, KimError, Result};
use crate::fetch::Downloader;
use crate::paths::Paths;
use crate::platform::Platform;
use crate::running::{self, RunningProcess};
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

pub const RECORD_SCHEMA: &str = "kim.install";
const LOCK_FILE: &str = ".kim.lock";
const MAX_ZIP_ENTRIES: usize = 10_000;
const MAX_UNPACKED_BYTES: u64 = 8 * 1024 * 1024 * 1024;
const SELF_CHECK_TIMEOUT: Duration = Duration::from_secs(15);

/// `<programs>\<tool>\current.json`: the active version. Replaced atomically on activation.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct InstallRecord {
    pub schema: String,
    pub schema_version: u32,
    pub tool: String,
    pub version: String,
    /// Entrypoint relative to the tool directory, `/`-separated.
    pub entrypoint: String,
    pub installed_at: String,
    pub sha256: String,
    pub source_url: String,
    pub catalog_version: u64,
    pub previous_version: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum Action {
    Install,
    Update,
    Downgrade,
    Reinstall,
}

/// What an install/update would do. Computing it changes nothing (`--dry-run` prints it).
#[derive(Debug, Clone, Serialize)]
pub struct Plan {
    pub tool: String,
    pub action: Action,
    pub from: Option<String>,
    pub to: String,
    pub channel: Channel,
    pub release_notes: String,
    pub asset: Asset,
    pub catalog_version: u64,
    pub tool_dir: PathBuf,
    /// Programs currently running from this tool's directory. They are never stopped.
    pub running: Vec<RunningProcess>,
    pub notes: Vec<String>,
}

#[derive(Debug, Clone, Default)]
pub struct PlanOptions {
    pub version: Option<String>,
    pub channel: Option<Channel>,
    pub allow_stale: bool,
    pub reinstall: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum Phase {
    Locking,
    Downloading,
    Verifying,
    Unpacking,
    SelfCheck,
    Activating,
    CleaningUp,
    Done,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(tag = "event", rename_all = "kebab-case")]
pub enum Event {
    Phase { phase: Phase },
    Progress { done: u64, total: u64 },
}

#[derive(Debug, Clone, Serialize)]
pub struct Outcome {
    pub tool: String,
    pub action: Action,
    pub from: Option<String>,
    pub to: String,
    pub entrypoint: PathBuf,
    pub removed_versions: Vec<String>,
    /// Old versions kept because a program from them is still running (removed later).
    pub kept_in_use: Vec<String>,
    pub running: Vec<RunningProcess>,
}

fn installed_record(paths: &Paths, tool: &str) -> Option<InstallRecord> {
    std::fs::read(paths.install_record(tool))
        .ok()
        .and_then(|b| serde_json::from_slice(&b).ok())
}

/// Resolves what `kim install <tool>` (or `kim update <tool>`) would do.
pub fn plan_install(
    loaded: &Loaded,
    paths: &Paths,
    platform: &Platform,
    tool_id: &str,
    opts: &PlanOptions,
) -> Result<Plan> {
    if tool_id == "kim" {
        return Err(
            KimError::new(ErrorCode::Usage, "Kim does not install or update itself")
                .with_hint("install new Kim versions manually; self-update is not supported"),
        );
    }
    let Some(tool) = loaded.catalog.tool(tool_id) else {
        let known: Vec<&str> = loaded.catalog.tools.iter().map(|t| t.id.as_str()).collect();
        return Err(KimError::new(
            ErrorCode::ToolNotFound,
            format!("no tool `{tool_id}` in the catalog"),
        )
        .with_hint(format!("known tools: {}", known.join(", "))));
    };
    if loaded.stale && !opts.allow_stale {
        return Err(KimError::new(
            ErrorCode::StaleCatalog,
            format!("the catalog expired on {}", loaded.catalog.expires_at),
        )
        .with_hint("run `kim catalog refresh`, or pass --allow-stale to use it anyway"));
    }
    let kim = semver::Version::parse(crate::KIM_VERSION).expect("Kim version is SemVer");
    let channel = opts.channel.unwrap_or(Channel::Stable);
    let release: &Release = if let Some(want) = &opts.version {
        let r = tool
            .releases
            .iter()
            .find(|r| &r.version == want)
            .ok_or_else(|| {
                KimError::new(
                    ErrorCode::NoRelease,
                    format!("{tool_id} has no release {want}"),
                )
            })?;
        if let Some(why) = incompatibility(r, platform, &kim) {
            return Err(KimError::new(
                ErrorCode::Incompatible,
                format!("{tool_id} {want} cannot be installed here: {why}"),
            ));
        }
        r
    } else {
        let view = status::evaluate_tool(tool, platform, &kim, &Installed::No, channel);
        match (&view.latest_compatible, &view.latest) {
            (Some(v), _) => tool
                .releases
                .iter()
                .find(|r| &r.version == v)
                .expect("release exists"),
            (None, Some(_)) => {
                return Err(KimError::new(
                    ErrorCode::Incompatible,
                    format!(
                        "no release of {tool_id} fits this machine: {}",
                        view.incompatible_reason.unwrap_or_default()
                    ),
                ));
            }
            (None, None) => {
                let mut e = KimError::new(
                    ErrorCode::NoRelease,
                    format!("{tool_id} has no release on the {channel:?} channel yet")
                        .to_lowercase(),
                );
                if let Some(note) = view.note {
                    e = e.with_hint(note);
                }
                return Err(e);
            }
        }
    };
    let asset = matching_asset(release, platform)
        .expect("compatible release has an asset")
        .clone();
    let to = semver::Version::parse(&release.version).expect("validated SemVer");

    let mut missing = Vec::new();
    for dep in &release.dependencies {
        let req = semver::VersionReq::parse(&dep.version_req).expect("validated");
        match status::installed(paths, &dep.tool) {
            Installed::Version(v) if req.matches(&v) => {}
            _ => missing.push(format!("{} {}", dep.tool, dep.version_req)),
        }
    }
    if !missing.is_empty() {
        return Err(KimError::new(
            ErrorCode::DependencyMissing,
            format!("{tool_id} {to} needs: {}", missing.join(", ")),
        )
        .with_hint("install the dependencies first; Kim never installs anything implicitly"));
    }

    let installed = status::installed(paths, tool_id);
    let (action, from) = match &installed {
        Installed::No => (Action::Install, None),
        Installed::Damaged(_) => (Action::Reinstall, None),
        Installed::Version(v) if to > *v => (Action::Update, Some(v.to_string())),
        Installed::Version(v) if to < *v && opts.version.is_some() => {
            (Action::Downgrade, Some(v.to_string()))
        }
        Installed::Version(v) if to == *v && opts.reinstall => {
            (Action::Reinstall, Some(v.to_string()))
        }
        Installed::Version(v) => {
            return Err(KimError::new(
                ErrorCode::Usage,
                format!("{tool_id} {v} is already installed (newest compatible: {to})"),
            )
            .with_hint(
                "nothing to do; use --reinstall to repair, or --version to pick another release",
            ));
        }
    };
    let tool_dir = paths.tool_dir(tool_id);
    let running = running::processes_under(&tool_dir);
    let mut notes = Vec::new();
    if !running.is_empty() {
        notes.push(format!(
            "{} program(s) from the installed {tool_id} are running; they keep running and are not stopped. The new version is used the next time {tool_id} starts.",
            running.len()
        ));
    }
    if loaded.stale {
        notes.push("installing from a stale catalog (--allow-stale)".into());
    }
    Ok(Plan {
        tool: tool_id.into(),
        action,
        from,
        to: to.to_string(),
        channel: release.channel,
        release_notes: release.release_notes.clone(),
        asset,
        catalog_version: loaded.catalog.catalog_version,
        tool_dir,
        running,
        notes,
    })
}

/// `kim update <tool>`: like install, but the tool must already be installed.
pub fn plan_update(
    loaded: &Loaded,
    paths: &Paths,
    platform: &Platform,
    tool_id: &str,
    opts: &PlanOptions,
) -> Result<Plan> {
    if tool_id != "kim" && matches!(status::installed(paths, tool_id), Installed::No) {
        return Err(KimError::new(
            ErrorCode::NotInstalled,
            format!("{tool_id} is not installed"),
        )
        .with_hint(format!(
            "install it explicitly with `kim install {tool_id}`"
        )));
    }
    plan_install(loaded, paths, platform, tool_id, opts)
}

/// One available update (display only; never applied by this function).
#[derive(Debug, Clone, Serialize)]
pub struct UpdateInfo {
    pub tool: String,
    pub installed: String,
    pub available: String,
    pub release_notes: String,
}

pub fn check_updates(
    loaded: &Loaded,
    paths: &Paths,
    platform: &Platform,
    channel: Channel,
) -> Vec<UpdateInfo> {
    status::evaluate(&loaded.catalog, paths, platform, channel)
        .into_iter()
        .filter(|v| v.status == status::ToolStatus::UpdateAvailable && v.id != "kim")
        .filter_map(|v| {
            let available = v.latest_compatible?;
            let notes = loaded
                .catalog
                .tool(&v.id)
                .and_then(|t| t.releases.iter().find(|r| r.version == available))
                .map(|r| r.release_notes.clone())
                .unwrap_or_default();
            Some(UpdateInfo {
                tool: v.id,
                installed: v.installed_version.unwrap_or_default(),
                available,
                release_notes: notes,
            })
        })
        .collect()
}

// ---------------------------------------------------------------------------------------------
// Locking and staging

/// Exclusive per-tool lock (`<tool dir>\.kim.lock`, holding the owner PID). Released on drop.
#[derive(Debug)]
pub struct ToolLock(PathBuf);

impl ToolLock {
    pub fn acquire(paths: &Paths, tool: &str) -> Result<Self> {
        let dir = paths.tool_dir(tool);
        std::fs::create_dir_all(&dir).map_err(|e| io_err(&dir, &e))?;
        let path = dir.join(LOCK_FILE);
        for _ in 0..2 {
            match std::fs::OpenOptions::new()
                .write(true)
                .create_new(true)
                .open(&path)
            {
                Ok(mut f) => {
                    use std::io::Write;
                    let _ = write!(f, "{}", std::process::id());
                    return Ok(Self(path));
                }
                Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => {
                    let owner: Option<u32> = std::fs::read_to_string(&path)
                        .ok()
                        .and_then(|s| s.trim().parse().ok());
                    if owner.is_some_and(|pid| pid != std::process::id() && running::pid_alive(pid))
                    {
                        return Err(KimError::new(
                            ErrorCode::Busy,
                            format!(
                                "another Kim operation on {tool} is in progress (PID {})",
                                owner.unwrap_or(0)
                            ),
                        ));
                    }
                    // Stale lock from a crashed operation: take it over.
                    let _ = std::fs::remove_file(&path);
                }
                Err(e) => return Err(io_err(&path, &e)),
            }
        }
        Err(KimError::new(
            ErrorCode::Busy,
            format!("cannot lock {}", path.display()),
        ))
    }
}

impl Drop for ToolLock {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.0);
    }
}

fn io_err(path: &Path, e: &std::io::Error) -> KimError {
    let mut err = KimError::new(ErrorCode::Io, format!("{}: {e}", path.display()));
    if e.kind() == std::io::ErrorKind::PermissionDenied {
        err = err.with_hint("Kim installs per user and never elevates; check folder permissions");
    }
    err
}

/// Removes staging leftovers of `tool` (safe while holding the tool lock).
fn clean_staging(paths: &Paths, tool: &str) {
    let prefix = format!("{tool}-");
    if let Ok(rd) = std::fs::read_dir(paths.staging_dir()) {
        for e in rd.flatten() {
            if e.file_name().to_string_lossy().starts_with(&prefix) {
                let _ = std::fs::remove_dir_all(e.path());
            }
        }
    }
}

struct Staging(PathBuf);

impl Staging {
    fn new(paths: &Paths, tool: &str) -> Result<Self> {
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or_default();
        let dir = paths
            .staging_dir()
            .join(format!("{tool}-{}-{nanos}", std::process::id()));
        std::fs::create_dir_all(&dir).map_err(|e| io_err(&dir, &e))?;
        Ok(Self(dir))
    }
}

impl Drop for Staging {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

// ---------------------------------------------------------------------------------------------
// Execute

/// Runs a plan. `progress` receives phase changes and byte counts; setting `cancel` aborts the
/// operation at the next check (before activation) with `cancelled`, changing nothing.
pub fn execute(
    plan: &Plan,
    paths: &Paths,
    downloader: &dyn Downloader,
    progress: &mut dyn FnMut(Event),
    cancel: &AtomicBool,
) -> Result<Outcome> {
    progress(Event::Phase {
        phase: Phase::Locking,
    });
    let _lock = ToolLock::acquire(paths, &plan.tool)?;
    clean_staging(paths, &plan.tool);
    let staging = Staging::new(paths, &plan.tool)?;
    let check_cancel = || {
        if cancel.load(Ordering::Relaxed) {
            Err(KimError::new(
                ErrorCode::Cancelled,
                "cancelled; nothing was changed",
            ))
        } else {
            Ok(())
        }
    };

    // 1. Download.
    progress(Event::Phase {
        phase: Phase::Downloading,
    });
    let package = staging.0.join("package");
    let total = plan.asset.size;
    progress(Event::Progress { done: 0, total });
    downloader
        .download(
            &plan.asset.url,
            &package,
            total,
            &mut |done| progress(Event::Progress { done, total }),
            cancel,
        )
        .map_err(|mut e| {
            if e.code == ErrorCode::Cancelled {
                e.message = "cancelled; nothing was changed".into();
            }
            e
        })?;
    check_cancel()?;

    // 2. Verify against the signed catalog.
    progress(Event::Phase {
        phase: Phase::Verifying,
    });
    verify_package(&package, &plan.asset, cancel)?;
    check_cancel()?;

    // 3. Unpack.
    progress(Event::Phase {
        phase: Phase::Unpacking,
    });
    let unpacked = staging.0.join("unpacked");
    std::fs::create_dir_all(&unpacked).map_err(|e| io_err(&unpacked, &e))?;
    match plan.asset.kind {
        AssetKind::Zip => unzip(&package, &unpacked, cancel)?,
        AssetKind::Exe => {
            let dest = unpacked.join(&plan.asset.entrypoint);
            if let Some(parent) = dest.parent() {
                std::fs::create_dir_all(parent).map_err(|e| io_err(parent, &e))?;
            }
            std::fs::copy(&package, &dest).map_err(|e| io_err(&dest, &e))?;
        }
    }
    let entry = unpacked.join(&plan.asset.entrypoint);
    if !entry.is_file() {
        return Err(KimError::new(
            ErrorCode::PackageInvalid,
            format!(
                "the package does not contain its entrypoint `{}`",
                plan.asset.entrypoint
            ),
        ));
    }
    check_cancel()?;

    // 4. Self-check of the staged program.
    progress(Event::Phase {
        phase: Phase::SelfCheck,
    });
    self_check(&entry)?;
    check_cancel()?;

    // 5. Activate: move into versions\<to>, then atomically replace current.json.
    progress(Event::Phase {
        phase: Phase::Activating,
    });
    let versions = plan.tool_dir.join("versions");
    std::fs::create_dir_all(&versions).map_err(|e| io_err(&versions, &e))?;
    let target = versions.join(&plan.to);
    let mut displaced: Option<PathBuf> = None;
    if target.exists() {
        let aside = versions.join(format!("{}.replaced-{}", plan.to, std::process::id()));
        let _ = std::fs::remove_dir_all(&aside);
        std::fs::rename(&target, &aside).map_err(|e| {
            KimError::new(
                ErrorCode::ToolRunning,
                format!("cannot replace {} (in use?): {e}", target.display()),
            )
            .with_hint("close the running program and retry; nothing was changed")
        })?;
        displaced = Some(aside);
    }
    if let Err(e) = std::fs::rename(&unpacked, &target) {
        if let Some(aside) = &displaced {
            let _ = std::fs::rename(aside, &target);
        }
        return Err(io_err(&target, &e));
    }
    let record = InstallRecord {
        schema: RECORD_SCHEMA.into(),
        schema_version: 1,
        tool: plan.tool.clone(),
        version: plan.to.clone(),
        entrypoint: format!(
            "versions/{}/{}",
            plan.to,
            plan.asset.entrypoint.replace('\\', "/")
        ),
        installed_at: now_rfc3339(),
        sha256: plan.asset.sha256.clone(),
        source_url: plan.asset.url.clone(),
        catalog_version: plan.catalog_version,
        previous_version: plan.from.clone(),
    };
    if let Err(e) = write_record(paths, &record) {
        // Commit failed: undo the move so the previous state is exactly restored.
        let _ = std::fs::remove_dir_all(&target);
        if let Some(aside) = &displaced {
            let _ = std::fs::rename(aside, &target);
        }
        return Err(e);
    }

    // 6. Clean up: staging, displaced copies, and old versions not in use (keep the previous one
    //    for rollback).
    progress(Event::Phase {
        phase: Phase::CleaningUp,
    });
    if let Some(aside) = displaced {
        let _ = std::fs::remove_dir_all(aside);
    }
    let (removed_versions, kept_in_use) = prune_versions(&versions, &plan.to, plan.from.as_deref());
    drop(staging);
    progress(Event::Phase { phase: Phase::Done });
    Ok(Outcome {
        tool: plan.tool.clone(),
        action: plan.action,
        from: plan.from.clone(),
        to: plan.to.clone(),
        entrypoint: plan.tool_dir.join(&record.entrypoint),
        removed_versions,
        kept_in_use,
        running: plan.running.clone(),
    })
}

fn now_rfc3339() -> String {
    let now = crate::time::now_utc();
    let secs = now.rem_euclid(86_400);
    format!(
        "{}T{:02}:{:02}:{:02}Z",
        crate::time::format_date(now),
        secs / 3600,
        (secs % 3600) / 60,
        secs % 60
    )
}

fn write_record(paths: &Paths, record: &InstallRecord) -> Result<()> {
    let path = paths.install_record(&record.tool);
    let tmp = path.with_extension("json.tmp");
    let json = serde_json::to_vec_pretty(record)
        .map_err(|e| KimError::new(ErrorCode::Internal, e.to_string()))?;
    {
        use std::io::Write;
        let mut f = std::fs::File::create(&tmp).map_err(|e| io_err(&tmp, &e))?;
        f.write_all(&json).map_err(|e| io_err(&tmp, &e))?;
        f.sync_all().map_err(|e| io_err(&tmp, &e))?;
    }
    std::fs::rename(&tmp, &path).map_err(|e| io_err(&path, &e))
}

fn prune_versions(
    versions: &Path,
    keep: &str,
    previous: Option<&str>,
) -> (Vec<String>, Vec<String>) {
    let mut removed = Vec::new();
    let mut kept = Vec::new();
    let Ok(rd) = std::fs::read_dir(versions) else {
        return (removed, kept);
    };
    for e in rd.flatten() {
        let name = e.file_name().to_string_lossy().to_string();
        if name == keep || Some(name.as_str()) == previous {
            continue;
        }
        if !running::processes_under(&e.path()).is_empty() {
            kept.push(name);
            continue;
        }
        match std::fs::remove_dir_all(e.path()) {
            Ok(()) => removed.push(name),
            Err(_) => kept.push(name),
        }
    }
    (removed, kept)
}

fn verify_package(package: &Path, asset: &Asset, cancel: &AtomicBool) -> Result<()> {
    use sha2::{Digest, Sha256};
    use std::io::Read;
    let size = std::fs::metadata(package)
        .map_err(|e| io_err(package, &e))?
        .len();
    if size != asset.size {
        return Err(KimError::new(
            ErrorCode::IntegrityFailed,
            format!(
                "downloaded {size} bytes, but the signed catalog says {}",
                asset.size
            ),
        )
        .with_hint("the download was incomplete or altered; nothing was changed — retry"));
    }
    let mut f = std::fs::File::open(package).map_err(|e| io_err(package, &e))?;
    let mut hasher = Sha256::new();
    let mut buf = vec![0u8; 256 * 1024];
    loop {
        if cancel.load(Ordering::Relaxed) {
            return Err(KimError::new(
                ErrorCode::Cancelled,
                "cancelled; nothing was changed",
            ));
        }
        let n = f.read(&mut buf).map_err(|e| io_err(package, &e))?;
        if n == 0 {
            break;
        }
        hasher.update(&buf[..n]);
    }
    let got = crate::catalog::signature::hex(&hasher.finalize());
    if got != asset.sha256 {
        return Err(KimError::new(
            ErrorCode::IntegrityFailed,
            format!(
                "SHA-256 mismatch: got {got}, the signed catalog says {}",
                asset.sha256
            ),
        )
        .with_hint("the package was altered or corrupted; nothing was changed"));
    }
    Ok(())
}

fn unzip(package: &Path, dest: &Path, cancel: &AtomicBool) -> Result<()> {
    let bad = |m: String| KimError::new(ErrorCode::PackageInvalid, m);
    let file = std::fs::File::open(package).map_err(|e| io_err(package, &e))?;
    let mut zip =
        zip::ZipArchive::new(file).map_err(|e| bad(format!("not a valid zip archive: {e}")))?;
    if zip.len() > MAX_ZIP_ENTRIES {
        return Err(bad(format!(
            "archive has {} entries (limit {MAX_ZIP_ENTRIES})",
            zip.len()
        )));
    }
    let mut total = 0u64;
    for i in 0..zip.len() {
        if cancel.load(Ordering::Relaxed) {
            return Err(KimError::new(
                ErrorCode::Cancelled,
                "cancelled; nothing was changed",
            ));
        }
        let mut entry = zip
            .by_index(i)
            .map_err(|e| bad(format!("corrupt archive entry {i}: {e}")))?;
        let Some(rel) = entry.enclosed_name() else {
            let name = entry.name().map(|n| n.into_owned()).unwrap_or_default();
            return Err(bad(format!("archive entry `{name}` has an unsafe path")));
        };
        let out = dest.join(&rel);
        if entry.is_dir() {
            std::fs::create_dir_all(&out).map_err(|e| io_err(&out, &e))?;
            continue;
        }
        total = total.saturating_add(entry.size());
        if total > MAX_UNPACKED_BYTES {
            return Err(bad("archive expands beyond the 8 GiB limit".into()));
        }
        if let Some(parent) = out.parent() {
            std::fs::create_dir_all(parent).map_err(|e| io_err(parent, &e))?;
        }
        let mut f = std::fs::File::create(&out).map_err(|e| io_err(&out, &e))?;
        let declared = entry.size();
        let written = std::io::copy(&mut std::io::Read::take(&mut entry, declared + 1), &mut f)
            .map_err(|e| bad(format!("cannot extract `{}`: {e}", rel.display())))?;
        if written != declared {
            return Err(bad(format!(
                "archive entry `{}` has an inconsistent size",
                rel.display()
            )));
        }
    }
    Ok(())
}

fn self_check(entry: &Path) -> Result<()> {
    let mut cmd = std::process::Command::new(entry);
    cmd.arg("--version")
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped());
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        const CREATE_NO_WINDOW: u32 = 0x0800_0000;
        cmd.creation_flags(CREATE_NO_WINDOW);
    }
    let mut child = cmd.spawn().map_err(|e| {
        KimError::new(
            ErrorCode::SelfCheckFailed,
            format!("cannot start {}: {e}", entry.display()),
        )
    })?;
    let started = std::time::Instant::now();
    loop {
        match child.try_wait() {
            Ok(Some(status)) if status.success() => return Ok(()),
            Ok(Some(status)) => {
                return Err(KimError::new(
                    ErrorCode::SelfCheckFailed,
                    format!("`{} --version` exited with {status}", entry.display()),
                ));
            }
            Ok(None) if started.elapsed() > SELF_CHECK_TIMEOUT => {
                let _ = child.kill();
                let _ = child.wait();
                return Err(KimError::new(
                    ErrorCode::SelfCheckFailed,
                    format!(
                        "`{} --version` did not finish within {SELF_CHECK_TIMEOUT:?}",
                        entry.display()
                    ),
                ));
            }
            Ok(None) => std::thread::sleep(Duration::from_millis(25)),
            Err(e) => return Err(KimError::new(ErrorCode::SelfCheckFailed, e.to_string())),
        }
    }
}

// ---------------------------------------------------------------------------------------------
// Uninstall

#[derive(Debug, Clone, Serialize)]
pub struct UninstallPlan {
    pub tool: String,
    pub version: Option<String>,
    pub tool_dir: PathBuf,
    pub running: Vec<RunningProcess>,
    /// User data that will be **kept** unless `purge_data` is set.
    pub data_dirs: Vec<PathBuf>,
    pub purge_data: bool,
}

#[derive(Debug, Clone, Serialize)]
pub struct UninstallOutcome {
    pub tool: String,
    pub removed_version: Option<String>,
    pub leftovers: Vec<String>,
    pub purged_data: Vec<PathBuf>,
    pub kept_data: Vec<PathBuf>,
}

pub fn plan_uninstall(paths: &Paths, tool: &str, purge_data: bool) -> Result<UninstallPlan> {
    if tool == "kim" {
        return Err(KimError::new(
            ErrorCode::Usage,
            "Kim does not uninstall itself",
        ));
    }
    let tool_dir = paths.tool_dir(tool);
    let record = installed_record(paths, tool);
    if record.is_none()
        && !paths.install_record(tool).exists()
        && !tool_dir.join("versions").exists()
    {
        return Err(KimError::new(
            ErrorCode::NotInstalled,
            format!("{tool} is not installed"),
        ));
    }
    Ok(UninstallPlan {
        tool: tool.into(),
        version: record.map(|r| r.version),
        running: running::processes_under(&tool_dir),
        data_dirs: paths
            .user_data_dirs(tool)
            .into_iter()
            .filter(|d| d.exists())
            .collect(),
        tool_dir,
        purge_data,
    })
}

pub fn execute_uninstall(plan: &UninstallPlan, paths: &Paths) -> Result<UninstallOutcome> {
    let lock = ToolLock::acquire(paths, &plan.tool)?;
    let running = running::processes_under(&plan.tool_dir);
    if !running.is_empty() {
        let pids: Vec<String> = running.iter().map(|p| p.pid.to_string()).collect();
        return Err(KimError::new(
            ErrorCode::ToolRunning,
            format!(
                "{} is running (PID {}); Kim never stops it",
                plan.tool,
                pids.join(", ")
            ),
        )
        .with_hint(if plan.tool == "subak" {
            "stop it yourself first (e.g. end your Subak sessions), then retry".to_string()
        } else {
            format!("close {} first, then retry", plan.tool)
        }));
    }
    clean_staging(paths, &plan.tool);
    // Commit point: once current.json is gone, the tool counts as uninstalled.
    let record = paths.install_record(&plan.tool);
    if record.exists() {
        std::fs::remove_file(&record).map_err(|e| io_err(&record, &e))?;
    }
    let mut leftovers = Vec::new();
    let versions = plan.tool_dir.join("versions");
    if versions.exists()
        && let Err(e) = std::fs::remove_dir_all(&versions)
    {
        leftovers.push(format!("{}: {e}", versions.display()));
    }
    drop(lock);
    if leftovers.is_empty() {
        let _ = std::fs::remove_dir(&plan.tool_dir);
    }
    let mut purged = Vec::new();
    let mut kept = Vec::new();
    for d in paths.user_data_dirs(&plan.tool) {
        if !d.exists() {
            continue;
        }
        if plan.purge_data {
            match std::fs::remove_dir_all(&d) {
                Ok(()) => purged.push(d),
                Err(e) => leftovers.push(format!("{}: {e}", d.display())),
            }
        } else {
            kept.push(d);
        }
    }
    Ok(UninstallOutcome {
        tool: plan.tool.clone(),
        removed_version: plan.version.clone(),
        leftovers,
        purged_data: purged,
        kept_data: kept,
    })
}
