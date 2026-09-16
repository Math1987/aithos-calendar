use crate::{agents::Agent, storage::Record};
use a2a_card::canonical::{b64url, canonicalize, signing_input};
use p256::ecdsa::{Signature, SigningKey, signature::Signer};
use serde_json::json;
use sha2::{Digest, Sha256};
use std::time::{Duration, SystemTime};

#[derive(Clone)]
pub struct Registry {
    pub origin: String,
    http: reqwest::Client,
}
impl Registry {
    pub fn new(origin: &str) -> Result<Self, lambda_http::Error> {
        let url = reqwest::Url::parse(origin)?;
        let local = matches!(url.host_str(), Some("127.0.0.1" | "localhost" | "[::1]"));
        if !(url.scheme() == "https" || (local && url.scheme() == "http"))
            || url.path() != "/"
            || url.query().is_some()
            || url.fragment().is_some()
            || !url.username().is_empty()
            || url.password().is_some()
        {
            return Err("Registry must be an HTTPS origin (loopback HTTP in tests)".into());
        }
        Ok(Self {
            origin: origin.trim_end_matches('/').into(),
            http: reqwest::Client::builder()
                .connect_timeout(Duration::from_secs(2))
                .timeout(Duration::from_secs(3))
                .redirect(reqwest::redirect::Policy::none())
                .build()?,
        })
    }
    pub fn prepare(
        &self,
        agent: Agent,
        owner: String,
        base: &str,
    ) -> Result<(Record, String), lambda_http::Error> {
        let key = SigningKey::random(&mut rand_core::OsRng);
        let point = key.verifying_key().to_encoded_point(false);
        let jwk = json!({"kty":"EC", "crv":"P-256", "x":b64url(point.x().unwrap()), "y":b64url(point.y().unwrap())});
        let kid = b64url(&Sha256::digest(canonicalize(&jwk)?));
        let mut header_fields = json!({"alg":"ES256", "typ":"JOSE", "kid":kid});
        // A2A requires HTTPS for jku. The key is supplied separately to the
        // registry, so omit this optional discovery hint for loopback tests.
        if self.origin.starts_with("https://") {
            header_fields["jku"] = json!(format!("{}/v1/agents/{kid}/jwks.json", self.origin));
        }
        let header = b64url(&canonicalize(&header_fields)?);
        let mut card = serde_json::to_value(agent.card(base))?;
        let payload = canonicalize(&card)?;
        let signature: Signature = key.sign(&signing_input(&header, &payload));
        card["signatures"] =
            json!([{"protected":header,"signature":b64url(&signature.to_bytes())}]);
        let canonical = a2a_card::validate_value(card)?;
        let now: chrono::DateTime<chrono::Utc> = SystemTime::now().into();
        let proof_payload = canonicalize(
            &json!({"action":"publish", "agentId":kid, "cardDigest":canonical.digest,
            "registryOrigin":self.origin, "issuedAt":now.to_rfc3339_opts(chrono::SecondsFormat::Secs, true)}),
        )?;
        let proof_header = b64url(&canonicalize(
            &json!({"alg":"ES256", "typ":"JOSE", "kid":kid}),
        )?);
        let proof_signature: Signature = key.sign(&signing_input(&proof_header, &proof_payload));
        let publication = json!({"agentCard":canonical.value,"keys":[jwk],"proofs":[{
            "protected":proof_header,"payload":b64url(&proof_payload),"signature":b64url(&proof_signature.to_bytes())
        }]});
        Ok((
            Record {
                card_url: format!("{}/v1/agents/{kid}/agent-card.json", self.origin),
                agent,
                owner,
                registry_id: kid,
                card_bytes: String::from_utf8(canonical.bytes)?,
                card_digest: canonical.digest,
                publication,
                published: false,
            },
            b64url(&key.to_bytes()),
        ))
    }
    pub async fn publish(&self, record: &Record) -> Result<(), &'static str> {
        let response = self
            .http
            .put(format!("{}/v1/agents/{}", self.origin, record.registry_id))
            .json(&record.publication)
            .send()
            .await
            .map_err(|_| "registry_unavailable")?;
        if !response.status().is_success() {
            tracing::warn!(
                event = "registry_write_failed",
                status = response.status().as_u16()
            );
            return Err("registry_rejected");
        }
        // Verify the public bytes before making the agent discoverable locally.
        // Registry caching may briefly lag a successful PUT; replay is safe.
        let mut response = self
            .http
            .get(&record.card_url)
            .send()
            .await
            .map_err(|_| "registry_unavailable")?;
        if !response.status().is_success() {
            return Err("registry_not_ready");
        }
        let mut bytes = Vec::new();
        while let Some(chunk) = response.chunk().await.map_err(|_| "registry_unavailable")? {
            if bytes.len() + chunk.len() > 64 * 1024 {
                return Err("registry_card_mismatch");
            }
            bytes.extend_from_slice(&chunk);
        }
        if bytes != record.card_bytes.as_bytes() {
            return Err("registry_card_mismatch");
        }
        Ok(())
    }
}
