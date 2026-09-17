//! Durable, authenticated requests. The browser polls; closing it does not stop a job.
use super::{
    model::Model,
    preferences,
    state::{Result, StateStore},
};
use crate::{
    auth::{Auth, current, error, now, origin_ok},
    connected::Connected,
};
use axum::{
    Json, Router,
    extract::{Path, State},
    http::{HeaderMap, StatusCode},
    response::{IntoResponse, Response},
    routing::{get, post},
};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::sync::Arc;
#[derive(Clone, Serialize, Deserialize)]
pub struct Job {
    pub id: String,
    pub peer: String,
    pub host: String,
    pub status: String,
    pub created: i64,
    pub lease_until: i64,
    pub attempts: u32,
    pub result: Value,
}
#[async_trait::async_trait]
pub trait Queue: Send + Sync {
    async fn enqueue(&self, id: &str, delay: i32) -> Result<()>;
}
pub struct SqsQueue {
    pub client: aws_sdk_sqs::Client,
    pub url: String,
}
#[async_trait::async_trait]
impl Queue for SqsQueue {
    async fn enqueue(&self, id: &str, delay: i32) -> Result<()> {
        self.client
            .send_message()
            .queue_url(&self.url)
            .message_body(id)
            .delay_seconds(delay)
            .send()
            .await
            .map_err(|_| "task_queue_unavailable")?;
        Ok(())
    }
}
pub struct Jobs {
    pub store: Arc<dyn StateStore>,
    pub queue: Arc<dyn Queue>,
}
impl Jobs {
    async fn enqueue(&self, id: &str, delay: i32) -> Result<()> {
        self.queue.enqueue(id, delay).await
    }
    #[tracing::instrument(skip_all, fields(task_id=id))]
    pub async fn process(
        &self,
        id: &str,
        service: &Connected,
        model: Option<&Arc<Model>>,
    ) -> Result<()> {
        if !valid_id(id) {
            return Err("invalid_task");
        }
        let key = format!("job:{id}");
        let row = self.store.read(&key).await?.ok_or("invalid_task")?;
        let mut job: Job = serde_json::from_value(row.value).map_err(|_| "invalid_task")?;
        if ["booked", "failed", "no_common_slot", "needs_attention"].contains(&job.status.as_str())
        {
            return Ok(());
        }
        if job.lease_until > now() {
            return Err("task_busy");
        }
        job.status = "working".into();
        job.lease_until = now() + 300;
        job.attempts += 1;
        if !self
            .store
            .cas(
                &key,
                Some(row.revision),
                serde_json::to_value(&job).unwrap(),
            )
            .await?
        {
            return Err("task_busy");
        }
        let revision = row.revision + 1;
        let work = async {
            let existing = service
                .bookings
                .get(id)
                .await
                .map_err(|_| "storage_unavailable")?;
            if existing.is_none() {
                let (own, _host) = tokio::join!(
                    preferences::prepare(
                        service.calendars.as_ref(),
                        service.store.as_ref(),
                        model,
                        &job.peer,
                        &job.host
                    ),
                    preferences::prepare(
                        service.calendars.as_ref(),
                        service.store.as_ref(),
                        model,
                        &job.host,
                        &job.peer
                    )
                );
                job.result = json!({"analysis_source":own.source,"previous_meetings":own.previous_meetings,
                    "explanation":if own.source=="learned" {"Selected from both calendars using evidenced preferences and shared availability."}else{"Selected from shared availability with deterministic defaults where preferences were unavailable."}});
                if service
                    .propose_with_id(&job.peer, &job.host, id)
                    .await?
                    .is_none()
                {
                    return Ok(json!({"status":"no_common_slot","reserved":false}));
                }
            }
            service.confirm(&job.peer, id).await
        };
        let result = tokio::time::timeout(std::time::Duration::from_secs(210), work)
            .await
            .unwrap_or(Err("task_timeout"));
        let mut again = false;
        match result {
            Ok(value) => {
                let status = value["status"].as_str().unwrap_or("failed");
                job.status = match status {
                    "booked" => "booked",
                    "no_common_slot" => "no_common_slot",
                    "slot_unavailable" | "failed" => "failed",
                    _ if job.attempts < 20 => {
                        again = true;
                        "working"
                    }
                    _ => "needs_attention",
                }
                .into();
                if let (Some(a), Some(b)) = (job.result.as_object_mut(), value.as_object()) {
                    a.extend(b.clone());
                } else {
                    job.result = value;
                }
            }
            Err(code) => {
                // Retry a durable booking only by its original ID. Never repeat a
                // potentially submitted Google write with a newly generated ID.
                let pending = service
                    .bookings
                    .get(id)
                    .await
                    .map_err(|_| "storage_unavailable")?
                    .is_some();
                again = pending && job.attempts < 20;
                job.status = if again {
                    "working"
                } else if pending {
                    "needs_attention"
                } else {
                    "failed"
                }
                .into();
                job.result["error"] = json!(code);
            }
        }
        job.lease_until = 0;
        if !self
            .store
            .cas(&key, Some(revision), serde_json::to_value(&job).unwrap())
            .await?
        {
            return Err("task_state_conflict");
        }
        if again {
            self.enqueue(id, 5).await?;
        }
        Ok(())
    }
}
fn valid_id(id: &str) -> bool {
    id.len() == 42 && id.starts_with("gc") && id[2..].bytes().all(|b| b.is_ascii_hexdigit())
}
pub fn router(auth: Auth) -> Router {
    Router::new()
        .route("/calendar/tasks", post(start))
        .route("/calendar/tasks/{id}", get(status))
        .layer(axum::extract::DefaultBodyLimit::max(4096))
        .layer(axum::middleware::from_fn(crate::auth::private_response))
        .with_state(Arc::new(auth))
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Input {
    host_url: String,
    request_id: String,
}
async fn start(
    State(auth): State<Arc<Auth>>,
    headers: HeaderMap,
    Json(input): Json<Input>,
) -> Response {
    if !origin_ok(&auth, &headers) {
        return error(StatusCode::FORBIDDEN, "invalid_origin");
    }
    let account = match current(&auth, &headers).await {
        Ok(a) => a,
        Err(e) => return e,
    };
    let Some(jobs) = auth.connected.as_ref().and_then(|s| s.jobs.as_ref()) else {
        return error(StatusCode::SERVICE_UNAVAILABLE, "agent_unavailable");
    };
    let host = match crate::connected::parse_host(&input.host_url, &auth.website) {
        Ok(h) => h,
        Err(e) => return error(StatusCode::BAD_REQUEST, e),
    };
    if host == account.id {
        return error(StatusCode::BAD_REQUEST, "same_account");
    }
    if uuid::Uuid::parse_str(&input.request_id).is_err() {
        return error(StatusCode::BAD_REQUEST, "invalid_request_id");
    }
    let id = format!(
        "gc{}",
        &crate::booking_api::digest(&format!("{}:{}", account.id, input.request_id))[..40]
    );
    let key = format!("job:{id}");
    let job = Job {
        id: id.clone(),
        peer: account.id.clone(),
        host: host.clone(),
        status: "queued".into(),
        created: now(),
        lease_until: 0,
        attempts: 0,
        result: json!({}),
    };
    let result = async {
        if !jobs
            .store
            .cas(&key, None, serde_json::to_value(job).unwrap())
            .await?
        {
            let row = jobs.store.read(&key).await?.ok_or("task_unavailable")?;
            let previous: Job = serde_json::from_value(row.value).map_err(|_| "invalid_task")?;
            if previous.peer != account.id || previous.host != host {
                return Err("request_id_reused");
            }
            if previous.status != "queued" {
                return Ok(());
            }
        }
        jobs.enqueue(&id, 0).await
    }
    .await;
    match result {
        Ok(()) => (
            StatusCode::ACCEPTED,
            Json(json!({"id":id,"status":"queued"})),
        )
            .into_response(),
        Err(code) => error(StatusCode::SERVICE_UNAVAILABLE, code),
    }
}
async fn status(
    State(auth): State<Arc<Auth>>,
    headers: HeaderMap,
    Path(id): Path<String>,
) -> Response {
    let account = match current(&auth, &headers).await {
        Ok(a) => a,
        Err(e) => return e,
    };
    if !valid_id(&id) {
        return error(StatusCode::NOT_FOUND, "unknown_task");
    }
    let Some(jobs) = auth.connected.as_ref().and_then(|s| s.jobs.as_ref()) else {
        return error(StatusCode::SERVICE_UNAVAILABLE, "agent_unavailable");
    };
    match jobs.store.read(&format!("job:{id}")).await {
        Ok(Some(row)) => match serde_json::from_value::<Job>(row.value) {
            Ok(j) if j.peer == account.id => {
                Json(json!({"id":j.id,"status":j.status,"result":j.result})).into_response()
            }
            _ => error(StatusCode::NOT_FOUND, "unknown_task"),
        },
        Ok(None) => error(StatusCode::NOT_FOUND, "unknown_task"),
        Err(code) => error(StatusCode::SERVICE_UNAVAILABLE, code),
    }
}
