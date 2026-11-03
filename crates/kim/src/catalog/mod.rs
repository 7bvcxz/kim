//! The Kim tool catalog: data model, parsing and validation (docs/CATALOG_FORMAT.md).

pub mod details;
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
    // --- Optional tool metadata (CATALOG_FORMAT §2.1, Step 47). Every field is additive: a
    // catalog without them is unchanged, and older Kim versions ignore them.
    /// Short feature summaries, one line each.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub features: Vec<String>,
    /// How the program runs (`on-demand`, `window`, `tray`, `host`; see [`KNOWN_RUNS`]).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub runs: Option<String>,
    /// How users and agents start it: GUI, headless CLI commands, MCP server.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub entry_points: Vec<EntryPoint>,
    /// Documentation and integration guides (`https://` only).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub links: Vec<Link>,
    /// A summary of the tool's hotkey manifest (HOTKEY_CONTRACT §3.4).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub shortcuts: Option<Shortcuts>,
    /// Security-relevant things the tool does (see [`KNOWN_PERMISSIONS`]), disclosed before an
    /// install.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub permissions: Vec<Permission>,
}

/// One way to start a tool (CATALOG_FORMAT §2.1).
#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Serialize)]
pub struct EntryPoint {
    /// `gui`, `cli` or `mcp` (see [`KNOWN_ENTRY_KINDS`]). A `cli` entry point is headless: it
    /// never opens a window (CLI_CONTRACT, `KIM-FR-012`).
    pub kind: String,
    /// What to run, e.g. `noeul capture --region <x,y,w,h> --output <file> --json`.
    pub command: String,
    pub summary: String,
    pub status: EntryStatus,
}

/// Whether an entry point exists in a released version or is only planned.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum EntryStatus {
    Available,
    Planned,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Serialize)]
pub struct Link {
    /// `docs`, `mcp`, `shortcuts`, … (see [`KNOWN_LINK_KINDS`]).
    pub kind: String,
    pub title: String,
    pub url: String,
}

/// The default shortcuts a tool ships, as its hotkey manifest declares them.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Serialize)]
pub struct Shortcuts {
    /// File name of the manifest inside the package (`hotkeys.manifest.json`).
    pub manifest: String,
    /// The strongest input mechanism the tool uses: `focus`, `register` or `hook`.
    pub backend: String,
    pub actions: Vec<ShortcutSummary>,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Serialize)]
pub struct ShortcutSummary {
    pub action: String,
    pub title: String,
    /// `global`, `app` or `context:<id>`: where the default bindings are active.
    pub scope: String,
    /// Default bindings in canonical key syntax (HOTKEY_CONTRACT §5); empty = unbound.
    #[serde(default)]
    pub keys: Vec<String>,
    /// `false` for keys the contract fixes (e.g. Ddalgak's emergency chord).
    #[serde(default = "yes", skip_serializing_if = "is_true")]
    pub bindable: bool,
}

/// One security-relevant capability of a tool.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Serialize)]
pub struct Permission {
    pub id: String,
    /// When and how the tool uses it, in one sentence.
    pub detail: String,
    /// `true` when the tool uses it only after the user turned it on.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub opt_in: bool,
}

fn yes() -> bool {
    true
}

#[allow(clippy::trivially_copy_pass_by_ref)] // serde's skip_serializing_if passes a reference
fn is_true(b: &bool) -> bool {
    *b
}

/// Values of `tools[].runs` this Kim describes. Other values are shown as they are.
pub const KNOWN_RUNS: &[(&str, &str)] = &[
    (
        "on-demand",
        "Runs only while a command or window needs it, then exits",
    ),
    ("window", "Runs while its window is open"),
    (
        "tray",
        "Keeps running in the notification area until you quit it",
    ),
    (
        "host",
        "Keeps a background host process while it owns sessions",
    ),
];

/// Permission ids this Kim describes, with the label Kim shows. Other ids are shown as they are
/// (with the catalog's `detail`).
pub const KNOWN_PERMISSIONS: &[(&str, &str)] = &[
    ("global-hotkeys", "Registers system-wide shortcuts"),
    ("keyboard-hook", "Installs a low-level keyboard hook"),
    ("screen-capture", "Captures the screen"),
    ("clipboard", "Reads and writes the clipboard"),
    ("input-injection", "Injects keyboard and mouse input"),
    ("launch-programs", "Starts programs and opens websites"),
    (
        "window-control",
        "Moves, activates and closes other windows",
    ),
    ("autostart", "Can start when you sign in"),
    ("network", "Uses the network"),
];

pub const KNOWN_ENTRY_KINDS: &[&str] = &["gui", "cli", "mcp"];
pub const KNOWN_LINK_KINDS: &[&str] = &["docs", "mcp", "shortcuts", "source"];
pub const KNOWN_BACKENDS: &[&str] = &["focus", "register", "hook"];

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
        validate_metadata(tool, &mut p);
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

/// Characters, not bytes: summaries may be Korean.
fn len_ok(s: &str, max: usize) -> bool {
    !s.trim().is_empty() && s.chars().count() <= max
}

/// Rules for the optional tool metadata (CATALOG_FORMAT §2.1).
fn validate_metadata(tool: &Tool, p: &mut Vec<String>) {
    let t = &tool.id;
    if tool.features.len() > 16 {
        p.push(format!("tool `{t}`: at most 16 features"));
    }
    for (i, f) in tool.features.iter().enumerate() {
        if !len_ok(f, 120) {
            p.push(format!("tool `{t}` feature {i}: must be 1-120 characters"));
        }
    }
    if let Some(r) = &tool.runs
        && !is_tool_id(r)
    {
        p.push(format!(
            "tool `{t}`: runs `{r}` must match ^[a-z][a-z0-9-]{{1,31}}$"
        ));
    }
    if tool.entry_points.len() > 32 {
        p.push(format!("tool `{t}`: at most 32 entry points"));
    }
    for e in &tool.entry_points {
        let ctx = format!("tool `{t}` entry point `{}`", e.command);
        if !is_tool_id(&e.kind) {
            p.push(format!("{ctx}: kind must match ^[a-z][a-z0-9-]{{1,31}}$"));
        }
        if !len_ok(&e.command, 200) || !len_ok(&e.summary, 200) {
            p.push(format!(
                "{ctx}: command and summary must be 1-200 characters"
            ));
        }
        // No fake completeness: a tool nobody can download has nothing available to run.
        if tool.releases.is_empty() && e.status == EntryStatus::Available {
            p.push(format!(
                "{ctx}: a tool without releases can only declare planned entry points"
            ));
        }
    }
    if tool.links.len() > 16 {
        p.push(format!("tool `{t}`: at most 16 links"));
    }
    for l in &tool.links {
        let ctx = format!("tool `{t}` link `{}`", l.url);
        if !is_tool_id(&l.kind) {
            p.push(format!("{ctx}: kind must match ^[a-z][a-z0-9-]{{1,31}}$"));
        }
        if !len_ok(&l.title, 80) {
            p.push(format!("{ctx}: title must be 1-80 characters"));
        }
        if !l.url.starts_with("https://") || l.url.len() <= "https://".len() {
            p.push(format!("{ctx}: url must be an https:// URL"));
        }
    }
    if let Some(s) = &tool.shortcuts {
        validate_shortcuts(t, s, p);
    }
    if tool.permissions.len() > 32 {
        p.push(format!("tool `{t}`: at most 32 permissions"));
    }
    let mut seen = HashSet::new();
    for perm in &tool.permissions {
        let ctx = format!("tool `{t}` permission `{}`", perm.id);
        if !is_tool_id(&perm.id) {
            p.push(format!("{ctx}: id must match ^[a-z][a-z0-9-]{{1,31}}$"));
        }
        if !seen.insert(perm.id.as_str()) {
            p.push(format!("{ctx}: duplicate id"));
        }
        if !len_ok(&perm.detail, 200) {
            p.push(format!("{ctx}: detail must be 1-200 characters"));
        }
    }
}

fn validate_shortcuts(t: &str, s: &Shortcuts, p: &mut Vec<String>) {
    use kim_hotkeys::keys::{Sequence, is_action_id, is_name_id};
    if !is_safe_relative_path(&s.manifest)
        || s.manifest.contains(['/', '\\'])
        || !s.manifest.ends_with(".json")
    {
        p.push(format!(
            "tool `{t}` shortcuts: manifest must be a file name ending in .json"
        ));
    }
    if !is_tool_id(&s.backend) {
        p.push(format!(
            "tool `{t}` shortcuts: backend must match ^[a-z][a-z0-9-]{{1,31}}$"
        ));
    }
    if s.actions.len() > 64 {
        p.push(format!("tool `{t}` shortcuts: at most 64 actions"));
    }
    let mut seen = HashSet::new();
    for a in &s.actions {
        let ctx = format!("tool `{t}` shortcut `{}`", a.action);
        if !is_action_id(&a.action) {
            p.push(format!("{ctx}: not an action ID (HOTKEY_CONTRACT §3.1)"));
        }
        if !seen.insert(a.action.as_str()) {
            p.push(format!("{ctx}: duplicate action"));
        }
        if !len_ok(&a.title, 120) {
            p.push(format!("{ctx}: title must be 1-120 characters"));
        }
        let scope_ok = matches!(a.scope.as_str(), "global" | "app")
            || a.scope.strip_prefix("context:").is_some_and(is_name_id);
        if !scope_ok {
            p.push(format!(
                "{ctx}: scope must be `global`, `app` or `context:<id>`"
            ));
        }
        if a.keys.len() > 4 {
            p.push(format!("{ctx}: at most 4 default bindings"));
        }
        for k in &a.keys {
            if let Err(e) = Sequence::parse(k) {
                p.push(format!(
                    "{ctx}: keys `{k}` are not canonical key syntax ({})",
                    e.message
                ));
            }
        }
    }
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

    fn with_metadata() -> serde_json::Value {
        let mut v = minimal();
        v["tools"][0]["features"] = serde_json::json!(["F1 captures a region"]);
        v["tools"][0]["runs"] = "tray".into();
        v["tools"][0]["entry_points"] = serde_json::json!([
            {"kind": "cli", "command": "subak list --json", "summary": "s", "status": "available"},
            {"kind": "mcp", "command": "subak mcp", "summary": "s", "status": "planned"}
        ]);
        v["tools"][0]["links"] =
            serde_json::json!([{"kind": "docs", "title": "Docs", "url": "https://x.invalid/d"}]);
        v["tools"][0]["shortcuts"] = serde_json::json!({
            "manifest": "hotkeys.manifest.json", "backend": "register",
            "actions": [
                {"action": "capture.region", "title": "Capture", "scope": "global", "keys": ["F1"]},
                {"action": "pin.close", "title": "Close", "scope": "context:pin", "keys": ["Escape"]},
                {"action": "engine.emergency-off", "title": "Off", "scope": "global",
                 "keys": ["Ctrl+Alt+Shift+Escape"], "bindable": false}
            ]
        });
        v["tools"][0]["permissions"] = serde_json::json!([
            {"id": "screen-capture", "detail": "only while capturing"},
            {"id": "a-future-permission", "detail": "unknown ids are allowed", "opt_in": true}
        ]);
        v
    }

    /// CATALOG_FORMAT §2.1: the metadata is optional and additive (KIM-FR-050).
    #[test]
    fn tool_metadata_is_optional_additive_and_round_trips() {
        let c = parse_value(&with_metadata()).expect("valid metadata");
        let t = c.tool("subak").unwrap();
        assert_eq!(t.runs.as_deref(), Some("tray"));
        assert_eq!(t.entry_points[0].status, EntryStatus::Available);
        let s = t.shortcuts.as_ref().unwrap();
        assert!(s.actions[0].bindable && !s.actions[2].bindable);
        assert!(t.permissions[1].opt_in && !t.permissions[0].opt_in);
        // An old catalog parses as before and serializes without any of the new keys.
        let old = parse_value(&minimal()).unwrap();
        let v = serde_json::to_value(&old.tools[0]).unwrap();
        for k in [
            "features",
            "runs",
            "entry_points",
            "links",
            "shortcuts",
            "permissions",
        ] {
            assert!(v.get(k).is_none(), "{k} must not appear in an old catalog");
        }
        // And the new keys survive a round trip.
        let again: Tool = serde_json::from_value(serde_json::to_value(t).unwrap()).unwrap();
        assert_eq!(&again, t);
    }

    #[test]
    fn every_metadata_problem_is_reported() {
        let mut v = with_metadata();
        let t = &mut v["tools"][0];
        t["features"] = serde_json::json!(["", "x".repeat(121)]);
        t["runs"] = "In The Tray".into();
        t["entry_points"][0]["kind"] = "GUI".into();
        t["links"][0]["url"] = "http://x.invalid/d".into();
        t["shortcuts"]["manifest"] = "../hotkeys.manifest.json".into();
        t["shortcuts"]["actions"][0]["action"] = "capture".into();
        t["shortcuts"]["actions"][0]["keys"] = serde_json::json!(["shift+ctrl+x"]);
        t["shortcuts"]["actions"][1]["scope"] = "window".into();
        t["permissions"][1]["id"] = "screen-capture".into();
        t["permissions"][0]["detail"] = "".into();
        let all = parse_value(&v).unwrap_err().details.join("\n");
        for needle in [
            "feature 0: must be 1-120",
            "feature 1: must be 1-120",
            "runs `In The Tray`",
            "kind must match",
            "url must be an https",
            "manifest must be a file name",
            "not an action ID",
            "not canonical key syntax",
            "scope must be `global`, `app` or `context:<id>`",
            "duplicate id",
            "detail must be 1-200",
        ] {
            assert!(all.contains(needle), "missing `{needle}` in:\n{all}");
        }
    }

    /// No fake completeness: a tool nobody can download cannot claim a working entry point.
    #[test]
    fn a_tool_without_releases_declares_only_planned_entry_points() {
        let mut v = with_metadata();
        v["tools"][0]["releases"] = serde_json::json!([]);
        let all = parse_value(&v).unwrap_err().details.join("\n");
        assert!(
            all.contains("can only declare planned entry points"),
            "{all}"
        );
        v["tools"][0]["entry_points"][0]["status"] = "planned".into();
        assert!(parse_value(&v).is_ok());
        v["tools"][0]["entry_points"][0]["status"] = "maybe".into();
        assert_eq!(parse_value(&v).unwrap_err().code, ErrorCode::CatalogInvalid);
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
