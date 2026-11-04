//! CLI contract tests of the real `noeul` binary without a running instance
//! (docs/CLI_CONTRACT.md; NOEUL-FR-110, NOEUL-FR-111, NOEUL-FR-114, NOEUL-FR-100).
//!
//! Every `--json` document is validated against cli-envelope-v1, and its `data` against the
//! command's published schema (tool-capabilities-v1, hotkeys-cli-v1, noeul-ipc-v1). Every
//! sandbox has its own profile and endpoint; nothing here starts Noeul.

mod common;

use common::{Keys, Sandbox, validate};
use serde_json::{Value, json};

fn assert_valid(name: &str, v: &Value) {
    let e = validate(name, v);
    assert!(e.is_empty(), "violates {name}: {e:?}\n{v:#}");
}

#[test]
fn help_version_and_no_command() {
    let sb = Sandbox::new("help");
    let out = sb.run(&["--help"]);
    assert!(out.status.success());
    let text = String::from_utf8_lossy(&out.stdout);
    for word in [
        "start",
        "capture",
        "hotkeys",
        "planned",
        "F1 opens the region selection overlay",
    ] {
        assert!(text.contains(word), "--help mentions {word}:\n{text}");
    }
    let out = sb.run(&["--version"]);
    assert_eq!(
        String::from_utf8_lossy(&out.stdout).trim(),
        format!("noeul {}", env!("CARGO_PKG_VERSION"))
    );
    let out = sb.run(&[]);
    assert!(out.status.success(), "no command prints help");
    let (code, doc) = sb.json(&[]);
    assert_eq!((code, doc["error"]["code"].as_str()), (2, Some("usage")));
    assert_eq!(doc["command"], "noeul");
}

#[test]
fn capabilities_version_status_and_config_are_headless_reads() {
    let sb = Sandbox::new("reads");
    let (_, caps) = sb.json(&["capabilities"]);
    assert_valid("tool-capabilities-v1", &caps["data"]);
    let opens: Vec<&str> = caps["data"]["commands"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|c| c["opens_window"] == true)
        .map(|c| c["name"].as_str().unwrap())
        .collect();
    assert_eq!(
        opens,
        ["start", "run", "capture"],
        "the instance launchers, and `capture --interactive` (the overlay, Step 50)"
    );
    assert!(
        caps["data"]["commands"]
            .as_array()
            .unwrap()
            .iter()
            .all(|c| c["network"] == false),
        "no network (NOEUL-NFR-008)"
    );

    let (_, v) = sb.json(&["version"]);
    assert_eq!(v["data"]["ipc_protocol"], json!({"min": 1, "max": 1}));

    let before = std::fs::read_dir(sb.root.join("Roaming").join("mouv").join("noeul"))
        .unwrap()
        .count();
    let (code, st) = sb.json(&["status"]);
    assert_eq!(code, 0);
    assert_eq!(st["data"]["running"], false);
    assert_valid("noeul-ipc-v1#offline_status", &st["data"]);
    let (_, cfg) = sb.json(&["config", "show"]);
    assert_eq!(cfg["data"]["settings"]["state"], "default");
    assert!(
        cfg["data"]["paths"]["config_file"]
            .as_str()
            .unwrap()
            .ends_with("config.toml")
    );
    let after = std::fs::read_dir(sb.root.join("Roaming").join("mouv").join("noeul"))
        .unwrap()
        .count();
    assert_eq!(before, after, "reads write nothing");
    assert!(
        !sb.root.join("Local").join("mouv").exists(),
        "no data folder is created (NOEUL-SEC-005)"
    );
}

#[test]
fn instance_commands_without_an_instance_say_so() {
    let sb = Sandbox::new("absent");
    for args in [
        &["health"][..],
        &["pin", "list"],
        &["pin", "reopen"],
        &["quit", "--yes"],
        &["capture", "--region", "0,0,1,1", "--pin"],
        &["pin", "close", "pin-1", "--yes"],
    ] {
        let (code, doc) = sb.json(args);
        if cfg!(windows) {
            assert_eq!(code, 3, "{args:?}: {doc:#}");
            assert_eq!(doc["error"]["code"], "noeul_not_running", "{args:?}");
            assert!(
                doc["error"]["hint"]
                    .as_str()
                    .unwrap()
                    .contains("noeul start")
            );
        } else {
            assert_eq!(doc["error"]["code"], "unsupported_platform", "{args:?}");
        }
    }
}

#[test]
fn usage_errors_are_contract_documents() {
    let sb = Sandbox::new("usage");
    for (args, command) in [
        (&["capture", "--region", "1,2,3", "--pin"][..], "capture"),
        (&["capture", "--region", "0,0,0,4", "--pin"], "capture"),
        (&["capture", "--region", "0,0,4,4"], "capture"),
        (&["pin", "close"], "pin.close"),
        (&["pin", "close", "pin-1", "--all"], "pin.close"),
        (&["frobnicate"], "noeul"),
        (&["status", "--quiet", "--verbose"], "status"),
    ] {
        let (code, doc) = sb.json(args);
        assert_eq!(code, 2, "{args:?}: {doc:#}");
        assert_eq!(doc["error"]["code"], "usage");
        assert_eq!(doc["command"], command, "{args:?}");
    }
    let out = sb.run(&["capture", "--region", "x", "--pin"]);
    assert_eq!(out.status.code(), Some(2));
    assert!(
        out.stdout.is_empty(),
        "a failing command writes nothing to stdout"
    );
    assert!(String::from_utf8_lossy(&out.stderr).starts_with("noeul: error: "));
}

#[test]
fn hotkeys_commands_follow_the_contract() {
    let sb = Sandbox::with_keys("hotkeys", Keys::Off);
    std::fs::remove_file(sb.hotkeys_file()).unwrap(); // start from the defaults
    let hk = |args: &[&str]| {
        let mut full = vec!["hotkeys"];
        full.extend_from_slice(args);
        let (code, doc) = sb.json(&full);
        assert_valid("hotkeys-cli-v1", &doc);
        (code, doc)
    };
    let (_, list) = hk(&["list"]);
    let ids: Vec<&str> = list["data"]["actions"]
        .as_array()
        .unwrap()
        .iter()
        .map(|a| a["id"].as_str().unwrap())
        .filter(|a| !a.starts_with("nav.") && *a != "window.close")
        .collect();
    assert_eq!(
        ids,
        noeul::hotkeys::IMPLEMENTED,
        "only implemented actions (NOEUL-FR-114)"
    );
    let paste = list["data"]["actions"]
        .as_array()
        .unwrap()
        .iter()
        .find(|a| a["id"] == "paste.float")
        .unwrap();
    assert_eq!(paste["bindings"][0]["keys"], "F3");
    let expected = if cfg!(windows) {
        "not_running"
    } else {
        "unknown"
    };
    assert_eq!(paste["bindings"][0]["registration"], expected);

    let (code, dry) = hk(&["set", "paste.float", "ctrl+alt+shift+f21", "--dry-run"]);
    assert_eq!((code, dry["data"]["written"].as_bool()), (0, Some(false)));
    assert!(!sb.hotkeys_file().exists(), "a dry run writes nothing");
    let (_, set) = hk(&["set", "paste.float", "Ctrl+Alt+Shift+F21"]);
    assert_eq!(set["data"]["written"], true);
    assert!(sb.hotkeys_file().exists());
    let (_, show) = hk(&["show", "paste.float"]);
    assert_eq!(show["data"]["action"]["source"], "user");

    let (code, refused) = hk(&["set", "paste.float", "Ctrl+Alt+Delete"]);
    assert_eq!(
        (code, refused["error"]["code"].as_str()),
        (2, Some("hotkey_reserved"))
    );
    let (code, refused) = hk(&["set", "color.copy", "C"]);
    assert_eq!(
        (code, refused["error"]["code"].as_str()),
        (3, Some("action_not_found")),
        "color.copy is planned (N3), not in this build"
    );
    let (code, needs_yes) = hk(&["reset", "--all"]);
    assert_eq!(
        (code, needs_yes["error"]["code"].as_str()),
        (7, Some("confirmation_required"))
    );
    let (_, check) = hk(&["check"]);
    assert_eq!(check["data"]["summary"]["errors"], 0);
    let (_, export) = hk(&["export"]);
    assert_eq!(export["data"]["document"]["tool"], "noeul");
    let (_, off) = hk(&["disable", "--all"]);
    assert_eq!(off["data"]["written"], true);
    let (_, on) = hk(&["enable", "--all"]);
    assert_eq!(on["data"]["written"], true);
    let (_, reset) = hk(&["reset", "--all", "--yes"]);
    assert_eq!(reset["data"]["written"], true);
    assert!(
        sb.root.join("Roaming/mouv/noeul/hotkeys.json.bak").exists(),
        "reset --all keeps a backup"
    );
}

#[test]
fn check_all_tools_reads_installed_manifests_only() {
    let sb = Sandbox::with_keys("alltools", Keys::Off);
    std::fs::remove_file(sb.hotkeys_file()).unwrap();
    // A hypothetical installed Subak whose manifest binds F3 globally.
    let dir = sb.root.join("Local/Programs/mouv/subak");
    std::fs::create_dir_all(dir.join("versions/0.1.0")).unwrap();
    std::fs::write(
        dir.join("current.json"),
        r#"{"schema":"kim.install","schema_version":1,"tool":"subak","version":"0.1.0"}"#,
    )
    .unwrap();
    let manifest = json!({"schema": "kim.hotkey-manifest", "schema_version": 1, "hotkey_contract": 1,
        "tool": "subak", "version": "0.1.0", "backend": "register",
        "actions": [{"id": "gui.show", "title": "Show Subak", "scopes": ["global"], "defaults": ["F3"]}]});
    std::fs::write(
        dir.join("versions/0.1.0/hotkeys.manifest.json"),
        serde_json::to_vec(&manifest).unwrap(),
    )
    .unwrap();
    let (code, doc) = sb.json(&["hotkeys", "check", "--all-tools"]);
    assert_eq!(code, 0, "conflicts are findings, not failures");
    assert_valid("hotkeys-cli-v1", &doc);
    let tools: Vec<&str> = doc["data"]["tools"]
        .as_array()
        .unwrap()
        .iter()
        .map(|t| t["tool"].as_str().unwrap())
        .collect();
    assert_eq!(tools, ["noeul", "subak"]);
    let c = doc["data"]["conflicts"]
        .as_array()
        .unwrap()
        .iter()
        .find(|c| c["code"] == "cross_tool_global")
        .expect("F3 in two tools is reported");
    assert!(
        c["offers"]
            .as_array()
            .unwrap()
            .iter()
            .any(|o| o["kind"] == "rebind")
    );
    let human =
        String::from_utf8_lossy(&sb.run(&["hotkeys", "check", "--all-tools"]).stdout).to_string();
    assert!(human.contains("fix:"), "{human}");
}
