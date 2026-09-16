mod a2a;
mod agents;
mod discovery;
pub mod logging;
mod scheduling;

use axum::{
    Json, Router,
    extract::{Path, State},
    http::StatusCode,
    routing::get,
};
use serde_json::json;
use std::sync::Arc;

pub fn app(base_url: &str) -> Router {
    app_with_catalog(
        base_url,
        &format!(
            "{}/.well-known/ai-catalog.json",
            base_url.trim_end_matches('/')
        ),
    )
    .expect("valid local application configuration")
}

pub fn app_with_catalog(base_url: &str, catalog_url: &str) -> Result<Router, lambda_http::Error> {
    let directory = discovery::PeerDirectory::new(base_url, catalog_url)?;
    let discovery = Router::new()
        .route("/.well-known/ai-catalog.json", get(catalog))
        .route("/agents/{tenant}/agent-card.json", get(card))
        .with_state(base_url.trim_end_matches('/').to_owned());
    let protocol =
        a2a_server::jsonrpc::jsonrpc_router(Arc::new(a2a::CalendarHandler { directory }));
    Ok(Router::new()
        .route(
            "/health",
            get(|| async {
                (
                    [("cache-control", "no-store")],
                    Json(json!({"status": "ok", "service": "calendar"})),
                )
            }),
        )
        .merge(discovery)
        // Mount at exactly /a2a, matching the URL advertised by both cards.
        .nest_service("/a2a", protocol))
}

async fn catalog(State(base_url): State<String>) -> Json<ai_catalog::AiCatalog> {
    Json(agents::catalog(&base_url))
}

async fn card(
    State(base_url): State<String>,
    Path(tenant): Path<String>,
) -> Result<Json<::a2a::AgentCard>, StatusCode> {
    agents::find(Some(&tenant))
        .map(|agent| Json(agent.card(&base_url)))
        .map_err(|_| StatusCode::NOT_FOUND)
}
