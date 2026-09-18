//! Operator key custody in AWS KMS (`ECC_NIST_P256`, `ECDSA_SHA_256`).
//!
//! The private key never leaves KMS: this process only hashes the JWS
//! signing input and asks KMS to sign the digest. The public key is read
//! once at startup and published as the operator JWK Set.
use super::{OperatorSigner, TrustError, jose};
use async_trait::async_trait;
use aws_sdk_kms::{
    primitives::Blob,
    types::{KeySpec, MessageType, SigningAlgorithmSpec},
};
use p256::{
    ecdsa::{Signature, VerifyingKey},
    pkcs8::DecodePublicKey,
};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};

pub struct KmsSigner {
    client: aws_sdk_kms::Client,
    key_id: String,
    jwk: Value,
    kid: String,
}

impl KmsSigner {
    pub async fn load(client: aws_sdk_kms::Client, key_id: String) -> Result<Self, TrustError> {
        let output = client
            .get_public_key()
            .key_id(&key_id)
            .send()
            .await
            .map_err(|error| {
                tracing::error!(target: "calendar::trust", event = "kms_public_key_unavailable", code = %error);
                TrustError::SignerUnavailable
            })?;
        if output.key_spec() != Some(&KeySpec::EccNistP256) {
            return Err(TrustError::InvalidInput("kms_key_spec"));
        }
        let der = output
            .public_key()
            .ok_or(TrustError::SignerUnavailable)?
            .as_ref();
        let public = p256::PublicKey::from_public_key_der(der)
            .map_err(|_| TrustError::InvalidInput("kms_public_key"))?;
        let verifying = VerifyingKey::from(&public);
        let mut jwk = jose::public_jwk(&verifying);
        let kid = jose::thumbprint(&jwk)?;
        jwk["kid"] = json!(kid);
        tracing::info!(target: "calendar::trust", event = "operator_key_loaded", kid = %kid);
        Ok(Self {
            client,
            key_id,
            jwk,
            kid,
        })
    }
}

#[async_trait]
impl OperatorSigner for KmsSigner {
    fn kid(&self) -> &str {
        &self.kid
    }
    fn public_jwk(&self) -> Value {
        self.jwk.clone()
    }
    async fn sign(&self, signing_input: &[u8]) -> Result<[u8; 64], TrustError> {
        let digest = Sha256::digest(signing_input);
        let output = self
            .client
            .sign()
            .key_id(&self.key_id)
            .message(Blob::new(digest.to_vec()))
            .message_type(MessageType::Digest)
            .signing_algorithm(SigningAlgorithmSpec::EcdsaSha256)
            .send()
            .await
            .map_err(|error| {
                tracing::error!(target: "calendar::trust", event = "kms_sign_failed", code = %error);
                TrustError::SignerUnavailable
            })?;
        let der = output
            .signature()
            .ok_or(TrustError::SignerUnavailable)?
            .as_ref();
        // KMS returns ASN.1 DER; JWS wants the fixed-size `r || s` form.
        let signature = Signature::from_der(der).map_err(|_| TrustError::SignerUnavailable)?;
        let signature = signature.normalize_s().unwrap_or(signature);
        Ok(signature.to_bytes().into())
    }
}
