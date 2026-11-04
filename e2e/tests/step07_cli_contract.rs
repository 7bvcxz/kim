//! E2E — Step 07: Kim CLI contract and headless foundation (docs/CLI_CONTRACT.md,
//! docs/KIM_CLI.md).
//!
//! Scenario A (release `kim`): an agent manages Kim headlessly on a fresh machine — status,
//! version, capabilities, installed tools, catalog — every `--json` document matches the
//! published schemas, error exit codes equal `error.exit_code`, and nothing is written.
//!
//! Scenario B (release `subak`): Subak honestly describes itself through the
//! contract (`capabilities --json`, structured usage errors).
//!
//! Scenario C (debug `kim`, test-signed catalog, loopback downloads): the real release `subak`
//! binary is installed as tool `subak` with a capabilities manifest; the agent discovers it,
//! negotiates requirements, probes the installed Subak live, and uninstalls with explicit
//! consent. Offline mode and non-interactive rules hold throughout.
//!
//! Scenario D (release `kim`): credentials in URLs never reach output.

#[path = "../../xtask/src/jsonschema.rs"]
mod jsonschema;

use kim_e2e::{TempDir, bin, bin_debug, run_sandboxed};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::io::{Read, Write};
use std::path::Path;
use std::process::{Command, Output, Stdio};

const SEED: [u8; 32] = [77u8; 32];

fn schema(name: &str) -> Value {
    let p = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("..")
        .join("docs/contracts/schemas")
        .join(format!("{name}.schema.json"));
    serde_json::from_slice(&std::fs::read(p).unwrap()).unwrap()
}

fn assert_schema(name: &str, doc: &Value) {
    let s = schema(name);
    let errors = jsonschema::Validator::new(&s).validate(doc).unwrap();
    assert!(errors.is_empty(), "{name}: {errors:?}\n{doc:#}");
}

fn doc_of(out: &Output) -> (i32, Value) {
    let v: Value = serde_json::from_slice(&out.stdout).unwrap_or_else(|e| {
        panic!(
            "not one JSON document ({e}): {}",
            String::from_utf8_lossy(&out.stdout)
        )
    });
    let code = out.status.code().unwrap_or(-1);
    if v["ok"] == false {
        assert_eq!(i64::from(code), v["error"]["exit_code"].as_i64().unwrap());
    }
    (code, v)
}

#[test]
#[ignore = "e2e: run via `cargo xtask e2e` only (AGENTS.md §4)"]
fn agent_manages_kim_headlessly_with_schema_valid_json_and_no_writes() {
    let kim = bin("kim");
    let sb = TempDir::new("step07-headless");
    for args in [
        &["status", "--json", "--no-input"][..],
        &["version", "--json"],
        &["capabilities", "--json"],
        &["tools", "list", "--json"],
        &[
            "tools",
            "describe",
            "kim",
            "--require-command",
            "install",
            "--json",
        ],
        &["catalog", "list", "--installed", "--json"],
        &["update", "check", "--json"],
        &[
            "settings",
            "set",
            "offline_mode",
            "true",
            "--dry-run",
            "--json",
        ],
    ] {
        let out = run_sandboxed(&kim, args, &sb);
        let (code, doc) = doc_of(&out);
        assert_eq!(code, 0, "{args:?}: {doc}");
        assert_schema("cli-envelope-v1", &doc);
        assert_schema("kim-cli-v1", &doc);
        assert!(out.stderr.is_empty(), "{args:?}: --json keeps stderr empty");
    }
    for (args, code, error) in [
        (&["--json"][..], 2, "usage"),
        (&["install", "--json"], 2, "usage"),
        (
            &["tools", "describe", "subak", "--json"],
            3,
            "not_installed",
        ),
        (&["install", "nope", "--json"], 3, "tool_not_found"),
    ] {
        let (c, doc) = doc_of(&run_sandboxed(&kim, args, &sb));
        assert_eq!(
            (c, doc["error"]["code"].as_str()),
            (code, Some(error)),
            "{args:?}"
        );
        assert_schema("kim-cli-v1", &doc);
    }
    let (_, version) = doc_of(&run_sandboxed(&kim, &["version", "--json"], &sb));
    assert_eq!(version["data"]["build"], "release");
    assert_eq!(version["data"]["cli_contract"], 1);
    assert_eq!(sb.entry_count(), 0, "headless management wrote nothing");
}

#[test]
#[ignore = "e2e: run via `cargo xtask e2e` only (AGENTS.md §4)"]
fn subak_describes_itself_honestly_through_the_contract() {
    let subak = bin("subak");
    let sb = TempDir::new("step07-subak");
    let (code, doc) = doc_of(&run_sandboxed(&subak, &["capabilities", "--json"], &sb));
    assert_eq!(code, 0);
    assert_schema("cli-envelope-v1", &doc);
    assert_schema("tool-capabilities-v1", &doc["data"]);
    assert_eq!(doc["data"]["implementation"], "implemented");
    let (code, doc) = doc_of(&run_sandboxed(&subak, &["attach", "--json"], &sb));
    assert_eq!((code, doc["error"]["code"].as_str()), (2, Some("usage")));
    assert_eq!(sb.entry_count(), 0);
}

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

/// A zip with the release `subak.exe` and its own capabilities document as manifest.
fn subak_package() -> Vec<u8> {
    let exe = std::fs::read(bin("subak")).unwrap();
    let out = Command::new(bin("subak"))
        .args(["capabilities", "--json"])
        .output()
        .unwrap();
    let doc: Value = serde_json::from_slice(&out.stdout).unwrap();
    let mut zip = zip::ZipWriter::new(std::io::Cursor::new(Vec::new()));
    let o = zip::write::SimpleFileOptions::default();
    zip.start_file("subak.exe", o).unwrap();
    zip.write_all(&exe).unwrap();
    zip.start_file("capabilities.json", o).unwrap();
    zip.write_all(&serde_json::to_vec_pretty(&doc["data"]).unwrap())
        .unwrap();
    zip.finish().unwrap().into_inner()
}

fn publish(sandbox: &Path, url: &str, body: &[u8]) {
    let subak_version = {
        let out = Command::new(bin("subak"))
            .arg("--version")
            .output()
            .unwrap();
        String::from_utf8_lossy(&out.stdout)
            .trim()
            .trim_start_matches("subak ")
            .to_string()
    };
    let doc = json!({"schema": "kim.catalog", "schema_version": 1, "catalog_version": 500,
        "generated_at": "2026-11-01T00:00:00Z", "expires_at": "2099-01-01T00:00:00Z", "publisher": "e2e",
        "tools": [{"id": "subak", "name": "Subak", "summary": "s", "description": "d", "license": "MIT",
                   "support": "preview", "releases": [{
            "version": subak_version, "channel": "stable", "published_at": "2026-11-01T00:00:00Z",
            "release_notes": "e2e", "assets": [{"os": std::env::consts::OS, "arch": std::env::consts::ARCH,
            "url": url, "size": body.len(), "sha256": kim::catalog::signature::hex(&Sha256::digest(body)),
            "kind": "zip", "entrypoint": "subak.exe"}]}]}]});
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

fn run_debug(sandbox: &Path, args: &[&str]) -> (i32, Value) {
    let out = Command::new(bin_debug("kim"))
        .args(args)
        .env("LOCALAPPDATA", sandbox.join("Local"))
        .env("APPDATA", sandbox.join("Roaming"))
        .env(
            "KIM_DEV_TRUSTED_KEY",
            kim::catalog::signature::public_key_hex(&SEED),
        )
        .stdin(Stdio::null())
        .output()
        .unwrap();
    let (code, doc) = doc_of(&out);
    assert_schema("kim-cli-v1", &doc);
    (code, doc)
}

#[test]
#[ignore = "e2e: run via `cargo xtask e2e` only (AGENTS.md §4)"]
fn agent_discovers_negotiates_and_probes_an_installed_subak() {
    let sb = TempDir::new("step07-discovery");
    let p = sb.path();
    let body = subak_package();
    let port = serve(vec![("/subak.zip", body.clone())]);
    publish(p, &format!("http://127.0.0.1:{port}/subak.zip"), &body);

    let (code, _) = run_debug(p, &["tools", "describe", "subak", "--json", "--no-input"]);
    assert_eq!(code, 3, "absent tool → not_installed");
    let (code, doc) = run_debug(p, &["install", "subak", "--json", "--no-input"]);
    assert_eq!(code, 0, "{doc}");

    let (_, list) = run_debug(p, &["tools", "list", "--json"]);
    let subak = list["data"]["tools"]
        .as_array()
        .unwrap()
        .iter()
        .find(|t| t["id"] == "subak")
        .unwrap()
        .clone();
    assert_eq!(subak["capabilities"]["source"], "manifest");

    let (code, doc) = run_debug(
        p,
        &[
            "tools",
            "describe",
            "subak",
            "--require-command",
            "capabilities",
            "--json",
        ],
    );
    assert_eq!(code, 0, "{doc}");
    assert_eq!(doc["data"]["capabilities"]["implementation"], "implemented");
    // Subak implements `terminal.list` since Step 10, so requiring it succeeds...
    let (code, doc) = run_debug(
        p,
        &[
            "tools",
            "describe",
            "subak",
            "--require-command",
            "terminal.list",
            "--json",
        ],
    );
    assert_eq!(code, 0, "{doc}");
    // ...and the refusal path uses a command no tool will ever declare, so this scenario does
    // not go stale as Subak grows (e2e run 1 failed exactly that way).
    let (code, doc) = run_debug(
        p,
        &[
            "tools",
            "describe",
            "subak",
            "--require-command",
            "e2e.never-implemented",
            "--json",
        ],
    );
    assert_eq!(
        (code, doc["error"]["code"].as_str()),
        (5, Some("capability_missing"))
    );
    let (code, doc) = run_debug(
        p,
        &[
            "tools",
            "describe",
            "subak",
            "--require-version",
            ">=99",
            "--json",
        ],
    );
    assert_eq!(
        (code, doc["error"]["code"].as_str()),
        (5, Some("tool_incompatible"))
    );
    // Live probe of the installed Subak (headless, time-limited) agrees with its manifest.
    let (code, probed) = run_debug(p, &["tools", "describe", "subak", "--probe", "--json"]);
    assert_eq!(code, 0, "{probed}");
    assert_eq!(probed["data"]["capabilities_source"], "probe");
    assert_eq!(probed["data"]["capabilities"], doc_capabilities(p));

    // Offline mode and non-interactive rules.
    let (code, _) = run_debug(p, &["settings", "set", "offline_mode", "true", "--json"]);
    assert_eq!(code, 0);
    let (code, doc) = run_debug(p, &["install", "subak", "--reinstall", "--json"]);
    assert_eq!(
        (code, doc["error"]["code"].as_str()),
        (6, Some("offline_mode"))
    );
    let (code, doc) = run_debug(p, &["uninstall", "subak", "--json"]);
    assert_eq!(
        (code, doc["error"]["code"].as_str()),
        (7, Some("confirmation_required"))
    );
    let (code, doc) = run_debug(p, &["uninstall", "subak", "--yes", "--json", "--no-input"]);
    assert_eq!(code, 0, "{doc}");
}

fn doc_capabilities(sandbox: &Path) -> Value {
    let (_, doc) = run_debug(sandbox, &["tools", "describe", "subak", "--json"]);
    doc["data"]["capabilities"].clone()
}

#[test]
#[ignore = "e2e: run via `cargo xtask e2e` only (AGENTS.md §4)"]
fn credentials_in_urls_never_reach_output() {
    let sb = TempDir::new("step07-secrets");
    let out = run_sandboxed(
        &bin("kim"),
        &[
            "catalog",
            "refresh",
            "--source",
            "http://user:e2e-secret@127.0.0.1:9/c.json?token=e2e-token",
            "--json",
            "--verbose",
        ],
        &sb,
    );
    let (code, doc) = doc_of(&out);
    assert_eq!(
        (code, doc["error"]["code"].as_str()),
        (6, Some("network_unavailable"))
    );
    for stream in [&out.stdout, &out.stderr] {
        let text = String::from_utf8_lossy(stream);
        assert!(
            !text.contains("e2e-secret") && !text.contains("e2e-token"),
            "{text}"
        );
    }
}
