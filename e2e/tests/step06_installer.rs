//! E2E — Step 06: explicit installer.
//!
//! Scenario A (debug `kim`, which trusts a test key from `KIM_DEV_TRUSTED_KEY`): a test-signed
//! catalog offers a tool served over loopback HTTP. The user installs it explicitly, sees an
//! update with `update check` (nothing changes), updates explicitly, a tampered package is
//! refused, and uninstall needs `--yes` and keeps user data.
//!
//! Scenario B (release `kim`): the development-key hook is compiled out, so the same
//! test-signed cache is ignored and nothing can be installed from it.

use kim_e2e::{TempDir, bin, bin_debug};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::io::{Read, Write};
use std::path::Path;
use std::process::{Command, Output};

const SEED: [u8; 32] = [44u8; 32];

fn serve(files: Vec<(&'static str, Vec<u8>)>) -> u16 {
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let port = listener.local_addr().unwrap().port();
    std::thread::spawn(move || {
        for mut s in listener.incoming().flatten() {
            let mut buf = [0u8; 2048];
            let n = s.read(&mut buf).unwrap_or(0);
            let req = String::from_utf8_lossy(&buf[..n]).to_string();
            let path = req.split_whitespace().nth(1).unwrap_or("").to_string();
            match files.iter().find(|(p, _)| *p == path) {
                Some((_, body)) => {
                    let _ = write!(
                        s,
                        "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                        body.len()
                    );
                    let _ = s.write_all(body);
                }
                None => {
                    let _ = s.write_all(
                        b"HTTP/1.1 404 Not Found\r\nContent-Length: 0\r\nConnection: close\r\n\r\n",
                    );
                }
            }
        }
    });
    port
}

fn package(tag: &[u8]) -> Vec<u8> {
    let kim = std::fs::read(bin("kim")).unwrap();
    let mut zip = zip::ZipWriter::new(std::io::Cursor::new(Vec::new()));
    let o = zip::write::SimpleFileOptions::default();
    zip.start_file("tool.exe", o).unwrap();
    zip.write_all(&kim).unwrap();
    zip.start_file("TAG", o).unwrap();
    zip.write_all(tag).unwrap();
    zip.finish().unwrap().into_inner()
}

fn release(v: &str, url: &str, body: &[u8], sha: Option<String>) -> Value {
    json!({"version": v, "channel": "stable", "published_at": "2026-11-01T00:00:00Z", "release_notes": v,
        "assets": [{"os": std::env::consts::OS, "arch": std::env::consts::ARCH, "url": url, "size": body.len(),
        "sha256": sha.unwrap_or_else(|| kim::catalog::signature::hex(&Sha256::digest(body))), "kind": "zip", "entrypoint": "tool.exe"}]})
}

fn publish(sandbox: &Path, version: u64, releases: &[Value]) {
    let doc = json!({"schema": "kim.catalog", "schema_version": 1, "catalog_version": version,
        "generated_at": "2026-11-01T00:00:00Z", "expires_at": "2099-01-01T00:00:00Z", "publisher": "e2e",
        "tools": [{"id": "e2e-tool", "name": "E2E Tool", "summary": "s", "description": "d", "license": "MIT",
                   "support": "stable", "releases": releases}]});
    let bytes = serde_json::to_vec_pretty(&doc).unwrap();
    let sig = serde_json::to_vec(&kim::catalog::signature::sign(&bytes, &SEED)).unwrap();
    let dir = sandbox
        .join("Local")
        .join("mouv")
        .join("kim")
        .join("catalog");
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(dir.join("catalog.json"), bytes).unwrap();
    std::fs::write(dir.join("catalog.json.sig"), sig).unwrap();
}

fn run(exe: &Path, sandbox: &Path, args: &[&str]) -> (Option<i32>, Value, Output) {
    let out = Command::new(exe)
        .args(args)
        .env("LOCALAPPDATA", sandbox.join("Local"))
        .env("APPDATA", sandbox.join("Roaming"))
        .env(
            "KIM_DEV_TRUSTED_KEY",
            kim::catalog::signature::public_key_hex(&SEED),
        )
        .output()
        .unwrap();
    let v = serde_json::from_slice(&out.stdout).unwrap_or(Value::Null);
    (out.status.code(), v, out)
}

#[test]
#[ignore = "e2e: run via `cargo xtask e2e` only (AGENTS.md §4)"]
fn explicit_install_update_and_uninstall_with_integrity_checks() {
    let sb = TempDir::new("installer");
    let p = sb.path();
    let (v1, v2) = (package(b"1"), package(b"2"));
    let port = serve(vec![("/1.zip", v1.clone()), ("/2.zip", v2.clone())]);
    let url = |f: &str| format!("http://127.0.0.1:{port}/{f}");
    let kim = bin_debug("kim");
    publish(p, 100, &[release("1.0.0", &url("1.zip"), &v1, None)]);

    let (code, v, _) = run(&kim, p, &["install", "e2e-tool", "--json"]);
    assert_eq!(code, Some(0), "{v}");
    publish(
        p,
        101,
        &[
            release("1.0.0", &url("1.zip"), &v1, None),
            release("2.0.0", &url("2.zip"), &v2, None),
        ],
    );
    let (code, v, _) = run(&kim, p, &["update", "check", "--json"]);
    assert_eq!(
        (code, v["data"]["updates"][0]["available"].as_str()),
        (Some(0), Some("2.0.0"))
    );

    // A tampered package for 3.0.0: refused, 1.0.0 stays active.
    publish(
        p,
        102,
        &[
            release("1.0.0", &url("1.zip"), &v1, None),
            release("3.0.0", &url("2.zip"), &v2, Some("0".repeat(64))),
        ],
    );
    let (code, v, _) = run(&kim, p, &["update", "e2e-tool", "--json"]);
    assert_eq!(
        (code, v["error"]["code"].as_str()),
        (Some(4), Some("integrity_failed"))
    );

    publish(
        p,
        103,
        &[
            release("1.0.0", &url("1.zip"), &v1, None),
            release("2.0.0", &url("2.zip"), &v2, None),
        ],
    );
    let (code, v, _) = run(&kim, p, &["update", "e2e-tool", "--json"]);
    assert_eq!(
        (code, v["data"]["result"]["to"].as_str()),
        (Some(0), Some("2.0.0")),
        "{v}"
    );

    std::fs::create_dir_all(p.join("Roaming").join("mouv").join("e2e-tool")).unwrap();
    let (code, _, _) = run(&kim, p, &["uninstall", "e2e-tool", "--json"]);
    assert_eq!(code, Some(7), "needs --yes when not interactive");
    let (code, v, _) = run(&kim, p, &["uninstall", "e2e-tool", "--yes", "--json"]);
    assert_eq!(code, Some(0), "{v}");
    assert!(
        p.join("Roaming").join("mouv").join("e2e-tool").exists(),
        "user data kept"
    );
}

#[test]
#[ignore = "e2e: run via `cargo xtask e2e` only (AGENTS.md §4)"]
fn release_builds_ignore_the_development_key() {
    let sb = TempDir::new("release-trust");
    let p = sb.path();
    let body = package(b"x");
    publish(
        p,
        100,
        &[release(
            "1.0.0",
            "https://example.invalid/x.zip",
            &body,
            None,
        )],
    );
    let (code, v, out) = run(&bin("kim"), p, &["catalog", "list", "--json"]);
    assert_eq!(code, Some(0));
    assert_eq!(
        v["data"]["catalog"]["source"], "bundled",
        "test-signed cache must not be trusted"
    );
    assert!(String::from_utf8_lossy(&out.stdout).contains("ignoring the cached catalog"));
    let (code, v, _) = run(&bin("kim"), p, &["install", "e2e-tool", "--json"]);
    assert_eq!(
        (code, v["error"]["code"].as_str()),
        (Some(3), Some("tool_not_found"))
    );
}
