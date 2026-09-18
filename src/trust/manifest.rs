//! AI Catalog trust manifests and catalog-level signatures (AI Catalog
//! §Trust Manifest, §Signature Verification).
//!
//! A manifest's `signature` is a detached compact JWS over
//! JCS(manifest without `signature`). The catalog's top-level `signature`
//! is computed exactly the same way over the whole document. Both are
//! signed by the operator key whose JWK Set is published at the manifest
//! `identity` URL.
use super::{EntryDraft, jose};
use chrono::{DateTime, SecondsFormat, Utc};
use serde_json::{Value, json};

pub const SIGNATURE: &str = "signature";
/// Media type of the operator JWK Set bound by the host manifest.
pub const JWKS_TYPE: &str = "application/jwk-set+json";
pub const CARD_TYPE: &str = "application/a2a-agent-card+json";

fn rfc3339(time: DateTime<Utc>) -> String {
    time.to_rfc3339_opts(SecondsFormat::Secs, true)
}

/// Bytes a manifest or catalog signature is computed over.
pub fn signing_payload(document: &Value) -> Result<Vec<u8>, jose::JoseError> {
    let mut stripped = document.clone();
    if let Some(object) = stripped.as_object_mut() {
        object.remove(SIGNATURE);
    }
    jose::canonicalize(&stripped)
}

/// An unsigned entry manifest. `provenance` records that the entry was
/// published from the catalog itself (this service is both publisher and
/// host), which is what an external provider would replace.
pub fn draft(
    identity: &str,
    entry: &EntryDraft,
    card_digest: &str,
    issued_at: DateTime<Utc>,
    expires_at: DateTime<Utc>,
) -> Value {
    json!({
        "identity": identity,
        "subject": {"type": entry.entry_type, "digest": card_digest, "url": entry.url},
        "issuedAt": rfc3339(issued_at),
        "expiresAt": rfc3339(expires_at),
        "provenance": [{"relation": "publishedFrom", "sourceId": entry.identifier}],
    })
}

/// The "account-verified" attestation: a self-contained `data:` URI (the
/// spec recommends inline attestations) stating that the agent's account
/// identity was verified by OpenID Connect sign-in. It names no person.
pub fn account_attestation(issued_at: DateTime<Utc>) -> Value {
    let statement = json!({"claim": "account-verified", "method": "google-oidc",
        "issuedAt": rfc3339(issued_at)});
    let bytes = jose::canonicalize(&statement).expect("statement canonicalizes");
    json!({
        "type": super::ACCOUNT_VERIFIED,
        "uri": format!("data:application/json;base64,{}", jose::b64url(&bytes)),
        "digest": jose::digest(&bytes),
        "size": bytes.len(),
        "description": "The agent's account identity was verified by OpenID Connect sign-in.",
    })
}

/// An unsigned host manifest binding the operator JWK Set itself.
pub fn host_draft(
    identity: &str,
    jwks_digest: &str,
    issued_at: DateTime<Utc>,
    expires_at: DateTime<Utc>,
) -> Value {
    json!({
        "identity": identity,
        "subject": {"type": JWKS_TYPE, "digest": jwks_digest, "url": identity},
        "issuedAt": rfc3339(issued_at),
        "expiresAt": rfc3339(expires_at),
    })
}

/// Whether a stored manifest still describes `entry` and is within its
/// validity window (with `margin` of remaining validity required).
pub fn is_current(
    manifest: &Value,
    entry: &EntryDraft,
    card_digest: &str,
    identity: &str,
    now: DateTime<Utc>,
    margin: chrono::Duration,
) -> bool {
    manifest["identity"] == identity
        && manifest["subject"]["type"] == entry.entry_type.as_str()
        && manifest["subject"]["url"] == entry.url.as_str()
        && manifest["subject"]["digest"] == card_digest
        && manifest["signature"].is_string()
        && manifest["expiresAt"]
            .as_str()
            .and_then(|t| DateTime::parse_from_rfc3339(t).ok())
            .is_some_and(|expires| expires.with_timezone(&Utc) > now + margin)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn payload_excludes_only_the_signature_member() {
        let signed = json!({"identity":"https://x/.well-known/jwks.json","signature":"a..b","issuedAt":"2030-01-01T00:00:00Z","subject":{"type":"t","digest":"sha256:00"}});
        let mut unsigned = signed.clone();
        unsigned.as_object_mut().unwrap().remove("signature");
        assert_eq!(
            signing_payload(&signed).unwrap(),
            jose::canonicalize(&unsigned).unwrap()
        );
    }

    #[test]
    fn currency_requires_matching_binding_identity_and_remaining_validity() {
        let entry = EntryDraft {
            identifier: "urn:air:x:agent:a".into(),
            entry_type: CARD_TYPE.into(),
            url: "https://x/agents/a/agent-card.json".into(),
        };
        let now = "2030-01-01T00:00:00Z".parse::<DateTime<Utc>>().unwrap();
        let mut manifest = draft(
            "https://x/.well-known/jwks.json",
            &entry,
            "sha256:00",
            now,
            now + chrono::Duration::days(10),
        );
        manifest["signature"] = json!("h..s");
        let ok = |m: &Value| {
            is_current(
                m,
                &entry,
                "sha256:00",
                "https://x/.well-known/jwks.json",
                now,
                chrono::Duration::days(1),
            )
        };
        assert!(ok(&manifest));
        assert!(!is_current(
            &manifest,
            &entry,
            "sha256:00",
            "https://x/.well-known/jwks.json",
            now + chrono::Duration::days(9),
            chrono::Duration::days(2)
        ));
        let mut moved = manifest.clone();
        moved["subject"]["url"] = json!("https://y/agents/a/agent-card.json");
        assert!(!ok(&moved));
        let mut other_digest = manifest.clone();
        other_digest["subject"]["digest"] = json!("sha256:01");
        assert!(!ok(&other_digest));
        let mut unsigned = manifest.clone();
        unsigned.as_object_mut().unwrap().remove("signature");
        assert!(!ok(&unsigned));
    }
}

#[cfg(test)]
mod sdk_equivalence {
    //! The signed payloads equal what the AI Catalog SDK canonicalizes, so
    //! a consumer built on `ai-catalog-trust` reproduces our bytes.
    use super::*;
    use crate::trust::{Claims, EntryDraft, LocalTrust, Operator, TrustProvider};

    #[tokio::test]
    async fn sdk_canonicalization_reproduces_our_signing_payloads() {
        let trust = LocalTrust::ephemeral("https://api.test");
        let operator = Operator::ephemeral("https://api.test");
        let entry = EntryDraft {
            identifier: "urn:air:api.test:agent:a".into(),
            entry_type: CARD_TYPE.into(),
            url: "https://api.test/agents/a/agent-card.json".into(),
        };
        let manifest = trust
            .manifest_for(
                &entry,
                b"card",
                &Claims {
                    account_verified: true,
                },
            )
            .await
            .unwrap();
        assert_eq!(manifest["attestations"][0]["type"], "account-verified");
        let typed: ai_catalog::TrustManifest = serde_json::from_value(manifest.clone()).unwrap();
        assert_eq!(
            ai_catalog_trust::canonicalize_trust_manifest(&typed)
                .unwrap()
                .into_bytes(),
            signing_payload(&manifest).unwrap()
        );
        let mut catalog = json!({
            "specVersion": "1.0",
            "host": {"displayName": "h", "identifier": operator.identity(),
                     "trustManifest": operator.host_manifest().await.unwrap()},
            "entries": [{"identifier": entry.identifier, "type": CARD_TYPE, "url": entry.url,
                         "tags": ["calendar"], "trustManifest": manifest}],
        });
        operator.sign_catalog(&mut catalog).await.unwrap();
        let typed: ai_catalog::AiCatalog = serde_json::from_value(catalog.clone()).unwrap();
        assert_eq!(
            ai_catalog_trust::canonicalize_catalog(&typed)
                .unwrap()
                .into_bytes(),
            signing_payload(&catalog).unwrap()
        );
        assert!(ai_catalog_trust::verify_digest(&jose::digest(b"card"), b"card").unwrap());
    }
}
