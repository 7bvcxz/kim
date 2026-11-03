//! The Kim tool catalog: data model, parsing and validation (docs/CATALOG_FORMAT.md).

pub mod signature;
pub mod status;
pub mod store;

use crate::error::{ErrorCode, KimError, Result};
use crate::time::parse_utc;
use serde::{Deserialize, Serialize};
use std::collections::HashSet;

pub const SCHEMA: &str = "kim.catalog";
pub const SCHEMA_VERSION: u32 = 1;
/// Upper bound for a catalog document, to keep parsing bounded.
pub const MAX_CATALOG_BYTES: u64 = 4 * 1024 * 1024;

#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Serialize)]
pub struct Catalog {
    pub schema: String,
    pub schema_version: u32,
    /// Monotonically increasing; used for rollback protection.
    pub catalog_version: u64,
    pub generated_at: String,
    /// After this instant the catalog is *stale*: still usable offline, but flagged.
    pub expires_at: String,
    pub publisher: String,
    pub tools: Vec<Tool>,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Serialize)]
pub struct Tool {
    pub id: String,
    pub name: String,
    pub summary: String,
    pub description: String,
    #[serde(default)]
    pub homepage: Option<String>,
    pub license: String,
    pub support: Support,
    pub releases: Vec<Release>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum Support {
    Stable,
    Preview,
    InDevelopment,
    Deprecated,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Serialize)]
pub struct Release {
    pub version: String,
    pub channel: Channel,
    pub published_at: String,
    pub release_notes: String,
    #[serde(default)]
    pub min_kim_version: Option<String>,
    #[serde(default)]
    pub dependencies: Vec<Dependency>,
    pub assets: Vec<Asset>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Deserialize, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum Channel {
    Stable,
    Preview,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Serialize)]
pub struct Dependency {
    pub tool: String,
    pub version_req: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Serialize)]
pub struct Asset {
    pub os: String,
    pub arch: String,
    #[serde(default)]
    pub min_os_build: Option<u32>,
    pub url: String,
    pub size: u64,
    pub sha256: String,
    pub kind: AssetKind,
    /// Executable path inside the package, relative, `/`-separated.
    pub entrypoint: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum AssetKind {
    Zip,
    Exe,
}

const KNOWN_OS: &[&str] = &["windows", "linux"];
/// Ids that would collide with CLI words (`kim update check`, …).
const RESERVED_IDS: &[&str] = &["check", "all", "self", "list"];
const KNOWN_ARCH: &[&str] = &["x86_64", "aarch64"];
const MAX_ASSET_BYTES: u64 = 4 * 1024 * 1024 * 1024;

impl Catalog {
    pub fn tool(&self, id: &str) -> Option<&Tool> {
        self.tools.iter().find(|t| t.id == id)
    }
}

/// Parses and fully validates a catalog document. All problems are collected into the error's
/// `details`, not just the first one.
pub fn parse(bytes: &[u8]) -> Result<Catalog> {
    if bytes.len() as u64 > MAX_CATALOG_BYTES {
        return Err(KimError::new(
            ErrorCode::CatalogInvalid,
            format!("catalog is larger than {MAX_CATALOG_BYTES} bytes"),
        ));
    }
    let value: serde_json::Value = serde_json::from_slice(bytes).map_err(|e| {
        KimError::new(
            ErrorCode::CatalogInvalid,
            format!("catalog is not valid JSON: {e}"),
        )
    })?;
    // Check the schema identity before the shape, so a future major version gets a clear error.
    let schema = value.get("schema").and_then(serde_json::Value::as_str);
    let schema_version = value
        .get("schema_version")
        .and_then(serde_json::Value::as_u64);
    if schema != Some(SCHEMA) {
        return Err(KimError::new(
            ErrorCode::CatalogInvalid,
            format!("not a Kim catalog (schema is {schema:?}, expected \"{SCHEMA}\")"),
        ));
    }
    if schema_version != Some(u64::from(SCHEMA_VERSION)) {
        return Err(KimError::new(
            ErrorCode::CatalogInvalid,
            format!(
                "unsupported catalog schema_version {schema_version:?}; this Kim reads version {SCHEMA_VERSION}"
            ),
        )
        .with_hint("a newer Kim may be required to read this catalog"));
    }
    let catalog: Catalog = serde_json::from_value(value).map_err(|e| {
        KimError::new(
            ErrorCode::CatalogInvalid,
            format!("catalog has an invalid shape: {e}"),
        )
    })?;
    let problems = validate(&catalog);
    if problems.is_empty() {
        Ok(catalog)
    } else {
        Err(KimError::new(
            ErrorCode::CatalogInvalid,
            format!("catalog violates {} format rule(s)", problems.len()),
        )
        .with_details(problems))
    }
}

/// Semantic validation beyond the JSON shape. Returns every problem found.
pub fn validate(c: &Catalog) -> Vec<String> {
    let mut p = Vec::new();
    let generated = parse_utc(&c.generated_at);
    let expires = parse_utc(&c.expires_at);
    if generated.is_none() {
        p.push(format!(
            "generated_at `{}` is not an RFC 3339 UTC timestamp",
            c.generated_at
        ));
    }
    if expires.is_none() {
        p.push(format!(
            "expires_at `{}` is not an RFC 3339 UTC timestamp",
            c.expires_at
        ));
    }
    if let (Some(g), Some(e)) = (generated, expires)
        && e <= g
    {
        p.push("expires_at must be later than generated_at".into());
    }
    if c.catalog_version == 0 {
        p.push("catalog_version must be at least 1".into());
    }
    if c.publisher.trim().is_empty() {
        p.push("publisher must not be empty".into());
    }

    let ids: HashSet<&str> = c.tools.iter().map(|t| t.id.as_str()).collect();
    let mut seen = HashSet::new();
    for tool in &c.tools {
        let t = &tool.id;
        if !seen.insert(t.as_str()) {
            p.push(format!("tool `{t}`: duplicate id"));
        }
        if !is_tool_id(t) {
            p.push(format!(
                "tool `{t}`: id must match ^[a-z][a-z0-9-]{{1,31}}$"
            ));
        }
        if RESERVED_IDS.contains(&t.as_str()) {
            p.push(format!("tool `{t}`: id is reserved by the Kim CLI"));
        }
        if tool.name.trim().is_empty() || tool.name.chars().count() > 64 {
            p.push(format!("tool `{t}`: name must be 1-64 characters"));
        }
        if tool.summary.trim().is_empty() || tool.summary.chars().count() > 120 {
            p.push(format!("tool `{t}`: summary must be 1-120 characters"));
        }
        if tool.license.trim().is_empty() {
            p.push(format!("tool `{t}`: license must not be empty"));
        }
        if let Some(h) = &tool.homepage
            && !h.starts_with("https://")
        {
            p.push(format!("tool `{t}`: homepage must be an https:// URL"));
        }
        let mut versions = HashSet::new();
        for r in &tool.releases {
            let v = &r.version;
            let ctx = format!("tool `{t}` release `{v}`");
            match semver::Version::parse(v) {
                Ok(parsed) => {
                    if !versions.insert(parsed) {
                        p.push(format!("{ctx}: duplicate version"));
                    }
                }
                Err(e) => p.push(format!("{ctx}: version is not SemVer ({e})")),
            }
            if parse_utc(&r.published_at).is_none() {
                p.push(format!(
                    "{ctx}: published_at is not an RFC 3339 UTC timestamp"
                ));
            }
            if let Some(min) = &r.min_kim_version
                && semver::Version::parse(min).is_err()
            {
                p.push(format!("{ctx}: min_kim_version `{min}` is not SemVer"));
            }
            for d in &r.dependencies {
                if d.tool == *t {
                    p.push(format!("{ctx}: depends on itself"));
                } else if !ids.contains(d.tool.as_str()) {
                    p.push(format!("{ctx}: depends on unknown tool `{}`", d.tool));
                }
                if semver::VersionReq::parse(&d.version_req).is_err() {
                    p.push(format!(
                        "{ctx}: dependency `{}` has invalid version_req `{}`",
                        d.tool, d.version_req
                    ));
                }
            }
            if r.assets.is_empty() {
                p.push(format!("{ctx}: has no assets"));
            }
            let mut targets = HashSet::new();
            for a in &r.assets {
                let actx = format!("{ctx} asset {}/{}", a.os, a.arch);
                if !KNOWN_OS.contains(&a.os.as_str()) {
                    p.push(format!("{actx}: unknown os (expected one of {KNOWN_OS:?})"));
                }
                if !KNOWN_ARCH.contains(&a.arch.as_str()) {
                    p.push(format!(
                        "{actx}: unknown arch (expected one of {KNOWN_ARCH:?})"
                    ));
                }
                if !targets.insert((a.os.as_str(), a.arch.as_str())) {
                    p.push(format!("{actx}: duplicate os/arch in one release"));
                }
                if crate::fetch::check_source_url(&a.url).is_err() || a.url.starts_with("file:") {
                    p.push(format!(
                        "{actx}: url must be https:// (http:// only for loopback)"
                    ));
                }
                if a.size == 0 || a.size > MAX_ASSET_BYTES {
                    p.push(format!("{actx}: size must be between 1 byte and 4 GiB"));
                }
                if a.sha256.len() != 64
                    || !a
                        .sha256
                        .bytes()
                        .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
                {
                    p.push(format!(
                        "{actx}: sha256 must be 64 lowercase hex characters"
                    ));
                }
                if !is_safe_relative_path(&a.entrypoint) {
                    p.push(format!(
                        "{actx}: entrypoint must be a relative path without `..`, drive or root"
                    ));
                }
            }
        }
    }
    p
}

fn is_tool_id(id: &str) -> bool {
    let b = id.as_bytes();
    (2..=32).contains(&b.len())
        && b[0].is_ascii_lowercase()
        && b.iter()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || *c == b'-')
}

fn is_safe_relative_path(p: &str) -> bool {
    !p.is_empty()
        && !p.starts_with('/')
        && !p.starts_with('\\')
        && !p.contains(':')
        && p.split(['/', '\\'])
            .all(|seg| !seg.is_empty() && seg != ".." && seg != ".")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn minimal() -> serde_json::Value {
        serde_json::json!({
            "schema": "kim.catalog", "schema_version": 1, "catalog_version": 3,
            "generated_at": "2026-11-04T00:00:00Z", "expires_at": "2027-05-04T00:00:00Z",
            "publisher": "mouv",
            "tools": [{
                "id": "subak", "name": "Subak", "summary": "Persistent terminal",
                "description": "d", "license": "MIT OR Apache-2.0", "support": "preview",
                "releases": [{
                    "version": "0.1.0", "channel": "preview",
                    "published_at": "2026-11-04T00:00:00Z", "release_notes": "first",
                    "assets": [{
                        "os": "windows", "arch": "x86_64", "min_os_build": 17763,
                        "url": "https://example.invalid/subak.zip", "size": 10,
                        "sha256": "0".repeat(64), "kind": "zip", "entrypoint": "subak.exe"
                    }]
                }]
            }]
        })
    }

    fn parse_value(v: &serde_json::Value) -> Result<Catalog> {
        parse(&serde_json::to_vec(v).unwrap())
    }

    #[test]
    fn minimal_catalog_is_valid() {
        let c = parse_value(&minimal()).expect("valid");
        assert_eq!(
            c.tool("subak").unwrap().releases[0].assets[0].kind,
            AssetKind::Zip
        );
    }

    #[test]
    fn unknown_fields_are_ignored_for_forward_compatibility() {
        let mut v = minimal();
        v["new_top_level_field"] = serde_json::json!(true);
        v["tools"][0]["future"] = serde_json::json!({"x": 1});
        assert!(parse_value(&v).is_ok());
    }

    #[test]
    fn wrong_schema_and_future_version_are_rejected_clearly() {
        let mut v = minimal();
        v["schema"] = "something.else".into();
        assert!(
            parse_value(&v)
                .unwrap_err()
                .message
                .contains("not a Kim catalog")
        );
        let mut v = minimal();
        v["schema_version"] = 2.into();
        let e = parse_value(&v).unwrap_err();
        assert_eq!(e.code, ErrorCode::CatalogInvalid);
        assert!(e.hint.unwrap().contains("newer Kim"));
    }

    #[test]
    fn malformed_json_and_shapes_are_catalog_invalid() {
        assert_eq!(
            parse(b"{not json").unwrap_err().code,
            ErrorCode::CatalogInvalid
        );
        let mut v = minimal();
        v["tools"][0]["support"] = "legendary".into();
        assert_eq!(parse_value(&v).unwrap_err().code, ErrorCode::CatalogInvalid);
    }

    #[test]
    fn every_semantic_problem_is_reported() {
        let mut v = minimal();
        let a = &mut v["tools"][0]["releases"][0]["assets"][0];
        a["url"] = "http://evil.example/subak.zip".into();
        a["sha256"] = "XYZ".into();
        a["entrypoint"] = "../../Windows/System32/evil.exe".into();
        a["arch"] = "sparc".into();
        v["tools"][0]["releases"][0]["version"] = "1.0".into();
        v["tools"][0]["releases"][0]["dependencies"] =
            serde_json::json!([{"tool": "ghost", "version_req": ">=1"}]);
        v["expires_at"] = "2020-01-01T00:00:00Z".into();
        let e = parse_value(&v).unwrap_err();
        let all = e.details.join("\n");
        for needle in [
            "url must be https",
            "sha256 must be 64",
            "entrypoint must be a relative path",
            "unknown arch",
            "not SemVer",
            "unknown tool `ghost`",
            "expires_at must be later",
        ] {
            assert!(all.contains(needle), "missing `{needle}` in:\n{all}");
        }
    }

    #[test]
    fn duplicate_tools_versions_and_targets_are_rejected() {
        let mut v = minimal();
        let tool = v["tools"][0].clone();
        v["tools"].as_array_mut().unwrap().push(tool);
        let r = v["tools"][0]["releases"][0].clone();
        v["tools"][0]["releases"].as_array_mut().unwrap().push(r);
        let asset = v["tools"][0]["releases"][0]["assets"][0].clone();
        v["tools"][0]["releases"][0]["assets"]
            .as_array_mut()
            .unwrap()
            .push(asset);
        let all = parse_value(&v).unwrap_err().details.join("\n");
        assert!(all.contains("duplicate id"), "{all}");
        assert!(all.contains("duplicate version"), "{all}");
        assert!(all.contains("duplicate os/arch"), "{all}");
    }

    #[test]
    fn reserved_ids_are_rejected() {
        let mut v = minimal();
        v["tools"][0]["id"] = "check".into();
        let all = parse_value(&v).unwrap_err().details.join(
            "
",
        );
        assert!(all.contains("reserved"), "{all}");
    }

    #[test]
    fn oversized_documents_are_refused_before_parsing() {
        let big = vec![b' '; usize::try_from(MAX_CATALOG_BYTES).unwrap() + 1];
        assert!(parse(&big).unwrap_err().message.contains("larger than"));
    }

    #[test]
    fn safe_paths() {
        assert!(is_safe_relative_path("bin/subak.exe"));
        for bad in ["", "/abs", "\\abs", "C:/x", "a/../b", "./a", "a//b"] {
            assert!(!is_safe_relative_path(bad), "{bad}");
        }
    }
}
