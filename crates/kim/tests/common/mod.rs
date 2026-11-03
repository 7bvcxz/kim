//! Shared helpers for integration tests: a loopback HTTP server, package builders, and a CLI
//! sandbox that runs the real `kim` binary with every per-user directory redirected.
#![allow(dead_code)]

use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::collections::{BTreeMap, HashMap};
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};
use std::sync::{Arc, Mutex};

// --- tiny HTTP server ------------------------------------------------------------------------

#[derive(Clone, Default)]
pub struct Server {
    pub files: Arc<Mutex<HashMap<String, Vec<u8>>>>,
    /// Pause between 16 KiB chunks (for cancellation tests).
    pub slow: Arc<Mutex<Option<std::time::Duration>>>,
    /// Announce the full `Content-Length` but close the connection after this many body bytes
    /// (a partial download).
    pub truncate_at: Arc<Mutex<Option<usize>>>,
    /// Number of requests served.
    pub hits: Arc<Mutex<usize>>,
    pub port: u16,
}

impl Server {
    pub fn start() -> Self {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let s = Server {
            port: listener.local_addr().unwrap().port(),
            ..Default::default()
        };
        let state = s.clone();
        std::thread::spawn(move || {
            for stream in listener.incoming().flatten() {
                let state = state.clone();
                std::thread::spawn(move || state.serve(stream));
            }
        });
        s
    }

    fn serve(&self, mut s: std::net::TcpStream) {
        let mut buf = [0u8; 4096];
        let n = s.read(&mut buf).unwrap_or(0);
        *self.hits.lock().unwrap() += 1;
        let req = String::from_utf8_lossy(&buf[..n]);
        let path = req.split_whitespace().nth(1).unwrap_or("/").to_string();
        let path = path.split('?').next().unwrap_or("/").to_string();
        let body = self.files.lock().unwrap().get(&path).cloned();
        let Some(body) = body else {
            let _ = s.write_all(
                b"HTTP/1.1 404 Not Found\r\nContent-Length: 0\r\nConnection: close\r\n\r\n",
            );
            return;
        };
        let _ = write!(
            s,
            "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
            body.len()
        );
        let slow = *self.slow.lock().unwrap();
        let send = (*self.truncate_at.lock().unwrap()).map_or(body.len(), |t| t.min(body.len()));
        for chunk in body[..send].chunks(16 * 1024) {
            if s.write_all(chunk).is_err() {
                return;
            }
            if let Some(d) = slow {
                std::thread::sleep(d);
            }
        }
        let _ = s.flush();
        // Dropping the stream closes the connection, early when truncating.
    }

    pub fn put(&self, path: &str, body: Vec<u8>) -> String {
        self.files.lock().unwrap().insert(path.into(), body);
        format!("http://127.0.0.1:{}{path}", self.port)
    }
}

/// A zip with `tool.exe` (a copy of kim.exe, so `--version` works) and extra files.
pub fn package(extra: &[(&str, &[u8])]) -> Vec<u8> {
    let mut zip = zip::ZipWriter::new(std::io::Cursor::new(Vec::new()));
    let opts = zip::write::SimpleFileOptions::default();
    zip.start_file("tool.exe", opts).unwrap();
    zip.write_all(&std::fs::read(env!("CARGO_BIN_EXE_kim")).unwrap())
        .unwrap();
    for (name, data) in extra {
        zip.start_file(*name, opts).unwrap();
        zip.write_all(data).unwrap();
    }
    zip.finish().unwrap().into_inner()
}

pub fn sha(b: &[u8]) -> String {
    kim::catalog::signature::hex(&Sha256::digest(b))
}

pub fn release(version: &str, url: &str, body: &[u8], entrypoint: &str) -> Value {
    json!({
        "version": version, "channel": "stable", "published_at": "2026-11-01T00:00:00Z",
        "release_notes": format!("notes {version}"),
        "assets": [{
            "os": std::env::consts::OS, "arch": std::env::consts::ARCH,
            "url": url, "size": body.len(), "sha256": sha(body), "kind": "zip",
            "entrypoint": entrypoint
        }]
    })
}

/// A valid `kim.capabilities` manifest for a test tool.
pub fn manifest(tool: &str, version: &str, commands: &[&str]) -> Vec<u8> {
    let cmds: Vec<Value> = commands
        .iter()
        .map(|c| {
            json!({"name": c, "usage": format!("{tool} {c}"), "summary": "s", "effect": "read",
                   "network": false, "dry_run": false, "requires_yes": false,
                   "opens_window": false})
        })
        .collect();
    serde_json::to_vec_pretty(&json!({
        "schema": "kim.capabilities", "schema_version": 1, "tool": tool, "version": version,
        "cli_contract": 1, "json_schema_version": 1, "implementation": "implemented",
        "summary": "test tool", "commands": cmds
    }))
    .unwrap()
}

// --- CLI sandbox -----------------------------------------------------------------------------

/// Seed of the development signing key the debug `kim` trusts via `KIM_DEV_TRUSTED_KEY`.
pub const DEV_SEED: [u8; 32] = [11u8; 32];

/// Per-test sandbox: every per-user location of `kim` points below `root`.
pub struct Sandbox {
    pub root: PathBuf,
    pub extra_env: Vec<(String, String)>,
}

impl Sandbox {
    pub fn new(tag: &str) -> Self {
        let root = std::env::temp_dir().join(format!("kim-t-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(&root).unwrap();
        Self {
            root,
            extra_env: Vec::new(),
        }
    }

    pub fn local(&self) -> PathBuf {
        self.root.join("Local")
    }

    pub fn roaming(&self) -> PathBuf {
        self.root.join("Roaming")
    }

    pub fn cmd(&self, args: &[&str]) -> Command {
        let mut c = Command::new(env!("CARGO_BIN_EXE_kim"));
        c.args(args)
            .env("LOCALAPPDATA", self.local())
            .env("APPDATA", self.roaming())
            .env("XDG_DATA_HOME", self.root.join("xdg"))
            .env("XDG_CONFIG_HOME", self.root.join("xdgc"))
            .env("HOME", &self.root)
            .env(
                "KIM_DEV_TRUSTED_KEY",
                kim::catalog::signature::public_key_hex(&DEV_SEED),
            )
            .env_remove("KIM_NO_INPUT")
            .stdin(Stdio::null());
        for (k, v) in &self.extra_env {
            c.env(k, v);
        }
        c
    }

    pub fn run(&self, args: &[&str]) -> Output {
        self.cmd(args).output().expect("run kim")
    }

    /// Runs with `--json` already in `args`; returns (exit code, document, raw output).
    pub fn json(&self, args: &[&str]) -> (i32, Value, Output) {
        let out = self.run(args);
        let v: Value = serde_json::from_slice(&out.stdout).unwrap_or_else(|e| {
            panic!(
                "{args:?}: stdout is not one JSON document ({e}):\n{}\nstderr: {}",
                String::from_utf8_lossy(&out.stdout),
                String::from_utf8_lossy(&out.stderr)
            )
        });
        (out.status.code().unwrap_or(-1), v, out)
    }

    pub fn programs(&self) -> PathBuf {
        #[cfg(windows)]
        return self.local().join("Programs").join("mouv");
        #[cfg(not(windows))]
        return self.root.join("xdg").join("mouv").join("programs");
    }

    pub fn tool_dir(&self, id: &str) -> PathBuf {
        self.programs().join(id)
    }

    pub fn cache_dir(&self) -> PathBuf {
        #[cfg(windows)]
        return self.local().join("mouv").join("kim").join("catalog");
        #[cfg(not(windows))]
        return self
            .root
            .join("xdg")
            .join("mouv")
            .join("kim")
            .join("catalog");
    }

    pub fn settings_file(&self) -> PathBuf {
        #[cfg(windows)]
        return self
            .roaming()
            .join("mouv")
            .join("kim")
            .join("settings.json");
        #[cfg(not(windows))]
        return self
            .root
            .join("xdgc")
            .join("mouv")
            .join("kim")
            .join("settings.json");
    }

    /// Writes a test-signed catalog into the cache: `subak` (unreleased) plus `example-tool`.
    pub fn publish(&self, version: u64, releases: &[Value]) {
        let bytes = signed_catalog(version, releases);
        let dir = self.cache_dir();
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("catalog.json"), &bytes).unwrap();
        std::fs::write(
            dir.join("catalog.json.sig"),
            serde_json::to_vec(&kim::catalog::signature::sign(&bytes, &DEV_SEED)).unwrap(),
        )
        .unwrap();
    }

    /// Every entry below the sandbox; for files also size, modification time and content hash.
    /// Equal snapshots before and after a command prove it wrote nothing. (Directory times are
    /// left out: NTFS refreshes the copy in the parent's index lazily, even on reads.)
    pub fn snapshot(&self) -> BTreeMap<PathBuf, (u64, Option<std::time::SystemTime>, String)> {
        type Entries = BTreeMap<PathBuf, (u64, Option<std::time::SystemTime>, String)>;
        fn walk(d: &Path, out: &mut Entries) {
            for e in std::fs::read_dir(d).into_iter().flatten().flatten() {
                let p = e.path();
                if p.is_dir() {
                    out.insert(p.clone(), (0, None, String::new()));
                    walk(&p, out);
                } else {
                    let meta = std::fs::metadata(&p).ok();
                    out.insert(
                        p.clone(),
                        (
                            meta.as_ref().map_or(0, std::fs::Metadata::len),
                            meta.and_then(|m| m.modified().ok()),
                            std::fs::read(&p).map(|b| sha(&b)).unwrap_or_default(),
                        ),
                    );
                }
            }
        }
        let mut out = BTreeMap::new();
        walk(&self.root, &mut out);
        out
    }
}

impl Drop for Sandbox {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.root);
    }
}

/// A test catalog document (signed separately by [`Sandbox::publish`]).
pub fn signed_catalog(version: u64, releases: &[Value]) -> Vec<u8> {
    let doc = json!({
        "schema": "kim.catalog", "schema_version": 1, "catalog_version": version,
        "generated_at": "2026-11-01T00:00:00Z", "expires_at": "2099-01-01T00:00:00Z",
        "publisher": "test",
        "tools": [
            {"id": "subak", "name": "Subak", "summary": "s", "description": "d", "license": "MIT",
             "support": "in-development", "releases": []},
            {"id": "example-tool", "name": "Example", "summary": "s", "description": "d",
             "license": "MIT", "support": "stable", "releases": releases}
        ]
    });
    serde_json::to_vec_pretty(&doc).unwrap()
}

/// `file://` URL of a local path.
pub fn file_url(p: &Path) -> String {
    let abs = std::fs::canonicalize(p).unwrap();
    let s = abs.display().to_string().replace('\\', "/");
    let s = s.trim_start_matches("//?/");
    if s.starts_with('/') {
        format!("file://{s}")
    } else {
        format!("file:///{s}")
    }
}
