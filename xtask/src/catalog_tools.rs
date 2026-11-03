//! Publisher tooling for the signed Kim catalog (docs/CATALOG_FORMAT.md §4).
//!
//! * `catalog-keygen --out <file>` creates a new ed25519 signing key (hex seed). The key file is
//!   a secret: keep it outside the repository; it is never committed.
//! * `catalog-sign --key <file> [<catalog.json>]` writes `<catalog.json>.sig`.

use std::path::{Path, PathBuf};

pub fn keygen(out: &Path) -> Result<(), String> {
    if out.exists() {
        return Err(format!(
            "{} already exists; refusing to overwrite a key",
            out.display()
        ));
    }
    let mut seed = [0u8; 32];
    getrandom::fill(&mut seed).map_err(|e| format!("OS random generator failed: {e}"))?;
    if let Some(dir) = out.parent() {
        std::fs::create_dir_all(dir).map_err(|e| format!("{}: {e}", dir.display()))?;
    }
    std::fs::write(out, format!("{}\n", kim::catalog::signature::hex(&seed)))
        .map_err(|e| format!("{}: {e}", out.display()))?;
    let public = kim::catalog::signature::public_key_hex(&seed);
    let id = key_id_of(&public)?;
    println!("wrote secret key {}", out.display());
    println!("public_key_hex = \"{public}\"");
    println!("key_id         = \"{id}\"");
    println!("Add the public key to TRUSTED_KEYS in crates/kim/src/catalog/signature.rs.");
    Ok(())
}

fn key_id_of(public_hex: &str) -> Result<String, String> {
    let bytes = kim::catalog::signature::unhex::<32>(public_hex).ok_or("bad public key")?;
    Ok(kim::catalog::signature::key_id(&bytes))
}

fn read_seed(key: &Path) -> Result<[u8; 32], String> {
    let text = std::fs::read_to_string(key).map_err(|e| format!("{}: {e}", key.display()))?;
    kim::catalog::signature::unhex::<32>(text.trim())
        .ok_or_else(|| format!("{} does not contain a 64-hex-digit seed", key.display()))
}

pub fn sign(key: &Path, catalog: &Path) -> Result<(), String> {
    let seed = read_seed(key)?;
    let bytes = std::fs::read(catalog).map_err(|e| format!("{}: {e}", catalog.display()))?;
    kim::catalog::parse(&bytes).map_err(|e| {
        format!(
            "refusing to sign an invalid catalog: {e}\n  {}",
            e.details.join("\n  ")
        )
    })?;
    let sig = kim::catalog::signature::sign(&bytes, &seed);
    let trusted = kim::catalog::signature::TRUSTED_KEYS
        .iter()
        .any(|k| k.key_id == sig.key_id);
    let out = sig_path(catalog);
    let json = serde_json::to_string_pretty(&sig).map_err(|e| e.to_string())?;
    std::fs::write(&out, format!("{json}\n")).map_err(|e| format!("{}: {e}", out.display()))?;
    println!("wrote {} (key {})", out.display(), sig.key_id);
    if !trusted {
        println!(
            "warning: key {} is not in TRUSTED_KEYS of this build",
            sig.key_id
        );
    }
    Ok(())
}

pub fn sig_path(catalog: &Path) -> PathBuf {
    let mut s = catalog.as_os_str().to_owned();
    s.push(".sig");
    PathBuf::from(s)
}
