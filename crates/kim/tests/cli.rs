//! Integration tests for the placeholder `kim` binary (Step 02).

use std::process::Command;

fn kim(args: &[&str]) -> std::process::Output {
    Command::new(env!("CARGO_BIN_EXE_kim"))
        .args(args)
        .output()
        .expect("failed to run kim binary")
}

#[test]
fn version_prints_package_version_and_succeeds() {
    let out = kim(&["--version"]);
    assert!(out.status.success());
    assert_eq!(
        String::from_utf8_lossy(&out.stdout).trim(),
        format!("kim {}", env!("CARGO_PKG_VERSION"))
    );
    assert!(out.stderr.is_empty());
}

#[test]
fn help_is_honest_about_placeholder_status() {
    let out = kim(&["--help"]);
    assert!(out.status.success());
    let text = String::from_utf8_lossy(&out.stdout);
    assert!(text.contains("pre-alpha placeholder"));
    assert!(text.contains("never installs, updates or starts other tools"));
}

#[test]
fn unknown_arguments_fail_with_usage_exit_code_on_stderr() {
    let out = kim(&["install", "subak"]);
    assert_eq!(out.status.code(), Some(2));
    assert!(out.stdout.is_empty(), "errors must not go to stdout");
    assert!(String::from_utf8_lossy(&out.stderr).contains("no commands are implemented yet"));
}
