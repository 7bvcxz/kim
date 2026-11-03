//! Headless UI tests for Kim Manager (egui_kittest: no window, no GPU). The UI is driven through
//! its accessibility tree, like a screen-reader user would, against a sandboxed backend.

use egui_kittest::Harness;
use egui_kittest::kittest::Queryable;
use kim::catalog::signature::{self, TrustedKey};
use kim::paths::Paths;
use kim::platform::Platform;
use kim_manager::{Backend, Banner, KimManagerApp};
use serde_json::json;
use sha2::{Digest, Sha256};
use std::io::{Read, Write};
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

const SEED: [u8; 32] = [21u8; 32];

struct Sandbox(PathBuf);

impl Sandbox {
    fn new(tag: &str) -> Self {
        let p = std::env::temp_dir().join(format!("kim-ui-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&p);
        std::fs::create_dir_all(&p).unwrap();
        Self(p)
    }
}

impl Drop for Sandbox {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

fn keys() -> Vec<TrustedKey> {
    let public: &'static str = Box::leak(signature::public_key_hex(&SEED).into_boxed_str());
    let id: &'static str =
        Box::leak(signature::key_id(&signature::unhex::<32>(public).unwrap()).into_boxed_str());
    vec![TrustedKey {
        key_id: id,
        public_key_hex: public,
        label: "ui test key",
    }]
}

/// Serves one file over loopback HTTP; returns its URL.
fn serve(body: Vec<u8>) -> String {
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let port = listener.local_addr().unwrap().port();
    std::thread::spawn(move || {
        for mut s in listener.incoming().flatten() {
            let mut buf = [0u8; 2048];
            let _ = s.read(&mut buf);
            let _ = write!(
                s,
                "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                body.len()
            );
            let _ = s.write_all(&body);
        }
    });
    format!("http://127.0.0.1:{port}/tool.zip")
}

/// A package whose entrypoint is a copy of cmd.exe (exits 0 for `--version` with stdin closed).
fn package() -> Vec<u8> {
    let mut zip = zip::ZipWriter::new(std::io::Cursor::new(Vec::new()));
    zip.start_file("tool.exe", zip::write::SimpleFileOptions::default())
        .unwrap();
    zip.write_all(&std::fs::read(r"C:\Windows\System32\cmd.exe").unwrap())
        .unwrap();
    zip.finish().unwrap().into_inner()
}

fn backend(sb: &Sandbox) -> Backend {
    let paths = Paths::under(&sb.0);
    let body = package();
    let url = serve(body.clone());
    let doc = json!({
        "schema": "kim.catalog", "schema_version": 1, "catalog_version": 500,
        "generated_at": "2026-11-01T00:00:00Z", "expires_at": "2099-01-01T00:00:00Z",
        "publisher": "test",
        "tools": [
            {"id": "kim", "name": "Kim", "summary": "Kim itself", "description": "d", "license": "MIT", "support": "in-development", "releases": []},
            {"id": "example-tool", "name": "Example Tool", "summary": "Fixture", "description": "A fixture tool for UI tests.", "license": "MIT", "support": "stable",
             "releases": [{"version": "1.0.0", "channel": "stable", "published_at": "2026-11-01T00:00:00Z", "release_notes": "n",
                "assets": [{"os": std::env::consts::OS, "arch": std::env::consts::ARCH, "url": url, "size": body.len(),
                            "sha256": signature::hex(&Sha256::digest(&body)), "kind": "zip", "entrypoint": "tool.exe"}]}]}
        ]
    });
    let bytes = serde_json::to_vec_pretty(&doc).unwrap();
    let sig = serde_json::to_vec(&signature::sign(&bytes, &SEED)).unwrap();
    let dir = paths.catalog_cache_dir();
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(dir.join("catalog.json"), bytes).unwrap();
    std::fs::write(dir.join("catalog.json.sig"), sig).unwrap();
    let mut keys = keys();
    keys.extend_from_slice(signature::TRUSTED_KEYS);
    Backend {
        paths,
        keys,
        downloader: Arc::new(kim::fetch::DefaultFetcher::default()),
        settings_path: Some(sb.0.join("settings.json")),
        platform: Platform::current(),
        catalog_source: "http://127.0.0.1:9/never-used.json".into(),
    }
}

fn harness(app: KimManagerApp) -> Harness<'static, KimManagerApp> {
    Harness::builder()
        .with_size(egui::vec2(1000.0, 640.0))
        .build_ui_state(|ui, app: &mut KimManagerApp| app.ui(ui), app)
}

use eframe::egui;

#[test]
fn shows_tools_and_installs_only_after_the_button_is_pressed() {
    let sb = Sandbox::new("install");
    let be = backend(&sb);
    let paths = be.paths.clone();
    let mut h = harness(KimManagerApp::new(be, None));
    h.run();
    assert!(
        h.query_by_label("Example Tool, available").is_some(),
        "list row with its status"
    );
    assert!(
        !paths.install_record("example-tool").exists(),
        "opening the window installs nothing"
    );

    h.get_by_label("Install 1.0.0").click();
    h.run();
    assert!(
        h.state_mut().wait_idle(Duration::from_secs(30)),
        "install finishes"
    );
    h.run();
    assert!(paths.install_record("example-tool").exists());
    assert!(
        matches!(h.state().banner(), Some(Banner::Success(m)) if m.contains("1.0.0")),
        "{:?}",
        h.state().banner()
    );
    assert!(h.query_by_label("Uninstall…").is_some());
}

#[test]
fn uninstall_requires_confirmation_and_keeps_data_by_default() {
    let sb = Sandbox::new("uninstall");
    let be = backend(&sb);
    let paths = be.paths.clone();
    let data = paths.user_data_dirs("example-tool")[0].clone();
    let mut h = harness(KimManagerApp::new(be, None));
    h.run();
    h.get_by_label("Install 1.0.0").click();
    h.run();
    assert!(h.state_mut().wait_idle(Duration::from_secs(30)));
    h.run();
    std::fs::create_dir_all(&data).unwrap();

    // Opening the dialog and backing out changes nothing.
    h.get_by_label("Uninstall…").click();
    h.run();
    h.get_by_label("Keep installed").click();
    h.run();
    assert!(paths.install_record("example-tool").exists());

    // Purge needs a second, explicit acknowledgement.
    h.get_by_label("Uninstall…").click();
    h.run();
    h.get_by_label("Also delete this tool's settings and data")
        .click();
    h.run();
    assert!(
        h.query_by_label("Uninstall and delete data").is_some(),
        "the destructive button is labelled for what it does"
    );
    h.get_by_label("Also delete this tool's settings and data")
        .click();
    h.run();
    h.get_by_label("Uninstall").click();
    h.run();
    assert!(h.state_mut().wait_idle(Duration::from_secs(30)));
    h.run();
    assert!(!paths.install_record("example-tool").exists());
    assert!(data.exists(), "user data is kept by default");
}

#[test]
fn update_checks_on_launch_are_off_by_default_and_offline_mode_blocks_installs() {
    let sb = Sandbox::new("settings");
    let be = backend(&sb);
    let mut h = harness(KimManagerApp::new(be, None));
    h.run();
    assert!(
        !h.state().is_busy(),
        "no network activity at launch by default"
    );
    h.get_by_label("Offline mode").click();
    h.run();
    let saved: serde_json::Value =
        serde_json::from_slice(&std::fs::read(sb.0.join("settings.json")).unwrap()).unwrap();
    assert_eq!(saved["offline_mode"], true);
    assert_eq!(saved["check_updates_on_launch"], false);
    h.state_mut().start_install("example-tool");
    h.run();
    assert!(
        matches!(h.state().banner(), Some(Banner::Error { code, .. }) if code == "offline_mode")
    );
}

#[test]
fn incompatible_and_unreleased_tools_offer_no_install_button() {
    let sb = Sandbox::new("noinstall");
    let mut be = backend(&sb);
    be.platform = Platform {
        os: "linux".into(),
        arch: "x86_64".into(),
        os_build: None,
    };
    let mut h = harness(KimManagerApp::new(be, None));
    h.run();
    assert!(h.query_by_label("Install 1.0.0").is_none());
    assert!(h.query_by_label_contains("Not installable here").is_some());
    assert!(h.state().views().iter().any(|v| v.id == "example-tool"));
}
