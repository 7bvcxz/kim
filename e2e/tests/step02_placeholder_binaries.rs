//! E2E — Step 02: independent binaries (originally placeholders; kept valid as they grow).
//!
//! Scenario: a user runs the freshly built release `kim` and `subak` binaries. Each answers
//! `--version`/`--help`, rejects unknown commands with exit code 2 on stderr, and writes nothing
//! to the user's data directories (KIM-FR-002: running Kim installs/starts nothing).

use kim_e2e::{TempDir, bin, run_sandboxed};

fn check_tool(name: &str) {
    let exe = bin(name);
    let sandbox = TempDir::new(name);

    let version = run_sandboxed(&exe, &["--version"], &sandbox);
    assert!(version.status.success());
    assert!(String::from_utf8_lossy(&version.stdout).starts_with(&format!("{name} ")));

    let help = run_sandboxed(&exe, &["--help"], &sandbox);
    assert!(help.status.success());
    let help = String::from_utf8_lossy(&help.stdout).to_lowercase();
    assert!(help.contains(name), "help names the tool");
    // Both tools promise not to act on their own (KIM-SEC-001, KIM-FR-012).
    assert!(help.contains("never") || help.contains("no "), "{help}");

    let bad = run_sandboxed(&exe, &["definitely-not-a-command"], &sandbox);
    assert_eq!(bad.status.code(), Some(2));
    assert!(bad.stdout.is_empty());

    assert_eq!(
        sandbox.entry_count(),
        0,
        "{name} must not write to user data dirs"
    );
}

#[test]
#[ignore = "e2e: run via `cargo xtask e2e` only (AGENTS.md §4)"]
fn kim_binary_is_independent_and_side_effect_free() {
    check_tool("kim");
}

#[test]
#[ignore = "e2e: run via `cargo xtask e2e` only (AGENTS.md §4)"]
fn subak_binary_is_independent_and_side_effect_free() {
    check_tool("subak");
}
