//! Caller authentication (inbound direction), a prototype.
//!
//! A trust manifest authenticates the *called* agent to the caller. Nothing
//! in A2A or AI Catalog authenticates the *caller* to the called agent:
//! A2A only lets a card declare HTTP security schemes (bearer, OAuth2,
//! OpenID, mTLS), and AI Catalog describes artifacts, not requests.
//!
//! This prototype reuses the one thing every agent here already has, a
//! published key, to sign each request:
//!
//! ```text
//! Agent-Signature: BASE64URL(protected) . BASE64URL(claims) . BASE64URL(signature)
//! protected = {"alg":"ES256","kid":<caller card kid>,"typ":"a2a-caller+jws"}
//! claims    = {"iss":<caller urn>,"aud":<callee urn>,"mid":<messageId>,"op":<operation>,"iat":…,"exp":…}
//! ```
//!
//! The called agent resolves `iss` through the same catalog chain it uses
//! for outbound calls (catalog signature, manifest, card digest, card JWS,
//! at the policy level the operation requires), takes the caller's key set
//! from the card's `jku`, requires `kid` to be the key that signed the
//! caller's card, verifies the JWS, and checks `aud`, `mid`, `op` and the
//! validity window. `mid` binds the signature to this A2A message.
//!
//! Known limits, documented in `docs/trust-manifest-evaluation.md`: no
//! replay cache within the validity window (60 s), no channel binding, and
//! a card compromise is a caller compromise.
use super::{AgentKey, jose};
use chrono::{DateTime, Duration, Utc};
use serde_json::{Value, json};

pub const HEADER: &str = "agent-signature";
pub const TYP: &str = "a2a-caller+jws";
pub const VALIDITY: Duration = Duration::seconds(60);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CallerError {
    Missing,
    Malformed,
    Invalid,
    Expired,
    AudienceMismatch,
    MessageMismatch,
    /// The signing key is not the key that signed the caller's card.
    KeyMismatch,
    /// The caller failed the trust chain at the required policy level.
    Unguaranteed,
}

impl CallerError {
    pub fn code(self) -> &'static str {
        match self {
            Self::Missing => "caller_signature_missing",
            Self::Malformed => "caller_signature_malformed",
            Self::Invalid => "caller_signature_invalid",
            Self::Expired => "caller_signature_expired",
            Self::AudienceMismatch => "caller_audience_mismatch",
            Self::MessageMismatch => "caller_message_mismatch",
            Self::KeyMismatch => "caller_key_mismatch",
            Self::Unguaranteed => "caller_unguaranteed",
        }
    }
}

/// What a signed request asserts about itself.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Claims {
    pub issuer: String,
    pub audience: String,
    pub message_id: String,
    pub operation: String,
    pub kid: String,
}

pub fn sign(
    key: &AgentKey,
    issuer: &str,
    audience: &str,
    message_id: &str,
    operation: &str,
    now: DateTime<Utc>,
) -> Result<String, jose::JoseError> {
    let protected = jose::protected_header(key.kid(), json!({"typ": TYP}))?;
    let claims = jose::canonicalize(&json!({
        "iss": issuer, "aud": audience, "mid": message_id, "op": operation,
        "iat": now.timestamp(), "exp": (now + VALIDITY).timestamp(),
    }))?;
    let signature = key.sign(&jose::signing_input(&protected, &claims));
    Ok(format!(
        "{protected}.{}.{}",
        jose::b64url(&claims),
        jose::b64url(&signature)
    ))
}

/// Split the header value and read the unverified issuer, so the called
/// agent knows whom to resolve before it can verify anything.
pub fn issuer_of(header: &str) -> Result<String, CallerError> {
    let (_, claims, _) = split(header)?;
    claims["iss"]
        .as_str()
        .map(str::to_owned)
        .ok_or(CallerError::Malformed)
}

fn split(header: &str) -> Result<(jose::Protected, Value, Vec<u8>), CallerError> {
    let mut parts = header.split('.');
    let (Some(protected), Some(payload), Some(signature), None) =
        (parts.next(), parts.next(), parts.next(), parts.next())
    else {
        return Err(CallerError::Malformed);
    };
    let protected = jose::parse_protected(protected, signature).map_err(|error| match error {
        jose::JoseError::Algorithm => CallerError::Invalid,
        _ => CallerError::Malformed,
    })?;
    if protected.header["typ"] != TYP {
        return Err(CallerError::Malformed);
    }
    let payload = jose::b64url_decode(payload).map_err(|_| CallerError::Malformed)?;
    let claims: Value = serde_json::from_slice(&payload).map_err(|_| CallerError::Malformed)?;
    if !claims.is_object() {
        return Err(CallerError::Malformed);
    }
    Ok((protected, claims, payload))
}

/// Verify a header value with the caller's key set. `card_kid` is the key
/// that signed the caller's (already verified) card.
pub fn verify(
    header: &str,
    keys: &jose::Jwks,
    card_kid: &str,
    audience: &str,
    message_id: &str,
    operation: &str,
    now: DateTime<Utc>,
) -> Result<Claims, CallerError> {
    let (protected, claims, payload) = split(header)?;
    let kid = protected.kid().ok_or(CallerError::Malformed)?.to_owned();
    if kid != card_kid {
        return Err(CallerError::KeyMismatch);
    }
    jose::verify(&protected, &payload, keys).map_err(|_| CallerError::Invalid)?;
    // Claims are trusted only after the signature.
    let (Some(issuer), Some(aud), Some(mid), Some(op), Some(iat), Some(exp)) = (
        claims["iss"].as_str(),
        claims["aud"].as_str(),
        claims["mid"].as_str(),
        claims["op"].as_str(),
        claims["iat"].as_i64(),
        claims["exp"].as_i64(),
    ) else {
        return Err(CallerError::Malformed);
    };
    let ts = now.timestamp();
    if exp < ts || iat > ts + 300 || exp - iat > 2 * VALIDITY.num_seconds() {
        return Err(CallerError::Expired);
    }
    if aud != audience {
        return Err(CallerError::AudienceMismatch);
    }
    if mid != message_id || op != operation {
        return Err(CallerError::MessageMismatch);
    }
    Ok(Claims {
        issuer: issuer.to_owned(),
        audience: aud.to_owned(),
        message_id: mid.to_owned(),
        operation: op.to_owned(),
        kid,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn signed_requests_bind_issuer_audience_message_and_time() {
        let key = AgentKey::random();
        let keys = jose::Jwks::parse(&key.jwks()).unwrap();
        let now = Utc::now();
        let header = sign(
            &key,
            "urn:air:x:agent:a",
            "urn:air:x:agent:b",
            "m1",
            "get_availability",
            now,
        )
        .unwrap();
        assert_eq!(issuer_of(&header).unwrap(), "urn:air:x:agent:a");
        let ok = verify(
            &header,
            &keys,
            key.kid(),
            "urn:air:x:agent:b",
            "m1",
            "get_availability",
            now,
        )
        .unwrap();
        assert_eq!(ok.kid, key.kid());
        let check = |aud: &str, mid: &str, op: &str, at: DateTime<Utc>| {
            verify(&header, &keys, key.kid(), aud, mid, op, at).unwrap_err()
        };
        assert_eq!(
            check("urn:air:x:agent:c", "m1", "get_availability", now),
            CallerError::AudienceMismatch
        );
        assert_eq!(
            check("urn:air:x:agent:b", "m2", "get_availability", now),
            CallerError::MessageMismatch
        );
        assert_eq!(
            check("urn:air:x:agent:b", "m1", "commit_booking", now),
            CallerError::MessageMismatch
        );
        assert_eq!(
            check(
                "urn:air:x:agent:b",
                "m1",
                "get_availability",
                now + Duration::seconds(61)
            ),
            CallerError::Expired
        );
        let other = AgentKey::random();
        assert_eq!(
            verify(
                &header,
                &jose::Jwks::parse(&other.jwks()).unwrap(),
                key.kid(),
                "urn:air:x:agent:b",
                "m1",
                "get_availability",
                now
            )
            .unwrap_err(),
            CallerError::Invalid
        );
        assert_eq!(
            verify(
                &header,
                &keys,
                other.kid(),
                "urn:air:x:agent:b",
                "m1",
                "get_availability",
                now
            )
            .unwrap_err(),
            CallerError::KeyMismatch
        );
        // Extending the validity window without re-signing is detected.
        let payload = jose::b64url(&jose::canonicalize(&json!({"iss":"urn:air:x:agent:a","aud":"urn:air:x:agent:b","mid":"m1","op":"get_availability","iat":now.timestamp(),"exp":now.timestamp()+3600})).unwrap());
        let parts: Vec<&str> = header.split('.').collect();
        let tampered = format!("{}.{payload}.{}", parts[0], parts[2]);
        assert_eq!(
            verify(
                &tampered,
                &keys,
                key.kid(),
                "urn:air:x:agent:b",
                "m1",
                "get_availability",
                now
            )
            .unwrap_err(),
            CallerError::Invalid
        );
        assert_eq!(issuer_of("nope").unwrap_err(), CallerError::Malformed);
    }
}
