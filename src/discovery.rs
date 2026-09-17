use crate::scheduling::Availability;
use a2a::{AgentCard, Message, Part, PartContent, Role, SendMessageRequest, SendMessageResponse};
use a2a_client::{A2AClientFactory, jsonrpc::JsonRpcTransportFactory};
use ai_catalog::AiCatalog;
use reqwest::{Client, Url};
use serde::de::DeserializeOwned;
use serde_json::json;
use std::{sync::Arc, time::Duration};

#[derive(Debug, Clone, Copy)]
pub enum PeerError {
    DiscoveryUnavailable,
    NotFound,
    InvalidCard,
    Unavailable,
    InvalidResponse,
    Timeout,
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
        }
    }
}

pub struct PeerDirectory {
    http: Client,
    live_http: Client,
    catalog_url: Url,
    agent_origin: Url,
    registry_origin: Option<Url>,
}

fn network_error(error: reqwest::Error, otherwise: PeerError) -> PeerError {
    if error.is_timeout() {
        PeerError::Timeout
    } else {
        otherwise
    }
}

impl PeerDirectory {
    pub fn new_with_registry(
        public_url: &str,
        catalog_url: &str,
        registry_origin: Option<&str>,
    ) -> Result<Self, lambda_http::Error> {
        // Explicit provider avoids a platform-dependent TLS default in the SDK.
        let _ = a2a_client::rustls::crypto::aws_lc_rs::default_provider().install_default();
        let agent_origin = Url::parse(public_url)?;
        let catalog_url = Url::parse(catalog_url)?;
        for url in [&agent_origin, &catalog_url] {
            let local = matches!(url.host_str(), Some("127.0.0.1" | "localhost" | "[::1]"));
            if !(url.scheme() == "https" || (url.scheme() == "http" && local))
                || !url.username().is_empty()
                || url.password().is_some()
                || url.fragment().is_some()
            {
                return Err("Discovery configuration requires HTTPS (or loopback HTTP), without credentials or fragments".into());
            }
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
            agent_origin,
            registry_origin: registry_origin.map(Url::parse).transpose()?,
        })
    }

    fn trusted_url(&self, value: &str, is_card: bool) -> Result<Url, PeerError> {
        let url = Url::parse(value).map_err(|_| PeerError::InvalidCard)?;
        // Only this deployment's agents are enabled in this gate. The
        // catalog may move to Aithos independently of the agent-serving origin.
        if !(url.origin() == self.agent_origin.origin()
            || (is_card
                && self
                    .registry_origin
                    .as_ref()
                    .is_some_and(|r| url.origin() == r.origin())))
            || !url.username().is_empty()
            || url.password().is_some()
            || url.fragment().is_some()
        {
            return Err(PeerError::InvalidCard);
        }
        Ok(url)
    }

    async fn fetch<T: DeserializeOwned>(
        &self,
        url: Url,
        failure: PeerError,
    ) -> Result<T, PeerError> {
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
            if bytes.len() + chunk.len() > 64 * 1024 {
                return Err(failure);
            }
            bytes.extend_from_slice(&chunk);
        }
        serde_json::from_slice(&bytes).map_err(|_| failure)
    }

    pub async fn account_call(
        &self,
        peer: &str,
        token: &str,
        operation: serde_json::Value,
    ) -> Result<serde_json::Value, PeerError> {
        let catalog: AiCatalog = self
            .fetch(self.catalog_url.clone(), PeerError::DiscoveryUnavailable)
            .await?;
        if catalog.spec_version != "1.0"
            || catalog
                .entries
                .iter()
                .filter(|e| e.identifier == peer)
                .count()
                > 1
        {
            return Err(PeerError::DiscoveryUnavailable);
        }
        let entry = catalog.get_by_id(peer).ok_or(PeerError::NotFound)?;
        if entry.entry_type != "application/a2a-agent-card+json" {
            return Err(PeerError::InvalidCard);
        }
        let url = self.trusted_url(entry.url.as_deref().ok_or(PeerError::InvalidCard)?, true)?;
        let mut raw: serde_json::Value = self.fetch(url, PeerError::InvalidCard).await?;
        // A2A canonical JSON omits empty StringList.list. SDK 0.3.1's reader
        // expects it; adapt only the in-memory client view, never signed bytes.
        fn sdk_requirements(v: &mut serde_json::Value) {
            if let Some(requirements) = v
                .get_mut("securityRequirements")
                .and_then(|r| r.as_array_mut())
            {
                for r in requirements {
                    if let Some(schemes) = r.get_mut("schemes").and_then(|s| s.as_object_mut()) {
                        for value in schemes.values_mut() {
                            if value.as_object().is_some_and(|v| v.is_empty()) {
                                *value = json!({"list":[]});
                            }
                        }
                    }
                }
            }
        }
        sdk_requirements(&mut raw);
        if let Some(skills) = raw.get_mut("skills").and_then(|s| s.as_array_mut()) {
            for skill in skills {
                sdk_requirements(skill);
            }
        }
        let mut card: AgentCard =
            serde_json::from_value(raw).map_err(|_| PeerError::InvalidCard)?;
        let tenant = peer
            .strip_prefix("urn:aithos:calendar:agent:")
            .ok_or(PeerError::InvalidCard)?;
        card.supported_interfaces.retain(|i| {
            i.protocol_binding == "JSONRPC"
                && i.tenant.as_deref() == Some(tenant)
                && i.url == format!("{}/a2a", self.agent_origin.as_str().trim_end_matches('/'))
        });
        if card.supported_interfaces.is_empty() {
            return Err(PeerError::InvalidCard);
        }
        tracing::info!(
            event = "connected_peer_call",
            peer,
            recipient_tenant = tenant,
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
        let factory = A2AClientFactory::builder()
            .no_defaults()
            .with_interceptor(Arc::new(a2a_client::middleware::LoggingInterceptor))
            .register(Arc::new(JsonRpcTransportFactory::new(Some(http))))
            .build();
        let client = factory
            .create_from_card(&card)
            .await
            .map_err(|_| PeerError::InvalidCard)?;
        let request = SendMessageRequest {
            tenant: None,
            message: Message::new(Role::User, vec![Part::data(operation)]),
            configuration: None,
            metadata: None,
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
        let catalog: AiCatalog = self
            .fetch(self.catalog_url.clone(), PeerError::DiscoveryUnavailable)
            .await?;
        if catalog.spec_version != "1.0" {
            return Err(PeerError::DiscoveryUnavailable);
        }
        if catalog
            .entries
            .iter()
            .filter(|entry| entry.identifier == peer)
            .count()
            > 1
        {
            return Err(PeerError::InvalidCard);
        }
        if catalog.spec_version != "1.0"
            || catalog
                .entries
                .iter()
                .filter(|e| e.identifier == peer)
                .count()
                > 1
        {
            return Err(PeerError::DiscoveryUnavailable);
        }
        let entry = catalog.get_by_id(peer).ok_or(PeerError::NotFound)?;
        if entry.entry_type != "application/a2a-agent-card+json" {
            return Err(PeerError::InvalidCard);
        }
        if entry.entry_type != "application/a2a-agent-card+json" {
            return Err(PeerError::InvalidCard);
        }
        let url = self.trusted_url(entry.url.as_deref().ok_or(PeerError::InvalidCard)?, true)?;
        // The SDK resolver only appends a well-known path. Here the catalog
        // already supplies the full card URL, so fetch it directly as AgentCard.
        let mut card: AgentCard = self.fetch(url, PeerError::InvalidCard).await?;
        card.supported_interfaces
            .retain(|interface| interface.protocol_binding == "JSONRPC");
        if card.supported_interfaces.is_empty() {
            return Err(PeerError::InvalidCard);
        }
        for interface in &card.supported_interfaces {
            self.trusted_url(&interface.url, false)?;
            if interface.tenant.as_deref().is_none_or(str::is_empty) {
                return Err(PeerError::InvalidCard);
            }
        }
        let factory = A2AClientFactory::builder()
            .no_defaults()
            .with_interceptor(Arc::new(a2a_client::middleware::LoggingInterceptor))
            .register(Arc::new(JsonRpcTransportFactory::new(Some(
                if window.is_some() {
                    self.live_http.clone()
                } else {
                    self.http.clone()
                },
            ))))
            .build();
        let (client, interface) = factory
            .create_from_card_with_interface(&card)
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
