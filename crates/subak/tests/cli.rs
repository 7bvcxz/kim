//! Integration tests for the placeholder `subak` binary (Step 02).

use std::process::Command;

fn subak(args: &[&str]) -> std::process::Output {
    Command::new(env!("CARGO_BIN_EXE_subak"))
        .args(args)
        .output()
        .expect("failed to run subak binary")
}

#[test]
fn version_prints_package_version_and_succeeds() {
    let out = subak(&["--version"]);
    assert!(out.status.success());
    assert_eq!(
        String::from_utf8_lossy(&out.stdout).trim(),
        format!("subak {}", env!("CARGO_PKG_VERSION"))
    );
    assert!(out.stderr.is_empty());
}

#[test]
fn help_is_honest_about_placeholder_status() {
    let out = subak(&["--help"]);
    assert!(out.status.success());
    let text = String::from_utf8_lossy(&out.stdout);
    assert!(text.contains("pre-alpha placeholder"));
    assert!(text.contains("starts no background process"));
}

#[test]
fn unknown_arguments_fail_with_usage_exit_code_on_stderr() {
    let out = subak(&["new", "work"]);
    assert_eq!(out.status.code(), Some(2));
    assert!(out.stdout.is_empty(), "errors must not go to stdout");
    assert!(String::from_utf8_lossy(&out.stderr).contains("no commands are implemented yet"));
}
