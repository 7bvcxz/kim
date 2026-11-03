//! Catalog sources and trust decisions (docs/CATALOG_FORMAT.md §5).
//!
//! * **bundled** — compiled into `kim.exe`; always available offline.
//! * **cache** — the last catalog fetched by an explicit `kim catalog refresh`, stored in
//!   `%LOCALAPPDATA%\mouv\kim\catalog\`.
//!
//! Both are signature-verified on every load. The one with the higher `catalog_version` wins.
//! An invalid cache is ignored with a warning (never trusted, never deleted silently). Loading
//! never touches the network and never writes anything.

use super::signature::{self, TrustedKey, VerifiedSigner};
use super::{Catalog, MAX_CATALOG_BYTES};
use crate::error::{ErrorCode, KimError, Result};
use crate::fetch::Fetcher;
use crate::paths::Paths;
use crate::time::parse_utc;
use serde::Serialize;
use std::path::Path;

/// The catalog compiled into this build.
pub const BUNDLED_CATALOG: &[u8] = include_bytes!(concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/../../catalog/catalog.json"
));
pub const BUNDLED_SIGNATURE: &[u8] = include_bytes!(concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/../../catalog/catalog.json.sig"
));

/// Default remote location, used only by an explicit `kim catalog refresh`.
pub const DEFAULT_SOURCE: &str =
    "https://raw.githubusercontent.com/hellomouv/kim/main/catalog/catalog.json";

pub const CACHE_FILE: &str = "catalog.json";
pub const CACHE_SIG_FILE: &str = "catalog.json.sig";

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum Source {
    Bundled,
    Cache,
}

/// A verified catalog plus where it came from.
#[derive(Debug, Clone)]
pub struct Loaded {
    pub catalog: Catalog,
    pub source: Source,
    pub signer: VerifiedSigner,
    pub stale: bool,
    /// Non-fatal problems, e.g. an invalid cache that was ignored.
    pub warnings: Vec<String>,
}

/// Verifies signature and content of one catalog document.
pub fn verify_document(
    bytes: &[u8],
    sig: &[u8],
    keys: &[TrustedKey],
) -> Result<(Catalog, VerifiedSigner)> {
    let signer = signature::verify(bytes, sig, keys)?;
    let catalog = super::parse(bytes)?;
    Ok((catalog, signer))
}

pub fn is_stale(catalog: &Catalog, now: i64) -> bool {
    parse_utc(&catalog.expires_at).is_none_or(|exp| now >= exp)
}

/// Loads the best trusted catalog from the bundled copy and the cache.
pub fn load(paths: &Paths, keys: &[TrustedKey], now: i64) -> Result<Loaded> {
    load_with_bundle(paths, keys, now, BUNDLED_CATALOG, BUNDLED_SIGNATURE)
}

pub fn load_with_bundle(
    paths: &Paths,
    keys: &[TrustedKey],
    now: i64,
    bundle: &[u8],
    bundle_sig: &[u8],
) -> Result<Loaded> {
    let (bundled, bundled_signer) = verify_document(bundle, bundle_sig, keys).map_err(|e| {
        KimError::new(
            ErrorCode::Internal,
            format!("the catalog bundled in this Kim build failed verification: {e}"),
        )
    })?;
    let mut warnings = Vec::new();
    let mut best = (bundled, Source::Bundled, bundled_signer);
    match read_cache(paths)? {
        None => {}
        Some((bytes, sig)) => match verify_document(&bytes, &sig, keys) {
            Ok((cached, signer)) => {
                if cached.catalog_version > best.0.catalog_version {
                    best = (cached, Source::Cache, signer);
                }
            }
            Err(e) => warnings.push(format!(
                "ignoring the cached catalog in {}: {e}",
                paths.catalog_cache_dir().display()
            )),
        },
    }
    let stale = is_stale(&best.0, now);
    if stale {
        warnings.push(format!(
            "the catalog expired on {}; run `kim catalog refresh` when online",
            best.0.expires_at
        ));
    }
    Ok(Loaded {
        catalog: best.0,
        source: best.1,
        signer: best.2,
        stale,
        warnings,
    })
}

fn read_cache(paths: &Paths) -> Result<Option<(Vec<u8>, Vec<u8>)>> {
    let dir = paths.catalog_cache_dir();
    let cat = dir.join(CACHE_FILE);
    let sig = dir.join(CACHE_SIG_FILE);
    if !cat.exists() && !sig.exists() {
        return Ok(None);
    }
    let read = |p: &Path| {
        std::fs::read(p)
            .map_err(|e| KimError::new(ErrorCode::Io, format!("cannot read {}: {e}", p.display())))
    };
    if !cat.exists() || !sig.exists() {
        // A half-written pair is treated as an invalid cache, not as an I/O failure.
        return Ok(Some((
            read(&cat).unwrap_or_default(),
            read(&sig).unwrap_or_default(),
        )));
    }
    Ok(Some((read(&cat)?, read(&sig)?)))
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct RefreshOutcome {
    pub source_url: String,
    pub previous_version: u64,
    pub fetched_version: u64,
    /// True when the cache now holds a newer catalog than before.
    pub updated: bool,
}

/// A fetched, verified catalog that passed rollback protection but is not written yet.
#[derive(Debug, Clone)]
pub struct Fetched {
    pub outcome: RefreshOutcome,
    pub catalog: Catalog,
    pub signer: VerifiedSigner,
    bytes: Vec<u8>,
    sig: Vec<u8>,
}

impl Fetched {
    /// The catalog Kim would use after committing this refresh (the newer of the two).
    pub fn effective(&self, current: Loaded, now: i64) -> Loaded {
        if self.outcome.updated {
            let stale = is_stale(&self.catalog, now);
            let warnings = if stale {
                vec![format!(
                    "the catalog expired on {}; run `kim catalog refresh` when online",
                    self.catalog.expires_at
                )]
            } else {
                Vec::new()
            };
            Loaded {
                catalog: self.catalog.clone(),
                source: Source::Cache,
                signer: self.signer.clone(),
                stale,
                warnings,
            }
        } else {
            current
        }
    }
}

/// Explicit refresh: fetch catalog and signature, verify, enforce rollback protection, then
/// write the cache atomically. On any failure the existing cache is left untouched.
pub fn refresh(
    paths: &Paths,
    keys: &[TrustedKey],
    now: i64,
    source_url: &str,
    fetcher: &dyn Fetcher,
) -> Result<RefreshOutcome> {
    let fetched = fetch_verified(paths, keys, now, source_url, fetcher)?;
    commit(paths, &fetched)?;
    Ok(fetched.outcome)
}

/// Writes a verified refresh into the cache (atomically, see [`refresh`]).
pub fn commit(paths: &Paths, fetched: &Fetched) -> Result<()> {
    write_cache(paths, &fetched.bytes, &fetched.sig)
}

/// The read-only half of [`refresh`] (used by `--dry-run`): fetches, verifies and checks for
/// rollback, but writes nothing.
pub fn fetch_verified(
    paths: &Paths,
    keys: &[TrustedKey],
    now: i64,
    source_url: &str,
    fetcher: &dyn Fetcher,
) -> Result<Fetched> {
    crate::fetch::check_source_url(source_url)?;
    let current = load(paths, keys, now)?;
    let bytes = fetcher.get(source_url, MAX_CATALOG_BYTES)?;
    let sig = fetcher.get(&format!("{source_url}.sig"), signature::MAX_SIGNATURE_BYTES)?;
    let (fetched, signer) = verify_document(&bytes, &sig, keys)?;
    let previous_version = current.catalog.catalog_version;
    if fetched.catalog_version < previous_version {
        return Err(KimError::new(
            ErrorCode::CatalogRollback,
            format!(
                "fetched catalog version {} is older than the trusted version {previous_version}",
                fetched.catalog_version
            ),
        )
        .with_hint("the source may be outdated or replaying an old catalog; nothing was changed"));
    }
    if fetched.catalog_version == previous_version && fetched != current.catalog {
        return Err(KimError::new(
            ErrorCode::CatalogRollback,
            format!("fetched catalog reuses version {previous_version} with different content"),
        )
        .with_hint("a publisher must bump catalog_version for every change; nothing was changed"));
    }
    let updated = fetched.catalog_version > previous_version;
    Ok(Fetched {
        outcome: RefreshOutcome {
            // Never echo credentials from a user-supplied source URL (KIM-SEC-013).
            source_url: crate::redact::url(source_url),
            previous_version,
            fetched_version: fetched.catalog_version,
            updated,
        },
        catalog: fetched,
        signer,
        bytes,
        sig,
    })
}

fn write_cache(paths: &Paths, bytes: &[u8], sig: &[u8]) -> Result<()> {
    let dir = paths.catalog_cache_dir();
    std::fs::create_dir_all(&dir).map_err(|e| KimError::io(&dir, &e))?;
    // Write both temp files first, then replace: a crash in between leaves a pair that fails
    // verification and is ignored on the next load (never a trusted half-update).
    let tmp_cat = dir.join(format!("{CACHE_FILE}.tmp"));
    let tmp_sig = dir.join(format!("{CACHE_SIG_FILE}.tmp"));
    write_synced(&tmp_cat, bytes)?;
    write_synced(&tmp_sig, sig)?;
    replace(&tmp_cat, &dir.join(CACHE_FILE))?;
    replace(&tmp_sig, &dir.join(CACHE_SIG_FILE))
}

fn write_synced(path: &Path, data: &[u8]) -> Result<()> {
    use std::io::Write;
    let io = |e: std::io::Error| KimError::io(path, &e);
    let mut f = std::fs::File::create(path).map_err(io)?;
    f.write_all(data).map_err(io)?;
    f.sync_all().map_err(io)
}

fn replace(from: &Path, to: &Path) -> Result<()> {
    // std::fs::rename replaces an existing file atomically on Windows (MoveFileExW with
    // MOVEFILE_REPLACE_EXISTING) and on Unix (rename(2)).
    std::fs::rename(from, to).map_err(|e| KimError::io(to, &e))
}

#[cfg(test)]
mod tests {
    use super::super::signature::test_keys::{OTHER_SEED, SEED, sig_bytes, trusted};
    use super::*;
    use crate::time::parse_utc;
    use std::collections::HashMap;

    fn doc(version: u64, expires: &str) -> Vec<u8> {
        serde_json::to_vec_pretty(&serde_json::json!({
            "schema": "kim.catalog", "schema_version": 1, "catalog_version": version,
            "generated_at": "2026-11-01T00:00:00Z", "expires_at": expires,
            "publisher": "mouv",
            "tools": [{"id": "subak", "name": "Subak", "summary": "s", "description": "d",
                       "license": "MIT", "support": "in-development", "releases": []}]
        }))
        .unwrap()
    }

    struct Tmp(std::path::PathBuf);
    impl Tmp {
        fn new(tag: &str) -> Self {
            let p = std::env::temp_dir().join(format!(
                "kim-store-{tag}-{}-{:?}",
                std::process::id(),
                std::thread::current().id()
            ));
            let _ = std::fs::remove_dir_all(&p);
            std::fs::create_dir_all(&p).unwrap();
            Self(p)
        }
        fn paths(&self) -> Paths {
            Paths::under(&self.0)
        }
    }
    impl Drop for Tmp {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    #[derive(Default)]
    struct MapFetcher(HashMap<String, Vec<u8>>);
    impl Fetcher for MapFetcher {
        fn get(&self, url: &str, limit: u64) -> Result<Vec<u8>> {
            let b = self.0.get(url).cloned().ok_or_else(|| {
                KimError::new(ErrorCode::NetworkUnavailable, format!("offline: {url}"))
            })?;
            assert!(b.len() as u64 <= limit);
            Ok(b)
        }
    }

    const URL: &str = "https://example.invalid/catalog.json";
    const FUTURE: &str = "2099-01-01T00:00:00Z";

    fn now() -> i64 {
        parse_utc("2026-11-04T00:00:00Z").unwrap()
    }

    fn fetcher_with(bytes: &[u8], sig: &[u8]) -> MapFetcher {
        let mut m = MapFetcher::default();
        m.0.insert(URL.into(), bytes.to_vec());
        m.0.insert(format!("{URL}.sig"), sig.to_vec());
        m
    }

    #[test]
    fn the_real_bundled_catalog_is_signed_by_a_trusted_key_and_installs_nothing() {
        let (c, signer) =
            verify_document(BUNDLED_CATALOG, BUNDLED_SIGNATURE, signature::TRUSTED_KEYS)
                .expect("bundled catalog must verify with this build's trusted keys");
        assert_eq!(signer.key_id, signature::TRUSTED_KEYS[0].key_id);
        assert!(c.tool("subak").is_some(), "Subak is listed as a candidate");
        // Until real, signed releases exist, the bundled catalog must not offer any package.
        assert!(
            c.tools.iter().all(|t| t.releases.is_empty()),
            "the bundled catalog must not ship placeholder releases"
        );
    }

    #[test]
    fn bundled_is_used_offline_and_nothing_is_written() {
        let t = Tmp::new("offline");
        let b = doc(1, FUTURE);
        let loaded =
            load_with_bundle(&t.paths(), &trusted(), now(), &b, &sig_bytes(&b, &SEED)).unwrap();
        assert_eq!(loaded.source, Source::Bundled);
        assert!(!loaded.stale && loaded.warnings.is_empty());
        assert!(
            !t.paths().catalog_cache_dir().exists(),
            "load must not write"
        );
    }

    #[test]
    fn stale_catalog_is_still_usable_but_flagged() {
        let t = Tmp::new("stale");
        let b = doc(1, "2026-11-02T00:00:00Z");
        let loaded =
            load_with_bundle(&t.paths(), &trusted(), now(), &b, &sig_bytes(&b, &SEED)).unwrap();
        assert!(loaded.stale);
        assert!(loaded.warnings[0].contains("expired"));
    }

    #[test]
    fn newer_valid_cache_wins_and_invalid_cache_is_ignored_with_warning() {
        let t = Tmp::new("cache");
        let p = t.paths();
        let dir = p.catalog_cache_dir();
        std::fs::create_dir_all(&dir).unwrap();
        let b = doc(1, FUTURE);
        let c = doc(5, FUTURE);
        std::fs::write(dir.join(CACHE_FILE), &c).unwrap();
        std::fs::write(dir.join(CACHE_SIG_FILE), sig_bytes(&c, &SEED)).unwrap();
        let loaded = load_with_bundle(&p, &trusted(), now(), &b, &sig_bytes(&b, &SEED)).unwrap();
        assert_eq!(
            (loaded.source, loaded.catalog.catalog_version),
            (Source::Cache, 5)
        );

        // Tamper with the cached catalog: it must be ignored, not trusted.
        let mut tampered = c;
        tampered.extend_from_slice(b" ");
        std::fs::write(dir.join(CACHE_FILE), &tampered).unwrap();
        let loaded = load_with_bundle(&p, &trusted(), now(), &b, &sig_bytes(&b, &SEED)).unwrap();
        assert_eq!(loaded.source, Source::Bundled);
        assert!(
            loaded.warnings[0].contains("ignoring the cached catalog"),
            "{:?}",
            loaded.warnings
        );

        // Missing signature half: also ignored.
        std::fs::remove_file(dir.join(CACHE_SIG_FILE)).unwrap();
        let loaded = load_with_bundle(&p, &trusted(), now(), &b, &sig_bytes(&b, &SEED)).unwrap();
        assert_eq!(loaded.source, Source::Bundled);
        assert_eq!(loaded.warnings.len(), 1);
    }

    #[test]
    fn bundled_catalog_failing_verification_is_an_internal_error() {
        let t = Tmp::new("badbundle");
        let b = doc(1, FUTURE);
        let e = load_with_bundle(
            &t.paths(),
            &trusted(),
            now(),
            &b,
            &sig_bytes(&b, &OTHER_SEED),
        )
        .unwrap_err();
        assert_eq!(e.code, ErrorCode::Internal);
    }

    #[test]
    fn refresh_accepts_newer_rejects_rollback_tampering_and_untrusted_keys() {
        let t = Tmp::new("refresh");
        let p = t.paths();
        // Refresh logic loads the real bundled catalog; use versions above any bundled version.
        let base = super::super::parse(BUNDLED_CATALOG)
            .map(|c| c.catalog_version)
            .unwrap_or(1);
        let keys = test_keys_plus_bundled();
        let newer = doc(base + 10, FUTURE);
        let out = refresh(
            &p,
            &keys,
            now(),
            URL,
            &fetcher_with(&newer, &sig_bytes(&newer, &SEED)),
        )
        .unwrap();
        assert!(out.updated);
        assert_eq!(out.fetched_version, base + 10);
        assert!(p.catalog_cache_dir().join(CACHE_FILE).exists());

        // Older version → rollback refused, cache untouched.
        let older = doc(base + 2, FUTURE);
        let e = refresh(
            &p,
            &keys,
            now(),
            URL,
            &fetcher_with(&older, &sig_bytes(&older, &SEED)),
        )
        .unwrap_err();
        assert_eq!(e.code, ErrorCode::CatalogRollback);
        assert_eq!(
            std::fs::read(p.catalog_cache_dir().join(CACHE_FILE)).unwrap(),
            newer
        );

        // Same version, different content → refused.
        let mut same_version: serde_json::Value = serde_json::from_slice(&newer).unwrap();
        same_version["publisher"] = "someone else".into();
        let same_version = serde_json::to_vec(&same_version).unwrap();
        let e = refresh(
            &p,
            &keys,
            now(),
            URL,
            &fetcher_with(&same_version, &sig_bytes(&same_version, &SEED)),
        )
        .unwrap_err();
        assert_eq!(e.code, ErrorCode::CatalogRollback);

        // Tampered bytes → signature failure.
        let sig = sig_bytes(&newer, &SEED);
        let mut tampered = newer.clone();
        tampered[10] ^= 1;
        let e = refresh(&p, &keys, now(), URL, &fetcher_with(&tampered, &sig)).unwrap_err();
        assert_eq!(e.code, ErrorCode::SignatureInvalid);

        // Signed by an unknown key.
        let rogue = doc(base + 20, FUTURE);
        let e = refresh(
            &p,
            &keys,
            now(),
            URL,
            &fetcher_with(&rogue, &sig_bytes(&rogue, &OTHER_SEED)),
        )
        .unwrap_err();
        assert_eq!(e.code, ErrorCode::UntrustedKey);

        // Offline → network error, cache untouched.
        let e = refresh(&p, &keys, now(), URL, &MapFetcher::default()).unwrap_err();
        assert_eq!(e.code, ErrorCode::NetworkUnavailable);
        assert_eq!(
            std::fs::read(p.catalog_cache_dir().join(CACHE_FILE)).unwrap(),
            newer
        );
        assert!(
            !p.catalog_cache_dir()
                .join(format!("{CACHE_FILE}.tmp"))
                .exists()
        );
    }

    #[test]
    fn refresh_refuses_insecure_sources_before_fetching() {
        let t = Tmp::new("insecure");
        let e = refresh(
            &t.paths(),
            &trusted(),
            now(),
            "http://example.com/c.json",
            &MapFetcher::default(),
        )
        .unwrap_err();
        assert_eq!(e.code, ErrorCode::UnsupportedSource);
    }

    /// Test key plus the real build keys, so `refresh` can load the real bundled catalog.
    fn test_keys_plus_bundled() -> Vec<TrustedKey> {
        let mut k = trusted();
        k.extend_from_slice(signature::TRUSTED_KEYS);
        k
    }
}
