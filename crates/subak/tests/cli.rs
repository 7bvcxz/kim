//! Integration tests for the placeholder `subak` binary (Step 02), and its CLI contract surface
//! (Step 07, docs/CLI_CONTRACT.md): `capabilities --json` honestly reports a placeholder with no
//! terminal commands, and usage errors with `--json` are structured. JSON outputs are validated
//! against the shared schemas with the same validator as `cargo xtask schema-check`.

#[path = "../../../xtask/src/jsonschema.rs"]
mod jsonschema;

use serde_json::Value;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};

fn subak(args: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_subak"))
        .args(args)
        .output()
        .expect("failed to run subak binary")
}

fn repo() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../..")
}

fn validate(schema: &str, doc: &Value) {
    let path = repo().join(format!("docs/contracts/schemas/{schema}.schema.json"));
    let schema_doc: Value = serde_json::from_slice(&std::fs::read(path).unwrap()).unwrap();
    let errors = jsonschema::Validator::new(&schema_doc)
        .validate(doc)
        .unwrap();
    assert!(errors.is_empty(), "{schema}: {errors:?}\n{doc:#}");
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
    assert!(text.contains("subak capabilities [--json]"));
}

#[test]
fn unknown_arguments_fail_with_usage_exit_code_on_stderr() {
    let out = subak(&["new", "work"]);
    assert_eq!(out.status.code(), Some(2));
    assert!(out.stdout.is_empty(), "errors must not go to stdout");
    let err = String::from_utf8_lossy(&out.stderr);
    assert!(err.starts_with("subak: error:"), "{err}");
    assert!(
        err.contains("no terminal commands are implemented yet"),
        "{err}"
    );
}

#[test]
fn capabilities_are_honest_and_follow_the_contract() {
    let out = subak(&["capabilities", "--json"]);
    assert!(out.status.success());
    assert!(out.stderr.is_empty());
    let doc: Value = serde_json::from_slice(&out.stdout).expect("one JSON document");
    validate("cli-envelope-v1", &doc);
    validate("tool-capabilities-v1", &doc["data"]);
    assert_eq!(doc["command"], "capabilities");
    let data = &doc["data"];
    assert_eq!(data["tool"], "subak");
    assert_eq!(data["version"], env!("CARGO_PKG_VERSION"));
    assert_eq!(
        data["implementation"], "placeholder",
        "no terminal features yet"
    );
    let names: Vec<&str> = data["commands"]
        .as_array()
        .unwrap()
        .iter()
        .map(|c| c["name"].as_str().unwrap())
        .collect();
    assert_eq!(names, ["capabilities"], "only commands that really exist");

    let example: Value = serde_json::from_slice(
        &std::fs::read(repo().join("docs/contracts/examples/tool-capabilities-v1/subak.json"))
            .unwrap(),
    )
    .unwrap();
    assert_eq!(
        data, &example,
        "copy crates/subak/src/capabilities.json to docs/contracts/examples/tool-capabilities-v1/subak.json"
    );

    let human = subak(&["capabilities"]);
    assert!(human.status.success());
    assert!(String::from_utf8_lossy(&human.stdout).contains("placeholder"));
}

#[test]
fn usage_errors_are_structured_with_json() {
    for args in [&["attach", "--json"][..], &["--json"]] {
        let out = subak(args);
        assert_eq!(out.status.code(), Some(2), "{args:?}");
        assert!(
            out.stderr.is_empty(),
            "{args:?}: --json errors are on stdout only"
        );
        let doc: Value = serde_json::from_slice(&out.stdout).expect("one JSON document");
        validate("cli-envelope-v1", &doc);
        assert_eq!(doc["error"]["code"], "usage");
        assert_eq!(doc["error"]["exit_code"], 2);
    }
}
