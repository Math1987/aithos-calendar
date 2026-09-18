//! Minimal JOSE toolkit shared by every signature in this service.
//!
//! Everything here is pure (no I/O, no clock). Three building blocks are
//! enough for A2A Agent Card signing and AI Catalog trust manifests:
//!
//! 1. RFC 8785 JSON Canonicalization Scheme (JCS), through the maintained
//!    `serde_jcs` crate, so that signer and verifier hash identical bytes.
//! 2. Unpadded base64url (RFC 7515 §2), through `base64ct`, which is
//!    constant-time and has no padding surprises.
//! 3. ES256 (ECDSA P-256 / SHA-256) detached JWS in compact serialization
//!    (RFC 7515 Appendix F): `BASE64URL(protected) || '.' || '' || '.' ||
//!    BASE64URL(signature)`, where the signing input is
//!    `ASCII(BASE64URL(protected) || '.' || BASE64URL(payload))`.
//!
//! Only ES256 is accepted. The expected algorithm is fixed by the trust
//! anchor (every key this service publishes is P-256), never read from the
//! `alg` header alone, as both specifications require.
use base64ct::{Base64UrlUnpadded, Encoding};
use p256::ecdsa::{Signature, VerifyingKey, signature::Verifier};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};

/// The only JWS algorithm this service produces or accepts.
pub const ALG: &str = "ES256";

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum JoseError {
    Canonicalization,
    Encoding,
    /// The compact serialization is not `header..signature`.
    Malformed,
    /// `alg` is missing, `none`, symmetric or otherwise not ES256.
    Algorithm,
    /// No key with the header's `kid` in the supplied key set.
    UnknownKey,
    /// The signature does not verify under the selected key.
    Signature,
    /// A JWK is not a well-formed P-256 public key.
    InvalidKey,
}

impl std::fmt::Display for JoseError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            Self::Canonicalization => "json_canonicalization_failed",
            Self::Encoding => "invalid_base64url",
            Self::Malformed => "malformed_jws",
            Self::Algorithm => "unsupported_jws_algorithm",
            Self::UnknownKey => "unknown_signing_key",
            Self::Signature => "invalid_signature",
            Self::InvalidKey => "invalid_jwk",
        })
    }
}
impl std::error::Error for JoseError {}

/// RFC 8785 canonical UTF-8 bytes of a JSON value.
pub fn canonicalize(value: &Value) -> Result<Vec<u8>, JoseError> {
    serde_jcs::to_vec(value).map_err(|_| JoseError::Canonicalization)
}

pub fn b64url(bytes: &[u8]) -> String {
    Base64UrlUnpadded::encode_string(bytes)
}

pub fn b64url_decode(text: &str) -> Result<Vec<u8>, JoseError> {
    Base64UrlUnpadded::decode_vec(text).map_err(|_| JoseError::Encoding)
}

/// `sha256:<lowercase hex>` (AI Catalog digest format).
pub fn digest(bytes: &[u8]) -> String {
    let mut out = String::with_capacity(71);
    out.push_str("sha256:");
    for byte in Sha256::digest(bytes) {
        use std::fmt::Write as _;
        let _ = write!(out, "{byte:02x}");
    }
    out
}

/// RFC 7515 §5.1 signing input for a detached payload.
pub fn signing_input(protected_b64: &str, payload: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(protected_b64.len() + 1 + payload.len().div_ceil(3) * 4);
    out.extend_from_slice(protected_b64.as_bytes());
    out.push(b'.');
    out.extend_from_slice(b64url(payload).as_bytes());
    out
}

/// Public P-256 JWK members, exactly the ones RFC 7638 thumbprints cover.
pub fn public_jwk(key: &VerifyingKey) -> Value {
    let point = key.to_encoded_point(false);
    json!({
        "kty": "EC",
        "crv": "P-256",
        "x": b64url(point.x().expect("uncompressed point has x")),
        "y": b64url(point.y().expect("uncompressed point has y")),
    })
}

/// RFC 7638 JWK thumbprint (base64url of SHA-256 over the canonical
/// required members), used as `kid` everywhere.
pub fn thumbprint(jwk: &Value) -> Result<String, JoseError> {
    let required = json!({"crv": jwk["crv"], "kty": jwk["kty"], "x": jwk["x"], "y": jwk["y"]});
    Ok(b64url(&Sha256::digest(canonicalize(&required)?)))
}

/// Parse a P-256 public JWK.
pub fn verifying_key(jwk: &Value) -> Result<VerifyingKey, JoseError> {
    if jwk["kty"] != "EC" || jwk["crv"] != "P-256" {
        return Err(JoseError::InvalidKey);
    }
    let x = b64url_decode(jwk["x"].as_str().ok_or(JoseError::InvalidKey)?)?;
    let y = b64url_decode(jwk["y"].as_str().ok_or(JoseError::InvalidKey)?)?;
    if x.len() != 32 || y.len() != 32 {
        return Err(JoseError::InvalidKey);
    }
    // Uncompressed SEC1 point: 0x04 || x || y.
    let mut sec1 = Vec::with_capacity(65);
    sec1.push(0x04);
    sec1.extend_from_slice(&x);
    sec1.extend_from_slice(&y);
    VerifyingKey::from_sec1_bytes(&sec1).map_err(|_| JoseError::InvalidKey)
}

/// A parsed JWK Set holding only the P-256 keys it could understand.
#[derive(Clone, Debug, Default)]
pub struct Jwks {
    keys: Vec<(String, VerifyingKey)>,
}

impl Jwks {
    /// Parse `{"keys":[...]}`. Keys that are not P-256 are ignored; a key
    /// whose `kid` does not equal its thumbprint is rejected so that a
    /// served key set cannot alias one key under another identifier.
    pub fn parse(document: &Value) -> Result<Self, JoseError> {
        let mut keys = Vec::new();
        for jwk in document["keys"].as_array().ok_or(JoseError::InvalidKey)? {
            if jwk["kty"] != "EC" || jwk["crv"] != "P-256" {
                continue;
            }
            let key = verifying_key(jwk)?;
            let kid = jwk["kid"].as_str().ok_or(JoseError::InvalidKey)?;
            if kid != thumbprint(jwk)? {
                return Err(JoseError::InvalidKey);
            }
            keys.push((kid.to_owned(), key));
        }
        Ok(Self { keys })
    }
    pub fn single(kid: String, key: VerifyingKey) -> Self {
        Self {
            keys: vec![(kid, key)],
        }
    }
    pub fn find(&self, kid: &str) -> Option<&VerifyingKey> {
        self.keys.iter().find(|(k, _)| k == kid).map(|(_, key)| key)
    }
    pub fn is_empty(&self) -> bool {
        self.keys.is_empty()
    }
    pub fn kids(&self) -> impl Iterator<Item = &str> {
        self.keys.iter().map(|(k, _)| k.as_str())
    }
}

/// The protected header every signature here carries. Extra members such as
/// `jku` or `typ` are added by the caller.
pub fn protected_header(kid: &str, extra: Value) -> Result<String, JoseError> {
    let mut header = json!({"alg": ALG, "kid": kid});
    if let Some(map) = extra.as_object() {
        for (k, v) in map {
            header[k] = v.clone();
        }
    }
    Ok(b64url(&canonicalize(&header)?))
}

/// Compact detached serialization `protected..signature`.
pub fn detached_compact(protected_b64: &str, signature: &[u8]) -> String {
    format!("{protected_b64}..{}", b64url(signature))
}

/// The decoded protected header of a JWS, after the checks that do not
/// depend on any key: three segments, empty payload, `alg` exactly ES256.
#[derive(Debug, Clone)]
pub struct Protected {
    pub encoded: String,
    pub header: Value,
    pub signature: Vec<u8>,
}

impl Protected {
    pub fn kid(&self) -> Option<&str> {
        self.header["kid"].as_str()
    }
}

/// Split and check a detached compact JWS without verifying it.
pub fn parse_detached(jws: &str) -> Result<Protected, JoseError> {
    let mut parts = jws.split('.');
    let (Some(header), Some(payload), Some(signature), None) =
        (parts.next(), parts.next(), parts.next(), parts.next())
    else {
        return Err(JoseError::Malformed);
    };
    if header.is_empty() || !payload.is_empty() || signature.is_empty() {
        return Err(JoseError::Malformed);
    }
    parse_protected(header, signature)
}

/// Check a `{protected, signature}` pair (A2A `AgentCardSignature`).
pub fn parse_protected(header: &str, signature: &str) -> Result<Protected, JoseError> {
    let decoded: Value =
        serde_json::from_slice(&b64url_decode(header)?).map_err(|_| JoseError::Malformed)?;
    if !decoded.is_object() {
        return Err(JoseError::Malformed);
    }
    // `none`, HS*, and every other algorithm are rejected here, before any
    // key is consulted. The trust anchor fixes ES256.
    if decoded["alg"] != ALG {
        return Err(JoseError::Algorithm);
    }
    let signature = b64url_decode(signature)?;
    if signature.len() != 64 {
        return Err(JoseError::Malformed);
    }
    Ok(Protected {
        encoded: header.to_owned(),
        header: decoded,
        signature,
    })
}

/// Verify a parsed signature over `payload` with the key named by `kid`.
pub fn verify(protected: &Protected, payload: &[u8], keys: &Jwks) -> Result<(), JoseError> {
    let key = protected
        .kid()
        .and_then(|kid| keys.find(kid))
        .ok_or(JoseError::UnknownKey)?;
    let signature =
        Signature::from_slice(&protected.signature).map_err(|_| JoseError::Malformed)?;
    key.verify(&signing_input(&protected.encoded, payload), &signature)
        .map_err(|_| JoseError::Signature)
}

#[cfg(test)]
mod tests {
    use super::*;
    use p256::ecdsa::{SigningKey, signature::Signer};

    /// RFC 8785 test vector (spec §3.2.3 / Appendix B style), reduced to
    /// members serde_json can represent exactly.
    #[test]
    fn jcs_orders_members_and_normalizes_numbers_and_strings() {
        let input = "{\"numbers\":[333333333.33333329,1E30,4.50,2e-3,0.000000000000000000000000001],\
            \"string\":\"\\u20ac$\\u000F\\u000aA'\\u0042\\u0022\\u005c\\\\\\\"\\/\",\
            \"literals\":[null,true,false]}";
        let value: Value = serde_json::from_str(input).unwrap();
        let expected = "{\"literals\":[null,true,false],\
            \"numbers\":[333333333.3333333,1e+30,4.5,0.002,1e-27],\
            \"string\":\"\u{20ac}$\\u000f\\nA'B\\\"\\\\\\\\\\\"/\"}";
        assert_eq!(
            String::from_utf8(canonicalize(&value).unwrap()).unwrap(),
            expected
        );
        let nested = json!({"b":{"z":1,"a":[{"y":2,"x":1}]},"a":"\u{fc}"});
        assert_eq!(
            canonicalize(&nested).unwrap(),
            "{\"a\":\"\u{fc}\",\"b\":{\"a\":[{\"x\":1,\"y\":2}],\"z\":1}}".as_bytes()
        );
    }

    #[test]
    fn digest_and_base64url_match_known_answers() {
        assert_eq!(
            digest(b"abc"),
            "sha256:ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
        );
        assert_eq!(b64url(b"\xfb\xff"), "-_8");
        assert_eq!(b64url_decode("-_8").unwrap(), b"\xfb\xff");
        assert_eq!(b64url_decode("-_8=").unwrap_err(), JoseError::Encoding);
        assert_eq!(b64url(b""), "");
    }

    #[test]
    fn thumbprint_is_rfc7638_over_required_members_only() {
        // Public key from RFC 7515 Appendix A.3 (ES256 example).
        let jwk = json!({"kty":"EC","crv":"P-256","kid":"ignored","use":"sig",
            "x":"f83OJ3D2xF1Bg8vub9tLe1gHMzV76e8Tus9uPHvRVEU","y":"x_FEzRu9m36HLN_tue659LNpXW6pCyStikYjKIWI5a0"});
        assert_eq!(
            thumbprint(&jwk).unwrap(),
            thumbprint(&json!({"kty":"EC","crv":"P-256","x":jwk["x"],"y":jwk["y"]})).unwrap()
        );
        assert_eq!(
            thumbprint(&jwk).unwrap(),
            "oKIywvGUpTVTyxMQ3bwIIeQUudfr_CkLMjCE19ECD-U"
        );
        verifying_key(&jwk).unwrap();
        let mut off_curve = jwk.clone();
        off_curve["y"] = jwk["x"].clone();
        assert_eq!(
            verifying_key(&off_curve).unwrap_err(),
            JoseError::InvalidKey
        );
    }

    #[test]
    fn detached_es256_round_trip_and_tamper_detection() {
        let key = SigningKey::random(&mut rand_core::OsRng);
        let jwk = public_jwk(key.verifying_key());
        let kid = thumbprint(&jwk).unwrap();
        let payload = canonicalize(&json!({"b":1,"a":"x"})).unwrap();
        let protected = protected_header(&kid, json!({})).unwrap();
        let signature: Signature = key.sign(&signing_input(&protected, &payload));
        let jws = detached_compact(&protected, &signature.to_bytes());
        assert_eq!(jws.split('.').count(), 3);
        assert_eq!(jws.split('.').nth(1), Some(""));

        let mut published = jwk.clone();
        published["kid"] = json!(kid);
        let keys = Jwks::parse(&json!({"keys":[published]})).unwrap();
        let parsed = parse_detached(&jws).unwrap();
        verify(&parsed, &payload, &keys).unwrap();
        assert_eq!(
            verify(&parsed, b"{\"a\":\"x\",\"b\":2}", &keys).unwrap_err(),
            JoseError::Signature
        );
        let other = Jwks::single("other".into(), *key.verifying_key());
        assert_eq!(
            verify(&parsed, &payload, &other).unwrap_err(),
            JoseError::UnknownKey
        );

        // A key set that aliases a key under a foreign kid is rejected outright.
        let mut aliased = jwk.clone();
        aliased["kid"] = json!("friendly-name");
        assert_eq!(
            Jwks::parse(&json!({"keys":[aliased]})).unwrap_err(),
            JoseError::InvalidKey
        );
    }

    #[test]
    fn algorithm_none_and_symmetric_headers_are_rejected_before_any_key() {
        let signature = b64url(&[0u8; 64]);
        for alg in ["none", "HS256", "ES384", "RS256"] {
            let header = b64url(&canonicalize(&json!({"alg":alg,"kid":"k"})).unwrap());
            assert_eq!(
                parse_detached(&format!("{header}..{signature}")).unwrap_err(),
                JoseError::Algorithm,
                "{alg}"
            );
        }
        let header = b64url(&canonicalize(&json!({"alg":"ES256","kid":"k"})).unwrap());
        assert_eq!(
            parse_detached(&format!("{header}.cGF5bG9hZA.{signature}")).unwrap_err(),
            JoseError::Malformed,
            "attached payloads are not detached JWS"
        );
        assert_eq!(
            parse_detached(&format!("{header}..{signature}.extra")).unwrap_err(),
            JoseError::Malformed
        );
        assert_eq!(
            parse_detached(&format!("{header}..{}", b64url(&[0u8; 63]))).unwrap_err(),
            JoseError::Malformed
        );
    }
}
