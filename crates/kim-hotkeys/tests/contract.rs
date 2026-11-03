//! The library against the hotkey contract's machine-checkable fixtures (docs/HOTKEY_CONTRACT.md
//! §15, KIM-FR-040, KIM-FR-046): every conflict and override scenario of
//! `docs/contracts/examples/hotkey-scenarios-v1/` is recomputed with the library and must equal
//! the expectation (conflict messages excepted), the computed documents must validate against
//! `hotkeys-cli-v1`, and the user-layer examples must load (or be rejected) exactly as the schema
//! says.

#[path = "../../../xtask/src/jsonschema.rs"]
mod jsonschema;

use kim_hotkeys::check::{Severity, ToolEntry, ToolState, analyze, report_json};
use kim_hotkeys::edit::{self, Edit, Options};
use kim_hotkeys::keys::{Sequence, Step};
use kim_hotkeys::layer::UserLayer;
use kim_hotkeys::manifest::{BindingSpec, Manifest};
use serde_json::{Value, json};
use std::path::{Path, PathBuf};

fn contracts() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../../docs/contracts")
}

fn read(p: &Path) -> Value {
    serde_json::from_slice(&std::fs::read(p).unwrap_or_else(|e| panic!("{}: {e}", p.display())))
        .unwrap()
}

fn schema(name: &str) -> Value {
    read(&contracts().join(format!("schemas/{name}.schema.json")))
}

fn def(schema: &Value, name: &str) -> Value {
    json!({"$defs": schema["$defs"].clone(), "$ref": format!("#/$defs/{name}")})
}

fn strip_messages(v: &mut Value) {
    match v {
        Value::Object(m) => {
            m.remove("message");
            m.values_mut().for_each(strip_messages);
        }
        Value::Array(a) => a.iter_mut().for_each(strip_messages),
        _ => {}
    }
}

fn valid(v: &jsonschema::Validator<'_>, doc: &Value, what: &str) {
    let errors = v.validate(doc).unwrap();
    assert!(errors.is_empty(), "{what}: {}\n{doc:#}", errors.join("\n"));
}

/// Recomputes one scenario with the library.
fn compute(s: &Value) -> Value {
    let mut tools = Vec::new();
    for t in s["tools"].as_array().unwrap() {
        let manifest = match &t["manifest"] {
            Value::String(n) => read(
                &contracts()
                    .join("examples/kim-hotkey-manifest-v1")
                    .join(format!("{n}.json")),
            ),
            m => m.clone(),
        };
        let manifest = Manifest::from_value(manifest).expect("scenario manifest is valid");
        let layer: Option<UserLayer> = (!t["config"].is_null())
            .then(|| serde_json::from_value(t["config"].clone()).expect("config parses"));
        if let Some(l) = &layer {
            let problems = l.problems(&manifest.tool, Some(&manifest));
            assert!(problems.is_empty(), "scenario config: {problems:?}");
        }
        tools.push(ToolState::new(manifest, layer));
    }
    let taken: Vec<Step> = s["environment"]["taken"]
        .as_array()
        .into_iter()
        .flatten()
        .map(|k| Sequence::parse(k.as_str().unwrap()).unwrap().steps()[0].clone())
        .collect();
    let Some(req) = s.get("request") else {
        let a = analyze(&tools, &taken);
        let entries: Vec<ToolEntry> = tools
            .iter()
            .map(|t| ToolEntry {
                tool: t.id().to_string(),
                version: Some(t.manifest.version.clone()),
                source: "manifest",
                enabled: t.enabled(),
            })
            .collect();
        return json!({"report": report_json(&entries, &a, Severity::Info)});
    };
    let flag = |k: &str| req[k].as_bool().unwrap_or(false);
    let idx = tools
        .iter()
        .position(|t| t.id() == req["tool"].as_str().unwrap())
        .unwrap();
    let action = req["action"].as_str().unwrap().to_string();
    let e = match req["command"].as_str().unwrap() {
        "set" => Edit::Set {
            action,
            bindings: serde_json::from_value::<Vec<BindingSpec>>(req["bindings"].clone()).unwrap(),
        },
        "disable" => Edit::Disable { action },
        _ => Edit::Reset { action },
    };
    let opts = Options {
        replace: flag("replace"),
        allow_conflict: flag("allow_conflict"),
        profile: None,
    };
    match edit::apply(&tools, &taken, idx, &e, &opts) {
        Err(r) => json!({
            "outcome": "refused",
            "error": {"code": r.error.code(), "exit_code": r.error.exit_code()},
            "conflicts": r.conflicts.iter().map(|c| c.to_json()).collect::<Vec<_>>(),
        }),
        Ok(a) => {
            let dry = flag("dry_run");
            let after = if dry {
                tools[idx].layer.clone()
            } else {
                a.layer.clone()
            };
            json!({
                "outcome": "applied",
                "written": !dry,
                "config_after": after.map(|l| serde_json::to_value(l).unwrap()),
                "changes": a.changes.iter().map(|c| c.to_json()).collect::<Vec<_>>(),
                "conflicts": a.conflicts.iter().map(|c| c.to_json()).collect::<Vec<_>>(),
            })
        }
    }
}

#[test]
fn all_contract_scenarios_match_the_library() {
    let cli = schema("hotkeys-cli-v1");
    let (report_def, conflict_def, change_def) = (
        def(&cli, "check_data"),
        def(&cli, "conflict"),
        def(&cli, "change"),
    );
    let report_v = jsonschema::Validator::new(&report_def);
    let conflict_v = jsonschema::Validator::new(&conflict_def);
    let change_v = jsonschema::Validator::new(&change_def);
    let dir = contracts().join("examples/hotkey-scenarios-v1");
    let mut files: Vec<PathBuf> = std::fs::read_dir(&dir)
        .unwrap()
        .flatten()
        .map(|e| e.path())
        .filter(|p| p.extension().is_some_and(|x| x == "json"))
        .collect();
    files.sort();
    assert_eq!(files.len(), 17, "the contract has 17 scenarios");
    let mut failures = Vec::new();
    for path in &files {
        let s = read(path);
        let computed = compute(&s);
        // The library's real output (messages included) is schema-valid.
        if let Some(r) = computed.get("report") {
            valid(&report_v, r, &path.display().to_string());
        }
        for c in computed["conflicts"].as_array().into_iter().flatten() {
            valid(&conflict_v, c, &path.display().to_string());
            assert!(
                c["message"].as_str().is_some_and(|m| !m.is_empty()),
                "every conflict has a message"
            );
        }
        for c in computed["changes"].as_array().into_iter().flatten() {
            valid(&change_v, c, &path.display().to_string());
        }
        let mut computed = computed;
        strip_messages(&mut computed);
        let mut expected = s["expected"].clone();
        strip_messages(&mut expected);
        if computed != expected {
            failures.push(format!(
                "{}:\ncomputed {computed:#}\nexpected {expected:#}",
                path.file_name().unwrap().to_string_lossy()
            ));
        }
    }
    assert!(failures.is_empty(), "{}", failures.join("\n\n"));
}

/// A wrong expectation must be noticed: the comparison is not a rubber stamp.
#[test]
fn a_wrong_expectation_differs() {
    let mut s =
        read(&contracts().join("examples/hotkey-scenarios-v1/02-noeul-f1-shadows-subak-help.json"));
    let mut computed = compute(&s);
    strip_messages(&mut computed);
    s["expected"]["report"]["conflicts"]
        .as_array_mut()
        .unwrap()
        .remove(0);
    s["expected"]["report"]["summary"]["warnings"] = json!(0);
    assert_ne!(computed, s["expected"]);
    assert_eq!(
        computed["report"]["conflicts"][0]["code"],
        "global_shadows_app"
    );
}

#[test]
fn user_layer_examples_load_exactly_as_the_schema_says() {
    let manifest_for = |tool: &str| {
        let p = contracts()
            .join("examples/kim-hotkey-manifest-v1")
            .join(format!("{tool}.json"));
        p.exists()
            .then(|| Manifest::parse(&std::fs::read(p).unwrap()).unwrap())
    };
    let dir = contracts().join("examples/kim-hotkeys-v1");
    let tmp = std::env::temp_dir().join(format!("kim-hotkeys-contract-{}", std::process::id()));
    std::fs::create_dir_all(&tmp).unwrap();
    let check = |path: &Path, want_valid: bool| {
        let doc = read(path);
        let tool = doc["tool"].as_str().unwrap_or("kim").to_string();
        let manifest = manifest_for(&tool).unwrap_or_else(|| manifest_for("kim").unwrap());
        let file = tmp.join("hotkeys.json");
        std::fs::copy(path, &file).unwrap();
        let loaded = kim_hotkeys::store::load(&file, &manifest);
        let ok = loaded.state == kim_hotkeys::store::ConfigState::User;
        assert_eq!(
            ok,
            want_valid,
            "{}: {:?} {:?}",
            path.display(),
            loaded.state,
            loaded.problems
        );
        if !want_valid {
            assert!(loaded.state.safe_mode() && !loaded.problems.is_empty());
        }
    };
    for e in std::fs::read_dir(&dir).unwrap().flatten() {
        if e.path().is_file() {
            check(&e.path(), true);
        }
    }
    for e in std::fs::read_dir(dir.join("invalid")).unwrap().flatten() {
        check(&e.path(), false);
    }
    let _ = std::fs::remove_dir_all(tmp);
}
