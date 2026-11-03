//! Detached ed25519 signatures for catalog documents (docs/CATALOG_FORMAT.md §4).
//!
//! The signed message is `DOMAIN || catalog bytes` (exact file bytes, no re-serialization), so a
//! signature made for another purpose (e.g. a release manifest) can never verify as a catalog.

use crate::error::{ErrorCode, KimError, Result};
use ed25519_dalek::{Signature, Signer, SigningKey, VerifyingKey};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

/// Domain-separation prefix for catalog signatures.
pub const DOMAIN: &[u8] = b"mouv.kim.catalog.v1\n";
pub const ALGORITHM: &str = "ed25519";
/// Upper bound for a signature file.
pub const MAX_SIGNATURE_BYTES: u64 = 16 * 1024;

/// A public key this Kim build trusts for catalogs.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TrustedKey {
    pub key_id: &'static str,
    pub public_key_hex: &'static str,
    pub label: &'static str,
}

/// Keys trusted by this build. Rotation: add the new key, publish catalogs signed by it, and
/// remove the old key in a later release (docs/CATALOG_FORMAT.md §4.3).
pub const TRUSTED_KEYS: &[TrustedKey] = &[TrustedKey {
    key_id: "4466fe26eca99520",
    public_key_hex: "036df1738476005a056c71ec97a57fc6055be9f697af82efd2445dbe8de15752",
    label: "mouv catalog development key 2026-11 (preview catalogs only)",
}];

/// Contents of a `catalog.json.sig` file.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Serialize)]
pub struct SignatureFile {
    pub algorithm: String,
    pub key_id: String,
    pub signature: String,
}

/// Who signed a verified document.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct VerifiedSigner {
    pub key_id: String,
    pub label: String,
}

/// Key identifier: the first 8 bytes of SHA-256(public key), as lowercase hex.
pub fn key_id(public: &[u8; 32]) -> String {
    hex(&Sha256::digest(public)[..8])
}

pub fn hex(bytes: &[u8]) -> String {
    use std::fmt::Write as _;
    bytes
        .iter()
        .fold(String::with_capacity(bytes.len() * 2), |mut s, b| {
            let _ = write!(s, "{b:02x}");
            s
        })
}

pub fn unhex<const N: usize>(s: &str) -> Option<[u8; N]> {
    if s.len() != N * 2 {
        return None;
    }
    let mut out = [0u8; N];
    for (i, chunk) in s.as_bytes().chunks(2).enumerate() {
        let pair = std::str::from_utf8(chunk).ok()?;
        out[i] = u8::from_str_radix(pair, 16).ok()?;
    }
    Some(out)
}

fn message(catalog: &[u8]) -> Vec<u8> {
    let mut m = Vec::with_capacity(DOMAIN.len() + catalog.len());
    m.extend_from_slice(DOMAIN);
    m.extend_from_slice(catalog);
    m
}

/// Signs catalog bytes with a 32-byte ed25519 seed (publisher tooling: `cargo xtask catalog-sign`).
pub fn sign(catalog: &[u8], seed: &[u8; 32]) -> SignatureFile {
    let key = SigningKey::from_bytes(seed);
    let sig = key.sign(&message(catalog));
    SignatureFile {
        algorithm: ALGORITHM.into(),
        key_id: key_id(&key.verifying_key().to_bytes()),
        signature: hex(&sig.to_bytes()),
    }
}

/// Public key (hex) for a seed.
pub fn public_key_hex(seed: &[u8; 32]) -> String {
    hex(&SigningKey::from_bytes(seed).verifying_key().to_bytes())
}

/// Verifies `sig_bytes` (a signature file) over `catalog` against `keys`.
pub fn verify(catalog: &[u8], sig_bytes: &[u8], keys: &[TrustedKey]) -> Result<VerifiedSigner> {
    if sig_bytes.len() as u64 > MAX_SIGNATURE_BYTES {
        return Err(KimError::new(
            ErrorCode::SignatureInvalid,
            "signature file is too large",
        ));
    }
    let file: SignatureFile = serde_json::from_slice(sig_bytes).map_err(|e| {
        KimError::new(
            ErrorCode::SignatureInvalid,
            format!("signature file is malformed: {e}"),
        )
    })?;
    if file.algorithm != ALGORITHM {
        return Err(KimError::new(
            ErrorCode::SignatureInvalid,
            format!("unsupported signature algorithm `{}`", file.algorithm),
        ));
    }
    let Some(trusted) = keys.iter().find(|k| k.key_id == file.key_id) else {
        return Err(KimError::new(
            ErrorCode::UntrustedKey,
            format!(
                "catalog is signed by key `{}`, which this Kim does not trust",
                file.key_id
            ),
        )
        .with_hint("only catalogs signed by mouv's published keys are accepted"));
    };
    let public = unhex::<32>(trusted.public_key_hex)
        .and_then(|b| VerifyingKey::from_bytes(&b).ok())
        .ok_or_else(|| KimError::new(ErrorCode::Internal, "built-in trusted key is malformed"))?;
    if key_id(&public.to_bytes()) != trusted.key_id {
        return Err(KimError::new(
            ErrorCode::Internal,
            "built-in trusted key id does not match its public key",
        ));
    }
    let sig = unhex::<64>(&file.signature)
        .map(|b| Signature::from_bytes(&b))
        .ok_or_else(|| {
            KimError::new(
                ErrorCode::SignatureInvalid,
                "signature is not 64 bytes of hex",
            )
        })?;
    public.verify_strict(&message(catalog), &sig).map_err(|_| {
        KimError::new(
            ErrorCode::SignatureInvalid,
            "signature does not match the catalog (modified or corrupted)",
        )
    })?;
    Ok(VerifiedSigner {
        key_id: trusted.key_id.into(),
        label: trusted.label.into(),
    })
}

#[cfg(test)]
pub(crate) mod test_keys {
    //! Deterministic keys for tests only. They are never trusted by a real build.
    use super::*;

    pub const SEED: [u8; 32] = [7u8; 32];
    pub const OTHER_SEED: [u8; 32] = [9u8; 32];

    pub fn trusted() -> Vec<TrustedKey> {
        let public: &'static str = Box::leak(public_key_hex(&SEED).into_boxed_str());
        let id: &'static str = Box::leak(key_id(&unhex::<32>(public).unwrap()).into_boxed_str());
        vec![TrustedKey {
            key_id: id,
            public_key_hex: public,
            label: "test key",
        }]
    }

    pub fn sig_bytes(catalog: &[u8], seed: &[u8; 32]) -> Vec<u8> {
        serde_json::to_vec(&sign(catalog, seed)).unwrap()
    }
}

#[cfg(test)]
mod tests {
    use super::test_keys::*;
    use super::*;

    #[test]
    fn sign_then_verify_round_trips() {
        let doc = br#"{"hello":"catalog"}"#;
        let signer = verify(doc, &sig_bytes(doc, &SEED), &trusted()).expect("valid");
        assert_eq!(signer.label, "test key");
    }

    #[test]
    fn any_byte_change_breaks_the_signature() {
        let doc = br#"{"hello":"catalog"}"#.to_vec();
        let sig = sig_bytes(&doc, &SEED);
        let mut tampered = doc.clone();
        tampered[3] ^= 1;
        assert_eq!(
            verify(&tampered, &sig, &trusted()).unwrap_err().code,
            ErrorCode::SignatureInvalid
        );
        let mut spaced = doc;
        spaced.push(b' ');
        assert_eq!(
            verify(&spaced, &sig, &trusted()).unwrap_err().code,
            ErrorCode::SignatureInvalid
        );
    }

    #[test]
    fn unknown_keys_are_untrusted() {
        let doc = b"{}";
        let e = verify(doc, &sig_bytes(doc, &OTHER_SEED), &trusted()).unwrap_err();
        assert_eq!(e.code, ErrorCode::UntrustedKey);
    }

    #[test]
    fn signature_reuse_across_domains_fails() {
        // A signature over the raw bytes (no domain prefix) must not verify.
        let doc = b"{}";
        let key = SigningKey::from_bytes(&SEED);
        let raw = SignatureFile {
            algorithm: ALGORITHM.into(),
            key_id: trusted()[0].key_id.into(),
            signature: hex(&key.sign(doc).to_bytes()),
        };
        let e = verify(doc, &serde_json::to_vec(&raw).unwrap(), &trusted()).unwrap_err();
        assert_eq!(e.code, ErrorCode::SignatureInvalid);
    }

    #[test]
    fn malformed_signature_files_are_rejected() {
        let doc = b"{}";
        for bad in [
            &b"not json"[..],
            br#"{"algorithm":"rsa","key_id":"x","signature":"00"}"#,
            br#"{"algorithm":"ed25519","key_id":"x"}"#,
        ] {
            let e = verify(doc, bad, &trusted()).unwrap_err();
            assert_eq!(
                e.code,
                ErrorCode::SignatureInvalid,
                "{}",
                String::from_utf8_lossy(bad)
            );
        }
        let mut f = sign(doc, &SEED);
        f.signature = "zz".repeat(64);
        let e = verify(doc, &serde_json::to_vec(&f).unwrap(), &trusted()).unwrap_err();
        assert_eq!(e.code, ErrorCode::SignatureInvalid);
    }

    #[test]
    fn hex_round_trip() {
        assert_eq!(hex(&[0, 15, 255]), "000fff");
        assert_eq!(unhex::<3>("000fff"), Some([0, 15, 255]));
        assert_eq!(unhex::<3>("000ff"), None);
        assert_eq!(unhex::<1>("gg"), None);
    }
}
