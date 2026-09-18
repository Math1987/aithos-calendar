use crate::{
    agents::{Agent, Publisher},
    booking_page::{BookingPages, PageError},
    scheduling::Slot,
    storage::{AgentStore, Record},
    trust::{AgentKey, Claims, EntryDraft, TrustError, TrustProvider, card, manifest::CARD_TYPE},
};
use axum::{
    Json,
    extract::{Request, State},
    http::StatusCode,
    middleware::Next,
    response::{IntoResponse, Response},
};
use serde::Deserialize;
use serde_json::{Value, json};
use std::{sync::Arc, time::Duration};

pub struct Identities {
    pub store: Arc<dyn AgentStore>,
    pub base: String,
    pub publisher: Publisher,
    /// Guarantor: signs entry manifests.
    pub trust: Arc<dyn TrustProvider>,
    /// Operator: signs the catalog document and the host manifest.
    pub operator: Arc<crate::trust::Operator>,
    pub website: String,
    pub pages: Arc<dyn BookingPages>,
    pub reader: Option<Arc<dyn crate::availability::AvailabilityReader>>,
    /// Cache of the last signed catalog (see `catalog.rs`).
    pub signed: crate::catalog::Signed,
    pub limits: crate::limits::Limits,
}

pub fn card_url(base: &str, id: &str) -> String {
    format!("{}/agents/{id}/agent-card.json", base.trim_end_matches('/'))
}
pub fn jwks_url(base: &str, id: &str) -> String {
    format!("{}/agents/{id}/jwks.json", base.trim_end_matches('/'))
}

/// Issue a publishable record for `agent`: a fresh signing key, the signed
/// card served on this deployment, and the guarantor-signed catalog manifest
/// bound to those exact card bytes. Nothing leaves this process; the record
/// is discoverable as soon as it is stored.
pub async fn issue(
    trust: &dyn TrustProvider,
    agent: Agent,
    booking_page_url: Option<String>,
    base: &str,
) -> Result<(Record, AgentKey), TrustError> {
    reissue(trust, agent, booking_page_url, base, AgentKey::random()).await
}

/// Same as [`issue`] with an existing key (operator re-signing).
pub async fn reissue(
    trust: &dyn TrustProvider,
    agent: Agent,
    booking_page_url: Option<String>,
    base: &str,
    key: AgentKey,
) -> Result<(Record, AgentKey), TrustError> {
    let base = base.trim_end_matches('/');
    let card = serde_json::to_value(agent.card(base))
        .map_err(|_| TrustError::InvalidInput("agent_card"))?;
    let signed = card::sign(card, &key, &jwks_url(base, &agent.id))?;
    let entry = EntryDraft {
        identifier: Publisher::from_base(base).urn(&agent.id),
        entry_type: CARD_TYPE.into(),
        url: card_url(base, &agent.id),
    };
    let claims = Claims {
        account_verified: agent.google_account,
    };
    let manifest = trust.manifest_for(&entry, &signed.bytes, &claims).await?;
    let record = Record {
        card_url: entry.url,
        card_bytes: String::from_utf8(signed.bytes)
            .map_err(|_| TrustError::InvalidInput("agent_card"))?,
        card_digest: signed.digest,
        card_version: signed.version,
        card_jwks: key.jwks(),
        updated_at: chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Secs, true),
        manifest: Some(manifest),
        booking_page_url,
        agent,
        published: true,
    };
    Ok((record, key))
}

/// Public onboarding. No caller identity, browser session or credentials required.
pub async fn deadline(req: Request, next: Next) -> Response {
    let mut response = match tokio::time::timeout(Duration::from_secs(12), next.run(req)).await {
        Ok(response) => response,
        Err(_) => failure(
            StatusCode::GATEWAY_TIMEOUT,
            "operation_incomplete_retry_same_url",
        ),
    };
    response.headers_mut().insert(
        "cache-control",
        axum::http::HeaderValue::from_static("no-store"),
    );
    response
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Definition {
    pub booking_page_url: String,
}
fn failure(status: StatusCode, code: &str) -> Response {
    (status, Json(json!({"error":code}))).into_response()
}
fn page_failure(error: PageError) -> Response {
    failure(
        match error {
            PageError::Invalid => StatusCode::BAD_REQUEST,
            PageError::Unavailable => StatusCode::SERVICE_UNAVAILABLE,
            PageError::Unrecognized => StatusCode::UNPROCESSABLE_ENTITY,
        },
        error.code(),
    )
}
fn view(state: &Identities, record: &Record) -> Value {
    let mut value = json!({"id":record.agent.id, "identifier":state.publisher.urn(&record.agent.id), "name":record.agent.name,
        "booking_page_url":record.booking_page_url, "share_url":format!("{}/book/{}",state.website.trim_end_matches('/'),record.agent.id),
        "mock_availability":record.agent.slots, "agent_card_url":record.card_url, "agent_jwks_url":jwks_url(&state.base, &record.agent.id),
        "card_digest":record.card_digest,
        "publication_status":if record.published {"published"} else {"pending"}, "mock":!record.agent.live, "reserved":false});
    if record.agent.live {
        value.as_object_mut().unwrap().remove("mock_availability");
    }
    value
}

pub async fn create(
    State(state): State<Arc<Identities>>,
    client: crate::limits::ClientIp,
    Json(definition): Json<Definition>,
) -> Response {
    if let Err(refused) = state
        .limits
        .hit(crate::limits::AGENT_CREATION_PER_IP, &client.0)
        .await
    {
        return refused.into_response();
    }
    let page = match tokio::time::timeout(
        Duration::from_secs(5),
        state.pages.resolve(&definition.booking_page_url),
    )
    .await
    {
        Ok(Ok(page)) => page,
        Ok(Err(error)) => return page_failure(error),
        Err(_) => return page_failure(PageError::Unavailable),
    };
    let id = page.agent_id();
    let existing = match state.store.get(&id).await {
        Ok(record) => record,
        Err(_) => return failure(StatusCode::SERVICE_UNAVAILABLE, "storage_unavailable"),
    };
    let mut record = if let Some(record) = existing {
        record
    } else {
        match tokio::time::timeout(Duration::from_secs(5), state.pages.validate(&page)).await {
            Ok(Ok(())) => (),
            Ok(Err(error)) => return page_failure(error),
            Err(_) => return page_failure(PageError::Unavailable),
        }
        let agent = Agent {
            id: id.clone(),
            live: state.reader.is_some(),
            google_account: false,
            name: format!(
                "Booking page {}{}",
                &id[..8],
                if state.reader.is_some() {
                    ""
                } else {
                    " (mock)"
                }
            ),
            slots: if state.reader.is_some() {
                vec![]
            } else {
                vec![Slot {
                    start: "2030-01-15T09:30:00Z".parse().unwrap(),
                    end: "2030-01-15T10:00:00Z".parse().unwrap(),
                }]
            },
        };
        let (candidate, key) = match issue(
            state.trust.as_ref(),
            agent,
            Some(page.url.clone()),
            &state.base,
        )
        .await
        {
            Ok(value) => value,
            Err(error) => {
                tracing::warn!(target: "calendar::trust", event = "card_signing_failed", tenant = %id, code = %error);
                return failure(StatusCode::SERVICE_UNAVAILABLE, "card_generation_failed");
            }
        };
        match state.store.create(&candidate, &key.encode()).await {
            Ok(true) => {
                tracing::info!(event="identity_created",tenant=%id, card_digest=%candidate.card_digest);
                candidate
            }
            Ok(false) => match state.store.get(&id).await {
                Ok(Some(winner)) => winner,
                _ => return failure(StatusCode::SERVICE_UNAVAILABLE, "storage_unavailable"),
            },
            Err(_) => return failure(StatusCode::SERVICE_UNAVAILABLE, "storage_unavailable"),
        }
    };
    // Never alter existing data, including legacy records or a theoretical hash collision.
    if record.booking_page_url.as_deref() != Some(page.url.as_str()) {
        return failure(StatusCode::CONFLICT, "booking_page_identity_conflict");
    }
    if !record.published {
        // Records created before publication became immediate.
        if state.store.publish(&record.agent.id).await.is_err() {
            return failure(StatusCode::SERVICE_UNAVAILABLE, "storage_unavailable");
        }
        record.published = true;
    } else {
        tracing::info!(event="identity_reused",tenant=%record.agent.id);
    }
    Json(view(&state, &record)).into_response()
}

/// Live meeting metadata only; never expose attendee contact details here.
pub async fn schedule(
    State(state): State<Arc<Identities>>,
    client: crate::limits::ClientIp,
    axum::extract::Path(id): axum::extract::Path<String>,
) -> Response {
    let response = async {
        if !crate::valid_tenant(&id) {
            return failure(StatusCode::NOT_FOUND, "unknown_agent");
        }
        if let Err(refused) = state
            .limits
            .hit(crate::limits::SCHEDULE_READS_PER_IP, &client.0)
            .await
        {
            return refused.into_response();
        }
        let record = match state.store.get(&id).await {
            Ok(Some(r)) if r.published => r,
            Ok(_) => return failure(StatusCode::NOT_FOUND, "unknown_agent"),
            Err(_) => return failure(StatusCode::SERVICE_UNAVAILABLE, "storage_unavailable"),
        };
        if record.agent.google_account {
            return Json(json!({"id":id,"mode":"google_account"})).into_response();
        }
        if !record.agent.live {
            return failure(StatusCode::CONFLICT, "agent_upgrade_required");
        }
        let (Some(url), Some(reader)) = (record.booking_page_url, &state.reader) else {
            return failure(StatusCode::SERVICE_UNAVAILABLE, "availability_unavailable");
        };
        let window = crate::scheduling::Window::next_month();
        match reader.read(&crate::booking_page::BookingPage {url}, window.start, window.end).await {
            Ok(s) => Json(json!({"id":id, "mock":false, "reserved":false, "schedule":crate::scheduling::ScheduleInfo::from(&s)})).into_response(),
            Err(_) => failure(StatusCode::SERVICE_UNAVAILABLE, "availability_unavailable"),
        }
    };
    let mut response = tokio::time::timeout(Duration::from_secs(9), response)
        .await
        .unwrap_or_else(|_| failure(StatusCode::GATEWAY_TIMEOUT, "timeout"));
    response.headers_mut().insert(
        "cache-control",
        axum::http::HeaderValue::from_static("no-store"),
    );
    response
}
