mod a2a;
mod agents;
pub mod availability;
pub mod booking;
pub mod booking_page;
mod discovery;
pub mod identities;
pub mod logging;
pub mod registry;
mod scheduling;
pub mod storage;

use axum::{
    Json, Router,
    extract::{DefaultBodyLimit, Path, State},
    http::StatusCode,
    middleware,
    response::{IntoResponse, Response},
    routing::{get, post},
};
use identities::Identities;
use serde_json::json;
use std::sync::Arc;

/// Explicit fixture constructor for tests. The Lambda entry point uses DynamoDB.
pub fn app(base_url: &str) -> Router {
    app_with_catalog(
        base_url,
        &format!(
            "{}/.well-known/ai-catalog.json",
            base_url.trim_end_matches('/')
        ),
    )
    .expect("valid fixture configuration")
}
pub fn app_with_catalog(base_url: &str, catalog_url: &str) -> Result<Router, lambda_http::Error> {
    app_with_store(
        base_url,
        catalog_url,
        Arc::new(storage::MemoryStore::fixtures(base_url)),
        None,
        base_url.to_owned(),
    )
}
pub fn app_with_store(
    base_url: &str,
    catalog_url: &str,
    store: Arc<dyn storage::AgentStore>,
    registry: Option<registry::Registry>,
    website: String,
) -> Result<Router, lambda_http::Error> {
    app_with_pages(
        base_url,
        catalog_url,
        store,
        registry,
        website,
        Arc::new(booking_page::GoogleBookingPages::new()?),
    )
}

/// Inject the page identity adapter for isolated HTTP acceptance tests.
pub fn app_with_pages(
    base_url: &str,
    catalog_url: &str,
    store: Arc<dyn storage::AgentStore>,
    registry: Option<registry::Registry>,
    website: String,
    pages: Arc<dyn booking_page::BookingPages>,
) -> Result<Router, lambda_http::Error> {
    app_with_reader(base_url, catalog_url, store, registry, website, pages, None)
}

pub fn app_with_reader(
    base_url: &str,
    catalog_url: &str,
    store: Arc<dyn storage::AgentStore>,
    registry: Option<registry::Registry>,
    website: String,
    pages: Arc<dyn booking_page::BookingPages>,
    reader: Option<Arc<dyn availability::AvailabilityReader>>,
) -> Result<Router, lambda_http::Error> {
    let directory = discovery::PeerDirectory::new_with_registry(
        base_url,
        catalog_url,
        registry.as_ref().map(|r| r.origin.as_str()),
    )?;
    let state = Arc::new(Identities {
        store: store.clone(),
        base: base_url.trim_end_matches('/').into(),
        registry,
        website,
        pages,
        reader: reader.clone(),
    });
    let onboarding = Router::new()
        .route("/agents", post(identities::create))
        .layer(DefaultBodyLimit::max(4 * 1024))
        .route_layer(middleware::from_fn(identities::deadline));
    let routes = Router::new()
        .route("/.well-known/ai-catalog.json", get(catalog))
        .route("/agents/{tenant}/agent-card.json", get(card))
        .route("/agents/{tenant}/schedule", get(identities::schedule))
        .merge(onboarding)
        .with_state(state);
    let protocol = a2a_server::jsonrpc::jsonrpc_router(Arc::new(a2a::CalendarHandler {
        directory,
        store,
        reader,
    }));
    Ok(Router::new()
        .route(
            "/health",
            get(|| async {
                (
                    [("cache-control", "no-store")],
                    Json(json!({"status":"ok","service":"calendar"})),
                )
            }),
        )
        .merge(routes)
        .nest_service("/a2a", protocol))
}
async fn catalog(State(state): State<Arc<Identities>>) -> Response {
    let Ok(mut records) = state.store.published().await else {
        return StatusCode::SERVICE_UNAVAILABLE.into_response();
    };
    records.sort_by(|a, b| a.agent.id.cmp(&b.agent.id));
    let value = json!({"specVersion":"1.0", "host":{"displayName":"Calendar agents"}, "entries":records.iter().map(|r| json!({
        "identifier":r.agent.identifier(),"displayName":r.agent.name,"type":"application/a2a-agent-card+json",
        "url":r.card_url,"description":if r.agent.live {"Real public availability; no booking."} else {"Mock scheduling agent; no calendar access or booking."},"tags":if r.agent.live {vec!["calendar","availability"]} else {vec!["calendar","mock"]}
    })).collect::<Vec<_>>()});
    // Match the discovery client's bounded document size, without partial results.
    if serde_json::to_vec(&value).map_or(true, |v| v.len() > 64 * 1024) {
        return StatusCode::SERVICE_UNAVAILABLE.into_response();
    }
    ([("cache-control", "no-store")], Json(value)).into_response()
}
pub(crate) fn valid_tenant(id: &str) -> bool {
    !id.is_empty()
        && id.len() <= 64
        && id
            .bytes()
            .all(|c| c.is_ascii_alphanumeric() || c == b'-' || c == b'_')
}
async fn card(State(state): State<Arc<Identities>>, Path(tenant): Path<String>) -> Response {
    if !valid_tenant(&tenant) {
        return StatusCode::NOT_FOUND.into_response();
    }
    match state.store.get(&tenant).await {
        Ok(Some(record)) if record.published => (
            [
                ("content-type", "application/json"),
                ("cache-control", "no-store"),
            ],
            record.card_bytes,
        )
            .into_response(),
        Ok(_) => StatusCode::NOT_FOUND.into_response(),
        Err(_) => StatusCode::SERVICE_UNAVAILABLE.into_response(),
    }
}
