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

/// Kim Manager's shortcuts (Step 46, HOTKEY_CONTRACT §13.2; KIM-FR-041, KIM-FR-043,
/// KIM-FR-044, KIM-SEC-001): F1 lists them, F5 refreshes like the button, a binding changed in
/// hotkeys.json applies when the window regains focus, the tool-wide switch turns them off, and
/// a destructive shortcut opens the same confirmation as its button. Focus backend only: no
/// hotkey is registered and no hook is installed.
#[test]
fn keyboard_shortcuts_follow_hotkeys_json_and_never_skip_confirmation() {
    let sb = Sandbox::new("shortcuts");
    let mut be = backend(&sb);
    // A source Kim refuses before any network access, so the result is deterministic (and its
    // message is Kim's own, not a localized OS error the test font cannot draw).
    be.catalog_source = "ftp://never-used.invalid/catalog.json".into();
    let layer = be.paths.roaming_data.join("kim").join("hotkeys.json");
    let mut h = harness(KimManagerApp::new(be, None));
    h.run();

    // F5 does what "Refresh catalog" does (here: the source is refused).
    assert!(h.state().banner().is_none());
    h.key_press(egui::Key::F5);
    h.run();
    assert!(h.state_mut().wait_idle(Duration::from_secs(30)));
    h.run();
    assert!(
        matches!(h.state().banner(), Some(Banner::Error { code, .. }) if code == "unsupported_source"),
        "{:?}",
        h.state().banner()
    );
    // (The long refusal text pushes the Dismiss button out of the test window; dismiss directly.)
    h.state_mut().dismiss_banner();
    h.run();
    assert!(
        h.state().banner().is_none(),
        "dismissed: {:?}",
        h.state().banner()
    );

    // F1 opens the shortcut list (the reserved keys are listed as fixed); F1 closes it again.
    h.key_press(egui::Key::F1);
    h.run();
    assert!(h.state().shortcuts_visible());
    assert!(h.query_by_label("Keyboard shortcuts").is_some());
    assert!(h.query_by_label("Refresh the catalog").is_some());
    assert!(h.query_by_label("Close the window").is_some());
    h.key_press(egui::Key::F1);
    h.run();
    assert!(!h.state().shortcuts_visible());

    // `kim hotkeys set catalog.refresh Ctrl+R` while the window is open, then focus returns.
    std::fs::create_dir_all(layer.parent().unwrap()).unwrap();
    std::fs::write(
        &layer,
        json!({"schema": "kim.hotkeys", "schema_version": 1, "tool": "kim",
               "overrides": [{"action": "catalog.refresh", "bindings": ["Ctrl+R"]},
                             {"action": "tool.uninstall", "bindings": ["Ctrl+Shift+U"]}]})
        .to_string(),
    )
    .unwrap();
    assert!(
        h.state().banner().is_none(),
        "dismissed: {:?}",
        h.state().banner()
    );
    h.event(egui::Event::WindowFocused(true));
    h.run();
    assert!(
        h.state().banner().is_none(),
        "focus: {:?}",
        h.state().banner()
    );
    h.key_press(egui::Key::F5);
    h.run();
    assert!(
        h.state().banner().is_none() && !h.state().is_busy(),
        "F5 is no longer bound: {:?}",
        h.state().banner()
    );
    h.key_press_modifiers(egui::Modifiers::CTRL, egui::Key::R);
    h.run();
    assert!(h.state_mut().wait_idle(Duration::from_secs(30)));
    h.run();
    assert!(
        matches!(h.state().banner(), Some(Banner::Error { .. })),
        "Ctrl+R refreshed"
    );
    // (The long refusal text pushes the Dismiss button out of the test window; dismiss directly.)
    h.state_mut().dismiss_banner();
    h.run();

    // A destructive shortcut opens the same confirmation as the button; nothing is removed.
    h.get_by_label("Install 1.0.0").click();
    h.run();
    assert!(h.state_mut().wait_idle(Duration::from_secs(30)));
    h.run();
    let installed =
        sb.0.join("Programs")
            .join("mouv")
            .join("example-tool")
            .join("current.json");
    assert!(installed.exists());
    h.key_press_modifiers(egui::Modifiers::CTRL | egui::Modifiers::SHIFT, egui::Key::U);
    h.run();
    assert_eq!(h.state().confirming(), Some("example-tool"));
    assert!(h.query_by_label("Keep installed").is_some());
    h.get_by_label("Keep installed").click();
    h.run();
    assert!(
        installed.exists(),
        "a shortcut never skips the confirmation"
    );

    // A file broken while the window is open keeps the current shortcuts (§8.3).
    std::fs::write(&layer, b"{ broken").unwrap();
    h.event(egui::Event::WindowFocused(true));
    h.run();
    h.key_press(egui::Key::F1);
    h.run();
    assert!(h.state().shortcuts_visible(), "F1 still works");
    assert!(
        h.query_by_label_contains("the previous shortcuts stay active")
            .is_some()
    );
    h.key_press(egui::Key::F1);
    h.run();

    // The tool-wide switch (`kim hotkeys disable --all`) turns every shortcut off.
    std::fs::write(
        &layer,
        json!({"schema": "kim.hotkeys", "schema_version": 1, "tool": "kim", "enabled": false})
            .to_string(),
    )
    .unwrap();
    h.event(egui::Event::WindowFocused(true));
    h.run();
    h.key_press(egui::Key::F1);
    h.run();
    assert!(!h.state().shortcuts_visible(), "all shortcuts are off");
}

/// `--no-hotkeys` starts with every shortcut off without touching hotkeys.json (KIM-SEC-023).
#[test]
fn no_hotkeys_start_flag_turns_every_shortcut_off() {
    let sb = Sandbox::new("nohotkeys");
    let be = backend(&sb);
    let layer = be.paths.roaming_data.join("kim").join("hotkeys.json");
    let mut app = KimManagerApp::new(be, None);
    app.disable_hotkeys();
    let mut h = harness(app);
    h.run();
    h.key_press(egui::Key::F1);
    h.run();
    h.key_press(egui::Key::F5);
    h.run();
    assert!(!h.state().shortcuts_visible() && !h.state().is_busy());
    assert!(!layer.exists(), "the file is not touched");
}
