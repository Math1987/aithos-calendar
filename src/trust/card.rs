//! A2A Agent Card signing and verification (A2A 1.0 §Agent Card Signing).
//!
//! The signed payload is the RFC 8785 canonical form of the card with its
//! top-level `signatures` member removed. Each `signatures[]` element is an
//! `AgentCardSignature {protected, signature}`; the payload is detached. The
//! protected header carries `alg` (ES256), `kid` (RFC 7638 thumbprint of the
//! agent key) and, over HTTPS, `jku` (the agent's JWK Set URL on this
//! service). The served bytes are the canonical form of the complete card,
//! so the AI Catalog `subject.digest` is a digest of exactly what a client
//! fetches.
use super::{AgentKey, SignedCard, TrustError, jose};
use serde_json::{Value, json};

pub const SIGNATURES: &str = "signatures";

/// SDK 0.3.1 serializes `securityRequirements` as OpenAPI-style maps; the
/// A2A 1.0 wire schema uses `schemes -> StringList`. Normalize before
/// signing so the served card is wire-conformant.
pub fn wire_requirements(value: &mut Value) {
    if let Some(requirements) = value
        .get_mut("securityRequirements")
        .and_then(|r| r.as_array_mut())
    {
        for requirement in requirements {
            if let Some(map) = requirement.as_object() {
                // Already in wire form (re-signing a served card).
                if map.len() == 1 && map.contains_key("schemes") {
                    continue;
                }
                let schemes: serde_json::Map<String, Value> = map
                    .iter()
                    .map(|(k, v)| {
                        (
                            k.clone(),
                            if v.as_array().is_some_and(Vec::is_empty) {
                                json!({})
                            } else {
                                json!({"list": v})
                            },
                        )
                    })
                    .collect();
                *requirement = json!({"schemes": schemes});
            }
        }
    }
}

/// The inverse adaptation for the SDK's reader: canonical JSON omits an
/// empty `StringList.list`, which SDK 0.3.1 expects. Only the in-memory
/// client view is adapted, never the signed bytes.
pub fn sdk_requirements(value: &mut Value) {
    if let Some(requirements) = value
        .get_mut("securityRequirements")
        .and_then(|r| r.as_array_mut())
    {
        for requirement in requirements {
            if let Some(schemes) = requirement
                .get_mut("schemes")
                .and_then(|s| s.as_object_mut())
            {
                for scheme in schemes.values_mut() {
                    if scheme.as_object().is_some_and(|v| v.is_empty()) {
                        *scheme = json!({"list": []});
                    }
                }
            }
        }
    }
}

fn for_each_requirement_holder(card: &mut Value, adapt: fn(&mut Value)) {
    adapt(card);
    if let Some(skills) = card.get_mut("skills").and_then(|s| s.as_array_mut()) {
        for skill in skills {
            adapt(skill);
        }
    }
}

/// A card as the SDK's `AgentCard` type reads it.
pub fn sdk_view(mut card: Value) -> Result<a2a::AgentCard, TrustError> {
    for_each_requirement_holder(&mut card, sdk_requirements);
    serde_json::from_value(card).map_err(|_| TrustError::InvalidInput("agent_card"))
}

/// Bytes a card signature is computed over.
pub fn signing_payload(card: &Value) -> Result<Vec<u8>, jose::JoseError> {
    let mut stripped = card.clone();
    if let Some(object) = stripped.as_object_mut() {
        object.remove(SIGNATURES);
    }
    jose::canonicalize(&stripped)
}

/// Sign a card with the agent key. The result replaces any previous
/// `signatures` member.
pub fn sign(mut card: Value, key: &AgentKey, jku: &str) -> Result<SignedCard, TrustError> {
    if !card.is_object() {
        return Err(TrustError::InvalidInput("agent_card"));
    }
    let version = card["version"]
        .as_str()
        .filter(|v| !v.is_empty())
        .ok_or(TrustError::InvalidInput("agent_card_version"))?
        .to_owned();
    for_each_requirement_holder(&mut card, wire_requirements);
    card.as_object_mut().unwrap().remove(SIGNATURES);
    // The SDK must be able to read what we serve; validate before signing.
    sdk_view(card.clone())?;
    let mut extra = json!({"typ": "JOSE"});
    // A2A requires HTTPS for jku; loopback test deployments omit the hint and
    // the verifier derives the key location from the agent origin instead.
    if jku.starts_with("https://") {
        extra["jku"] = json!(jku);
    }
    let protected = jose::protected_header(key.kid(), extra)?;
    let payload = jose::canonicalize(&card)?;
    let signature = key.sign(&jose::signing_input(&protected, &payload));
    card[SIGNATURES] = json!([{"protected": protected, "signature": jose::b64url(&signature)}]);
    let bytes = jose::canonicalize(&card)?;
    Ok(SignedCard {
        digest: jose::digest(&bytes),
        bytes,
        version,
        kid: key.kid().to_owned(),
    })
}

/// The single signature this service expects on a card, parsed and checked
/// for algorithm and shape but not yet verified.
pub fn signature_of(card: &Value) -> Result<jose::Protected, jose::JoseError> {
    let signatures = card[SIGNATURES]
        .as_array()
        .ok_or(jose::JoseError::Malformed)?;
    // Exactly one signature: a second one would either be redundant or an
    // attempt to smuggle an unverified header.
    let [signature] = signatures.as_slice() else {
        return Err(jose::JoseError::Malformed);
    };
    let (Some(protected), Some(value)) = (
        signature["protected"].as_str(),
        signature["signature"].as_str(),
    ) else {
        return Err(jose::JoseError::Malformed);
    };
    if signature
        .as_object()
        .is_some_and(|s| s.contains_key("header"))
    {
        // Unprotected headers carry nothing we would trust; reject them so
        // a card cannot present conflicting `kid`/`jku` values.
        return Err(jose::JoseError::Malformed);
    }
    jose::parse_protected(protected, value)
}

/// Verify a card's signature with the key set served at its `jku`.
pub fn verify(card: &Value, keys: &jose::Jwks) -> Result<(), jose::JoseError> {
    let protected = signature_of(card)?;
    jose::verify(&protected, &signing_payload(card)?, keys)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn card() -> Value {
        json!({
            "name":"Test","description":"d","version":"0.6.0",
            "supportedInterfaces":[{"url":"https://api.example.com/a2a","protocolBinding":"JSONRPC","protocolVersion":"1.0","tenant":"t"}],
            "capabilities":{},"defaultInputModes":["application/json"],"defaultOutputModes":["application/json"],
            "securitySchemes":{"calendarOperation":{"httpAuthSecurityScheme":{"scheme":"Bearer","description":"x"}}},
            "skills":[{"id":"get_availability","name":"n","description":"d","tags":["calendar"],
                "securityRequirements":[{"calendarOperation":[]}]}]
        })
    }

    #[test]
    fn signed_card_is_canonical_wire_conformant_and_verifies_only_with_its_key() {
        let key = AgentKey::random();
        let signed = sign(card(), &key, "https://api.example.com/agents/t/jwks.json").unwrap();
        let value: Value = serde_json::from_slice(&signed.bytes).unwrap();
        assert_eq!(
            jose::canonicalize(&value).unwrap(),
            signed.bytes,
            "served bytes are canonical"
        );
        assert_eq!(signed.digest, jose::digest(&signed.bytes));
        assert_eq!(signed.version, "0.6.0");
        assert!(
            value["skills"][0]["securityRequirements"][0]["schemes"]["calendarOperation"]
                .is_object()
        );
        let protected = signature_of(&value).unwrap();
        assert_eq!(protected.kid(), Some(key.kid()));
        assert_eq!(
            protected.header["jku"],
            "https://api.example.com/agents/t/jwks.json"
        );
        assert_eq!(protected.header["typ"], "JOSE");
        let keys = jose::Jwks::parse(&key.jwks()).unwrap();
        verify(&value, &keys).unwrap();
        // The SDK reads the served bytes after the in-memory adaptation.
        sdk_view(value.clone()).unwrap();

        let other = jose::Jwks::parse(&AgentKey::random().jwks()).unwrap();
        assert_eq!(
            verify(&value, &other).unwrap_err(),
            jose::JoseError::UnknownKey
        );
        let mut tampered = value.clone();
        tampered["name"] = json!("Evil");
        assert_eq!(
            verify(&tampered, &keys).unwrap_err(),
            jose::JoseError::Signature
        );
        let mut two = value.clone();
        two["signatures"]
            .as_array_mut()
            .unwrap()
            .push(value["signatures"][0].clone());
        assert_eq!(verify(&two, &keys).unwrap_err(), jose::JoseError::Malformed);
        let mut unprotected = value.clone();
        unprotected["signatures"][0]["header"] = json!({"kid":"other"});
        assert_eq!(
            verify(&unprotected, &keys).unwrap_err(),
            jose::JoseError::Malformed
        );
    }

    #[test]
    fn loopback_jku_is_omitted_and_invalid_cards_are_refused() {
        let key = AgentKey::random();
        let signed = sign(card(), &key, "http://127.0.0.1:1/agents/t/jwks.json").unwrap();
        let value: Value = serde_json::from_slice(&signed.bytes).unwrap();
        assert!(signature_of(&value).unwrap().header.get("jku").is_none());
        let mut no_version = card();
        no_version["version"] = json!("");
        assert!(sign(no_version, &key, "https://x/").is_err());
        assert!(sign(json!({"version":"1.0"}), &key, "https://x/").is_err());
        // Re-signing replaces, never appends.
        let again = sign(value, &key, "https://api.example.com/agents/t/jwks.json").unwrap();
        let again: Value = serde_json::from_slice(&again.bytes).unwrap();
        assert_eq!(again["signatures"].as_array().unwrap().len(), 1);
    }
}
