//! CLI integration tests for `kim` (Step 05: read-only catalog commands and explicit refresh).
//! Every test runs the real binary with per-user directories redirected into a temp sandbox.

use serde_json::Value;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};

struct Sandbox(PathBuf);

impl Sandbox {
    fn new(tag: &str) -> Self {
        let p = std::env::temp_dir().join(format!("kim-cli-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&p);
        std::fs::create_dir_all(&p).unwrap();
        Self(p)
    }

    fn run(&self, args: &[&str]) -> Output {
        Command::new(env!("CARGO_BIN_EXE_kim"))
            .args(args)
            .env("LOCALAPPDATA", self.0.join("Local"))
            .env("APPDATA", self.0.join("Roaming"))
            .env("XDG_DATA_HOME", self.0.join("xdg"))
            .env("HOME", &self.0)
            // Any accidental network access would go to a dead proxy and fail.
            .env("HTTPS_PROXY", "http://127.0.0.1:9")
            .env("HTTP_PROXY", "http://127.0.0.1:9")
            .env("ALL_PROXY", "http://127.0.0.1:9")
            .output()
            .expect("run kim")
    }

    fn json(&self, args: &[&str]) -> (Option<i32>, Value) {
        let out = self.run(args);
        let v: Value = serde_json::from_slice(&out.stdout).unwrap_or_else(|e| {
            panic!(
                "stdout is not JSON ({e}): {}\nstderr: {}",
                String::from_utf8_lossy(&out.stdout),
                String::from_utf8_lossy(&out.stderr)
            )
        });
        (out.status.code(), v)
    }

    fn cache_dir(&self) -> PathBuf {
        #[cfg(windows)]
        return self
            .0
            .join("Local")
            .join("mouv")
            .join("kim")
            .join("catalog");
        #[cfg(not(windows))]
        return self.0.join("xdg").join("mouv").join("kim").join("catalog");
    }

    fn entries(&self) -> usize {
        fn count(d: &Path) -> usize {
            std::fs::read_dir(d).map_or(0, |rd| {
                rd.flatten()
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

impl Drop for Sandbox {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

fn repo_catalog() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../../catalog/catalog.json")
}

fn file_url(p: &Path) -> String {
    let abs = std::fs::canonicalize(p).unwrap();
    let s = abs.display().to_string().replace('\\', "/");
    let s = s.trim_start_matches("//?/");
    if s.starts_with('/') {
        format!("file://{s}")
    } else {
        format!("file:///{s}")
    }
}

fn tool<'a>(list: &'a Value, id: &str) -> &'a Value {
    list["data"]["tools"]
        .as_array()
        .unwrap()
        .iter()
        .find(|t| t["id"] == id)
        .unwrap_or_else(|| panic!("tool {id} missing"))
}

#[test]
fn version_and_help() {
    let s = Sandbox::new("help");
    let out = s.run(&["--version"]);
    assert!(out.status.success());
    assert_eq!(
        String::from_utf8_lossy(&out.stdout).trim(),
        format!("kim {}", env!("CARGO_PKG_VERSION"))
    );
    let help = s.run(&["--help"]);
    assert!(help.status.success());
    let text = String::from_utf8_lossy(&help.stdout);
    assert!(text.contains("never installs, updates, removes or starts a tool by itself"));
    assert!(text.contains("catalog"));
    let bare = s.run(&[]);
    assert!(bare.status.success());
    assert!(String::from_utf8_lossy(&bare.stdout).contains("Usage"));
}

#[test]
fn usage_errors_exit_2_and_are_json_when_asked() {
    let s = Sandbox::new("usage");
    let out = s.run(&["install", "subak"]);
    assert_eq!(out.status.code(), Some(2));
    assert!(out.stdout.is_empty(), "errors go to stderr in human mode");
    let (code, v) = s.json(&["catalog", "frobnicate", "--json"]);
    assert_eq!(code, Some(2));
    assert_eq!(v["ok"], false);
    assert_eq!(v["error"]["code"], "usage");
}

#[test]
fn list_works_offline_from_the_bundled_catalog_and_writes_nothing() {
    let s = Sandbox::new("list");
    let out = s.run(&["catalog", "list"]);
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let text = String::from_utf8_lossy(&out.stdout);
    assert!(
        text.contains("subak") && text.contains("not released"),
        "{text}"
    );
    assert!(text.contains("Nothing was changed"));

    let (code, v) = s.json(&["catalog", "list", "--json"]);
    assert_eq!(code, Some(0));
    assert_eq!(v["ok"], true);
    assert_eq!(v["command"], "catalog.list");
    assert_eq!(v["data"]["catalog"]["source"], "bundled");
    assert_eq!(
        v["data"]["catalog"]["signed_by"]["key_id"],
        "4466fe26eca99520"
    );
    assert_eq!(tool(&v, "subak")["status"], "unreleased");
    assert_eq!(tool(&v, "kim")["status"], "installed");
    assert_eq!(
        s.entries(),
        0,
        "listing must not create any file (KIM-FR-002)"
    );
}

#[test]
fn show_known_and_unknown_tools() {
    let s = Sandbox::new("show");
    let (code, v) = s.json(&["catalog", "show", "subak", "--json"]);
    assert_eq!(code, Some(0));
    assert_eq!(v["data"]["tool"]["id"], "subak");
    assert_eq!(v["data"]["releases"].as_array().unwrap().len(), 0);
    let (code, v) = s.json(&["catalog", "show", "nope", "--json"]);
    assert_eq!(code, Some(3));
    assert_eq!(v["error"]["code"], "tool_not_found");
    let out = s.run(&["catalog", "show", "nope"]);
    assert_eq!(out.status.code(), Some(3));
    assert!(String::from_utf8_lossy(&out.stderr).contains("known tools: kim, subak"));
}

#[test]
fn verify_accepts_the_bundled_catalog_and_rejects_tampering_and_unknown_signers() {
    let s = Sandbox::new("verify");
    let catalog = repo_catalog();
    let (code, v) = s.json(&["catalog", "verify", catalog.to_str().unwrap(), "--json"]);
    assert_eq!(code, Some(0), "{v}");
    assert_eq!(v["data"]["valid"], true);

    let tampered = s.0.join("tampered.json");
    let mut bytes = std::fs::read(&catalog).unwrap();
    let pos = bytes.windows(5).position(|w| w == b"Subak").unwrap();
    bytes[pos] = b'X';
    std::fs::write(&tampered, &bytes).unwrap();
    std::fs::copy(
        catalog.with_extension("json.sig"),
        s.0.join("tampered.json.sig"),
    )
    .unwrap();
    let (code, v) = s.json(&["catalog", "verify", tampered.to_str().unwrap(), "--json"]);
    assert_eq!(code, Some(4));
    assert_eq!(v["error"]["code"], "signature_invalid");

    // A signature by a key this build does not trust.
    let rogue_sig = s.0.join("rogue.sig");
    std::fs::write(
        &rogue_sig,
        br#"{"algorithm":"ed25519","key_id":"0000000000000000","signature":"00"}"#,
    )
    .unwrap();
    let (code, v) = s.json(&[
        "catalog",
        "verify",
        catalog.to_str().unwrap(),
        "--sig",
        rogue_sig.to_str().unwrap(),
        "--json",
    ]);
    assert_eq!(code, Some(4));
    assert_eq!(v["error"]["code"], "untrusted_key");
}

#[test]
fn refresh_is_explicit_verified_and_refuses_insecure_or_missing_sources() {
    let s = Sandbox::new("refresh");
    let url = file_url(&repo_catalog());
    let (code, v) = s.json(&["catalog", "refresh", "--source", &url, "--json"]);
    assert_eq!(code, Some(0), "{v}");
    assert_eq!(v["data"]["updated"], false, "same version as bundled");
    assert!(s.cache_dir().join("catalog.json").exists());
    assert!(s.cache_dir().join("catalog.json.sig").exists());

    let (code, v) = s.json(&[
        "catalog",
        "refresh",
        "--source",
        "http://example.com/c.json",
        "--json",
    ]);
    assert_eq!(code, Some(2));
    assert_eq!(v["error"]["code"], "unsupported_source");

    let missing = format!("{url}.missing");
    let (code, v) = s.json(&["catalog", "refresh", "--source", &missing, "--json"]);
    assert_eq!(code, Some(6));
    assert_eq!(v["error"]["code"], "network_unavailable");
}

#[test]
fn a_corrupted_cache_is_ignored_with_a_warning() {
    let s = Sandbox::new("corrupt");
    let dir = s.cache_dir();
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(dir.join("catalog.json"), b"{ not json").unwrap();
    std::fs::write(dir.join("catalog.json.sig"), b"{}").unwrap();
    let out = s.run(&["catalog", "list"]);
    assert!(out.status.success());
    assert!(String::from_utf8_lossy(&out.stderr).contains("ignoring the cached catalog"));
    let (code, v) = s.json(&["catalog", "list", "--json"]);
    assert_eq!(code, Some(0));
    assert_eq!(v["data"]["catalog"]["source"], "bundled");
    assert!(
        v["warnings"][0]
            .as_str()
            .unwrap()
            .contains("ignoring the cached catalog")
    );
    let quiet = s.run(&["catalog", "list", "--quiet"]);
    assert!(quiet.stderr.is_empty(), "--quiet suppresses warnings");
}

#[test]
fn status_reports_source_and_trusted_keys() {
    let s = Sandbox::new("status");
    let (code, v) = s.json(&["catalog", "status", "--json"]);
    assert_eq!(code, Some(0));
    assert_eq!(v["data"]["catalog"]["stale"], false);
    assert_eq!(v["data"]["trusted_keys"][0]["key_id"], "4466fe26eca99520");
}
