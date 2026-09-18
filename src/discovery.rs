//! Discovery of a peer agent through the AI Catalog, with the trust chain
//! verified before any A2A call:
//!
//! 1. fetch the catalog and verify its signature under the operator key set
//!    named by `host.identifier`, which must live on the catalog's origin;
//! 2. select the entry, verify its `trustManifest` (signature by a pinned
//!    guarantor, `subject` restating the entry, validity window);
//! 3. fetch the card and check `sha256(bytes) == subject.digest`;
//! 4. verify the card's own JWS with the key set at its `jku`, which must be
//!    the agent's key location on the agent origin;
//! 5. only then hand the card to the A2A SDK.
//!
//! The origin allow-list that predates the trust chain is kept as a network
//! policy (which hosts this deployment will talk to), not as a source of
//! trust. Every fetch is HTTPS (loopback HTTP in tests), follows no
//! redirect, and is bounded in time and size.
use crate::{
    agents::Publisher,
    scheduling::Availability,
    trust::{card, jose, verify},
};
use a2a::{AgentCard, Message, Part, PartContent, Role, SendMessageRequest, SendMessageResponse};
use a2a_client::{A2AClientFactory, jsonrpc::JsonRpcTransportFactory};
use reqwest::{Client, Url};
use serde_json::{Value, json};
use std::{
    collections::HashMap,
    sync::{Arc, Mutex},
    time::{Duration, Instant},
};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PeerError {
    DiscoveryUnavailable,
    NotFound,
    InvalidCard,
    Unavailable,
    InvalidResponse,
    Timeout,
    /// The catalog lists the same identifier and version twice.
    DuplicateEntry,
    /// The catalog names an operator key set outside the catalog's origin.
    UntrustedOperator,
    /// The manifest was signed by a guarantor this client does not pin.
    UntrustedGuarantor,
    /// A verification step of the trust chain failed.
    Trust(verify::VerifyError),
    /// A key set could not be fetched or parsed.
    KeysUnavailable,
}

impl PeerError {
    pub fn code(self) -> &'static str {
        match self {
            Self::DiscoveryUnavailable => "discovery_unavailable",
            Self::NotFound => "peer_not_found",
            Self::InvalidCard => "invalid_peer_card",
            Self::Unavailable => "peer_unavailable",
            Self::InvalidResponse => "invalid_peer_response",
            Self::Timeout => "timeout",
            Self::DuplicateEntry => "catalog_duplicate_entry",
            Self::UntrustedOperator => "untrusted_operator",
            Self::UntrustedGuarantor => "untrusted_guarantor",
            Self::Trust(error) => error.code(),
            Self::KeysUnavailable => "keys_unavailable",
        }
    }
    pub fn message(self) -> &'static str {
        match self {
            Self::DiscoveryUnavailable => "The configured catalog could not be loaded",
            Self::NotFound => "The requested peer is not in the catalog",
            Self::InvalidCard => "The peer card is invalid or outside the allowed agent origin",
            Self::Unavailable => "The peer could not be reached through A2A",
            Self::InvalidResponse => "The peer did not return valid availability",
            Self::Timeout => "The peer exchange exceeded its time limit",
            Self::DuplicateEntry => {
                "The catalog lists the peer more than once for the same version"
            }
            Self::UntrustedOperator => "The catalog operator key set is not on the catalog origin",
            Self::UntrustedGuarantor => "The manifest guarantor is not a trusted identity",
            Self::Trust(_) => "The peer failed trust verification; no call was made",
            Self::KeysUnavailable => "A verification key set could not be loaded",
        }
    }
}

impl From<verify::VerifyError> for PeerError {
    fn from(error: verify::VerifyError) -> Self {
        Self::Trust(error)
    }
}

/// A card that passed every step of the chain.
pub struct VerifiedPeer {
    pub card: AgentCard,
    pub tenant: String,
    pub card_digest: String,
}

pub struct PeerDirectory {
    http: Client,
    live_http: Client,
    catalog_url: Url,
    agent_origin: Url,
    publisher: Publisher,
    trusted_guarantors: Vec<String>,
    keys: Mutex<HashMap<String, (Instant, Arc<jose::Jwks>)>>,
}

const KEYS_TTL: Duration = Duration::from_secs(300);
const MAX_DOCUMENT: usize = 64 * 1024;

fn network_error(error: reqwest::Error, otherwise: PeerError) -> PeerError {
    if error.is_timeout() {
        PeerError::Timeout
    } else {
        otherwise
    }
}

fn loopback(url: &Url) -> bool {
    matches!(url.host_str(), Some("127.0.0.1" | "localhost" | "[::1]"))
}

fn safe_url(url: &Url) -> bool {
    (url.scheme() == "https" || (url.scheme() == "http" && loopback(url)))
        && url.username().is_empty()
        && url.password().is_none()
        && url.fragment().is_none()
}

impl PeerDirectory {
    pub fn new(
        public_url: &str,
        catalog_url: &str,
        trusted_guarantors: Vec<String>,
    ) -> Result<Self, lambda_http::Error> {
        // Explicit provider avoids a platform-dependent TLS default in the SDK.
        let _ = a2a_client::rustls::crypto::aws_lc_rs::default_provider().install_default();
        let agent_origin = Url::parse(public_url)?;
        let catalog_url = Url::parse(catalog_url)?;
        for url in [&agent_origin, &catalog_url] {
            if !safe_url(url) {
                return Err("Discovery configuration requires HTTPS (or loopback HTTP), without credentials or fragments".into());
            }
        }
        for identity in &trusted_guarantors {
            if !Url::parse(identity).is_ok_and(|url| safe_url(&url)) {
                return Err("Trusted guarantors must be HTTPS JWK Set URLs".into());
            }
        }
        if trusted_guarantors.is_empty() {
            return Err("At least one trusted guarantor is required".into());
        }
        let http = Client::builder()
            .connect_timeout(Duration::from_secs(2))
            .timeout(Duration::from_secs(3))
            .redirect(reqwest::redirect::Policy::none())
            .build()?;
        Ok(Self {
            live_http: Client::builder()
                .connect_timeout(Duration::from_secs(2))
                .timeout(Duration::from_secs(10))
                .redirect(reqwest::redirect::Policy::none())
                .build()?,
            http,
            catalog_url,
            publisher: Publisher::from_base(public_url),
            agent_origin,
            trusted_guarantors,
            keys: Mutex::new(HashMap::new()),
        })
    }

    pub fn publisher(&self) -> &Publisher {
        &self.publisher
    }

    /// Network policy: this deployment only talks to its own agent origin.
    fn trusted_url(&self, value: &str) -> Result<Url, PeerError> {
        let url = Url::parse(value).map_err(|_| PeerError::InvalidCard)?;
        if url.origin() != self.agent_origin.origin() || !safe_url(&url) {
            return Err(PeerError::InvalidCard);
        }
        Ok(url)
    }

    async fn fetch_bytes(&self, url: Url, failure: PeerError) -> Result<Vec<u8>, PeerError> {
        let mut response = self
            .http
            .get(url)
            .send()
            .await
            .map_err(|e| network_error(e, failure))?;
        if !response.status().is_success() {
            return Err(failure);
        }
        let mut bytes = Vec::new();
        while let Some(chunk) = response
            .chunk()
            .await
            .map_err(|e| network_error(e, failure))?
        {
            if bytes.len() + chunk.len() > MAX_DOCUMENT {
                return Err(failure);
            }
            bytes.extend_from_slice(&chunk);
        }
        Ok(bytes)
    }

    /// The operator key set named by `host.identifier`: an HTTPS JWK Set on
    /// the catalog's own origin, so the catalog signature proves control of
    /// the origin it was fetched from.
    async fn operator_keys(&self, identity: &str) -> Result<Arc<jose::Jwks>, PeerError> {
        let url = Url::parse(identity).map_err(|_| PeerError::UntrustedOperator)?;
        if url.origin() != self.catalog_url.origin() || !safe_url(&url) {
            return Err(PeerError::UntrustedOperator);
        }
        self.keys_at(identity).await
    }

    /// The key set of a guarantor this client pins by configuration.
    async fn guarantor_keys(&self, identity: &str) -> Result<Arc<jose::Jwks>, PeerError> {
        if !self.trusted_guarantors.iter().any(|t| t == identity) {
            return Err(PeerError::UntrustedGuarantor);
        }
        self.keys_at(identity).await
    }

    /// The key set published at an identity URL, cached briefly.
    async fn keys_at(&self, identity: &str) -> Result<Arc<jose::Jwks>, PeerError> {
        if let Some((fetched, keys)) = self.keys.lock().ok().and_then(|c| c.get(identity).cloned())
            && fetched.elapsed() < KEYS_TTL
        {
            return Ok(keys);
        }
        let url = Url::parse(identity).map_err(|_| PeerError::KeysUnavailable)?;
        if !safe_url(&url) {
            return Err(PeerError::KeysUnavailable);
        }
        let bytes = self.fetch_bytes(url, PeerError::KeysUnavailable).await?;
        let document: Value =
            serde_json::from_slice(&bytes).map_err(|_| PeerError::KeysUnavailable)?;
        let keys = jose::Jwks::parse(&document).map_err(|_| PeerError::KeysUnavailable)?;
        if keys.is_empty() {
            return Err(PeerError::KeysUnavailable);
        }
        let keys = Arc::new(keys);
        if let Ok(mut cache) = self.keys.lock() {
            cache.insert(identity.to_owned(), (Instant::now(), keys.clone()));
        }
        Ok(keys)
    }

    /// The agent key set at the location this client derives itself.
    async fn agent_keys(&self, jwks_url: &Url) -> Result<jose::Jwks, PeerError> {
        let bytes = self
            .fetch_bytes(jwks_url.clone(), PeerError::KeysUnavailable)
            .await?;
        let document: Value =
            serde_json::from_slice(&bytes).map_err(|_| PeerError::KeysUnavailable)?;
        jose::Jwks::parse(&document).map_err(|_| PeerError::KeysUnavailable)
    }

    fn select_entry<'a>(&self, catalog: &'a Value, peer: &str) -> Result<&'a Value, PeerError> {
        let entries = catalog["entries"]
            .as_array()
            .ok_or(PeerError::DiscoveryUnavailable)?;
        let matching: Vec<&Value> = entries.iter().filter(|e| e["identifier"] == peer).collect();
        if matching.is_empty() {
            return Err(PeerError::NotFound);
        }
        // Multi-version listings must be unique on (identifier, version);
        // the newest `updatedAt` wins, then the highest version string.
        for (i, a) in matching.iter().enumerate() {
            if matching[..i].iter().any(|b| b["version"] == a["version"]) {
                return Err(PeerError::DuplicateEntry);
            }
        }
        let entry = matching
            .into_iter()
            .max_by(|a, b| {
                (a["updatedAt"].as_str(), a["version"].as_str())
                    .cmp(&(b["updatedAt"].as_str(), b["version"].as_str()))
            })
            .expect("non-empty");
        if entry["type"] != crate::trust::manifest::CARD_TYPE {
            return Err(PeerError::InvalidCard);
        }
        Ok(entry)
    }

    /// Steps 1 to 4. Every outcome is logged with the peer and trace so the
    /// public log shows which link of the chain held or broke.
    pub async fn resolve(&self, peer: &str, trace_id: &str) -> Result<VerifiedPeer, PeerError> {
        let outcome = self.resolve_inner(peer, trace_id).await;
        match &outcome {
            Ok(verified) => {
                tracing::info!(target: "calendar::trust", event = "peer_verified", peer, trace_id, card_digest = %verified.card_digest)
            }
            Err(error) => {
                tracing::warn!(target: "calendar::trust", event = "peer_rejected", peer, trace_id, code = error.code())
            }
        }
        outcome
    }

    async fn resolve_inner(&self, peer: &str, trace_id: &str) -> Result<VerifiedPeer, PeerError> {
        let tenant = self
            .publisher
            .tenant(peer)
            .ok_or(PeerError::NotFound)?
            .to_owned();
        // 1. Catalog.
        let bytes = self
            .fetch_bytes(self.catalog_url.clone(), PeerError::DiscoveryUnavailable)
            .await?;
        let catalog: Value =
            serde_json::from_slice(&bytes).map_err(|_| PeerError::DiscoveryUnavailable)?;
        tracing::info!(target: "calendar::trust", event = "catalog_fetched", trace_id, bytes = bytes.len(),
            entries = catalog["entries"].as_array().map_or(0, Vec::len));
        let operator = catalog["host"]["identifier"]
            .as_str()
            .ok_or(PeerError::UntrustedOperator)?;
        let operator_keys = self.operator_keys(operator).await.inspect_err(
            |error| tracing::warn!(target: "calendar::trust", event = "catalog_signature_rejected", trace_id, code = error.code()),
        )?;
        verify::verify_catalog(&catalog, &operator_keys).inspect_err(
            |error| tracing::warn!(target: "calendar::trust", event = "catalog_signature_rejected", trace_id, code = error.code()),
        )?;
        tracing::info!(target: "calendar::trust", event = "catalog_signature_verified", trace_id, kid = operator_keys.kids().next());
        // 2. Entry and manifest.
        let entry = self.select_entry(&catalog, peer)?;
        let entry_url = entry["url"].as_str().ok_or(PeerError::InvalidCard)?;
        let manifest = &entry["trustManifest"];
        if manifest.is_null() {
            tracing::warn!(target: "calendar::trust", event = "manifest_rejected", peer, trace_id, code = verify::VerifyError::ManifestMissing.code());
            return Err(verify::VerifyError::ManifestMissing.into());
        }
        let identity = manifest["identity"].as_str().unwrap_or_default();
        let guarantor_keys = self.guarantor_keys(identity).await.inspect_err(
            |error| tracing::warn!(target: "calendar::trust", event = "manifest_rejected", peer, trace_id, code = error.code()),
        )?;
        let binding = verify::verify_manifest(
            manifest,
            &guarantor_keys,
            identity,
            crate::trust::manifest::CARD_TYPE,
            entry_url,
            chrono::Utc::now(),
        )
        .inspect_err(
            |error| tracing::warn!(target: "calendar::trust", event = "manifest_rejected", peer, trace_id, code = error.code()),
        )?;
        tracing::info!(target: "calendar::trust", event = "manifest_verified", peer, trace_id, card_digest = %binding.digest);
        // 3. Card bytes.
        let card_url = self.trusted_url(entry_url)?;
        let card_bytes = self.fetch_bytes(card_url, PeerError::InvalidCard).await?;
        verify::verify_card_digest(&card_bytes, &binding.digest).inspect_err(
            |error| tracing::warn!(target: "calendar::trust", event = "card_digest_rejected", peer, trace_id, code = error.code()),
        )?;
        tracing::info!(target: "calendar::trust", event = "card_digest_verified", peer, trace_id, card_digest = %binding.digest);
        // 4. Card signature.
        let raw: Value = serde_json::from_slice(&card_bytes).map_err(|_| PeerError::InvalidCard)?;
        let jwks_url = self.trusted_url(&crate::identities::jwks_url(
            self.agent_origin.as_str(),
            &tenant,
        ))?;
        let header = verify::card_key_location(&raw, jwks_url.as_str()).inspect_err(
            |error| tracing::warn!(target: "calendar::trust", event = "card_signature_rejected", peer, trace_id, code = error.code()),
        )?;
        let agent_keys = self.agent_keys(&jwks_url).await?;
        verify::verify_card(&raw, &agent_keys).inspect_err(
            |error| tracing::warn!(target: "calendar::trust", event = "card_signature_rejected", peer, trace_id, code = error.code()),
        )?;
        tracing::info!(target: "calendar::trust", event = "card_signature_verified", peer, trace_id, kid = header.kid());
        // 5. Only now the SDK view of the card.
        let mut card = card::sdk_view(raw).map_err(|_| PeerError::InvalidCard)?;
        // Only JSON-RPC interfaces for this tenant on the agent origin remain;
        // the SDK picks among them.
        card.supported_interfaces.retain(|i| {
            i.protocol_binding == "JSONRPC"
                && i.tenant.as_deref() == Some(tenant.as_str())
                && self.trusted_url(&i.url).is_ok()
        });
        if card.supported_interfaces.is_empty() {
            return Err(PeerError::InvalidCard);
        }
        Ok(VerifiedPeer {
            card,
            tenant,
            card_digest: binding.digest,
        })
    }

    fn client(&self, http: Client) -> A2AClientFactory {
        A2AClientFactory::builder()
            .no_defaults()
            .with_interceptor(Arc::new(a2a_client::middleware::LoggingInterceptor))
            .register(Arc::new(JsonRpcTransportFactory::new(Some(http))))
            .build()
    }

    pub async fn account_call(
        &self,
        peer: &str,
        token: &str,
        operation: Value,
        trace_id: &str,
    ) -> Result<Value, PeerError> {
        let verified = self.resolve(peer, trace_id).await?;
        tracing::info!(
            event = "connected_peer_call",
            peer,
            recipient_tenant = %verified.tenant,
            operation = operation["operation"].as_str()
        );
        let mut headers = reqwest::header::HeaderMap::new();
        let mut value = reqwest::header::HeaderValue::from_str(&format!("Bearer {token}"))
            .map_err(|_| PeerError::Unavailable)?;
        value.set_sensitive(true);
        headers.insert(reqwest::header::AUTHORIZATION, value);
        let http = Client::builder()
            .default_headers(headers)
            .redirect(reqwest::redirect::Policy::none())
            .timeout(Duration::from_secs(14))
            .connect_timeout(Duration::from_secs(2))
            .build()
            .map_err(|_| PeerError::Unavailable)?;
        let client = self
            .client(http)
            .create_from_card(&verified.card)
            .await
            .map_err(|_| PeerError::InvalidCard)?;
        let request = SendMessageRequest {
            tenant: None,
            message: Message::new(Role::User, vec![Part::data(operation)]),
            configuration: None,
            metadata: Some([(String::from("calendarTraceId"), json!(trace_id))].into()),
        };
        let reply = client
            .send_message(&request)
            .await
            .map_err(|_| PeerError::Unavailable)?;
        let SendMessageResponse::Message(message) = reply else {
            return Err(PeerError::InvalidResponse);
        };
        if message.role != Role::Agent {
            return Err(PeerError::InvalidResponse);
        }
        message
            .parts
            .iter()
            .find_map(|p| {
                if let PartContent::Data(v) = &p.content {
                    Some(v.clone())
                } else {
                    None
                }
            })
            .ok_or(PeerError::InvalidResponse)
    }

    pub async fn availability(
        &self,
        peer: &str,
        trace_id: &str,
        caller: &str,
        window: Option<&crate::scheduling::Window>,
    ) -> Result<Availability, PeerError> {
        let verified = self.resolve(peer, trace_id).await?;
        let factory = self.client(if window.is_some() {
            self.live_http.clone()
        } else {
            self.http.clone()
        });
        let (client, interface) = factory
            .create_from_card_with_interface(&verified.card)
            .await
            .map_err(|_| PeerError::InvalidCard)?;
        tracing::info!(
            event = "peer_call",
            caller,
            peer,
            recipient_tenant = interface.tenant.as_deref(),
        );
        // Only this leaf operation is sent, never another find_common_slot.
        let mut operation = json!({"operation":"get_availability"});
        if let Some(window) = window {
            operation["window"] = serde_json::to_value(window).unwrap();
        }
        let request = SendMessageRequest {
            tenant: None, // The SDK fills this from the selected AgentInterface.
            message: Message::new(Role::User, vec![Part::data(operation)]),
            configuration: None,
            metadata: Some([(String::from("calendarTraceId"), json!(trace_id))].into()),
        };
        let reply = tokio::time::timeout(
            if window.is_some() {
                Duration::from_secs(10)
            } else {
                Duration::from_millis(2500)
            },
            client.send_message(&request),
        )
        .await
        .map_err(|_| PeerError::Timeout)?
        .map_err(|_| PeerError::Unavailable)?;
        let SendMessageResponse::Message(message) = reply else {
            return Err(PeerError::InvalidResponse);
        };
        if message.role != Role::Agent {
            return Err(PeerError::InvalidResponse);
        }
        let parts: Vec<_> = message
            .parts
            .iter()
            .filter_map(|part| match &part.content {
                PartContent::Data(value) => Some(value),
                _ => None,
            })
            .collect();
        if parts.len() != 1 {
            return Err(PeerError::InvalidResponse);
        }
        let availability: Availability =
            serde_json::from_value(parts[0].clone()).map_err(|_| PeerError::InvalidResponse)?;
        if availability.status != "availability"
            || availability.agent != peer
            || availability.mock != window.is_none()
            || availability.trace_id != trace_id
            || availability.slots.len() > if window.is_some() { 10_000 } else { 100 }
            || availability.slots.iter().any(|slot| slot.start >= slot.end)
        {
            return Err(PeerError::InvalidResponse);
        }
        if let Some(window) = window {
            let info = availability
                .schedule
                .as_ref()
                .ok_or(PeerError::InvalidResponse)?;
            if info.window != *window
                || !info.window.valid()
                || !(1..=1440).contains(&info.duration_minutes)
                || info.title.is_empty()
                || info.title.len() > 2048
                || info.timezone.is_empty()
                || info.timezone.len() > 100
                || availability.slots.iter().any(|s| {
                    s.start < window.start
                        || s.end > window.end
                        || s.end - s.start
                            != chrono::Duration::minutes(i64::from(info.duration_minutes))
                })
            {
                return Err(PeerError::InvalidResponse);
            }
        }
        Ok(availability)
    }
}
