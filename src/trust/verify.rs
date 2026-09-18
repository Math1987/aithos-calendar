//! The consumer side of the trust chain, as pure checks over documents that
//! the discovery client has already fetched (see `discovery.rs` for the
//! network policy around those fetches).
//!
//! Verification order, each step gated on the previous one:
//!
//! 1. Catalog document: `specVersion` major is 1 and the top-level
//!    `signature` verifies under the pinned operator JWK Set.
//! 2. Entry manifest: `identity` is the pinned operator identity, the
//!    signature verifies, `subject.type`/`subject.url` restate the entry,
//!    `issuedAt` is not in the future and `expiresAt` has not passed.
//! 3. Card bytes: `sha256(bytes) == subject.digest`.
//! 4. Card signature: the `jku` (when present) is the agent's JWK Set URL on
//!    the agent origin, and the JWS verifies under that key set.
//!
//! Each failure has its own code so an auditor reading `/logs` sees which
//! link of the chain broke.
use super::{card, jose, manifest};
use chrono::{DateTime, Duration, Utc};
use serde_json::Value;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum VerifyError {
    CatalogUnsupportedVersion,
    CatalogSignatureMissing,
    CatalogSignatureInvalid,
    ManifestMissing,
    ManifestMalformed,
    ManifestIdentityMismatch,
    ManifestSignatureInvalid,
    ManifestSubjectMismatch,
    ManifestNotYetValid,
    ManifestExpired,
    CardDigestMismatch,
    CardKeyLocationInvalid,
    CardSignatureInvalid,
}

impl VerifyError {
    pub fn code(self) -> &'static str {
        match self {
            Self::CatalogUnsupportedVersion => "catalog_unsupported_version",
            Self::CatalogSignatureMissing => "catalog_signature_missing",
            Self::CatalogSignatureInvalid => "catalog_signature_invalid",
            Self::ManifestMissing => "manifest_missing",
            Self::ManifestMalformed => "manifest_malformed",
            Self::ManifestIdentityMismatch => "manifest_identity_mismatch",
            Self::ManifestSignatureInvalid => "manifest_signature_invalid",
            Self::ManifestSubjectMismatch => "manifest_subject_mismatch",
            Self::ManifestNotYetValid => "manifest_not_yet_valid",
            Self::ManifestExpired => "manifest_expired",
            Self::CardDigestMismatch => "card_digest_mismatch",
            Self::CardKeyLocationInvalid => "card_key_location_invalid",
            Self::CardSignatureInvalid => "card_signature_invalid",
        }
    }
}

/// Tolerated clock skew between signer and verifier for `issuedAt`.
pub const CLOCK_SKEW: Duration = Duration::minutes(5);

/// AI Catalog §Versioning: accept any 1.x document, reject other majors.
pub fn version_supported(spec_version: &str) -> bool {
    let mut parts = spec_version.split('.');
    matches!(
        (parts.next(), parts.next(), parts.next()),
        (Some("1"), Some(minor), None) if !minor.is_empty() && minor.bytes().all(|b| b.is_ascii_digit())
    )
}

/// Step 1.
pub fn verify_catalog(catalog: &Value, keys: &jose::Jwks) -> Result<(), VerifyError> {
    if !catalog["specVersion"]
        .as_str()
        .is_some_and(version_supported)
    {
        return Err(VerifyError::CatalogUnsupportedVersion);
    }
    let signature = catalog[manifest::SIGNATURE]
        .as_str()
        .ok_or(VerifyError::CatalogSignatureMissing)?;
    let protected =
        jose::parse_detached(signature).map_err(|_| VerifyError::CatalogSignatureInvalid)?;
    let payload =
        manifest::signing_payload(catalog).map_err(|_| VerifyError::CatalogSignatureInvalid)?;
    jose::verify(&protected, &payload, keys).map_err(|_| VerifyError::CatalogSignatureInvalid)
}

/// What a verified manifest promises about the artifact.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Binding {
    pub digest: String,
    pub expires_at: Option<DateTime<Utc>>,
}

fn timestamp(value: &Value) -> Result<DateTime<Utc>, VerifyError> {
    value
        .as_str()
        .and_then(|t| DateTime::parse_from_rfc3339(t).ok())
        .map(|t| t.with_timezone(&Utc))
        .ok_or(VerifyError::ManifestMalformed)
}

/// Step 2.
pub fn verify_manifest(
    manifest: &Value,
    keys: &jose::Jwks,
    identity: &str,
    entry_type: &str,
    entry_url: &str,
    now: DateTime<Utc>,
) -> Result<Binding, VerifyError> {
    if manifest.is_null() {
        return Err(VerifyError::ManifestMissing);
    }
    if !manifest.is_object() {
        return Err(VerifyError::ManifestMalformed);
    }
    if manifest["identity"] != identity {
        return Err(VerifyError::ManifestIdentityMismatch);
    }
    let signature = manifest[manifest::SIGNATURE]
        .as_str()
        .ok_or(VerifyError::ManifestMalformed)?;
    let protected =
        jose::parse_detached(signature).map_err(|_| VerifyError::ManifestSignatureInvalid)?;
    let payload =
        manifest::signing_payload(manifest).map_err(|_| VerifyError::ManifestMalformed)?;
    jose::verify(&protected, &payload, keys).map_err(|_| VerifyError::ManifestSignatureInvalid)?;
    // Only after the signature: the binding is what the signer committed to.
    let subject = &manifest["subject"];
    let digest = subject["digest"]
        .as_str()
        .ok_or(VerifyError::ManifestMalformed)?;
    let hex = digest
        .strip_prefix("sha256:")
        .ok_or(VerifyError::ManifestMalformed)?;
    if hex.len() != 64
        || !hex
            .bytes()
            .all(|b| b.is_ascii_hexdigit() && !b.is_ascii_uppercase())
    {
        return Err(VerifyError::ManifestMalformed);
    }
    if subject["type"] != entry_type || subject["url"] != entry_url {
        return Err(VerifyError::ManifestSubjectMismatch);
    }
    if timestamp(&manifest["issuedAt"])? > now + CLOCK_SKEW {
        return Err(VerifyError::ManifestNotYetValid);
    }
    let expires_at = if manifest["expiresAt"].is_null() {
        None
    } else {
        let expires_at = timestamp(&manifest["expiresAt"])?;
        if expires_at <= now {
            return Err(VerifyError::ManifestExpired);
        }
        Some(expires_at)
    };
    Ok(Binding {
        digest: digest.to_owned(),
        expires_at,
    })
}

/// Step 3.
pub fn verify_card_digest(bytes: &[u8], expected: &str) -> Result<(), VerifyError> {
    if jose::digest(bytes) == expected {
        Ok(())
    } else {
        Err(VerifyError::CardDigestMismatch)
    }
}

/// Step 4a: where the card says its key lives, checked against where this
/// client will accept it from. Returns the parsed signature header.
pub fn card_key_location(card: &Value, expected_jku: &str) -> Result<jose::Protected, VerifyError> {
    let protected = card::signature_of(card).map_err(|_| VerifyError::CardSignatureInvalid)?;
    if protected.kid().is_none() {
        return Err(VerifyError::CardKeyLocationInvalid);
    }
    match protected.header.get("jku") {
        None => Ok(protected),
        Some(jku) if jku == expected_jku => Ok(protected),
        Some(_) => Err(VerifyError::CardKeyLocationInvalid),
    }
}

/// Step 4b.
pub fn verify_card(card: &Value, keys: &jose::Jwks) -> Result<(), VerifyError> {
    card::verify(card, keys).map_err(|_| VerifyError::CardSignatureInvalid)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::trust::{EntryDraft, LocalTrust, TrustProvider};
    use serde_json::json;

    #[test]
    fn spec_versions() {
        for ok in ["1.0", "1.1", "1.42"] {
            assert!(version_supported(ok), "{ok}");
        }
        for bad in ["2.0", "0.9", "1", "1.", "1.0.1", "1.x", " 1.0", "01.0"] {
            assert!(!version_supported(bad), "{bad}");
        }
    }

    #[tokio::test]
    async fn manifest_checks_fail_with_distinct_codes() {
        let trust = LocalTrust::ephemeral("https://api.test");
        let keys = jose::Jwks::parse(&trust.jwks()).unwrap();
        let entry = EntryDraft {
            identifier: "urn:air:api.test:agent:a".into(),
            entry_type: manifest::CARD_TYPE.into(),
            url: "https://api.test/agents/a/agent-card.json".into(),
        };
        let bytes = b"{\"card\":true}";
        let signed = trust.manifest_for(&entry, bytes).await.unwrap();
        let now = Utc::now();
        let check = |m: &Value| {
            verify_manifest(
                m,
                &keys,
                trust.identity(),
                &entry.entry_type,
                &entry.url,
                now,
            )
        };
        let binding = check(&signed).unwrap();
        assert_eq!(binding.digest, jose::digest(bytes));
        assert!(binding.expires_at.unwrap() > now);
        verify_card_digest(bytes, &binding.digest).unwrap();
        assert_eq!(
            verify_card_digest(b"{\"card\":false}", &binding.digest).unwrap_err(),
            VerifyError::CardDigestMismatch
        );

        assert_eq!(
            check(&Value::Null).unwrap_err(),
            VerifyError::ManifestMissing
        );
        assert_eq!(
            check(&json!("x")).unwrap_err(),
            VerifyError::ManifestMalformed
        );
        let mut identity = signed.clone();
        identity["identity"] = json!("https://evil.test/.well-known/jwks.json");
        assert_eq!(
            check(&identity).unwrap_err(),
            VerifyError::ManifestIdentityMismatch
        );
        let mut tampered = signed.clone();
        tampered["subject"]["digest"] = json!(jose::digest(b"other"));
        assert_eq!(
            check(&tampered).unwrap_err(),
            VerifyError::ManifestSignatureInvalid
        );
        let mut unsigned = signed.clone();
        unsigned.as_object_mut().unwrap().remove("signature");
        assert_eq!(
            check(&unsigned).unwrap_err(),
            VerifyError::ManifestMalformed
        );
        let rogue = LocalTrust::ephemeral("https://api.test");
        let forged = rogue.manifest_for(&entry, bytes).await.unwrap();
        assert_eq!(
            check(&forged).unwrap_err(),
            VerifyError::ManifestSignatureInvalid
        );

        // Subject mismatch is only reported for a genuine signature: the
        // signer bound a different artifact than the entry claims.
        let moved = EntryDraft {
            url: "https://api.test/agents/b/agent-card.json".into(),
            ..entry.clone()
        };
        let other = trust.manifest_for(&moved, bytes).await.unwrap();
        assert_eq!(
            check(&other).unwrap_err(),
            VerifyError::ManifestSubjectMismatch
        );

        let expired = verify_manifest(
            &signed,
            &keys,
            trust.identity(),
            &entry.entry_type,
            &entry.url,
            now + trust.manifest_ttl + Duration::seconds(1),
        )
        .unwrap_err();
        assert_eq!(expired, VerifyError::ManifestExpired);
        let future = verify_manifest(
            &signed,
            &keys,
            trust.identity(),
            &entry.entry_type,
            &entry.url,
            now - CLOCK_SKEW - Duration::minutes(1),
        )
        .unwrap_err();
        assert_eq!(future, VerifyError::ManifestNotYetValid);
    }

    #[tokio::test]
    async fn catalog_signature_covers_every_member_but_itself() {
        let trust = LocalTrust::ephemeral("https://api.test");
        let keys = jose::Jwks::parse(&trust.jwks()).unwrap();
        let mut catalog = json!({"specVersion":"1.0","host":{"displayName":"h"},"entries":[]});
        assert_eq!(
            verify_catalog(&catalog, &keys).unwrap_err(),
            VerifyError::CatalogSignatureMissing
        );
        trust.sign_catalog(&mut catalog).await.unwrap();
        verify_catalog(&catalog, &keys).unwrap();
        let mut altered = catalog.clone();
        altered["entries"] = json!([{"identifier":"x","type":"t","url":"https://x"}]);
        assert_eq!(
            verify_catalog(&altered, &keys).unwrap_err(),
            VerifyError::CatalogSignatureInvalid
        );
        let mut v2 = catalog.clone();
        v2["specVersion"] = json!("2.0");
        assert_eq!(
            verify_catalog(&v2, &keys).unwrap_err(),
            VerifyError::CatalogUnsupportedVersion
        );
        let mut minor = json!({"specVersion":"1.3","host":{"displayName":"h"},"entries":[]});
        trust.sign_catalog(&mut minor).await.unwrap();
        verify_catalog(&minor, &keys).unwrap();
        let rogue = jose::Jwks::parse(&LocalTrust::ephemeral("https://api.test").jwks()).unwrap();
        assert_eq!(
            verify_catalog(&catalog, &rogue).unwrap_err(),
            VerifyError::CatalogSignatureInvalid
        );
    }
}
