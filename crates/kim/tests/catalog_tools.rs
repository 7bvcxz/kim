//! Step 47: Noeul and Ddalgak in the Kim tool catalog (docs/CATALOG_FORMAT.md §2.1, §6;
//! docs/RELEASE_POLICY.md §1). Real `kim` binary in a sandbox, test-signed fixture catalogs that
//! reuse the bundled Noeul and Ddalgak metadata, real packages over loopback HTTP.
//!
//! Requirements: KIM-FR-001, KIM-FR-002, KIM-FR-034, KIM-FR-050, KIM-FR-051, KIM-FR-052,
//! KIM-SEC-001, KIM-SEC-005, KIM-SEC-006.

mod common;

use common::{DEV_SEED, Sandbox, Server, package, sha};
use kim::catalog::store::BUNDLED_CATALOG;
use kim::error::ErrorCode;
use kim::install::{self, Event, Phase, PlanOptions};
use kim::platform::Platform;
use serde_json::{Value, json};
use std::sync::atomic::AtomicBool;

/// One tool of the real bundled catalog (its real metadata).
fn bundled(id: &str) -> Value {
    let doc: Value = serde_json::from_slice(BUNDLED_CATALOG).unwrap();
    doc["tools"]
        .as_array()
        .unwrap()
        .iter()
        .find(|t| t["id"] == id)
        .unwrap_or_else(|| panic!("{id} is in the bundled catalog"))
        .clone()
}

/// A release of the package at `url`, entrypoint `tool.exe` (a copy of kim.exe).
fn rel(version: &str, url: &str, body: &[u8], deps: &[(&str, &str)]) -> Value {
    json!({
        "version": version, "channel": "stable", "published_at": "2026-11-01T00:00:00Z",
        "release_notes": format!("test release {version}"),
        "dependencies": deps.iter().map(|(t, r)| json!({"tool": t, "version_req": r})).collect::<Vec<_>>(),
        "assets": [{"os": std::env::consts::OS, "arch": std::env::consts::ARCH, "url": url,
                    "size": body.len(), "sha256": sha(body), "kind": "zip",
                    "entrypoint": "tool.exe"}]
    })
}

/// Writes a test-signed catalog with kim, subak and the given Noeul/Ddalgak releases.
fn publish(sb: &Sandbox, version: u64, noeul: &[Value], ddalgak: &[Value]) {
    let mut n = bundled("noeul");
    n["releases"] = json!(noeul);
    let mut d = bundled("ddalgak");
    d["releases"] = json!(ddalgak);
    let doc = json!({
        "schema": "kim.catalog", "schema_version": 1, "catalog_version": version,
        "generated_at": "2026-11-01T00:00:00Z", "expires_at": "2099-01-01T00:00:00Z",
        "publisher": "test", "tools": [bundled("kim"), bundled("subak"), n, d]
    });
    let bytes = serde_json::to_vec_pretty(&doc).unwrap();
    let dir = sb.cache_dir();
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(dir.join("catalog.json"), &bytes).unwrap();
    std::fs::write(
        dir.join("catalog.json.sig"),
        serde_json::to_vec(&kim::catalog::signature::sign(&bytes, &DEV_SEED)).unwrap(),
    )
    .unwrap();
}

/// Tool directories under the programs root (Kim's dot-dirs excluded).
fn installed_dirs(sb: &Sandbox) -> Vec<String> {
    let mut v: Vec<String> = std::fs::read_dir(sb.programs())
        .map(|rd| {
            rd.flatten()
                .map(|e| e.file_name().to_string_lossy().into_owned())
                .filter(|n| !n.starts_with('.'))
                .collect()
        })
        .unwrap_or_default();
    v.sort();
    v
}

fn noeul_manifest() -> Vec<u8> {
    std::fs::read(
        std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../docs/contracts/examples/kim-hotkey-manifest-v1/noeul.json"),
    )
    .unwrap()
}

/// KIM-FR-002, KIM-FR-051: a fresh machine with only Kim. Everything a user or agent can look at
/// works, a planned tool cannot be installed (with an error that says it is planned), and
/// nothing is written: Kim alone never installs Subak, Noeul or Ddalgak.
#[test]
fn kim_alone_installs_no_other_tool_and_planned_tools_cannot_be_downloaded() {
    let sb = Sandbox::new("s47-fresh");
    let before = sb.snapshot();
    for args in [
        &["status", "--json"][..],
        &["catalog", "list", "--json"],
        &["catalog", "show", "noeul", "--json"],
        &["catalog", "show", "ddalgak", "--json"],
        &["tools", "list", "--json"],
        &["update", "check", "--json"],
        &["hotkeys", "check", "--all-tools", "--json"],
    ] {
        let (code, doc, _) = sb.json(args);
        assert_eq!((code, &doc["ok"]), (0, &json!(true)), "{args:?}: {doc}");
    }
    for tool in ["subak", "noeul", "ddalgak"] {
        for dry in [true, false] {
            let mut args = vec!["install", tool, "--json", "--no-input"];
            if dry {
                args.push("--dry-run");
            }
            let (code, doc, _) = sb.json(&args);
            assert_eq!(code, 5, "{args:?}: {doc}");
            assert_eq!(doc["error"]["code"], "no_release");
            assert!(
                doc["error"]["message"]
                    .as_str()
                    .unwrap()
                    .contains("is planned: no release has been published yet"),
                "{doc}"
            );
            assert!(
                doc["error"]["hint"]
                    .as_str()
                    .unwrap()
                    .contains("signed release")
            );
        }
    }
    let out = sb.run(&["install", "noeul"]);
    assert_eq!(out.status.code(), Some(5));
    assert!(String::from_utf8_lossy(&out.stderr).contains("noeul is planned"));
    assert_eq!(sb.snapshot(), before, "nothing was written");
    assert!(installed_dirs(&sb).is_empty());
}

/// KIM-FR-050, KIM-FR-051, KIM-SEC-006: `catalog show` carries the declared metadata plus what
/// Kim decides itself, in JSON and human form.
#[test]
fn catalog_show_reports_the_metadata_of_noeul_and_ddalgak() {
    let sb = Sandbox::new("s47-show");
    let (_, n, _) = sb.json(&["catalog", "show", "noeul", "--json"]);
    let d = &n["data"];
    assert_eq!(d["tool"]["availability"], "planned");
    assert_eq!(d["tool"]["status"], "unreleased");
    assert_eq!(d["runs"]["id"], "tray");
    let perms: Vec<&str> = d["permissions"]
        .as_array()
        .unwrap()
        .iter()
        .map(|p| p["id"].as_str().unwrap())
        .collect();
    assert_eq!(
        perms,
        ["global-hotkeys", "screen-capture", "clipboard", "autostart"]
    );
    assert_eq!(d["permissions"][1]["label"], "Captures the screen");
    assert_eq!(d["shortcuts"]["source"], "catalog");
    assert_eq!(d["shortcuts"]["backend"], "register");
    assert_eq!(d["shortcuts"]["actions"][0]["keys"], json!(["F1"]));
    assert!(
        d["entry_points"]
            .as_array()
            .unwrap()
            .iter()
            .all(|e| e["status"] == "planned")
    );
    assert!(
        d["install"]["program_dir"]
            .as_str()
            .unwrap()
            .ends_with("noeul")
    );
    assert_eq!(
        (&d["install"]["per_user"], &d["install"]["elevation"]),
        (&json!(true), &json!(false))
    );
    assert_eq!(d["manage"]["automatic_updates"], false);
    assert_eq!(d["manage"]["update"], "kim update noeul");

    let (_, g, _) = sb.json(&["catalog", "show", "ddalgak", "--json"]);
    let hook = g["data"]["permissions"]
        .as_array()
        .unwrap()
        .iter()
        .find(|p| p["id"] == "keyboard-hook")
        .unwrap();
    assert_eq!(hook["opt_in"], true);
    let emergency = g["data"]["shortcuts"]["actions"]
        .as_array()
        .unwrap()
        .iter()
        .find(|a| a["action"] == "engine.emergency-off")
        .unwrap();
    assert_eq!(
        (&emergency["keys"], &emergency["bindable"]),
        (&json!(["Ctrl+Alt+Shift+Escape"]), &json!(false))
    );

    let out = sb.run(&["catalog", "show", "ddalgak"]);
    let text = String::from_utf8_lossy(&out.stdout);
    for needle in [
        "Release:  planned (no release has been published yet)",
        "Installs a low-level keyboard hook (only after you turn it on)",
        "Ctrl+Alt+Shift+Escape",
        "Updates:  manual only, never automatic",
        "[planned]",
    ] {
        assert!(text.contains(needle), "missing `{needle}`:\n{text}");
    }
}

/// KIM-FR-001, KIM-SEC-001, KIM-SEC-006: installing one tool installs only that tool. A
/// declared dependency is disclosed and must be installed by its own explicit command first;
/// the plan shows the tool's permissions before anything is downloaded.
#[test]
fn installing_one_tool_never_installs_another_and_dependencies_need_their_own_install() {
    let sb = Sandbox::new("s47-deps");
    let srv = Server::start();
    let nb = package(&[("hotkeys.manifest.json", &noeul_manifest())]);
    let nu = srv.put("/noeul-0.1.0.zip", nb.clone());
    let db = package(&[]);
    let du = srv.put("/ddalgak-0.1.0.zip", db.clone());
    // A hypothetical dependency, to exercise disclosure; the real Ddalgak declares none.
    publish(
        &sb,
        100,
        &[rel("0.1.0", &nu, &nb, &[])],
        &[rel("0.1.0", &du, &db, &[("noeul", ">=0.1.0")])],
    );

    for args in [
        &["install", "ddalgak", "--dry-run", "--json"][..],
        &["install", "ddalgak", "--json"],
    ] {
        let (code, doc, _) = sb.json(args);
        assert_eq!(code, 7, "{args:?}: {doc}");
        assert_eq!(doc["error"]["code"], "dependency_missing");
        assert!(doc["error"]["message"].as_str().unwrap().contains("noeul"));
    }
    assert!(
        installed_dirs(&sb).is_empty(),
        "nothing installed implicitly"
    );

    // The plan discloses what Noeul may do before anything is downloaded.
    let (code, plan, _) = sb.json(&["install", "noeul", "--dry-run", "--json"]);
    assert_eq!(code, 0, "{plan}");
    let ids: Vec<&str> = plan["data"]["plan"]["permissions"]
        .as_array()
        .unwrap()
        .iter()
        .map(|p| p["id"].as_str().unwrap())
        .collect();
    assert!(ids.contains(&"screen-capture") && ids.contains(&"global-hotkeys"));
    let out = sb.run(&["install", "noeul", "--dry-run"]);
    let text = String::from_utf8_lossy(&out.stdout);
    assert!(
        text.contains("permits  Captures the screen: "),
        "human plan: {text}"
    );
    assert!(
        installed_dirs(&sb).is_empty(),
        "a dry run downloads nothing"
    );

    let (code, doc, _) = sb.json(&["install", "noeul", "--json"]);
    assert_eq!(code, 0, "{doc}");
    assert_eq!(installed_dirs(&sb), ["noeul"], "only Noeul");

    let (code, plan, _) = sb.json(&["install", "ddalgak", "--dry-run", "--json"]);
    assert_eq!(code, 0, "{plan}");
    assert_eq!(
        plan["data"]["plan"]["dependencies"],
        json!([{"tool": "noeul", "version_req": ">=0.1.0"}])
    );
    let out = sb.run(&["install", "ddalgak", "--dry-run"]);
    assert!(
        String::from_utf8_lossy(&out.stdout)
            .contains("needs    noeul >=0.1.0 (installed; Kim never installs a dependency")
    );
    let (code, doc, _) = sb.json(&["install", "ddalgak", "--json"]);
    assert_eq!(code, 0, "{doc}");
    assert_eq!(installed_dirs(&sb), ["ddalgak", "noeul"], "never Subak");

    // Status reflects both; the installed Noeul's shortcuts come from its installed manifest.
    let (_, list, _) = sb.json(&["catalog", "list", "--installed", "--json"]);
    let ids: Vec<&str> = list["data"]["tools"]
        .as_array()
        .unwrap()
        .iter()
        .map(|t| t["id"].as_str().unwrap())
        .collect();
    assert_eq!(ids, ["kim", "noeul", "ddalgak"]);
    let (_, show, _) = sb.json(&["catalog", "show", "noeul", "--json"]);
    assert_eq!(show["data"]["shortcuts"]["source"], "installed");
    // Ddalgak's test package ships no manifest: the catalog's defaults, with the reason.
    let (_, show, _) = sb.json(&["catalog", "show", "ddalgak", "--json"]);
    assert_eq!(show["data"]["shortcuts"]["source"], "catalog");
    assert!(
        show["data"]["shortcuts"]["note"]
            .as_str()
            .unwrap()
            .contains("ships no hotkey manifest")
    );
}

/// KIM-FR-052: an installed tool's shortcuts are its installed manifest plus the user's
/// `hotkeys.json` (the discovery rules of `kim hotkeys check --all-tools`), never the catalog's.
#[test]
fn installed_shortcuts_follow_the_installed_manifest_and_the_users_changes() {
    let sb = Sandbox::new("s47-keys");
    let srv = Server::start();
    let nb = package(&[("hotkeys.manifest.json", &noeul_manifest())]);
    let nu = srv.put("/n.zip", nb.clone());
    publish(&sb, 100, &[rel("0.1.0", &nu, &nb, &[])], &[]);
    let (code, doc, _) = sb.json(&["install", "noeul", "--json"]);
    assert_eq!(code, 0, "{doc}");

    #[cfg(windows)]
    let layer = sb.roaming().join("mouv").join("noeul").join("hotkeys.json");
    #[cfg(not(windows))]
    let layer = sb
        .root
        .join("xdgc")
        .join("mouv")
        .join("noeul")
        .join("hotkeys.json");
    std::fs::create_dir_all(layer.parent().unwrap()).unwrap();
    std::fs::write(
        &layer,
        json!({"schema": "kim.hotkeys", "schema_version": 1, "tool": "noeul",
               "overrides": [{"action": "capture.region", "bindings": ["Shift+F1"]},
                             {"action": "paste.float", "disabled": true}]})
        .to_string(),
    )
    .unwrap();
    let (_, show, _) = sb.json(&["catalog", "show", "noeul", "--json"]);
    let s = &show["data"]["shortcuts"];
    assert_eq!(s["source"], "installed");
    let by = |id: &str| {
        s["actions"]
            .as_array()
            .unwrap()
            .iter()
            .find(|a| a["action"] == id)
            .unwrap()
            .clone()
    };
    assert_eq!(
        (
            by("capture.region")["keys"].clone(),
            by("capture.region")["state"].clone()
        ),
        (json!(["Shift+F1"]), json!("user"))
    );
    assert_eq!(
        (
            by("paste.float")["keys"].clone(),
            by("paste.float")["state"].clone()
        ),
        (json!([]), json!("disabled"))
    );

    // A manifest that describes another version is not trusted (same rule as the check).
    let dir = sb.tool_dir("noeul").join("versions").join("0.1.0");
    let mut m: Value = serde_json::from_slice(&noeul_manifest()).unwrap();
    m["version"] = "9.9.9".into();
    std::fs::write(dir.join("hotkeys.manifest.json"), m.to_string()).unwrap();
    let (_, show, _) = sb.json(&["catalog", "show", "noeul", "--json"]);
    assert_eq!(show["data"]["shortcuts"]["source"], "catalog");
    assert!(
        show["data"]["shortcuts"]["note"]
            .as_str()
            .unwrap()
            .contains("not trusted")
    );
}

/// KIM-FR-005, KIM-FR-007: progress and rollback for a Noeul release from a test-signed
/// catalog, through the library the CLI and Kim Manager share.
#[test]
fn noeul_install_reports_progress_and_a_failed_update_rolls_back() {
    let sb = Sandbox::new("s47-progress");
    let paths = kim::paths::Paths::under(&sb.root);
    let srv = Server::start();
    let loaded = |noeul: Vec<Value>| {
        let mut n = bundled("noeul");
        n["releases"] = json!(noeul);
        let doc = json!({
            "schema": "kim.catalog", "schema_version": 1, "catalog_version": 7,
            "generated_at": "2026-11-01T00:00:00Z", "expires_at": "2099-01-01T00:00:00Z",
            "publisher": "test", "tools": [bundled("kim"), n, bundled("ddalgak")]
        });
        kim::catalog::store::Loaded {
            catalog: kim::catalog::parse(&serde_json::to_vec(&doc).unwrap()).unwrap(),
            source: kim::catalog::store::Source::Cache,
            signer: kim::catalog::signature::VerifiedSigner {
                key_id: "test".into(),
                label: "test".into(),
            },
            stale: false,
            warnings: vec![],
        }
    };
    let run = |plan: &install::Plan| {
        let mut events = Vec::new();
        let r = install::execute(
            plan,
            &paths,
            &kim::fetch::DefaultFetcher::default(),
            &mut |e| events.push(e),
            &AtomicBool::new(false),
        );
        (r, events)
    };
    let record = || std::fs::read(paths.install_record("noeul")).ok();
    let platform = Platform::current();

    // A release whose package is not downloadable (yet): network_unavailable, nothing changes.
    let b1 = package(&[("V", b"1")]);
    let u1 = format!("http://127.0.0.1:{}/1.zip", srv.port);
    let l = loaded(vec![rel("0.1.0", &u1, &b1, &[])]);
    let plan =
        install::plan_install(&l, &paths, &platform, "noeul", &PlanOptions::default()).unwrap();
    let e = run(&plan).0.unwrap_err();
    assert_eq!(e.code, ErrorCode::NetworkUnavailable, "{}", e.message);
    assert!(record().is_none());
    assert!(
        std::fs::read_dir(paths.staging_dir()).map_or(true, |mut rd| rd.next().is_none()),
        "no partial download is left"
    );

    srv.put("/1.zip", b1.clone());
    assert!(
        plan.permissions.iter().any(|p| p.id == "screen-capture"),
        "disclosed in the plan"
    );
    let (r, events) = run(&plan);
    r.expect("install succeeds");
    let phases: Vec<Phase> = events
        .iter()
        .filter_map(|e| match e {
            Event::Phase { phase } => Some(*phase),
            Event::Progress { .. } => None,
        })
        .collect();
    assert_eq!(phases.first(), Some(&Phase::Locking));
    assert_eq!(phases.last(), Some(&Phase::Done));
    assert!(phases.contains(&Phase::Verifying) && phases.contains(&Phase::SelfCheck));
    assert!(events.iter().any(
        |e| matches!(e, Event::Progress { done, total } if *done == *total && *total == b1.len() as u64)
    ));
    let before = record().unwrap();

    // A corrupted update (the server sends other bytes than the signed catalog promises).
    let b2 = package(&[("V", b"2")]);
    let u2 = srv.put("/2.zip", b"not the package".to_vec());
    let l = loaded(vec![
        rel("0.1.0", &u1, &b1, &[]),
        rel("0.2.0", &u2, &b2, &[]),
    ]);
    let plan =
        install::plan_update(&l, &paths, &platform, "noeul", &PlanOptions::default()).unwrap();
    let e = run(&plan).0.unwrap_err();
    assert_eq!(e.code, ErrorCode::IntegrityFailed);
    assert_eq!(record().unwrap(), before, "0.1.0 stays active");
    assert!(
        !paths
            .tool_dir("noeul")
            .join("versions")
            .join("0.2.0")
            .exists()
    );
    let tools: Vec<String> = std::fs::read_dir(&paths.programs)
        .unwrap()
        .flatten()
        .map(|e| e.file_name().to_string_lossy().into_owned())
        .filter(|n| !n.starts_with('.'))
        .collect();
    assert_eq!(tools, ["noeul"], "no other tool appeared");

    // The real package: the update succeeds and keeps 0.1.0 for rollback.
    srv.put("/2.zip", b2);
    let out = run(&plan).0.unwrap();
    assert_eq!(
        (out.from.as_deref(), out.to.as_str()),
        (Some("0.1.0"), "0.2.0")
    );
    assert!(
        paths
            .tool_dir("noeul")
            .join("versions")
            .join("0.1.0")
            .exists()
    );
}

/// KIM-FR-034, KIM-SEC-005: while a background utility runs, `kim uninstall` refuses with a
/// hint for how *the user* quits it, `kim update` installs side by side, and Kim never stops it.
#[test]
#[cfg(windows)]
fn a_running_background_utility_is_never_stopped() {
    use std::os::windows::process::CommandExt;
    const CREATE_NO_WINDOW: u32 = 0x0800_0000;
    let sb = Sandbox::new("s47-running");
    let srv = Server::start();
    let ping = std::fs::read(r"C:\Windows\System32\PING.EXE").unwrap();
    let b1 = package(&[("noeul-tray.exe", &ping)]);
    let u1 = srv.put("/1.zip", b1.clone());
    publish(&sb, 100, &[rel("0.1.0", &u1, &b1, &[])], &[]);
    let (code, doc, _) = sb.json(&["install", "noeul", "--json"]);
    assert_eq!(code, 0, "{doc}");

    let tray = sb
        .tool_dir("noeul")
        .join("versions")
        .join("0.1.0")
        .join("noeul-tray.exe");
    let mut child = std::process::Command::new(&tray)
        .args(["-n", "60", "127.0.0.1"])
        .stdout(std::process::Stdio::null())
        .creation_flags(CREATE_NO_WINDOW)
        .spawn()
        .unwrap();
    std::thread::sleep(std::time::Duration::from_millis(300));

    let dry = sb.run(&["uninstall", "noeul", "--dry-run"]);
    let dry_text = String::from_utf8_lossy(&dry.stdout).to_string();
    let (refused_code, refused, _) = sb.json(&["uninstall", "noeul", "--yes", "--json"]);

    let b2 = package(&[("noeul-tray.exe", &ping), ("V", b"2")]);
    let u2 = srv.put("/2.zip", b2.clone());
    publish(
        &sb,
        101,
        &[rel("0.1.0", &u1, &b1, &[]), rel("0.2.0", &u2, &b2, &[])],
        &[],
    );
    let (update_code, updated, _) = sb.json(&["update", "noeul", "--json"]);
    let alive = child.try_wait().unwrap().is_none();
    let _ = child.kill();
    let _ = child.wait();

    assert!(dry_text.contains("running  PID"), "{dry_text}");
    assert!(dry_text.contains("quit it from its tray icon yourself"));
    assert_eq!(refused_code, 7, "{refused}");
    assert_eq!(refused["error"]["code"], "tool_running");
    assert!(
        refused["error"]["hint"]
            .as_str()
            .unwrap()
            .contains("notification area; quit it from its tray icon yourself"),
        "{refused}"
    );
    assert_eq!(update_code, 0, "{updated}");
    assert_eq!(
        updated["data"]["result"]["running"]
            .as_array()
            .unwrap()
            .len(),
        1
    );
    assert!(alive, "Kim stopped nothing");
    assert!(tray.exists(), "the running version's files are kept");

    // Quit by the user: now the uninstall works and the user's data stays.
    let data = sb.roaming().join("mouv").join("noeul");
    std::fs::create_dir_all(&data).unwrap();
    let (code, doc, _) = sb.json(&["uninstall", "noeul", "--yes", "--json"]);
    assert_eq!(code, 0, "{doc}");
    assert!(installed_dirs(&sb).is_empty());
    assert!(data.exists());
}
