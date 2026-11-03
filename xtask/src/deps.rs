//! Dependency-boundary checks on `Cargo.lock` (ADR 0002, ADR 0014): headless packages must not
//! (transitively) depend on GUI crates.

use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;

/// Crates that belong only to GUI binaries.
pub const GUI_CRATES: &[&str] = &[
    "eframe",
    "egui",
    "egui-winit",
    "egui_glow",
    "egui-wgpu",
    "winit",
    "glow",
    "glutin",
    "wgpu",
    "accesskit_winit",
];

/// Packages that must stay headless.
pub const HEADLESS: &[&str] = &["kim", "kim-hotkeys", "subak"];

/// Parses `[[package]]` entries of a Cargo.lock into name → dependency names.
pub fn parse_lock(text: &str) -> BTreeMap<String, Vec<String>> {
    let mut graph: BTreeMap<String, Vec<String>> = BTreeMap::new();
    let mut name: Option<String> = None;
    let mut in_deps = false;
    for line in text.lines() {
        let l = line.trim();
        if l == "[[package]]" {
            name = None;
            in_deps = false;
        } else if let Some(n) = l
            .strip_prefix("name = \"")
            .and_then(|s| s.strip_suffix('"'))
        {
            name = Some(n.to_string());
            graph.entry(n.to_string()).or_default();
        } else if l.starts_with("dependencies = [") {
            in_deps = true;
        } else if in_deps && l == "]" {
            in_deps = false;
        } else if in_deps && let Some(n) = &name {
            // `"serde 1.0.1",` or `"serde",`
            let dep = l
                .trim_matches(|c| c == '"' || c == ',')
                .split(' ')
                .next()
                .unwrap_or("");
            if !dep.is_empty() {
                graph.get_mut(n).expect("entry").push(dep.to_string());
            }
        }
    }
    graph
}

/// All packages reachable from `root`.
pub fn closure(graph: &BTreeMap<String, Vec<String>>, root: &str) -> BTreeSet<String> {
    let mut seen = BTreeSet::new();
    let mut stack = vec![root.to_string()];
    while let Some(n) = stack.pop() {
        if seen.insert(n.clone()) {
            for d in graph.get(&n).into_iter().flatten() {
                stack.push(d.clone());
            }
        }
    }
    seen
}

/// Returns violations: headless packages that reach a GUI crate.
pub fn check(root: &Path) -> Result<Vec<String>, String> {
    let text =
        std::fs::read_to_string(root.join("Cargo.lock")).map_err(|e| format!("Cargo.lock: {e}"))?;
    let graph = parse_lock(&text);
    let mut problems = Vec::new();
    for pkg in HEADLESS {
        if !graph.contains_key(*pkg) {
            problems.push(format!(
                "headless package `{pkg}` is missing from Cargo.lock"
            ));
            continue;
        }
        let reach = closure(&graph, pkg);
        for gui in GUI_CRATES {
            if reach.contains(*gui) {
                problems.push(format!(
                    "headless package `{pkg}` depends on GUI crate `{gui}`"
                ));
            }
        }
    }
    Ok(problems)
}

#[cfg(test)]
mod tests {
    use super::*;

    const LOCK: &str = r#"
[[package]]
name = "app"
version = "0.1.0"
dependencies = [
 "lib",
]

[[package]]
name = "lib"
version = "0.1.0"
dependencies = [
 "winit 0.30.0",
]

[[package]]
name = "winit"
version = "0.30.0"
"#;

    #[test]
    fn transitive_gui_dependencies_are_found() {
        let g = parse_lock(LOCK);
        let reach = closure(&g, "app");
        assert!(reach.contains("winit"));
        assert!(!closure(&g, "winit").contains("app"));
    }

    #[test]
    fn repository_headless_binaries_have_no_gui_dependencies() {
        let root = Path::new(env!("CARGO_MANIFEST_DIR")).parent().unwrap();
        let problems = check(root).expect("Cargo.lock readable");
        assert!(problems.is_empty(), "{}", problems.join("\n"));
        // And the GUI package really does use the GUI stack (the check is not vacuous).
        let g = parse_lock(&std::fs::read_to_string(root.join("Cargo.lock")).unwrap());
        assert!(closure(&g, "kim-manager").contains("eframe"));
    }
}
