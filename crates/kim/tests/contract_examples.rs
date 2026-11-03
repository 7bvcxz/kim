//! The published JSON schema (docs/contracts/schemas/kim-catalog-v1.schema.json) and Kim's Rust
//! parser must agree: every valid example parses, every invalid example is rejected.

use std::path::Path;

fn examples(sub: &str) -> Vec<std::path::PathBuf> {
    let dir = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../docs/contracts/examples/kim-catalog-v1")
        .join(sub);
    let mut v: Vec<_> = std::fs::read_dir(&dir)
        .unwrap_or_else(|e| panic!("{}: {e}", dir.display()))
        .flatten()
        .map(|e| e.path())
        .filter(|p| p.extension().is_some_and(|x| x == "json"))
        .collect();
    v.sort();
    assert!(!v.is_empty(), "no examples in {}", dir.display());
    v
}

#[test]
fn valid_schema_examples_parse() {
    for p in examples("") {
        let bytes = std::fs::read(&p).unwrap();
        if let Err(e) = kim::catalog::parse(&bytes) {
            panic!("{} should parse: {e} {:?}", p.display(), e.details);
        }
    }
}

#[test]
fn invalid_schema_examples_are_rejected_by_the_parser() {
    for p in examples("invalid") {
        let bytes = std::fs::read(&p).unwrap();
        let err = kim::catalog::parse(&bytes).expect_err(&p.display().to_string());
        assert_eq!(
            err.code,
            kim::error::ErrorCode::CatalogInvalid,
            "{}",
            p.display()
        );
    }
}

#[test]
fn bundled_example_matches_the_bundled_catalog() {
    let example = std::fs::read(
        Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../docs/contracts/examples/kim-catalog-v1/bundled.json"),
    )
    .unwrap();
    assert_eq!(
        example,
        kim::catalog::store::BUNDLED_CATALOG,
        "refresh docs/contracts/examples/kim-catalog-v1/bundled.json from catalog/catalog.json"
    );
}
