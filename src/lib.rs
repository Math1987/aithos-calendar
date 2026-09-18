mod a2a;
pub mod agent;
pub mod agents;
pub mod auth;
pub mod auth_store;
pub mod availability;
pub mod booking;
pub mod booking_api;
pub mod booking_page;
pub mod booking_store;
pub mod catalog;
pub mod connected;
pub mod discovery;
pub mod google_calendar;
pub mod google_identity;
pub mod identities;
pub mod lab;
pub mod limits;
pub mod logging;
pub mod public_logs;
mod scheduling;
pub mod storage;
pub mod trust;

use axum::{
    Json, Router,
    extract::DefaultBodyLimit,
    middleware,
    response::{IntoResponse, Response},
    routing::{get, post},
};
use identities::Identities;
use serde_json::json;
use std::sync::Arc;
use trust::TrustProvider;

/// Everything an application instance needs besides its request handlers.
/// Optional members default to the fixture-friendly choice so tests only
/// set what they exercise.
pub struct Config {
    pub base_url: String,
    pub catalog_url: String,
    pub website: String,
    /// Guarantor (signs entry manifests).
    pub trust: Arc<dyn TrustProvider>,
    /// Operator (signs the catalog document).
    pub operator: Arc<trust::Operator>,
    pub store: Arc<dyn storage::AgentStore>,
    pub pages: Arc<dyn booking_page::BookingPages>,
    pub reader: Option<Arc<dyn availability::AvailabilityReader>>,
    pub connected: Option<Arc<connected::Connected>>,
    /// Guarantor identities whose manifests the discovery client accepts.
    /// Defaults to this deployment's own guarantor.
    pub trusted_guarantors: Vec<String>,
    /// Trust level required per outgoing operation.
    pub policies: trust::Policies,
    /// Scenario lab keys (successor and impostor guarantor keys).
    pub lab: Arc<lab::Lab>,
    /// The public log feed (sink + store).
    pub public_logs: public_logs::PublicLogs,
    /// Rate limits (fixed windows on the private store; memory by default).
    pub limits: limits::Limits,
}

impl Config {
    /// Fixture configuration: Alice and Bob signed by an ephemeral key.
    pub async fn fixtures(base_url: &str) -> Self {
        let lab = Arc::new(lab::Lab::from_env());
        let trust: Arc<dyn TrustProvider> =
            Arc::new(trust::LocalTrust::ephemeral(base_url).with_published_key(lab.next_jwk()));
        let store = Arc::new(storage::MemoryStore::fixtures(base_url, trust.as_ref()).await);
        Self::new(base_url, trust, store).with_lab(lab)
    }
    pub fn new(
        base_url: &str,
        trust: Arc<dyn TrustProvider>,
        store: Arc<dyn storage::AgentStore>,
    ) -> Self {
        let base_url = base_url.trim_end_matches('/').to_owned();
        Self {
            catalog_url: format!("{base_url}/.well-known/ai-catalog.json"),
            website: base_url.clone(),
            trusted_guarantors: vec![trust.identity().to_owned()],
            policies: trust::Policies::default(),
            lab: Arc::new(lab::Lab::from_env()),
            public_logs: public_logs::PublicLogs::memory(Arc::new(public_logs::Sink::default())),
            limits: limits::Limits::new(Arc::new(auth_store::MemoryAuthStore::default())),
            operator: Arc::new(trust::Operator::ephemeral(&base_url)),
            trust,
            store,
            pages: Arc::new(booking_page::GoogleBookingPages::new().expect("page adapter")),
            reader: None,
            connected: None,
            base_url,
        }
    }
    pub fn with_catalog(mut self, catalog_url: &str) -> Self {
        self.catalog_url = catalog_url.to_owned();
        self
    }
    pub fn with_website(mut self, website: &str) -> Self {
        self.website = website.to_owned();
        self
    }
    pub fn with_pages(mut self, pages: Arc<dyn booking_page::BookingPages>) -> Self {
        self.pages = pages;
        self
    }
    pub fn with_reader(
        mut self,
        reader: Option<Arc<dyn availability::AvailabilityReader>>,
    ) -> Self {
        self.reader = reader;
        self
    }
    pub fn with_connected(mut self, connected: Option<Arc<connected::Connected>>) -> Self {
        self.connected = connected;
        self
    }
    pub fn with_trusted_guarantors(mut self, identities: Vec<String>) -> Self {
        self.trusted_guarantors = identities;
        self
    }
    pub fn with_operator(mut self, operator: Arc<trust::Operator>) -> Self {
        self.operator = operator;
        self
    }
    pub fn with_policies(mut self, policies: trust::Policies) -> Self {
        self.policies = policies;
        self
    }
    pub fn with_lab(mut self, lab: Arc<lab::Lab>) -> Self {
        self.lab = lab;
        self
    }
    pub fn with_public_logs(mut self, logs: public_logs::PublicLogs) -> Self {
        self.public_logs = logs;
        self
    }
    pub fn with_limits(mut self, limits: limits::Limits) -> Self {
        self.limits = limits;
        self
    }
    /// The discovery client this configuration implies.
    pub fn directory(&self) -> Result<discovery::PeerDirectory, lambda_http::Error> {
        Ok(discovery::PeerDirectory::new(
            &self.base_url,
            &self.catalog_url,
            self.trusted_guarantors.clone(),
        )?
        .with_store(self.store.clone()))
    }
}

/// Explicit fixture constructor for tests. The Lambda entry point uses DynamoDB.
pub async fn app(base_url: &str) -> Router {
    build(Config::fixtures(base_url).await).expect("valid fixture configuration")
}

pub fn build(config: Config) -> Result<Router, lambda_http::Error> {
    let directory = config.directory()?;
    let state = Arc::new(Identities {
        store: config.store.clone(),
        base: config.base_url.clone(),
        publisher: agents::Publisher::from_base(&config.base_url),
        trust: config.trust,
        operator: config.operator,
        website: config.website,
        pages: config.pages,
        reader: config.reader.clone(),
        signed: catalog::Signed::default(),
        limits: config.limits.clone(),
    });
    let lab_state = lab::LabState {
        identities: state.clone(),
        lab: config.lab,
        trusted_guarantors: config.trusted_guarantors.clone(),
    };
    let lab_routes = Router::new()
        .route("/lab", get(lab::index))
        .route("/lab/report", get(lab::report))
        .route(
            "/lab/rogue/trust-provider/.well-known/jwks.json",
            get(lab::rogue_jwks),
        )
        .route(
            "/lab/{scenario}/.well-known/ai-catalog.json",
            get(lab::serve_catalog),
        )
        .route(
            "/lab/{scenario}/agents/{tenant}/agent-card.json",
            get(lab::serve_card),
        )
        .with_state(lab_state);
    let onboarding = Router::new()
        .route("/agents", post(identities::create))
        .layer(DefaultBodyLimit::max(4 * 1024))
        .route_layer(middleware::from_fn(identities::deadline));
    let routes = Router::new()
        .route("/.well-known/ai-catalog.json", get(catalog::serve))
        .route("/.well-known/jwks.json", get(catalog::operator_jwks))
        .route(
            "/trust-provider/.well-known/jwks.json",
            get(catalog::guarantor_jwks),
        )
        .route("/agents/{tenant}/agent-card.json", get(catalog::card))
        .route("/agents/{tenant}/jwks.json", get(catalog::agent_jwks))
        .route("/agents/{tenant}/schedule", get(identities::schedule))
        .merge(onboarding)
        .with_state(state);
    let protocol = a2a_server::jsonrpc::jsonrpc_router(Arc::new(a2a::CalendarHandler {
        publisher: agents::Publisher::from_base(&config.base_url),
        policies: config.policies,
        directory,
        connected: config.connected,
        store: config.store,
        reader: config.reader,
    }));
    let feed = Router::new()
        .route("/logs/events", get(public_logs::events))
        .with_state(config.public_logs.clone());
    // The SDK router accepts 10 MiB bodies; a message here is a few KiB.
    let protocol = Router::new()
        .fallback_service(protocol)
        .layer(DefaultBodyLimit::max(64 * 1024));
    Ok(Router::new()
        .route("/health", get(health))
        .merge(routes)
        .merge(lab_routes)
        .merge(feed)
        .nest_service("/a2a", protocol)
        .layer(middleware::from_fn_with_state(
            config.public_logs,
            public_logs::flush_after,
        )))
}

async fn health() -> Response {
    (
        [("cache-control", "no-store")],
        Json(json!({"status":"ok","service":"calendar"})),
    )
        .into_response()
}

pub(crate) fn valid_tenant(id: &str) -> bool {
    !id.is_empty()
        && id.len() <= 64
        && id
            .bytes()
            .all(|c| c.is_ascii_alphanumeric() || c == b'-' || c == b'_')
}
