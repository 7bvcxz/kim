//! The rule store: `rules.json` on disk (docs/DDALGAK_RULE_MODEL.md §2, §9, §10; ADR 0006,
//! ADR 0028; `DDALGAK-FR-005`, `DDALGAK-FR-006`, `DDALGAK-FR-031`, `DDALGAK-NFR-008`).
//!
//! | File (in `%APPDATA%\mouv\ddalgak\`) | What |
//! | --- | --- |
//! | `rules.json` | the saved rules (`ddalgak.rules` v1) |
//! | `rules.json.bak` | the previous saved version (replaced by every write) |
//! | `rules.json.tmp` | a write in progress; a crash leaves it behind, `rules.json` stays intact |
//! | `rules.json.lock` | the writers' lock (an OS file lock, released when the process ends) |
//! | `rules.draft.json` | an unsaved draft (`--draft`), based on one saved revision |
//! | `history/<UTC time>-r<revision>.json` | a snapshot of every applied change, the last 50 kept |
//! | `rules.corrupt-<UTC time>.json` | an unusable file that a restore replaced, kept as it was |
//!
//! - **Reading never writes.** A missing file is one empty profile; a file that is not JSON, not
//!   a `ddalgak.rules` document or fails the schema outside its rules is not loaded
//!   (`rules_invalid`) and never rewritten automatically; a newer `schema_version` is read-only.
//! - **Every write** holds the lock, re-reads the file, checks the revision the caller read
//!   (`rule_conflict` otherwise), copies the current file to `rules.json.bak`, writes the new
//!   text to `rules.json.tmp`, flushes it to disk and renames it over `rules.json` (atomic on
//!   NTFS), then writes the history snapshot. A crash at any point leaves either the old or the
//!   new file, never a torn one.
//! - **Migrations** are pure `vN → vN+1` functions; the first write of a migrated document keeps
//!   the old file as `.bak` and as a history snapshot (origin `migration`). Version 1 is the
//!   first version, so the shipped chain is empty; the mechanism is tested with a test chain.

use crate::model::{Document, RuleEntry, SCHEMA, SCHEMA_VERSION};
use crate::paths::Paths;
use crate::validate::{self, Context, Report};
use serde_json::{Value, json};
use std::fs::File;
use std::hash::{Hash, Hasher};
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

/// Snapshots kept in `history/` (§10).
pub const HISTORY_KEEP: usize = 50;
/// How long a writer waits for another writer before giving up with `busy`.
pub const LOCK_TIMEOUT: Duration = Duration::from_secs(10);

/// One migration step `vN → vN+1` of the raw document.
pub type Migration = fn(Value) -> Result<Value, String>;

/// The shipped migration chain: empty, `schema_version` 1 is the first version.
pub const MIGRATIONS: &[Migration] = &[];

/// The files of one rules store.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Files {
    pub rules: PathBuf,
}

fn sibling(p: &Path, suffix: &str) -> PathBuf {
    let mut s = p.as_os_str().to_owned();
    s.push(suffix);
    PathBuf::from(s)
}

impl Files {
    pub fn new(paths: &Paths) -> Files {
        Files {
            rules: paths.rules(),
        }
    }

    pub fn dir(&self) -> PathBuf {
        self.rules
            .parent()
            .map_or_else(|| PathBuf::from("."), Path::to_path_buf)
    }

    pub fn backup(&self) -> PathBuf {
        sibling(&self.rules, ".bak")
    }

    pub fn tmp(&self) -> PathBuf {
        sibling(&self.rules, ".tmp")
    }

    pub fn lock_file(&self) -> PathBuf {
        sibling(&self.rules, ".lock")
    }

    pub fn draft(&self) -> PathBuf {
        self.dir().join("rules.draft.json")
    }

    pub fn history(&self) -> PathBuf {
        self.dir().join("history")
    }
}

/// What the file looked like.
#[derive(Clone, PartialEq, Eq, Debug)]
pub enum FileState {
    Missing,
    Loaded,
    /// Not loaded: why.
    Invalid(String),
    /// Written by a newer Ddalgak; not loaded, never rewritten.
    NewerVersion(u64),
}

impl FileState {
    pub fn as_str(&self) -> &'static str {
        match self {
            FileState::Missing => "missing",
            FileState::Loaded => "loaded",
            FileState::Invalid(_) => "invalid",
            FileState::NewerVersion(_) => "newer_version",
        }
    }

    /// Whether the engine may run with this file and writers may change it.
    pub fn usable(&self) -> bool {
        matches!(self, FileState::Missing | FileState::Loaded)
    }

    pub fn problem(&self) -> Option<String> {
        match self {
            FileState::Invalid(why) => Some(why.clone()),
            FileState::NewerVersion(v) => {
                Some(format!("schema_version {v} is newer than this Ddalgak"))
            }
            _ => None,
        }
    }
}

/// A read and checked rules document.
#[derive(Clone, Debug)]
pub struct Loaded {
    pub state: FileState,
    /// The document (an empty one for a missing file; `None` when not loaded).
    pub doc: Option<Document>,
    /// The JSON that was checked (after migration).
    pub value: Option<Value>,
    pub report: Report,
    /// The version the file had before it was migrated in memory.
    pub migrated_from: Option<u64>,
    /// A hash of the bytes read (`0` for a missing file), to notice real changes.
    pub hash: u64,
    /// The bytes' `revision`, also when the file is not loaded.
    pub revision: Option<u64>,
}

impl Loaded {
    pub fn revision(&self) -> u64 {
        self.doc
            .as_ref()
            .map_or_else(|| self.revision.unwrap_or(0), Document::revision)
    }
}

fn hash(bytes: &[u8]) -> u64 {
    let mut h = std::collections::hash_map::DefaultHasher::new();
    bytes.hash(&mut h);
    h.finish()
}

/// Reads `files.rules` and checks it.
pub fn read(files: &Files, ctx: &Context) -> Loaded {
    read_path(&files.rules, ctx, MIGRATIONS)
}

/// Reads a rules document from `path` with a migration chain.
pub fn read_path(path: &Path, ctx: &Context, chain: &[Migration]) -> Loaded {
    match std::fs::read(path) {
        Ok(bytes) => interpret(&bytes, ctx, chain),
        Err(e) if e.kind() == io::ErrorKind::NotFound => missing(),
        Err(e) => not_loaded(
            FileState::Invalid(format!("cannot read the file: {e}")),
            0,
            None,
        ),
    }
}

/// A missing file: one empty profile `default` with mode `base` (§2).
fn missing() -> Loaded {
    let doc = Document::empty();
    Loaded {
        state: FileState::Missing,
        value: Some(doc.to_value()),
        doc: Some(doc),
        report: Report::default(),
        migrated_from: None,
        hash: 0,
        revision: None,
    }
}

fn not_loaded(state: FileState, hash: u64, revision: Option<u64>) -> Loaded {
    Loaded {
        state,
        doc: None,
        value: None,
        report: Report::default(),
        migrated_from: None,
        hash,
        revision,
    }
}

/// Interprets the bytes of a rules file; `chain[i]` migrates version
/// `SCHEMA_VERSION - chain.len() + i` to the next.
pub fn interpret(bytes: &[u8], ctx: &Context, chain: &[Migration]) -> Loaded {
    let h = hash(bytes);
    let invalid = |why: String, rev: Option<u64>| not_loaded(FileState::Invalid(why), h, rev);
    let mut value: Value = match serde_json::from_slice(bytes) {
        Ok(v) => v,
        Err(e) => return invalid(format!("not valid JSON: {e}"), None),
    };
    let revision = value.get("revision").and_then(Value::as_u64);
    if !value.is_object() {
        return invalid("the document is not a JSON object".into(), None);
    }
    if value.get("schema").and_then(Value::as_str) != Some(SCHEMA) {
        return invalid(format!("`schema` is not `{SCHEMA}`"), revision);
    }
    let Some(version) = value.get("schema_version").and_then(Value::as_u64) else {
        return invalid(
            "`schema_version` is missing or not a number".into(),
            revision,
        );
    };
    if version > SCHEMA_VERSION {
        return not_loaded(FileState::NewerVersion(version), h, revision);
    }
    let oldest = SCHEMA_VERSION.saturating_sub(chain.len() as u64);
    if version < oldest {
        return invalid(
            format!("schema_version {version} is not supported"),
            revision,
        );
    }
    let mut migrated_from = None;
    if version < SCHEMA_VERSION {
        let skip = usize::try_from(version - oldest).unwrap_or(usize::MAX);
        for (i, step) in chain.iter().enumerate().skip(skip) {
            match step(value) {
                Ok(v) => value = v,
                Err(e) => {
                    return invalid(
                        format!("migrating from schema_version {}: {e}", oldest + i as u64),
                        revision,
                    );
                }
            }
        }
        migrated_from = Some(version);
    }
    let report = validate::check(&value, ctx);
    if !report.usable() {
        let why = report
            .file
            .first()
            .map_or_else(String::new, |p| p.message.clone());
        let mut l = invalid(why, revision);
        l.report = report;
        l.value = Some(value);
        return l;
    }
    let mut doc: Document = match serde_json::from_value(value.clone()) {
        Ok(d) => d,
        Err(e) => {
            return invalid(
                format!("the document does not fit the rules types: {e}"),
                revision,
            );
        }
    };
    // A rule that fails the schema is kept exactly as written.
    for &(pi, ri) in &report.schema_invalid {
        if let Some(raw) = value["profiles"][pi]["rules"].get(ri).cloned()
            && let Some(e) = doc.profiles.get_mut(pi).and_then(|p| p.rules.get_mut(ri))
        {
            *e = RuleEntry::Raw(raw);
        }
    }
    Loaded {
        state: FileState::Loaded,
        doc: Some(doc),
        value: Some(value),
        report,
        migrated_from,
        hash: h,
        revision,
    }
}

/// Why a store operation failed.
#[derive(Debug)]
pub enum StoreError {
    /// The OS refused access (read-only file or folder).
    PermissionDenied(String),
    /// Another writer holds the lock.
    Busy(String),
    /// The file is not usable (`rules_invalid`); it is never rewritten automatically.
    Invalid(String),
    /// Written by a newer Ddalgak (`rules_from_newer_version`).
    Newer(u64),
    /// The revision changed since the caller read it (`rule_conflict`).
    Conflict {
        expected: u64,
        found: u64,
    },
    /// Nothing to restore or apply.
    NotFound(String),
    Io(String),
}

impl std::fmt::Display for StoreError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            StoreError::PermissionDenied(m)
            | StoreError::Busy(m)
            | StoreError::Invalid(m)
            | StoreError::NotFound(m)
            | StoreError::Io(m) => f.write_str(m),
            StoreError::Newer(v) => write!(
                f,
                "rules.json has schema_version {v}, written by a newer Ddalgak; it is read-only"
            ),
            StoreError::Conflict { expected, found } => write!(
                f,
                "the rules changed since they were read (revision {expected}, now {found}); nothing was written"
            ),
        }
    }
}

fn io_err(what: &str, path: &Path, e: &io::Error) -> StoreError {
    let m = format!("{what} {}: {e}", path.display());
    if e.kind() == io::ErrorKind::PermissionDenied {
        StoreError::PermissionDenied(m)
    } else {
        StoreError::Io(m)
    }
}

/// The writers' lock: an exclusive OS lock on `rules.json.lock`, held until dropped. The OS
/// releases it when the process ends, so a crashed writer never blocks the next one.
#[derive(Debug)]
pub struct Lock {
    _file: File,
}

/// Takes the writers' lock, waiting up to `timeout` for another writer.
pub fn lock(files: &Files, timeout: Duration) -> Result<Lock, StoreError> {
    let dir = files.dir();
    std::fs::create_dir_all(&dir).map_err(|e| io_err("cannot create", &dir, &e))?;
    let path = files.lock_file();
    let file = File::options()
        .create(true)
        .truncate(false)
        .write(true)
        .open(&path)
        .map_err(|e| io_err("cannot open the lock file", &path, &e))?;
    let until = Instant::now() + timeout;
    loop {
        match file.try_lock() {
            Ok(()) => return Ok(Lock { _file: file }),
            Err(std::fs::TryLockError::WouldBlock) => {
                if Instant::now() >= until {
                    return Err(StoreError::Busy(format!(
                        "another Ddalgak is changing the rules (waited {} ms for {})",
                        timeout.as_millis(),
                        path.display()
                    )));
                }
                std::thread::sleep(Duration::from_millis(15));
            }
            Err(std::fs::TryLockError::Error(e)) => {
                return Err(io_err("cannot lock", &path, &e));
            }
        }
    }
}

/// The lock timeout: [`LOCK_TIMEOUT`], or `DDALGAK_TEST_LOCK_TIMEOUT_MS` in debug builds (tests).
pub fn lock_timeout() -> Duration {
    #[cfg(debug_assertions)]
    if let Some(ms) = std::env::var("DDALGAK_TEST_LOCK_TIMEOUT_MS")
        .ok()
        .and_then(|v| v.parse().ok())
    {
        return Duration::from_millis(ms);
    }
    LOCK_TIMEOUT
}

/// Debug-build test hook (`DDALGAK_TEST_CRASH_IN_SAVE=<marker file>`): stop in the middle of
/// writing `rules.json.tmp`, create the marker and wait to be killed. Compiled out of release
/// builds.
#[cfg(debug_assertions)]
fn crash_point(path: &Path, f: &mut File, text: &[u8]) -> io::Result<()> {
    let Some(marker) = std::env::var_os("DDALGAK_TEST_CRASH_IN_SAVE") else {
        return Ok(());
    };
    if path.file_name().is_none_or(|n| n != "rules.json") {
        return Ok(());
    }
    f.write_all(&text[..text.len() / 2])?;
    f.sync_all()?;
    std::fs::write(&marker, b"in the middle of a save\n")?;
    std::thread::sleep(Duration::from_secs(60));
    // Never finish this save, even when nobody killed the process.
    std::process::exit(70);
}

/// Writes `text` to `path` atomically: `<path>.tmp`, flush to disk, rename over `path`.
pub fn write_atomic(path: &Path, text: &[u8]) -> Result<(), StoreError> {
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir).map_err(|e| io_err("cannot create", dir, &e))?;
    }
    if let Ok(m) = std::fs::metadata(path)
        && m.permissions().readonly()
    {
        return Err(StoreError::PermissionDenied(format!(
            "{} is read-only; nothing was written",
            path.display()
        )));
    }
    let tmp = sibling(path, ".tmp");
    let written = (|| -> io::Result<()> {
        let mut f = File::create(&tmp)?;
        #[cfg(debug_assertions)]
        crash_point(path, &mut f, text)?;
        f.write_all(text)?;
        f.sync_all()?;
        Ok(())
    })();
    if let Err(e) = written {
        let _ = std::fs::remove_file(&tmp);
        return Err(io_err("cannot write", &tmp, &e));
    }
    // Another program (an editor, a virus scanner) may hold the file open for a moment.
    let until = Instant::now() + Duration::from_millis(1500);
    loop {
        match std::fs::rename(&tmp, path) {
            Ok(()) => return Ok(()),
            Err(e) if e.kind() == io::ErrorKind::PermissionDenied && Instant::now() < until => {
                std::thread::sleep(Duration::from_millis(50));
            }
            Err(e) => {
                let _ = std::fs::remove_file(&tmp);
                return Err(io_err("cannot replace", path, &e));
            }
        }
    }
}

/// Where a change came from (§12): `cli`, `gui`, `restore`, `migration`, `backup`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Origin {
    pub origin: &'static str,
    /// The command (`rule.add`) for the history.
    pub command: String,
}

/// What a write did.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Saved {
    pub revision: u64,
    pub backup: Option<PathBuf>,
    pub snapshot: Option<PathBuf>,
}

/// Saves `doc` as the next revision of the file. The caller holds `lock` and built `doc` from the
/// file read under that lock; `expected` is the revision it started from.
pub fn save(
    files: &Files,
    _lock: &Lock,
    doc: &mut Document,
    expected: u64,
    origin: &Origin,
) -> Result<Saved, StoreError> {
    let current = std::fs::read(&files.rules);
    let current_bytes = match current {
        Ok(b) => Some(b),
        Err(e) if e.kind() == io::ErrorKind::NotFound => None,
        Err(e) => return Err(io_err("cannot read", &files.rules, &e)),
    };
    let ctx = Context::default();
    let now = current_bytes
        .as_deref()
        .map_or_else(missing, |b| interpret(b, &ctx, MIGRATIONS));
    match &now.state {
        FileState::NewerVersion(v) => return Err(StoreError::Newer(*v)),
        FileState::Invalid(why) if origin.origin != "restore" => {
            return Err(StoreError::Invalid(why.clone()));
        }
        _ => {}
    }
    let found = now.revision();
    if now.state.usable() && found != expected {
        return Err(StoreError::Conflict { expected, found });
    }
    if let Some(from) = now.migrated_from
        && let Some(b) = &current_bytes
    {
        // The first write of a migrated file keeps the old version in the history too.
        let _ = snapshot(
            files,
            b,
            found,
            &format!("v{from}"),
            "migration",
            &origin.command,
        );
    }
    let revision = found.max(expected) + 1;
    doc.revision = Some(revision);
    let text = doc.to_text();
    let mut backup = None;
    if let Some(b) = &current_bytes {
        if now.state.usable() {
            write_atomic(&files.backup(), b)?;
            backup = Some(files.backup());
        } else {
            // A restore over an unusable file: keep that file as it was, keep the backup.
            let corrupt = files
                .dir()
                .join(format!("rules.corrupt-{}.json", compact_time()));
            std::fs::write(&corrupt, b).map_err(|e| io_err("cannot keep", &corrupt, &e))?;
            backup = Some(corrupt);
        }
    }
    write_atomic(&files.rules, text.as_bytes())?;
    let snapshot = snapshot(
        files,
        text.as_bytes(),
        revision,
        "",
        origin.origin,
        &origin.command,
    )
    .ok();
    Ok(Saved {
        revision,
        backup,
        snapshot,
    })
}

/// `ddalgak rules backup`: a history snapshot of the saved file as it is (origin `backup`).
/// `None` when nothing was saved yet.
pub fn backup_now(files: &Files, _lock: &Lock) -> Result<Option<(PathBuf, u64)>, StoreError> {
    let bytes = match std::fs::read(&files.rules) {
        Ok(b) => b,
        Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(None),
        Err(e) => return Err(io_err("cannot read", &files.rules, &e)),
    };
    let l = interpret(&bytes, &Context::default(), MIGRATIONS);
    match l.state {
        FileState::NewerVersion(v) => return Err(StoreError::Newer(v)),
        FileState::Invalid(why) => return Err(StoreError::Invalid(why)),
        _ => {}
    }
    let revision = l.revision();
    let path = snapshot(files, &bytes, revision, "backup", "backup", "rules.backup")?;
    Ok(Some((path, revision)))
}

/// `20261104T090000Z`
pub fn compact_time() -> String {
    crate::time::now_rfc3339().replace(['-', ':'], "")
}

/// Writes a history snapshot of `bytes` (a rules document) and prunes the history.
fn snapshot(
    files: &Files,
    bytes: &[u8],
    revision: u64,
    tag: &str,
    origin: &str,
    command: &str,
) -> Result<PathBuf, StoreError> {
    let dir = files.history();
    std::fs::create_dir_all(&dir).map_err(|e| io_err("cannot create", &dir, &e))?;
    let mut doc: Value = serde_json::from_slice(bytes).unwrap_or(Value::Null);
    if let Some(o) = doc.as_object_mut() {
        o.insert(
            "snapshot".into(),
            json!({ "at": crate::time::now_rfc3339(), "origin": origin, "command": command }),
        );
    }
    let name = if tag.is_empty() {
        format!("{}-r{revision}.json", compact_time())
    } else {
        format!("{}-r{revision}-{tag}.json", compact_time())
    };
    let path = dir.join(name);
    let text = serde_json::to_string_pretty(&doc).unwrap_or_default();
    write_atomic(&path, format!("{text}\n").as_bytes())?;
    prune(&dir);
    Ok(path)
}

/// One history entry.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Snapshot {
    pub name: String,
    pub path: PathBuf,
    pub revision: Option<u64>,
    pub at: Option<String>,
    pub origin: Option<String>,
    pub command: Option<String>,
}

fn sort_key(name: &str) -> (String, u64) {
    let time = name.split('-').next().unwrap_or_default().to_string();
    let rev = name
        .split("-r")
        .nth(1)
        .and_then(|r| r.split(['-', '.']).next())
        .and_then(|r| r.parse().ok())
        .unwrap_or(0);
    (time, rev)
}

/// The snapshots, newest first.
pub fn history(files: &Files) -> Vec<Snapshot> {
    let mut out: Vec<Snapshot> = std::fs::read_dir(files.history())
        .into_iter()
        .flatten()
        .flatten()
        .filter_map(|e| {
            let path = e.path();
            let name = path.file_name()?.to_string_lossy().to_string();
            if !name.ends_with(".json") || name.ends_with(".tmp") {
                return None;
            }
            let v: Value = std::fs::read(&path)
                .ok()
                .and_then(|b| serde_json::from_slice(&b).ok())
                .unwrap_or(Value::Null);
            let s = &v["snapshot"];
            Some(Snapshot {
                name,
                revision: v["revision"].as_u64(),
                at: s["at"].as_str().map(str::to_string),
                origin: s["origin"].as_str().map(str::to_string),
                command: s["command"].as_str().map(str::to_string),
                path,
            })
        })
        .collect();
    out.sort_by_key(|s| std::cmp::Reverse(sort_key(&s.name)));
    out
}

fn prune(dir: &Path) {
    let files = Files {
        rules: dir.parent().unwrap_or(dir).join("rules.json"),
    };
    for old in history(&files).into_iter().skip(HISTORY_KEEP) {
        let _ = std::fs::remove_file(old.path);
    }
}

/// What a restore brings back.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Source {
    /// `rules.json.bak`.
    Backup,
    /// A file name in `history/`.
    Snapshot(String),
}

/// Reads what `source` would restore, checked like a rules file (the `snapshot` block removed).
pub fn restore_source(files: &Files, source: &Source) -> Result<(PathBuf, Document), StoreError> {
    let path = match source {
        Source::Backup => files.backup(),
        Source::Snapshot(name) => {
            if name.contains(['/', '\\']) || name.contains("..") {
                return Err(StoreError::NotFound(format!(
                    "`{name}` is not a snapshot name (see `ddalgak rules history`)"
                )));
            }
            files.history().join(name)
        }
    };
    let bytes = std::fs::read(&path).map_err(|e| {
        if e.kind() == io::ErrorKind::NotFound {
            StoreError::NotFound(format!("{} does not exist", path.display()))
        } else {
            io_err("cannot read", &path, &e)
        }
    })?;
    let mut v: Value = serde_json::from_slice(&bytes)
        .map_err(|e| StoreError::Invalid(format!("{}: not valid JSON: {e}", path.display())))?;
    if let Some(o) = v.as_object_mut() {
        o.remove("snapshot");
    }
    let text = serde_json::to_vec(&v).unwrap_or_default();
    let l = interpret(&text, &Context::default(), MIGRATIONS);
    match l.state {
        FileState::Loaded => Ok((path, l.doc.expect("loaded"))),
        FileState::NewerVersion(v) => Err(StoreError::Newer(v)),
        other => Err(StoreError::Invalid(format!(
            "{} is not a usable rules document: {}",
            path.display(),
            other.problem().unwrap_or_default()
        ))),
    }
}

/// An unsaved draft (`rules.draft.json`): a whole document based on one saved revision.
#[derive(Clone, Debug)]
pub struct Draft {
    pub doc: Document,
    /// The saved revision the draft started from (its `revision`).
    pub base: u64,
}

/// Reads the draft, if there is one.
pub fn read_draft(files: &Files) -> Result<Option<Draft>, StoreError> {
    let path = files.draft();
    match std::fs::read(&path) {
        Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(None),
        Err(e) => Err(io_err("cannot read", &path, &e)),
        Ok(bytes) => {
            let l = interpret(&bytes, &Context::default(), MIGRATIONS);
            match l.state {
                FileState::Loaded => {
                    let doc = l.doc.expect("loaded");
                    Ok(Some(Draft {
                        base: doc.revision(),
                        doc,
                    }))
                }
                FileState::NewerVersion(v) => Err(StoreError::Newer(v)),
                other => Err(StoreError::Invalid(format!(
                    "the draft {} is not usable: {}",
                    path.display(),
                    other.problem().unwrap_or_default()
                ))),
            }
        }
    }
}

/// Writes the draft (keeps its base revision).
pub fn write_draft(files: &Files, _lock: &Lock, draft: &Draft) -> Result<(), StoreError> {
    let mut doc = draft.doc.clone();
    doc.revision = Some(draft.base);
    write_atomic(&files.draft(), doc.to_text().as_bytes())
}

/// Removes the draft; `false` when there was none.
pub fn remove_draft(files: &Files, _lock: &Lock) -> Result<bool, StoreError> {
    match std::fs::remove_file(files.draft()) {
        Ok(()) => Ok(true),
        Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(false),
        Err(e) => Err(io_err("cannot remove", &files.draft(), &e)),
    }
}

/// Whether an interrupted save left `rules.json.tmp` behind (`rules.json` itself is intact).
pub fn interrupted_save(files: &Files) -> bool {
    files.tmp().exists()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{Action, EngineOp, EngineStep, Rule, Trigger};
    use serde_json::Map;

    fn sandbox(tag: &str) -> Files {
        let root = std::env::temp_dir().join(format!(
            "ddalgak-store-{tag}-{}-{:?}",
            std::process::id(),
            std::thread::current().id()
        ));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(&root).unwrap();
        Files {
            rules: root.join("rules.json"),
        }
    }

    fn cli(cmd: &str) -> Origin {
        Origin {
            origin: "cli",
            command: cmd.into(),
        }
    }

    fn engine_rule(id: &str, keys: &str) -> RuleEntry {
        RuleEntry::Typed(Box::new(Rule::new(
            id.into(),
            id.into(),
            Trigger::keys(keys),
            vec![Action::Engine(EngineStep {
                op: EngineOp::Pause,
                extra: Map::new(),
            })],
        )))
    }

    #[test]
    fn missing_file_save_backup_history_and_revisions() {
        let f = sandbox("save");
        let l = read(&f, &Context::default());
        assert_eq!(l.state, FileState::Missing);
        let mut doc = l.doc.unwrap();
        doc.profiles[0].rules.push(engine_rule("ra", "F13"));
        let lock = lock(&f, LOCK_TIMEOUT).unwrap();
        let s = save(&f, &lock, &mut doc, 0, &cli("rule.add")).unwrap();
        assert_eq!(s.revision, 1);
        assert_eq!(s.backup, None, "nothing to back up the first time");
        doc.profiles[0].rules.push(engine_rule("rb", "F14"));
        let s = save(&f, &lock, &mut doc, 1, &cli("rule.add")).unwrap();
        assert_eq!(s.revision, 2);
        let bak = interpret(
            &std::fs::read(f.backup()).unwrap(),
            &Context::default(),
            &[],
        );
        assert_eq!(bak.revision(), 1, "the backup is the previous version");
        let h = history(&f);
        assert_eq!(h.len(), 2);
        assert_eq!(h[0].revision, Some(2), "newest first");
        assert_eq!(h[0].origin.as_deref(), Some("cli"));
        assert!(!f.tmp().exists());
        let now = read(&f, &Context::default());
        assert_eq!(now.state, FileState::Loaded);
        assert_eq!(now.doc.unwrap().profiles[0].rules.len(), 2);
        // A writer that read revision 1 cannot overwrite revision 2.
        let err = save(&f, &lock, &mut doc, 1, &cli("rule.add")).unwrap_err();
        assert!(
            matches!(
                err,
                StoreError::Conflict {
                    expected: 1,
                    found: 2
                }
            ),
            "{err}"
        );
    }

    #[test]
    fn invalid_and_newer_files_are_never_rewritten() {
        let f = sandbox("invalid");
        std::fs::write(&f.rules, b"{ broken").unwrap();
        let l = read(&f, &Context::default());
        assert!(matches!(l.state, FileState::Invalid(_)));
        let lock = lock(&f, LOCK_TIMEOUT).unwrap();
        let mut doc = Document::empty();
        assert!(matches!(
            save(&f, &lock, &mut doc, 0, &cli("rule.add")),
            Err(StoreError::Invalid(_))
        ));
        assert_eq!(std::fs::read(&f.rules).unwrap(), b"{ broken", "preserved");
        std::fs::write(
            &f.rules,
            br#"{"schema":"ddalgak.rules","schema_version":9}"#,
        )
        .unwrap();
        assert_eq!(
            read(&f, &Context::default()).state,
            FileState::NewerVersion(9)
        );
        assert!(matches!(
            save(&f, &lock, &mut doc, 0, &cli("rule.add")),
            Err(StoreError::Newer(9))
        ));
    }

    #[test]
    fn a_restore_keeps_the_unusable_file_and_the_backup() {
        let f = sandbox("restore");
        let lock = lock(&f, LOCK_TIMEOUT).unwrap();
        let mut doc = Document::empty();
        doc.profiles[0].rules.push(engine_rule("ra", "F13"));
        save(&f, &lock, &mut doc, 0, &cli("rule.add")).unwrap();
        doc.profiles[0].rules.push(engine_rule("rb", "F14"));
        save(&f, &lock, &mut doc, 1, &cli("rule.add")).unwrap();
        std::fs::write(&f.rules, b"garbage").unwrap();
        let (_, mut back) = restore_source(&f, &Source::Backup).unwrap();
        assert_eq!(back.revision(), 1);
        let rev = back.revision();
        let s = save(
            &f,
            &lock,
            &mut back,
            rev,
            &Origin {
                origin: "restore",
                command: "rules.restore".into(),
            },
        )
        .unwrap();
        assert_eq!(s.revision, 2);
        let kept = s.backup.unwrap();
        assert_eq!(std::fs::read(&kept).unwrap(), b"garbage");
        assert!(
            kept.file_name()
                .unwrap()
                .to_string_lossy()
                .starts_with("rules.corrupt-")
        );
        assert_eq!(
            read(&f, &Context::default()).doc.unwrap().profiles[0]
                .rules
                .len(),
            1
        );
        // Snapshots restore too; their `snapshot` block is dropped.
        let name = history(&f).last().unwrap().name.clone();
        let (_, snap) = restore_source(&f, &Source::Snapshot(name)).unwrap();
        assert!(!snap.extra.contains_key("snapshot"));
        assert!(matches!(
            restore_source(&f, &Source::Snapshot("../rules.json".into())),
            Err(StoreError::NotFound(_))
        ));
    }

    #[test]
    fn the_history_keeps_the_last_fifty() {
        let f = sandbox("prune");
        let lock = lock(&f, LOCK_TIMEOUT).unwrap();
        let mut doc = Document::empty();
        for i in 0..55u64 {
            save(&f, &lock, &mut doc, i, &cli("rule.edit")).unwrap();
        }
        let h = history(&f);
        assert_eq!(h.len(), HISTORY_KEEP);
        assert_eq!(h[0].revision, Some(55));
        assert_eq!(h.last().unwrap().revision, Some(6));
    }

    #[test]
    fn the_lock_is_exclusive_and_released_on_drop() {
        let f = sandbox("lock");
        let held = lock(&f, LOCK_TIMEOUT).unwrap();
        let t = Instant::now();
        assert!(matches!(
            lock(&f, Duration::from_millis(120)),
            Err(StoreError::Busy(_))
        ));
        assert!(t.elapsed() >= Duration::from_millis(100));
        drop(held);
        assert!(lock(&f, Duration::from_millis(500)).is_ok());
    }

    #[test]
    fn migrations_run_in_memory_and_the_first_write_keeps_the_old_file() {
        fn v0_to_v1(mut v: Value) -> Result<Value, String> {
            // A pretend v0 stored `rules` at the top level.
            let rules = v["rules"].take();
            Ok(json!({
                "schema": "ddalgak.rules", "schema_version": 1, "hotkey_contract": 1,
                "revision": v["revision"], "active_profile": "default",
                "profiles": [ { "id": "default", "title": "Default", "default_mode": "base",
                    "modes": [ { "id": "base", "title": "Base" } ], "rules": rules } ]
            }))
        }
        let chain: &[Migration] = &[v0_to_v1];
        let old = json!({ "schema": "ddalgak.rules", "schema_version": 0, "revision": 4,
            "rules": [ { "id": "ra", "title": "A", "enabled": true, "trigger": { "keys": "F13" },
                         "steps": [ { "type": "engine", "op": "pause" } ] } ] });
        let l = interpret(old.to_string().as_bytes(), &Context::default(), chain);
        assert_eq!(l.state, FileState::Loaded);
        assert_eq!(l.migrated_from, Some(0));
        assert_eq!(l.doc.as_ref().unwrap().profiles[0].rules.len(), 1);
        // Without the chain (this version) the old file is not supported.
        let l2 = interpret(old.to_string().as_bytes(), &Context::default(), &[]);
        assert!(matches!(l2.state, FileState::Invalid(_)));
        // A broken migration step is reported, not half-applied.
        fn broken(_: Value) -> Result<Value, String> {
            Err("cannot convert".into())
        }
        let l3 = interpret(old.to_string().as_bytes(), &Context::default(), &[broken]);
        assert!(matches!(&l3.state, FileState::Invalid(w) if w.contains("cannot convert")));
    }

    #[test]
    fn a_read_only_file_is_refused_and_left_alone() {
        let f = sandbox("readonly");
        let lock = lock(&f, LOCK_TIMEOUT).unwrap();
        let mut doc = Document::empty();
        save(&f, &lock, &mut doc, 0, &cli("rule.add")).unwrap();
        let before = std::fs::read(&f.rules).unwrap();
        let mut p = std::fs::metadata(&f.rules).unwrap().permissions();
        p.set_readonly(true);
        std::fs::set_permissions(&f.rules, p.clone()).unwrap();
        let err = save(&f, &lock, &mut doc, 1, &cli("rule.add")).unwrap_err();
        assert!(matches!(err, StoreError::PermissionDenied(_)), "{err}");
        assert_eq!(std::fs::read(&f.rules).unwrap(), before);
        assert!(!f.tmp().exists());
        #[allow(clippy::permissions_set_readonly_false)]
        p.set_readonly(false);
        std::fs::set_permissions(&f.rules, p).unwrap();
    }

    #[test]
    fn drafts_keep_their_base_revision() {
        let f = sandbox("draft");
        let lock = lock(&f, LOCK_TIMEOUT).unwrap();
        assert!(read_draft(&f).unwrap().is_none());
        let mut doc = Document::empty();
        save(&f, &lock, &mut doc, 0, &cli("rule.add")).unwrap();
        let mut d = Draft {
            doc: doc.clone(),
            base: 1,
        };
        d.doc.profiles[0].rules.push(engine_rule("ra", "F13"));
        write_draft(&f, &lock, &d).unwrap();
        let back = read_draft(&f).unwrap().unwrap();
        assert_eq!(back.base, 1);
        assert_eq!(back.doc.profiles[0].rules.len(), 1);
        assert!(remove_draft(&f, &lock).unwrap());
        assert!(!remove_draft(&f, &lock).unwrap());
    }
}
