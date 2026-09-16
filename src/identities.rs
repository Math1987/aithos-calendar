use crate::{
    agents::Agent,
    registry::Registry,
    scheduling::Slot,
    storage::{AgentStore, Record},
};
use axum::{
    Json,
    extract::{Path, Request, State},
    http::StatusCode,
    middleware::Next,
    response::{IntoResponse, Response},
};
use lambda_http::{RequestExt, request::RequestContext};
use serde::Deserialize;
use serde_json::{Value, json};
use std::sync::Arc;

#[derive(Clone)]
pub struct Identities {
    pub store: Arc<dyn AgentStore>,
    pub base: String,
    pub registry: Option<Registry>,
    pub admin_account: String,
}

#[derive(Clone)]
pub struct Owner(pub String);

/// Identity comes only from API Gateway's verified IAM context, never an HTTP
/// header. Production routes also require AWS_IAM; direct local HTTP stays closed.
pub async fn authorize(
    State(state): State<Arc<Identities>>,
    mut req: Request,
    next: Next,
) -> Response {
    let owner = req.request_context_ref().and_then(|context| {
        let RequestContext::ApiGatewayV2(context) = context else {
            return None;
        };
        let iam = context.authorizer.as_ref()?.iam.as_ref()?;
        if iam.account_id.as_deref() != Some(state.admin_account.as_str())
            || state.admin_account.is_empty()
        {
            return None;
        }
        let arn = iam.user_arn.as_deref()?;
        let owner = if arn.contains(":assumed-role/") {
            arn.rsplit_once('/')?.0.to_owned() // Same role across temporary sessions.
        } else if arn.contains(":user/") || arn.ends_with(":root") {
            arn.to_owned()
        } else {
            return None;
        };
        Some(Owner(owner))
    });
    let Some(owner) = owner else {
        return failure(StatusCode::FORBIDDEN, "iam_authentication_required");
    };
    req.extensions_mut().insert(owner);
    let mut response =
        match tokio::time::timeout(std::time::Duration::from_secs(12), next.run(req)).await {
            Ok(response) => response,
            Err(_) => failure(
                StatusCode::GATEWAY_TIMEOUT,
                "operation_incomplete_retry_same_id",
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
    pub name: String,
    pub mock_availability: Vec<Slot>,
}
impl Definition {
    fn valid(&self) -> bool {
        !self.name.trim().is_empty()
            && self.name == self.name.trim()
            && self.name.len() <= 80
            && !self.name.chars().any(char::is_control)
            && !self.mock_availability.is_empty()
            && self.mock_availability.len() <= 32
            && self.mock_availability.iter().all(|s| s.start < s.end)
    }
    fn matches(&self, record: &Record) -> bool {
        self.name == record.agent.name && self.mock_availability == record.agent.slots
    }
}
fn failure(status: StatusCode, code: &str) -> Response {
    (status, Json(json!({"error":code}))).into_response()
}
fn valid_id(id: &str) -> bool {
    uuid::Uuid::parse_str(id).is_ok_and(|u| u.to_string() == id)
}
fn view(record: &Record) -> Value {
    json!({"id":record.agent.id, "identifier":record.agent.identifier(), "name":record.agent.name,
        "mock_availability":record.agent.slots, "registry_id":record.registry_id, "agent_card_url":record.card_url,
        "publication_status":if record.published {"published"} else {"pending"}, "mock":true})
}
async fn publish_record(state: &Identities, mut record: Record) -> Response {
    if record.published {
        return (StatusCode::OK, Json(view(&record))).into_response();
    }
    let Some(registry) = &state.registry else {
        return failure(StatusCode::SERVICE_UNAVAILABLE, "registry_not_configured");
    };
    // No background work: retries replay the exact stored card and proof.
    if let Err(code) = registry.publish(&record).await {
        tracing::warn!(event="publication_pending", tenant=%record.agent.id, code);
        let mut value = view(&record);
        value["error"] = json!(code);
        return (StatusCode::ACCEPTED, Json(value)).into_response();
    }
    if state.store.publish(&record.agent.id).await.is_err() {
        return failure(StatusCode::SERVICE_UNAVAILABLE, "storage_unavailable");
    }
    record.published = true;
    tracing::info!(event="identity_published",tenant=%record.agent.id,registry_id=%record.registry_id);
    (StatusCode::OK, Json(view(&record))).into_response()
}

pub async fn create(
    State(state): State<Arc<Identities>>,
    axum::Extension(owner): axum::Extension<Owner>,
    Path(id): Path<String>,
    Json(definition): Json<Definition>,
) -> Response {
    if !valid_id(&id) || !definition.valid() {
        return failure(StatusCode::BAD_REQUEST, "invalid_agent_definition");
    }
    let Some(registry) = &state.registry else {
        return failure(StatusCode::SERVICE_UNAVAILABLE, "registry_not_configured");
    };
    let existing = match state.store.get(&id).await {
        Ok(r) => r,
        Err(_) => return failure(StatusCode::SERVICE_UNAVAILABLE, "storage_unavailable"),
    };
    let record = if let Some(record) = existing {
        record
    } else {
        let agent = Agent {
            id: id.clone(),
            name: definition.name.clone(),
            slots: definition.mock_availability.clone(),
        };
        let (candidate, key) = match registry.prepare(agent, owner.0.clone(), &state.base) {
            Ok(v) => v,
            Err(_) => return failure(StatusCode::INTERNAL_SERVER_ERROR, "card_generation_failed"),
        };
        match state.store.create(&candidate, &key).await {
            Ok(true) => {
                tracing::info!(event="identity_created",tenant=%id);
                candidate
            }
            Ok(false) => match state.store.get(&id).await {
                Ok(Some(r)) => r,
                _ => return failure(StatusCode::SERVICE_UNAVAILABLE, "storage_unavailable"),
            },
            Err(_) => return failure(StatusCode::SERVICE_UNAVAILABLE, "storage_unavailable"),
        }
    };
    if record.owner != owner.0 {
        return failure(StatusCode::FORBIDDEN, "not_agent_owner");
    }
    if !definition.matches(&record) {
        return failure(StatusCode::CONFLICT, "agent_definition_conflict");
    }
    publish_record(&state, record).await
}

pub async fn status(
    State(state): State<Arc<Identities>>,
    axum::Extension(owner): axum::Extension<Owner>,
    Path(id): Path<String>,
) -> Response {
    if !valid_id(&id) {
        return failure(StatusCode::BAD_REQUEST, "invalid_agent_id");
    }
    match state.store.get(&id).await {
        Ok(Some(record)) if record.owner == owner.0 => Json(view(&record)).into_response(),
        Ok(Some(_)) => failure(StatusCode::FORBIDDEN, "not_agent_owner"),
        Ok(None) => failure(StatusCode::NOT_FOUND, "unknown_agent"),
        Err(_) => failure(StatusCode::SERVICE_UNAVAILABLE, "storage_unavailable"),
    }
}
pub async fn retry(
    State(state): State<Arc<Identities>>,
    axum::Extension(owner): axum::Extension<Owner>,
    Path(id): Path<String>,
) -> Response {
    if !valid_id(&id) {
        return failure(StatusCode::BAD_REQUEST, "invalid_agent_id");
    }
    match state.store.get(&id).await {
        Ok(Some(record)) if record.owner == owner.0 => publish_record(&state, record).await,
        Ok(Some(_)) => failure(StatusCode::FORBIDDEN, "not_agent_owner"),
        Ok(None) => failure(StatusCode::NOT_FOUND, "unknown_agent"),
        Err(_) => failure(StatusCode::SERVICE_UNAVAILABLE, "storage_unavailable"),
    }
}
