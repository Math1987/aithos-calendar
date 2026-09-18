//! The trust layer: who signs what, with which key, over which bytes.
//!
//! Three artifacts are signed and later verified end to end
//! (see `docs/trust-layer.md`):
//!
//! | Artifact | Signer | Bytes signed | Where the public key is |
//! | --- | --- | --- | --- |
//! | A2A Agent Card | the agent's own P-256 key, one per agent | JCS(card without `signatures`) | `https://<api>/agents/{id}/jwks.json` (`jku`) |
//! | AI Catalog `trustManifest` (host and every entry) | the catalog operator key | JCS(manifest without `signature`) | `https://<api>/.well-known/jwks.json` (= `identity`) |
//! | AI Catalog document | the catalog operator key | JCS(catalog without `signature`) | same operator key set |
//!
//! [`TrustProvider`] is the producing side. [`LocalTrust`] is the only
//! implementation in this proof of concept: trust decisions are made in this
//! process, and key custody is delegated to an [`OperatorSigner`] — an
//! in-memory key for tests and local runs, or AWS KMS in production so that
//! the private key never leaves the HSM boundary. A future external trust
//! provider is another `TrustProvider` implementation selected by
//! configuration (`TRUST_PROVIDER`); the A2A code and the catalog format do
//! not change.
//!
//! [`verify`] is the consuming side and has no dependency on the provider.
use async_trait::async_trait;
use p256::ecdsa::{Signature, SigningKey, VerifyingKey, signature::Signer};
use serde_json::{Value, json};
use std::sync::Arc;

pub mod card;
pub mod jose;
pub mod kms;
pub mod manifest;
pub mod verify;

pub use jose::{JoseError, Jwks};

#[derive(Debug)]
pub enum TrustError {
    Jose(JoseError),
    /// The signer (for example KMS) could not produce a signature.
    SignerUnavailable,
    /// The input is not a card or catalog this provider can sign.
    InvalidInput(&'static str),
}
impl From<JoseError> for TrustError {
    fn from(e: JoseError) -> Self {
        Self::Jose(e)
    }
}
impl std::fmt::Display for TrustError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Jose(e) => write!(f, "{e}"),
            Self::SignerUnavailable => f.write_str("signer_unavailable"),
            Self::InvalidInput(what) => write!(f, "invalid_input:{what}"),
        }
    }
}
impl std::error::Error for TrustError {}

/// Custody of the catalog operator's private key.
#[async_trait]
pub trait OperatorSigner: Send + Sync {
    /// RFC 7638 thumbprint of the public key, used as `kid`.
    fn kid(&self) -> &str;
    /// Public JWK including `kid`.
    fn public_jwk(&self) -> Value;
    /// Raw `r || s` ES256 signature over the JWS signing input.
    async fn sign(&self, signing_input: &[u8]) -> Result<[u8; 64], TrustError>;
}

/// An in-memory P-256 operator key. Used by tests and local runs; never in
/// production, where [`kms::KmsSigner`] is selected instead.
pub struct MemorySigner {
    key: SigningKey,
    jwk: Value,
    kid: String,
}
impl MemorySigner {
    pub fn random() -> Self {
        Self::from_key(SigningKey::random(&mut rand_core::OsRng))
    }
    pub fn from_key(key: SigningKey) -> Self {
        let mut jwk = jose::public_jwk(key.verifying_key());
        let kid = jose::thumbprint(&jwk).expect("public JWK canonicalizes");
        jwk["kid"] = json!(kid);
        Self { key, jwk, kid }
    }
    pub fn verifying_key(&self) -> &VerifyingKey {
        self.key.verifying_key()
    }
}
#[async_trait]
impl OperatorSigner for MemorySigner {
    fn kid(&self) -> &str {
        &self.kid
    }
    fn public_jwk(&self) -> Value {
        self.jwk.clone()
    }
    async fn sign(&self, signing_input: &[u8]) -> Result<[u8; 64], TrustError> {
        let signature: Signature = self.key.sign(signing_input);
        Ok(signature.to_bytes().into())
    }
}

/// One agent's signing key. Generated at onboarding, stored separately from
/// the readable record, and needed again only to re-sign that agent's card.
pub struct AgentKey {
    key: SigningKey,
    jwk: Value,
    kid: String,
}
impl AgentKey {
    pub fn random() -> Self {
        Self::from_key(SigningKey::random(&mut rand_core::OsRng))
    }
    pub fn from_key(key: SigningKey) -> Self {
        let mut jwk = jose::public_jwk(key.verifying_key());
        let kid = jose::thumbprint(&jwk).expect("public JWK canonicalizes");
        jwk["kid"] = json!(kid);
        Self { key, jwk, kid }
    }
    /// Base64url of the raw private scalar, the storage form.
    pub fn encode(&self) -> String {
        jose::b64url(&self.key.to_bytes())
    }
    pub fn decode(encoded: &str) -> Result<Self, TrustError> {
        let bytes = jose::b64url_decode(encoded)?;
        SigningKey::from_slice(&bytes)
            .map(Self::from_key)
            .map_err(|_| TrustError::InvalidInput("agent_key"))
    }
    pub fn kid(&self) -> &str {
        &self.kid
    }
    /// `{"keys":[jwk]}`, the document served at the card's `jku`.
    pub fn jwks(&self) -> Value {
        json!({"keys": [self.jwk]})
    }
    fn sign(&self, signing_input: &[u8]) -> [u8; 64] {
        let signature: Signature = self.key.sign(signing_input);
        signature.to_bytes().into()
    }
}

/// A signed Agent Card, byte-exact as it will be served.
#[derive(Clone, Debug)]
pub struct SignedCard {
    /// The exact bytes to serve (RFC 8785 canonical, `signatures` included).
    pub bytes: Vec<u8>,
    /// `sha256:<hex>` of [`Self::bytes`], the AI Catalog `subject.digest`.
    pub digest: String,
    /// The card's own `version` member.
    pub version: String,
    /// `kid` of the signing key.
    pub kid: String,
}

/// What a catalog entry says about the artifact a manifest must bind.
#[derive(Clone, Debug)]
pub struct EntryDraft {
    pub identifier: String,
    pub entry_type: String,
    pub url: String,
}

#[async_trait]
pub trait TrustProvider: Send + Sync {
    /// Operator identity URI. It is the URL of the operator JWK Set, so an
    /// AI Catalog consumer resolves the verification key from the identity
    /// alone (AI Catalog §Key Resolution, HTTPS URL form).
    fn identity(&self) -> &str;
    /// The operator JWK Set document, served verbatim at [`Self::identity`].
    fn jwks(&self) -> Value;
    /// Sign an A2A Agent Card (A2A §Agent Card Signing: JWS + JCS) with the
    /// agent's key. `jku` is where the agent's JWK Set will be served.
    async fn sign_card(
        &self,
        card: Value,
        key: &AgentKey,
        jku: &str,
    ) -> Result<SignedCard, TrustError>;
    /// Build and sign the AI Catalog `trustManifest` of one entry, binding
    /// `subject.digest` to the exact served card bytes.
    async fn manifest_for(
        &self,
        entry: &EntryDraft,
        card_bytes: &[u8],
    ) -> Result<Value, TrustError>;
    /// The host's own `trustManifest`, binding the operator JWK Set.
    async fn host_manifest(&self) -> Result<Value, TrustError>;
    /// Sign the whole catalog: `signature` over JCS(catalog without `signature`).
    async fn sign_catalog(&self, catalog: &mut Value) -> Result<(), TrustError>;
}

/// The proof-of-concept provider: signs in this process with an
/// [`OperatorSigner`] for the operator key and the agent's own key for cards.
pub struct LocalTrust {
    identity: String,
    signer: Arc<dyn OperatorSigner>,
    /// Validity of a freshly signed manifest.
    pub manifest_ttl: chrono::Duration,
}

impl LocalTrust {
    /// Operator identity for an API base URL: the operator JWK Set URL.
    pub fn identity_for(base_url: &str) -> String {
        format!("{}/.well-known/jwks.json", base_url.trim_end_matches('/'))
    }
    pub fn new(base_url: &str, signer: Arc<dyn OperatorSigner>) -> Self {
        Self {
            identity: Self::identity_for(base_url),
            signer,
            manifest_ttl: chrono::Duration::days(90),
        }
    }
    /// A provider with a random in-memory operator key (tests, local runs).
    pub fn ephemeral(base_url: &str) -> Self {
        Self::new(base_url, Arc::new(MemorySigner::random()))
    }
    fn now() -> chrono::DateTime<chrono::Utc> {
        chrono::Utc::now()
    }
    async fn sign_detached(&self, payload: &[u8]) -> Result<String, TrustError> {
        let protected = jose::protected_header(self.signer.kid(), json!({}))?;
        let signature = self
            .signer
            .sign(&jose::signing_input(&protected, payload))
            .await?;
        Ok(jose::detached_compact(&protected, &signature))
    }
}

#[async_trait]
impl TrustProvider for LocalTrust {
    fn identity(&self) -> &str {
        &self.identity
    }
    fn jwks(&self) -> Value {
        json!({"keys": [self.signer.public_jwk()]})
    }
    async fn sign_card(
        &self,
        card: Value,
        key: &AgentKey,
        jku: &str,
    ) -> Result<SignedCard, TrustError> {
        card::sign(card, key, jku)
    }
    async fn manifest_for(
        &self,
        entry: &EntryDraft,
        card_bytes: &[u8],
    ) -> Result<Value, TrustError> {
        let now = Self::now();
        let mut manifest = manifest::draft(
            &self.identity,
            entry,
            &jose::digest(card_bytes),
            now,
            now + self.manifest_ttl,
        );
        let signature = self
            .sign_detached(&manifest::signing_payload(&manifest)?)
            .await?;
        manifest["signature"] = json!(signature);
        Ok(manifest)
    }
    async fn host_manifest(&self) -> Result<Value, TrustError> {
        let now = Self::now();
        let jwks_bytes = jose::canonicalize(&self.jwks())?;
        let mut manifest = manifest::host_draft(
            &self.identity,
            &jose::digest(&jwks_bytes),
            now,
            now + self.manifest_ttl,
        );
        let signature = self
            .sign_detached(&manifest::signing_payload(&manifest)?)
            .await?;
        manifest["signature"] = json!(signature);
        Ok(manifest)
    }
    async fn sign_catalog(&self, catalog: &mut Value) -> Result<(), TrustError> {
        if !catalog.is_object() {
            return Err(TrustError::InvalidInput("catalog"));
        }
        let signature = self
            .sign_detached(&manifest::signing_payload(catalog)?)
            .await?;
        catalog["signature"] = json!(signature);
        Ok(())
    }
}

/// Select the provider from configuration. `TRUST_PROVIDER` names the
/// implementation (only `local` exists); `TRUST_KMS_KEY_ID` moves the
/// operator key into KMS. Without it the key is ephemeral, which is only
/// acceptable for local runs because the catalog identity changes on every
/// start.
pub async fn from_env(
    base_url: &str,
    kms: Option<aws_sdk_kms::Client>,
) -> Result<Arc<dyn TrustProvider>, lambda_http::Error> {
    let provider = std::env::var("TRUST_PROVIDER").unwrap_or_else(|_| "local".into());
    if provider != "local" {
        return Err(
            format!("Unknown TRUST_PROVIDER `{provider}`; only `local` is implemented").into(),
        );
    }
    let signer: Arc<dyn OperatorSigner> = match (std::env::var("TRUST_KMS_KEY_ID"), kms) {
        (Ok(key_id), Some(client)) => Arc::new(kms::KmsSigner::load(client, key_id).await?),
        (Ok(_), None) => return Err("TRUST_KMS_KEY_ID requires an AWS client".into()),
        (Err(_), _) => {
            tracing::warn!(
                target: "calendar::trust",
                event = "ephemeral_operator_key",
                "TRUST_KMS_KEY_ID is not set; using an in-memory operator key"
            );
            Arc::new(MemorySigner::random())
        }
    };
    Ok(Arc::new(LocalTrust::new(base_url, signer)))
}
