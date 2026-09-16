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
            Self::InvalidResponse => "The peer did not return valid mock availability",
            Self::Timeout => "The peer exchange exceeded its time limit",
        }
    }
}

pub struct PeerDirectory {
    http: Client,
    catalog_url: Url,
    agent_origin: Url,
}

fn network_error(error: reqwest::Error, otherwise: PeerError) -> PeerError {
    if error.is_timeout() {
        PeerError::Timeout
    } else {
        otherwise
    }
}

impl PeerDirectory {
    pub fn new(public_url: &str, catalog_url: &str) -> Result<Self, lambda_http::Error> {
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
            http,
            catalog_url,
            agent_origin,
        })
    }

    fn card_url(&self, value: &str) -> Result<Url, PeerError> {
        let url = Url::parse(value).map_err(|_| PeerError::InvalidCard)?;
        // Only this deployment's mock agents are enabled in this gate. The
        // catalog may move to Aithos independently of the agent-serving origin.
        if url.origin() != self.agent_origin.origin()
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

    pub async fn availability(
        &self,
        peer: &str,
        trace_id: &str,
        caller: &str,
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
        let entry = catalog.get_by_id(peer).ok_or(PeerError::NotFound)?;
        if entry.entry_type != "application/a2a-agent-card+json" {
            return Err(PeerError::InvalidCard);
        }
        let url = self.card_url(entry.url.as_deref().ok_or(PeerError::InvalidCard)?)?;
        // The SDK resolver only appends a well-known path. Here the catalog
        // already supplies the full card URL, so fetch it directly as AgentCard.
        let mut card: AgentCard = self.fetch(url, PeerError::InvalidCard).await?;
        card.supported_interfaces
            .retain(|interface| interface.protocol_binding == "JSONRPC");
        if card.supported_interfaces.is_empty() {
            return Err(PeerError::InvalidCard);
        }
        for interface in &card.supported_interfaces {
            self.card_url(&interface.url)?;
            if interface.tenant.as_deref().is_none_or(str::is_empty) {
                return Err(PeerError::InvalidCard);
            }
        }
        let factory = A2AClientFactory::builder()
            .no_defaults()
            .with_interceptor(Arc::new(a2a_client::middleware::LoggingInterceptor))
            .register(Arc::new(JsonRpcTransportFactory::new(Some(
                self.http.clone(),
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
        let request = SendMessageRequest {
            tenant: None, // The SDK fills this from the selected AgentInterface.
            message: Message::new(
                Role::User,
                vec![Part::data(json!({"operation":"get_availability"}))],
            ),
            configuration: None,
            metadata: Some([(String::from("calendarTraceId"), json!(trace_id))].into()),
        };
        let reply =
            tokio::time::timeout(Duration::from_millis(2500), client.send_message(&request))
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
            || !availability.mock
            || availability.trace_id != trace_id
            || availability.slots.len() > 100
            || availability.slots.iter().any(|slot| slot.start >= slot.end)
        {
            return Err(PeerError::InvalidResponse);
        }
        Ok(availability)
    }
}
