//! Step 19 (ADR 0048): the Host, the headless CLI and the terminal model they share do not depend
//! on the window's search, clipboard, link or export code. The search engine is headless
//! (`subak_term::search`, `subak_term::links`) and only the window (`src/gui/`) calls it; the
//! Host's model (`subak_term::screen`) does not know about it. Checked on the sources, so a
//! later change that wires one into the other fails here, beside `cargo xtask deps-check` (no
//! GUI crate in a headless package) and the release-binary import check of docs/SUBAK_GUI.md
//! §12.7. `SUBAK-FR-047`, `KIM-FR-012`.

use std::path::{Path, PathBuf};

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

/// The lines of `path` outside its `#[cfg(test)]` module (unit tests may use anything).
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

#[test]
fn the_host_and_the_cli_never_reach_the_window_search_or_clipboard_code() {
    let src = Path::new(env!("CARGO_MANIFEST_DIR")).join("src");
    let mut files = Vec::new();
    sources(&src, &mut files);
    let headless: Vec<&PathBuf> = files
        .iter()
        .filter(|p| !p.starts_with(src.join("gui")) && !p.starts_with(src.join("bin")))
        .collect();
    assert!(
        headless.iter().any(|p| p.ends_with("host.rs")) && headless.len() > 10,
        "{headless:?}"
    );
    let forbidden = [
        "gui::",
        "subak_term::search",
        "subak_term::links",
        "render::html",
        "PasteCheck",
        "regex",
    ];
    let mut found = Vec::new();
    for f in &headless {
        for (n, line) in code(f) {
            // The library declares the window's module (and the GUI binary calls into it).
            if line.trim() == "pub mod gui;" {
                continue;
            }
            for word in forbidden {
                if line.contains(word) {
                    found.push(format!("{}:{n}: {}", f.display(), line.trim()));
                }
            }
        }
    }
    assert!(
        found.is_empty(),
        "headless code reaches GUI search code:\n{}",
        found.join("\n")
    );
}

#[test]
fn the_terminal_model_the_host_runs_does_not_know_about_search() {
    let term = Path::new(env!("CARGO_MANIFEST_DIR")).join("../subak-term/src/screen.rs");
    let found: Vec<String> = code(&term)
        .into_iter()
        .filter(|(_, l)| {
            l.contains("search::") || l.contains("regex") || l.contains("crate::links")
        })
        .map(|(n, l)| format!("{n}: {}", l.trim()))
        .collect();
    assert!(found.is_empty(), "{}", found.join("\n"));
}
