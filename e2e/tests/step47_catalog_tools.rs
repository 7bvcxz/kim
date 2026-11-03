//! E2E — Step 47: Noeul and Ddalgak in the Kim tool catalog (docs/CATALOG_FORMAT.md §2.1, §6;
//! docs/RELEASE_POLICY.md §1). Requirements: KIM-FR-001, KIM-FR-002, KIM-FR-034, KIM-FR-050,
//! KIM-FR-051, KIM-FR-052, KIM-SEC-001, KIM-SEC-005, KIM-SEC-006.
//!
//! Scenario A (release `kim`, fresh profile, bundled catalog only): a user looks at the catalog,
//! sees Noeul and Ddalgak as *planned* with their features, shortcuts, permissions and manual-only
//! updates, tries to install them and is told that nothing has been released — and Kim, the only
//! installed tool, wrote nothing at all.
//!
//! Scenario B (debug `kim`, which trusts a test key from `KIM_DEV_TRUSTED_KEY`): a test-signed
//! catalog gives Noeul a release served over loopback. The dry run discloses what Noeul may do;
//! installing it installs nothing else; its shortcuts then come from the installed manifest;
//! while its "tray" process (a copy of PING.EXE) runs, uninstall is refused with a tray hint and
//! the process is never stopped; after the user quits it, uninstall works and keeps user data.
//!
//! Kim Manager shows the same entries; its headless (egui_kittest) tests cover the GUI, because
//! the e2e suite never opens a window on the maintainer's machine.

#[path = "../../xtask/src/jsonschema.rs"]
mod jsonschema;

use kim_e2e::{TempDir, bin, run_sandboxed};
use serde_json::{Value, json};
use std::path::Path;
use std::process::Output;

fn schema(name: &str) -> Value {
    let p = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("..")
        .join("docs/contracts/schemas")
        .join(format!("{name}.schema.json"));
    serde_json::from_slice(&std::fs::read(p).unwrap()).unwrap()
}

/// Parses one `--json` document and validates it against cli-envelope-v1 and kim-cli-v1.
fn checked(args: &[&str], out: &Output) -> (i32, Value) {
    let doc: Value = serde_json::from_slice(&out.stdout)
        .unwrap_or_else(|e| panic!("{args:?}: stdout is not one JSON document ({e})"));
    for name in ["cli-envelope-v1", "kim-cli-v1"] {
        let errors = jsonschema::Validator::new(&schema(name))
            .validate(&doc)
            .unwrap();
        assert!(errors.is_empty(), "{args:?} violates {name}: {errors:?}");
    }
    let code = out.status.code().unwrap_or(-1);
    if doc["ok"] == false {
        assert_eq!(i64::from(code), doc["error"]["exit_code"].as_i64().unwrap());
    }
    (code, doc)
}

#[test]
#[ignore = "e2e: run via `cargo xtask e2e` only (AGENTS.md §4)"]
fn a_fresh_machine_lists_noeul_and_ddalgak_as_planned_and_installs_nothing() {
    let sb = TempDir::new("step47-fresh");
    let kim = bin("kim");
    let run = |args: &[&str]| {
        let out = run_sandboxed(&kim, args, &sb);
        checked(args, &out)
    };

    let (code, list) = run(&["catalog", "list", "--json", "--no-input"]);
    assert_eq!(code, 0);
    let ids: Vec<&str> = list["data"]["tools"]
        .as_array()
        .unwrap()
        .iter()
        .map(|t| t["id"].as_str().unwrap())
        .collect();
    assert_eq!(ids, ["kim", "subak", "noeul", "ddalgak"]);
    for t in list["data"]["tools"].as_array().unwrap() {
        assert_eq!(t["availability"], "planned", "{t}");
    }

    let (_, noeul) = run(&["catalog", "show", "noeul", "--json", "--no-input"]);
    let d = &noeul["data"];
    assert_eq!(d["runs"]["id"], "tray");
    assert_eq!(d["shortcuts"]["actions"][0]["keys"], json!(["F1"]));
    assert!(
        d["permissions"]
            .as_array()
            .unwrap()
            .iter()
            .any(|p| p["id"] == "screen-capture")
    );
    assert_eq!(d["manage"]["automatic_updates"], false);
    let (_, ddalgak) = run(&["catalog", "show", "ddalgak", "--json", "--no-input"]);
    assert!(
        ddalgak["data"]["permissions"]
            .as_array()
            .unwrap()
            .iter()
            .any(|p| p["id"] == "keyboard-hook" && p["opt_in"] == true)
    );

    let human = run_sandboxed(&kim, &["catalog", "show", "noeul"], &sb);
    let text = String::from_utf8_lossy(&human.stdout);
    assert!(text.contains("Release:  planned"), "{text}");
    assert!(text.contains("Captures the screen"), "{text}");

    for tool in ["noeul", "ddalgak", "subak"] {
        let (code, doc) = run(&["install", tool, "--json", "--no-input"]);
        assert_eq!(
            (code, doc["error"]["code"].as_str()),
            (5, Some("no_release")),
            "{doc}"
        );
        assert!(
            doc["error"]["message"]
                .as_str()
                .unwrap()
                .contains("planned")
        );
    }
    assert_eq!(sb.entry_count(), 0, "Kim alone wrote and installed nothing");
}

/// Scenario B needs a Windows program to stand in for Noeul's tray process.
#[cfg(windows)]
mod released {
    use super::checked;
    use kim_e2e::{TempDir, bin, bin_debug};
    use serde_json::{Value, json};
    use sha2::{Digest, Sha256};
    use std::io::{Read, Write};
    use std::path::Path;
    use std::process::Command;

    const SEED: [u8; 32] = [47u8; 32];

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

    /// tool.exe (a copy of the release kim, passes `--version`), the stand-in tray process and the
    /// contract's Noeul hotkey manifest.
    fn noeul_package() -> Vec<u8> {
        let manifest = std::fs::read(
            Path::new(env!("CARGO_MANIFEST_DIR"))
                .join("../docs/contracts/examples/kim-hotkey-manifest-v1/noeul.json"),
        )
        .unwrap();
        let mut zip = zip::ZipWriter::new(std::io::Cursor::new(Vec::new()));
        let o = zip::write::SimpleFileOptions::default();
        for (name, bytes) in [
            ("tool.exe", std::fs::read(bin("kim")).unwrap()),
            (
                "noeul-tray.exe",
                std::fs::read(r"C:\Windows\System32\PING.EXE").unwrap(),
            ),
            ("hotkeys.manifest.json", manifest),
        ] {
            zip.start_file(name, o).unwrap();
            zip.write_all(&bytes).unwrap();
        }
        zip.finish().unwrap().into_inner()
    }

    /// The bundled kim, subak, noeul and ddalgak entries; Noeul gets one test release.
    fn publish(sandbox: &Path, url: &str, body: &[u8]) {
        let bundled: Value = serde_json::from_slice(
            &std::fs::read(Path::new(env!("CARGO_MANIFEST_DIR")).join("../catalog/catalog.json"))
                .unwrap(),
        )
        .unwrap();
        let mut tools: Vec<Value> = bundled["tools"].as_array().unwrap().clone();
        let noeul = tools.iter_mut().find(|t| t["id"] == "noeul").unwrap();
        noeul["releases"] = json!([{"version": "0.1.0", "channel": "stable",
            "published_at": "2026-11-01T00:00:00Z", "release_notes": "e2e release",
            "assets": [{"os": "windows", "arch": "x86_64", "url": url, "size": body.len(),
                        "sha256": kim::catalog::signature::hex(&Sha256::digest(body)),
                        "kind": "zip", "entrypoint": "tool.exe"}]}]);
        let doc = json!({"schema": "kim.catalog", "schema_version": 1, "catalog_version": 1000,
            "generated_at": "2026-11-01T00:00:00Z", "expires_at": "2099-01-01T00:00:00Z",
            "publisher": "e2e", "tools": tools});
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

    #[test]
    #[ignore = "e2e: run via `cargo xtask e2e` only (AGENTS.md §4)"]
    fn a_released_noeul_is_disclosed_installed_alone_and_never_stopped_while_it_runs() {
        use std::os::windows::process::CommandExt;
        const CREATE_NO_WINDOW: u32 = 0x0800_0000;
        let sb = TempDir::new("step47-noeul");
        let p = sb.path();
        let body = noeul_package();
        let port = serve(vec![("/noeul-0.1.0.zip", body.clone())]);
        publish(
            p,
            &format!("http://127.0.0.1:{port}/noeul-0.1.0.zip"),
            &body,
        );
        let kim = bin_debug("kim");
        let run = |args: &[&str]| {
            let out = Command::new(&kim)
                .args(args)
                .env("LOCALAPPDATA", p.join("Local"))
                .env("APPDATA", p.join("Roaming"))
                .env(
                    "KIM_DEV_TRUSTED_KEY",
                    kim::catalog::signature::public_key_hex(&SEED),
                )
                .output()
                .unwrap();
            checked(args, &out)
        };
        let programs = p.join("Local").join("Programs").join("mouv");
        let tools_installed = || -> Vec<String> {
            let mut v: Vec<String> = std::fs::read_dir(&programs)
                .map(|rd| {
                    rd.flatten()
                        .map(|e| e.file_name().to_string_lossy().into_owned())
                        .filter(|n| !n.starts_with('.'))
                        .collect()
                })
                .unwrap_or_default();
            v.sort();
            v
        };

        let (code, plan) = run(&["install", "noeul", "--dry-run", "--json", "--no-input"]);
        assert_eq!(code, 0, "{plan}");
        let disclosed: Vec<&str> = plan["data"]["plan"]["permissions"]
            .as_array()
            .unwrap()
            .iter()
            .map(|x| x["id"].as_str().unwrap())
            .collect();
        assert!(disclosed.contains(&"screen-capture") && disclosed.contains(&"global-hotkeys"));
        assert!(
            tools_installed().is_empty(),
            "the dry run downloaded nothing"
        );

        let (code, doc) = run(&["install", "noeul", "--json", "--no-input"]);
        assert_eq!(code, 0, "{doc}");
        assert_eq!(tools_installed(), ["noeul"], "only Noeul was installed");
        let (_, show) = run(&["catalog", "show", "noeul", "--json", "--no-input"]);
        assert_eq!(show["data"]["shortcuts"]["source"], "installed");
        let (_, check) = run(&["hotkeys", "check", "--all-tools", "--json", "--no-input"]);
        assert!(
            check["data"]["tools"]
                .as_array()
                .unwrap()
                .iter()
                .any(|t| t["tool"] == "noeul"),
            "{check}"
        );

        let tray = programs
            .join("noeul")
            .join("versions")
            .join("0.1.0")
            .join("noeul-tray.exe");
        let mut child = Command::new(&tray)
            .args(["-n", "60", "127.0.0.1"])
            .stdout(std::process::Stdio::null())
            .creation_flags(CREATE_NO_WINDOW)
            .spawn()
            .unwrap();
        std::thread::sleep(std::time::Duration::from_millis(500));
        let (refused_code, refused) = run(&["uninstall", "noeul", "--yes", "--json", "--no-input"]);
        let alive = child.try_wait().unwrap().is_none();
        let _ = child.kill();
        let _ = child.wait();
        assert_eq!(
            (refused_code, refused["error"]["code"].as_str()),
            (7, Some("tool_running")),
            "{refused}"
        );
        assert!(
            refused["error"]["hint"]
                .as_str()
                .unwrap()
                .contains("tray icon")
        );
        assert!(alive, "Kim never stops a running background utility");

        let data = p.join("Roaming").join("mouv").join("noeul");
        std::fs::create_dir_all(&data).unwrap();
        let (code, doc) = run(&["uninstall", "noeul", "--yes", "--json", "--no-input"]);
        assert_eq!(code, 0, "{doc}");
        assert!(tools_installed().is_empty());
        assert!(data.exists(), "user data is kept");
    }
}
