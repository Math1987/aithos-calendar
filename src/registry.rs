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
        booking_page_url: Option<String>,
        base: &str,
    ) -> Result<(Record, String), lambda_http::Error> {
        let key = SigningKey::random(&mut rand_core::OsRng);
        self.prepare_signed(agent, booking_page_url, base, key)
    }
    /// Operator-only upgrade: preserve the tenant, signing key and registry URL.
    pub fn upgrade_to_live(
        &self,
        record: &Record,
        encoded_key: &str,
        base: &str,
    ) -> Result<Record, lambda_http::Error> {
        if record.agent.live {
            return Err("Agent is already live".into());
        }
        let page = record
            .booking_page_url
            .as_ref()
            .ok_or("Fixture agents cannot be upgraded")?;
        if (crate::booking_page::BookingPage { url: page.clone() }).agent_id() != record.agent.id {
            return Err("Stored page does not match its tenant".into());
        }
        let previous: serde_json::Value = serde_json::from_str(&record.card_bytes)?;
        if previous["version"] != "0.3.0" {
            return Err("Unsupported migration source version".into());
        }
        let bytes = a2a_card::canonical::b64url_decode(encoded_key)?;
        let key = SigningKey::from_slice(&bytes).map_err(|_| "Invalid recovery key")?;
        let mut agent = record.agent.clone();
        agent.live = true;
        agent.slots.clear();
        agent.name = format!("Booking page {}", &agent.id[..8]);
        let (updated, _) =
            self.prepare_signed(agent, record.booking_page_url.clone(), base, key)?;
        if updated.registry_id != record.registry_id || updated.card_url != record.card_url {
            return Err("Recovery key or registry differs from existing identity".into());
        }
        Ok(updated)
    }
    /// Operator-only account-card upgrade; preserves the original signing identity.
    pub fn upgrade_account(
        &self,
        record: &Record,
        encoded_key: &str,
        base: &str,
    ) -> Result<Record, lambda_http::Error> {
        if !record.agent.google_account || record.booking_page_url.is_some() {
            return Err("Not an account-linked agent".into());
        }
        let previous: serde_json::Value = serde_json::from_str(&record.card_bytes)?;
        if previous["version"] != "0.5.0" {
            return Err("Unsupported account-card version".into());
        }
        let bytes = a2a_card::canonical::b64url_decode(encoded_key)?;
        let key = SigningKey::from_slice(&bytes).map_err(|_| "Invalid recovery key")?;
        let (updated, _) = self.prepare_signed(record.agent.clone(), None, base, key)?;
        if updated.registry_id != record.registry_id || updated.card_url != record.card_url {
            return Err("Recovery key differs from existing identity".into());
        }
        Ok(updated)
    }
    fn prepare_signed(
        &self,
        agent: Agent,
        booking_page_url: Option<String>,
        base: &str,
        key: SigningKey,
    ) -> Result<(Record, String), lambda_http::Error> {
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
        // SDK 0.3.1 serializes requirements as OpenAPI maps; the A2A 1.0
        // wire schema uses schemes -> StringList. Normalize before signing.
        fn wire_requirements(v: &mut serde_json::Value) {
            if let Some(requirements) = v
                .get_mut("securityRequirements")
                .and_then(|r| r.as_array_mut())
            {
                for requirement in requirements {
                    if let Some(map) = requirement.as_object() {
                        let schemes: serde_json::Map<String, serde_json::Value> = map
                            .iter()
                            .map(|(k, v)| {
                                (
                                    k.clone(),
                                    if v.as_array().is_some_and(Vec::is_empty) {
                                        json!({})
                                    } else {
                                        json!({"list":v})
                                    },
                                )
                            })
                            .collect();
                        *requirement = json!({"schemes":schemes});
                    }
                }
            }
        }
        wire_requirements(&mut card);
        if let Some(skills) = card.get_mut("skills").and_then(|s| s.as_array_mut()) {
            for skill in skills {
                wire_requirements(skill);
            }
        }
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
                booking_page_url,
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

#[cfg(test)]
mod account_tests {
    use super::*;
    #[test]
    fn account_card_uses_a2a_wire_security_schema() {
        let registry = Registry::new("https://registry.example.com").unwrap();
        let agent = Agent {
            id: "test".into(),
            name: "Test".into(),
            google_account: true,
            live: false,
            slots: vec![],
        };
        let (r, _) = registry
            .prepare(agent, None, "https://api.example.com")
            .unwrap();
        let v: serde_json::Value = serde_json::from_str(&r.card_bytes).unwrap();
        assert!(
            v["skills"][1]["securityRequirements"][0]["schemes"]["calendarOperation"].is_object()
        );
    }
}
