//! CLI integration tests for explicit install / update / uninstall / settings (Step 06).
//!
//! The debug `kim` binary trusts one extra development key from `KIM_DEV_TRUSTED_KEY` (compiled
//! out of release builds), so these tests can place a test-signed catalog in the sandboxed cache.

use serde_json::{Value, json};
use std::path::PathBuf;
use std::process::{Command, Output};

mod common;
use common::{Server, package, release};

const SEED: [u8; 32] = [11u8; 32];

struct Sandbox(PathBuf);

impl Sandbox {
    fn new(tag: &str) -> Self {
        let p = std::env::temp_dir().join(format!("kim-cli-install-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&p);
        std::fs::create_dir_all(&p).unwrap();
        Self(p)
    }

    fn local(&self) -> PathBuf {
        self.0.join("Local")
    }

    fn run(&self, args: &[&str]) -> Output {
        Command::new(env!("CARGO_BIN_EXE_kim"))
            .args(args)
            .env("LOCALAPPDATA", self.local())
            .env("APPDATA", self.0.join("Roaming"))
            .env("XDG_DATA_HOME", self.0.join("xdg"))
            .env("XDG_CONFIG_HOME", self.0.join("xdgc"))
            .env("HOME", &self.0)
            .env(
                "KIM_DEV_TRUSTED_KEY",
                kim::catalog::signature::public_key_hex(&SEED),
            )
            .output()
            .expect("run kim")
    }

    fn json(&self, args: &[&str]) -> (Option<i32>, Value) {
        let out = self.run(args);
        let v = serde_json::from_slice(&out.stdout).unwrap_or_else(|e| {
            panic!(
                "not JSON ({e}): {}\nstderr: {}",
                String::from_utf8_lossy(&out.stdout),
                String::from_utf8_lossy(&out.stderr)
            )
        });
        (out.status.code(), v)
    }

    /// Writes a test-signed catalog into the cache (version above the bundled one).
    #[allow(clippy::needless_pass_by_value)] // test helper: owned `vec![...]` literals read better
    fn publish(&self, version: u64, releases: Vec<Value>) {
        let doc = json!({
            "schema": "kim.catalog", "schema_version": 1, "catalog_version": version,
            "generated_at": "2026-11-01T00:00:00Z", "expires_at": "2099-01-01T00:00:00Z",
            "publisher": "test",
            "tools": [
                {"id": "subak", "name": "Subak", "summary": "s", "description": "d", "license": "MIT", "support": "in-development", "releases": []},
                {"id": "example-tool", "name": "Example", "summary": "s", "description": "d", "license": "MIT", "support": "stable", "releases": releases}
            ]
        });
        let bytes = serde_json::to_vec_pretty(&doc).unwrap();
        let sig = serde_json::to_vec(&kim::catalog::signature::sign(&bytes, &SEED)).unwrap();
        #[cfg(windows)]
        let dir = self.local().join("mouv").join("kim").join("catalog");
        #[cfg(not(windows))]
        let dir = self.0.join("xdg").join("mouv").join("kim").join("catalog");
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("catalog.json"), bytes).unwrap();
        std::fs::write(dir.join("catalog.json.sig"), sig).unwrap();
    }

    fn tool_dir(&self) -> PathBuf {
        #[cfg(windows)]
        return self
            .local()
            .join("Programs")
            .join("mouv")
            .join("example-tool");
        #[cfg(not(windows))]
        return self
            .0
            .join("xdg")
            .join("mouv")
            .join("programs")
            .join("example-tool");
    }
}

impl Drop for Sandbox {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

#[test]
fn explicit_install_update_and_uninstall_through_the_cli() {
    let sb = Sandbox::new("flow");
    let srv = Server::start();
    let v1 = package(&[("V", b"1")]);
    let u1 = srv.put("/1.zip", v1.clone());
    sb.publish(100, vec![release("1.0.0", &u1, &v1, "tool.exe")]);

    // Dry run plans but changes nothing.
    let (code, v) = sb.json(&["install", "example-tool", "--dry-run", "--json"]);
    assert_eq!(code, Some(0), "{v}");
    assert_eq!(v["data"]["dry_run"], true);
    assert_eq!(v["data"]["plan"]["action"], "install");
    assert!(!sb.tool_dir().exists(), "dry run creates nothing");

    // Real install.
    let (code, v) = sb.json(&["install", "example-tool", "--json"]);
    assert_eq!(code, Some(0), "{v}");
    assert_eq!(v["data"]["result"]["to"], "1.0.0");
    let (_, list) = sb.json(&["catalog", "list", "--json"]);
    let tool = list["data"]["tools"]
        .as_array()
        .unwrap()
        .iter()
        .find(|t| t["id"] == "example-tool")
        .unwrap()
        .clone();
    assert_eq!(tool["status"], "installed");

    // A newer release appears: `update check` reports it and changes nothing.
    let v2 = package(&[("V", b"2")]);
    let u2 = srv.put("/2.zip", v2.clone());
    sb.publish(
        101,
        vec![
            release("1.0.0", &u1, &v1, "tool.exe"),
            release("2.0.0", &u2, &v2, "tool.exe"),
        ],
    );
    let (code, v) = sb.json(&["update", "check", "--json"]);
    assert_eq!(code, Some(0));
    assert_eq!(v["data"]["updates"][0]["available"], "2.0.0");
    let rec: Value =
        serde_json::from_slice(&std::fs::read(sb.tool_dir().join("current.json")).unwrap())
            .unwrap();
    assert_eq!(rec["version"], "1.0.0", "checking never updates");

    // Explicit update.
    let (code, v) = sb.json(&["update", "example-tool", "--json"]);
    assert_eq!(code, Some(0), "{v}");
    assert_eq!(v["data"]["result"]["from"], "1.0.0");
    assert_eq!(v["data"]["result"]["to"], "2.0.0");

    // Uninstall needs confirmation when not interactive.
    let (code, v) = sb.json(&["uninstall", "example-tool", "--json"]);
    assert_eq!(code, Some(7));
    assert_eq!(v["error"]["code"], "confirmation_required");
    assert!(sb.tool_dir().join("current.json").exists());
    let out = sb.run(&["uninstall", "example-tool"]);
    assert_eq!(
        out.status.code(),
        Some(7),
        "stdin is not a terminal in tests"
    );

    // Data dirs exist → kept by default.
    let data = sb.0.join("Roaming").join("mouv").join("example-tool");
    std::fs::create_dir_all(&data).unwrap();
    let (code, v) = sb.json(&["uninstall", "example-tool", "--yes", "--json"]);
    assert_eq!(code, Some(0), "{v}");
    assert!(!sb.tool_dir().exists());
    #[cfg(windows)]
    assert!(data.exists(), "user data is kept unless --purge-data");
    let (code, v) = sb.json(&["uninstall", "example-tool", "--yes", "--json"]);
    assert_eq!(code, Some(3));
    assert_eq!(v["error"]["code"], "not_installed");
}

#[test]
fn errors_are_explicit_and_change_nothing() {
    let sb = Sandbox::new("errors");
    sb.publish(100, vec![]);
    let (code, v) = sb.json(&["install", "subak", "--json"]);
    assert_eq!(code, Some(5), "{v}");
    assert_eq!(v["error"]["code"], "no_release");
    let (code, v) = sb.json(&["install", "kim", "--json"]);
    assert_eq!(code, Some(2));
    assert_eq!(v["error"]["code"], "usage");
    let (code, v) = sb.json(&["install", "nope", "--json"]);
    assert_eq!(code, Some(3));
    assert_eq!(v["error"]["code"], "tool_not_found");
    let (code, v) = sb.json(&["update", "example-tool", "--json"]);
    assert_eq!(code, Some(3));
    assert_eq!(v["error"]["code"], "not_installed");
    assert!(!sb.local().join("Programs").exists(), "nothing was created");
}

#[test]
fn offline_mode_blocks_every_network_operation() {
    let sb = Sandbox::new("offline");
    let srv = Server::start();
    let body = package(&[]);
    let url = srv.put("/t.zip", body.clone());
    sb.publish(100, vec![release("1.0.0", &url, &body, "tool.exe")]);
    let (code, v) = sb.json(&["settings", "set", "offline_mode", "true", "--json"]);
    assert_eq!(code, Some(0));
    assert_eq!(v["data"]["settings"]["offline_mode"], true);
    let (code, v) = sb.json(&["install", "example-tool", "--json"]);
    assert_eq!(code, Some(6));
    assert_eq!(v["error"]["code"], "offline_mode");
    let (code, v) = sb.json(&["catalog", "refresh", "--json"]);
    assert_eq!(code, Some(6));
    assert_eq!(v["error"]["code"], "offline_mode");
    // Dry runs and read-only commands still work offline.
    let (code, _) = sb.json(&["install", "example-tool", "--dry-run", "--json"]);
    assert_eq!(code, Some(0));
    let (code, v) = sb.json(&["settings", "show", "--json"]);
    assert_eq!(code, Some(0));
    assert_eq!(
        v["data"]["settings"]["check_updates_on_launch"], false,
        "launch checks are opt-in"
    );
}

#[test]
fn a_tampered_package_is_refused_with_integrity_failed() {
    let sb = Sandbox::new("tamper");
    let srv = Server::start();
    let body = package(&[]);
    let mut evil = body.clone();
    let n = evil.len();
    evil[n / 2] ^= 0xff;
    let url = srv.put("/t.zip", evil);
    sb.publish(100, vec![release("1.0.0", &url, &body, "tool.exe")]);
    let (code, v) = sb.json(&["install", "example-tool", "--json"]);
    assert_eq!(code, Some(4), "{v}");
    assert_eq!(v["error"]["code"], "integrity_failed");
    assert!(!sb.tool_dir().join("current.json").exists());
}
