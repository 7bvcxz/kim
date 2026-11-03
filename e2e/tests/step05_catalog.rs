//! E2E — Step 05: Kim tool catalog.
//!
//! Scenario: on a machine with no network (every proxy points at a closed port), a user runs the
//! release `kim` binary. The bundled, signed catalog lists Subak as a not-yet-released candidate
//! without installing or starting anything and without writing files; an explicit refresh from a
//! local mirror verifies and caches the catalog; a tampered catalog and an unknown tool fail with
//! the documented exit codes.

use kim_e2e::{TempDir, bin};
use std::path::{Path, PathBuf};
use std::process::{Command, Output};

fn kim(sandbox: &TempDir, args: &[&str]) -> Output {
    let p = sandbox.path();
    Command::new(bin("kim"))
        .args(args)
        .env("LOCALAPPDATA", p.join("Local"))
        .env("APPDATA", p.join("Roaming"))
        .env("USERPROFILE", p)
        .env("HOME", p)
        .env("XDG_DATA_HOME", p.join("xdg"))
        .env("HTTPS_PROXY", "http://127.0.0.1:9")
        .env("HTTP_PROXY", "http://127.0.0.1:9")
        .env("ALL_PROXY", "http://127.0.0.1:9")
        .output()
        .expect("run kim")
}

fn json(out: &Output) -> serde_json::Value {
    serde_json::from_slice(&out.stdout).expect("stdout is one JSON document")
}

fn repo_catalog() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../catalog/catalog.json")
}

#[test]
#[ignore = "e2e: run via `cargo xtask e2e` only (AGENTS.md §4)"]
fn offline_listing_shows_subak_as_candidate_and_changes_nothing() {
    let sandbox = TempDir::new("catalog-list");
    let out = kim(&sandbox, &["catalog", "list", "--json"]);
    assert_eq!(out.status.code(), Some(0));
    let v = json(&out);
    let subak = v["data"]["tools"]
        .as_array()
        .unwrap()
        .iter()
        .find(|t| t["id"] == "subak")
        .unwrap();
    assert_eq!(subak["status"], "unreleased");
    assert_eq!(v["data"]["catalog"]["source"], "bundled");
    assert_eq!(
        sandbox.entry_count(),
        0,
        "listing installs, starts and writes nothing"
    );
}

#[test]
#[ignore = "e2e: run via `cargo xtask e2e` only (AGENTS.md §4)"]
fn explicit_refresh_from_a_local_mirror_verifies_and_caches() {
    let sandbox = TempDir::new("catalog-refresh");
    let abs = std::fs::canonicalize(repo_catalog()).unwrap();
    let abs = abs.display().to_string().replace('\\', "/");
    let url = format!(
        "file:///{}",
        abs.trim_start_matches("//?/").trim_start_matches('/')
    );
    let out = kim(
        &sandbox,
        &["catalog", "refresh", "--source", &url, "--json"],
    );
    assert_eq!(
        out.status.code(),
        Some(0),
        "{}",
        String::from_utf8_lossy(&out.stdout)
    );
    assert!(sandbox.entry_count() > 0, "refresh writes the cache");
    let status = json(&kim(&sandbox, &["catalog", "status", "--json"]));
    assert_eq!(status["data"]["catalog"]["stale"], false);
}

#[test]
#[ignore = "e2e: run via `cargo xtask e2e` only (AGENTS.md §4)"]
fn tampered_catalogs_and_unknown_tools_fail_with_documented_codes() {
    let sandbox = TempDir::new("catalog-errors");
    let tampered = sandbox.path().join("c.json");
    let mut bytes = std::fs::read(repo_catalog()).unwrap();
    let pos = bytes.windows(5).position(|w| w == b"Subak").unwrap();
    bytes[pos] = b'Z';
    std::fs::write(&tampered, bytes).unwrap();
    std::fs::copy(
        repo_catalog().with_extension("json.sig"),
        sandbox.path().join("c.json.sig"),
    )
    .unwrap();
    let out = kim(
        &sandbox,
        &["catalog", "verify", tampered.to_str().unwrap(), "--json"],
    );
    assert_eq!(out.status.code(), Some(4));
    assert_eq!(json(&out)["error"]["code"], "signature_invalid");

    let out = kim(&sandbox, &["catalog", "show", "no-such-tool", "--json"]);
    assert_eq!(out.status.code(), Some(3));
    assert_eq!(json(&out)["error"]["code"], "tool_not_found");
}
