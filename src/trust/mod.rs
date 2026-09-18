//! The trust layer: who signs what, with which key, over which bytes.
//!
//! Three roles hold keys (see `docs/trust-layer.md`):
//!
//! | Role | Signs | Bytes signed | Public key location |
//! | --- | --- | --- | --- |
//! | **Agent** (one P-256 key per agent) | its A2A Agent Card | JCS(card without `signatures`) | `https://<api>/agents/{id}/jwks.json` (card `jku`) |
//! | **Operator** (hosts catalog and cards) | the catalog document and the host `trustManifest` | JCS(document without `signature`) | `https://<api>/.well-known/jwks.json` (= `host.identifier`) |
//! | **Guarantor** (simulated trust provider) | every entry `trustManifest`, including its attestations | JCS(manifest without `signature`) | `https://<api>/trust-provider/.well-known/jwks.json` (= manifest `identity`) |
//!
//! The operator and the guarantor are two keys in this proof of concept even
//! though one deployment runs both, so that a consumer can pin guarantors
//! independently of the catalogs it reads. [`TrustProvider`] is the
//! guarantor role: [`LocalTrust`] is the only implementation, and an
//! external trust provider would be another implementation selected by
//! `TRUST_PROVIDER`, without changes to the A2A code or the catalog format.
//! Key custody is delegated to an [`OperatorSigner`]: in-memory for tests and
//! local runs, AWS KMS in production so private keys never leave the HSM.
//!
//! [`verify`] is the consuming side and depends on none of the above.
use async_trait::async_trait;
use p256::ecdsa::{Signature, SigningKey, VerifyingKey, signature::Signer};
use serde_json::{Value, json};
use std::sync::Arc;

pub mod card;
pub mod jose;
pub mod kms;
pub mod manifest;
pub mod policy;
pub mod verify;

pub use policy::{Policies, Policy};

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
    /// A key derived from a seed, so every process of one deployment holds
    /// the same lab keys. Never used for a production role.
    pub fn from_seed(seed: &str, purpose: &str) -> Self {
        use sha2::Digest;
        // Retry on the (about 2^-32) chance that the digest is not a scalar.
        for counter in 0u32.. {
            let scalar = sha2::Sha256::digest(format!("{purpose}:{counter}:{seed}").as_bytes());
            if let Ok(key) = SigningKey::from_slice(&scalar) {
                return Self::from_key(key);
            }
        }
        unreachable!("a valid scalar is found")
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
    /// The private key, for the lab's impostor agent only.
    pub fn signing_key(&self) -> &SigningKey {
        &self.key
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

/// What the guarantor knows about the agent behind an entry. Only facts
/// established by this deployment reach a manifest; nothing identifying.
#[derive(Clone, Debug, Default)]
pub struct Claims {
    /// The agent belongs to an account whose identity was verified by
    /// Google sign-in (OpenID Connect, `email_verified`).
    pub account_verified: bool,
}

/// Attestation type for a verified account (AI Catalog `attestations[].type`
/// is free-form; this value is documented in `docs/trust-layer.md`).
pub const ACCOUNT_VERIFIED: &str = "account-verified";

/// The guarantor role of AI Catalog: signs entry manifests that bind an
/// exact card and carry attestations a consumer's policy can require.
#[async_trait]
pub trait TrustProvider: Send + Sync {
    /// Guarantor identity URI: the URL of its JWK Set, so an AI Catalog
    /// consumer resolves the verification key from the identity alone
    /// (AI Catalog §Key Resolution, HTTPS URL form).
    fn identity(&self) -> &str;
    /// The guarantor JWK Set document, served verbatim at [`Self::identity`].
    fn jwks(&self) -> Value;
    /// Build and sign the `trustManifest` of one entry, binding
    /// `subject.digest` to the exact served card bytes.
    async fn manifest_for(
        &self,
        entry: &EntryDraft,
        card_bytes: &[u8],
        claims: &Claims,
    ) -> Result<Value, TrustError>;
    /// Same, with an explicit validity window. Only the scenario lab uses
    /// it, to obtain a genuinely signed but expired manifest.
    async fn manifest_dated(
        &self,
        entry: &EntryDraft,
        card_bytes: &[u8],
        claims: &Claims,
        issued_at: chrono::DateTime<chrono::Utc>,
        expires_at: chrono::DateTime<chrono::Utc>,
    ) -> Result<Value, TrustError>;
}

async fn sign_detached(signer: &dyn OperatorSigner, payload: &[u8]) -> Result<String, TrustError> {
    let protected = jose::protected_header(signer.kid(), json!({}))?;
    let signature = signer
        .sign(&jose::signing_input(&protected, payload))
        .await?;
    Ok(jose::detached_compact(&protected, &signature))
}

/// The operator role: hosts the catalog and signs the document itself.
pub struct Operator {
    identity: String,
    signer: Arc<dyn OperatorSigner>,
    pub manifest_ttl: chrono::Duration,
}

impl Operator {
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
    pub fn ephemeral(base_url: &str) -> Self {
        Self::new(base_url, Arc::new(MemorySigner::random()))
    }
    pub fn identity(&self) -> &str {
        &self.identity
    }
    pub fn jwks(&self) -> Value {
        json!({"keys": [self.signer.public_jwk()]})
    }
    /// The host's own `trustManifest`, binding the operator JWK Set.
    pub async fn host_manifest(&self) -> Result<Value, TrustError> {
        let now = chrono::Utc::now();
        let jwks_bytes = jose::canonicalize(&self.jwks())?;
        let mut manifest = manifest::host_draft(
            &self.identity,
            &jose::digest(&jwks_bytes),
            now,
            now + self.manifest_ttl,
        );
        let signature =
            sign_detached(self.signer.as_ref(), &manifest::signing_payload(&manifest)?).await?;
        manifest["signature"] = json!(signature);
        Ok(manifest)
    }
    /// Sign the whole catalog: `signature` over JCS(catalog without `signature`).
    pub async fn sign_catalog(&self, catalog: &mut Value) -> Result<(), TrustError> {
        if !catalog.is_object() {
            return Err(TrustError::InvalidInput("catalog"));
        }
        let signature =
            sign_detached(self.signer.as_ref(), &manifest::signing_payload(catalog)?).await?;
        catalog["signature"] = json!(signature);
        Ok(())
    }
}

/// The proof-of-concept guarantor: signs manifests in this process with the
/// key held by its [`OperatorSigner`].
pub struct LocalTrust {
    identity: String,
    signer: Arc<dyn OperatorSigner>,
    /// Further public keys published in the JWK Set (a rotation successor).
    extra_keys: Vec<Value>,
    /// Validity of a freshly signed manifest.
    pub manifest_ttl: chrono::Duration,
}

impl LocalTrust {
    /// Guarantor identity for an API base URL: its JWK Set URL, on the same
    /// host as the operator so AI Catalog's domain alignment rule holds.
    pub fn identity_for(base_url: &str) -> String {
        format!(
            "{}/trust-provider/.well-known/jwks.json",
            base_url.trim_end_matches('/')
        )
    }
    pub fn new(base_url: &str, signer: Arc<dyn OperatorSigner>) -> Self {
        Self {
            identity: Self::identity_for(base_url),
            signer,
            extra_keys: Vec::new(),
            manifest_ttl: chrono::Duration::days(90),
        }
    }
    /// A guarantor with a random in-memory key (tests, local runs).
    pub fn ephemeral(base_url: &str) -> Self {
        Self::new(base_url, Arc::new(MemorySigner::random()))
    }
    /// Publish an additional public key (with `kid`) in the JWK Set: how a
    /// rotation successor becomes verifiable before it signs anything.
    pub fn with_published_key(mut self, jwk: Value) -> Self {
        self.extra_keys.push(jwk);
        self
    }
    /// A guarantor that signs with `signer` under this guarantor's identity
    /// (a rotation successor, or a lab impostor).
    pub fn signing_as(&self, signer: Arc<dyn OperatorSigner>) -> Self {
        Self {
            identity: self.identity.clone(),
            signer,
            extra_keys: Vec::new(),
            manifest_ttl: self.manifest_ttl,
        }
    }
    /// Same signer under another identity (a lab guarantor nobody pins).
    pub fn with_identity(mut self, identity: String) -> Self {
        self.identity = identity;
        self
    }
}

#[async_trait]
impl TrustProvider for LocalTrust {
    fn identity(&self) -> &str {
        &self.identity
    }
    fn jwks(&self) -> Value {
        let mut keys = vec![self.signer.public_jwk()];
        keys.extend(self.extra_keys.iter().cloned());
        json!({"keys": keys})
    }
    async fn manifest_for(
        &self,
        entry: &EntryDraft,
        card_bytes: &[u8],
        claims: &Claims,
    ) -> Result<Value, TrustError> {
        let now = chrono::Utc::now();
        self.manifest_dated(entry, card_bytes, claims, now, now + self.manifest_ttl)
            .await
    }
    async fn manifest_dated(
        &self,
        entry: &EntryDraft,
        card_bytes: &[u8],
        claims: &Claims,
        issued_at: chrono::DateTime<chrono::Utc>,
        expires_at: chrono::DateTime<chrono::Utc>,
    ) -> Result<Value, TrustError> {
        let mut manifest = manifest::draft(
            &self.identity,
            entry,
            &jose::digest(card_bytes),
            issued_at,
            expires_at,
        );
        if claims.account_verified {
            manifest["attestations"] = json!([manifest::account_attestation(issued_at)]);
        }
        let signature =
            sign_detached(self.signer.as_ref(), &manifest::signing_payload(&manifest)?).await?;
        manifest["signature"] = json!(signature);
        Ok(manifest)
    }
}

/// Key custody for one role from configuration: a KMS key id when set,
/// otherwise an ephemeral in-memory key, which is only acceptable for local
/// runs because the published identity changes on every start.
async fn signer_from_env(
    variable: &str,
    kms: Option<&aws_sdk_kms::Client>,
) -> Result<Arc<dyn OperatorSigner>, lambda_http::Error> {
    Ok(match (std::env::var(variable), kms) {
        (Ok(key_id), Some(client)) => Arc::new(kms::KmsSigner::load(client.clone(), key_id).await?),
        (Ok(_), None) => return Err(format!("{variable} requires an AWS client").into()),
        (Err(_), _) => {
            tracing::warn!(
                target: "calendar::trust",
                event = "ephemeral_key",
                role = variable,
                "no KMS key configured; using an in-memory key"
            );
            Arc::new(MemorySigner::random())
        }
    })
}

/// The operator key from `OPERATOR_KMS_KEY_ID`.
pub async fn operator_from_env(
    base_url: &str,
    kms: Option<&aws_sdk_kms::Client>,
) -> Result<Arc<Operator>, lambda_http::Error> {
    Ok(Arc::new(Operator::new(
        base_url,
        signer_from_env("OPERATOR_KMS_KEY_ID", kms).await?,
    )))
}

/// The guarantor from configuration. `TRUST_PROVIDER` names the
/// implementation (only `local` exists); `TRUST_KMS_KEY_ID` moves its key
/// into KMS.
pub async fn from_env(
    base_url: &str,
    kms: Option<&aws_sdk_kms::Client>,
    lab: &crate::lab::Lab,
) -> Result<Arc<dyn TrustProvider>, lambda_http::Error> {
    let provider = std::env::var("TRUST_PROVIDER").unwrap_or_else(|_| "local".into());
    if provider != "local" {
        return Err(
            format!("Unknown TRUST_PROVIDER `{provider}`; only `local` is implemented").into(),
        );
    }
    Ok(Arc::new(
        LocalTrust::new(base_url, signer_from_env("TRUST_KMS_KEY_ID", kms).await?)
            .with_published_key(lab.next_jwk()),
    ))
}
