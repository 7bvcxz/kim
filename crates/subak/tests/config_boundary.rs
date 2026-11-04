//! Step 20 (ADR 0051 §3): the Host never reads Subak's settings. `config.toml` — the window's
//! appearance and the shell profiles — is read by the window and the `subak config`, `profile`
//! and `new` commands only; a profile reaches the Host as the explicit `command`, `cwd` and
//! `env` of `terminal.create`, and IPC v1's `profile` parameter stays refused (Step 33). So a
//! settings change can never change a running Terminal, and a broken settings file can never
//! stop the Host. Checked on the sources, beside the run-time check of
//! `tests/config_cli.rs` and `tests/gui_settings.rs`. `SUBAK-FR-046`, `SUBAK-FR-003`.

use std::path::{Path, PathBuf};

/// The lines of `path` outside its `#[cfg(test)]` module, without comments.
fn code(path: &Path) -> Vec<(usize, String)> {
    let text = std::fs::read_to_string(path).unwrap();
    let mut out = Vec::new();
    for (i, line) in text.lines().enumerate() {
        if line.trim_start().starts_with("#[cfg(test)]") {
            break;
        }
        let l = line.trim_start();
        if l.starts_with("//") {
            continue;
        }
        out.push((i + 1, line.to_string()));
    }
    out
}

fn sources(dir: &Path, out: &mut Vec<PathBuf>) {
    for e in std::fs::read_dir(dir).unwrap() {
        let p = e.unwrap().path();
        if p.is_dir() {
            sources(&p, out);
        } else if p.extension().is_some_and(|x| x == "rs") {
            out.push(p);
        }
    }
}

#[test]
fn the_host_never_reads_the_settings_or_the_profiles() {
    let src = Path::new(env!("CARGO_MANIFEST_DIR")).join("src");
    let mut host_side: Vec<PathBuf> = ["host.rs", "session.rs", "pty.rs", "proto.rs", "redact.rs"]
        .iter()
        .map(|f| src.join(f))
        .collect();
    sources(&src.join("win"), &mut host_side);
    assert!(host_side.len() > 6, "{host_side:?}");
    let forbidden = ["config::", "config_cmd", "config.toml", "toml::"];
    let mut found = Vec::new();
    for f in &host_side {
        for (n, line) in code(f) {
            for word in forbidden {
                if line.contains(word) {
                    found.push(format!("{}:{n}: {}", f.display(), line.trim()));
                }
            }
        }
    }
    assert!(
        found.is_empty(),
        "Host code reaches the settings:\n{}",
        found.join("\n")
    );
    // IPC v1's `profile` parameter is still refused by the Host (profiles are the clients').
    let host = std::fs::read_to_string(src.join("host.rs")).unwrap();
    assert!(
        host.contains("`profile` is part of IPC v1 but not implemented by this Host yet (Step 33)"),
        "the Host refuses terminal.create's profile"
    );
}

#[test]
fn settings_are_read_only_by_the_window_and_the_settings_commands() {
    let src = Path::new(env!("CARGO_MANIFEST_DIR")).join("src");
    let mut files = Vec::new();
    sources(&src, &mut files);
    let readers: Vec<String> = files
        .iter()
        .filter(|f| code(f).iter().any(|(_, l)| l.contains("config::load(")))
        .map(|f| {
            f.strip_prefix(&src)
                .unwrap()
                .display()
                .to_string()
                .replace('\\', "/")
        })
        .collect();
    let mut readers = readers;
    readers.sort();
    assert_eq!(
        readers,
        ["config_cmd.rs", "gui/window.rs"],
        "only the settings commands (and `new` through them) and the window load config.toml"
    );
}
