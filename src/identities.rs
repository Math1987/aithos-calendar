use crate::{
    agents::Agent,
    booking_page::{BookingPages, PageError},
    registry::Registry,
    scheduling::Slot,
    storage::{AgentStore, Record},
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

#[derive(Clone)]
pub struct Identities {
    pub store: Arc<dyn AgentStore>,
    pub base: String,
    pub registry: Option<Registry>,
    pub website: String,
    pub pages: Arc<dyn BookingPages>,
    pub reader: Option<Arc<dyn crate::availability::AvailabilityReader>>,
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
    let mut value = json!({"id":record.agent.id, "identifier":record.agent.identifier(), "name":record.agent.name,
        "booking_page_url":record.booking_page_url, "share_url":format!("{}/book/{}",state.website.trim_end_matches('/'),record.agent.id),
        "mock_availability":record.agent.slots, "registry_id":record.registry_id, "agent_card_url":record.card_url,
        "publication_status":if record.published {"published"} else {"pending"}, "mock":!record.agent.live, "reserved":false});
    if record.agent.live {
        value.as_object_mut().unwrap().remove("mock_availability");
    }
    value
}
async fn publish_record(state: &Identities, mut record: Record) -> Response {
    if record.published {
        tracing::info!(event="identity_reused",tenant=%record.agent.id);
        return Json(view(state, &record)).into_response();
    }
    let Some(registry) = &state.registry else {
        return failure(StatusCode::SERVICE_UNAVAILABLE, "registry_not_configured");
    };
    // Reposting the same page resumes these exact signed bytes and proof.
    if let Err(code) = registry.publish(&record).await {
        tracing::warn!(event="publication_pending", tenant=%record.agent.id, code);
        let mut value = view(state, &record);
        value["error"] = json!(code);
        return (StatusCode::ACCEPTED, Json(value)).into_response();
    }
    if state.store.publish(&record.agent.id).await.is_err() {
        return failure(StatusCode::SERVICE_UNAVAILABLE, "storage_unavailable");
    }
    record.published = true;
    tracing::info!(event="identity_published",tenant=%record.agent.id,registry_id=%record.registry_id);
    Json(view(state, &record)).into_response()
}

pub async fn create(
    State(state): State<Arc<Identities>>,
    Json(definition): Json<Definition>,
) -> Response {
    let Some(registry) = &state.registry else {
        return failure(StatusCode::SERVICE_UNAVAILABLE, "registry_not_configured");
    };
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
    let record = if let Some(record) = existing {
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
        let (candidate, key) = match registry.prepare(agent, Some(page.url.clone()), &state.base) {
            Ok(value) => value,
            Err(_) => return failure(StatusCode::INTERNAL_SERVER_ERROR, "card_generation_failed"),
        };
        match state.store.create(&candidate, &key).await {
            Ok(true) => {
                tracing::info!(event="identity_created",tenant=%id);
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
    publish_record(&state, record).await
}

/// Live meeting metadata only; never expose attendee contact details here.
pub async fn schedule(
    State(state): State<Arc<Identities>>,
    axum::extract::Path(id): axum::extract::Path<String>,
) -> Response {
    let response = async {
        if !crate::valid_tenant(&id) {
            return failure(StatusCode::NOT_FOUND, "unknown_agent");
        }
        let record = match state.store.get(&id).await {
            Ok(Some(r)) if r.published => r,
            Ok(_) => return failure(StatusCode::NOT_FOUND, "unknown_agent"),
            Err(_) => return failure(StatusCode::SERVICE_UNAVAILABLE, "storage_unavailable"),
        };
        if record.agent.google_account {
            return Json(json!({"id":id,"mode":"google_account","calendar_connected":false}))
                .into_response();
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
