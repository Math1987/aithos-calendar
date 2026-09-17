//! Browser booking boundary. All provider writes follow a durable atomic claim.
use crate::booking_page::BookingPage;
use crate::{
    availability::{AvailabilityReader, Slot, first_host_slot},
    booking::{
        Attendee, AttendeeDetails, BookingError, BookingProvider, BookingRequest, JobStatus,
    },
    booking_store::{BookingOperation, BookingStore, Stage},
    storage::AgentStore,
};
use axum::{
    Json, Router,
    extract::{DefaultBodyLimit, Path, State},
    http::StatusCode,
    response::{IntoResponse, Response},
    routing::{get, post},
};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::sync::Arc;
#[derive(Clone)]
pub struct Bookings {
    pub agents: Arc<dyn AgentStore>,
    pub store: Arc<dyn BookingStore>,
    pub reader: Arc<dyn AvailabilityReader>,
    pub provider: Arc<dyn BookingProvider>,
}
#[derive(Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Submission {
    pub id: String,
    pub host: String,
    pub peer: String,
    pub slot: Slot,
    #[serde(default)]
    pub attendee: AttendeeDetails,
}
pub fn router(state: Bookings) -> Router {
    Router::new()
        .route("/bookings", post(submit))
        .route("/bookings/{id}", get(status))
        .layer(DefaultBodyLimit::max(4096))
        .with_state(Arc::new(state))
}
pub fn digest(text: &str) -> String {
    format!("{:x}", Sha256::digest(text))
}
fn now() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_secs() as i64
}
fn response(status: StatusCode, v: Value) -> Response {
    (status, [("cache-control", "no-store")], Json(v)).into_response()
}
fn error(status: StatusCode, code: &str) -> Response {
    response(status, json!({"error":code}))
}
fn valid_id(id: &str) -> bool {
    uuid::Uuid::parse_str(id).is_ok_and(|v| v.get_version_num() == 4 && v.to_string() == id)
}
fn view(r: &BookingOperation) -> Response {
    let stage = if r.stage == Stage::Submitting && now() - r.created_at > 30 {
        Stage::Unknown
    } else {
        r.stage
    };
    response(
        if matches!(stage, Stage::Pending | Stage::Submitting) {
            StatusCode::ACCEPTED
        } else {
            StatusCode::OK
        },
        json!({"id":r.id,"host":r.host,"peer":r.peer,"slot":r.slot,"status":stage,"reserved":stage==Stage::Booked,"retry_after_ms":3000}),
    )
}
async fn submit(State(s): State<Arc<Bookings>>, Json(input): Json<Submission>) -> Response {
    if !valid_id(&input.id)
        || !crate::valid_tenant(&input.host)
        || !crate::valid_tenant(&input.peer)
        || input.host == input.peer
    {
        return error(StatusCode::BAD_REQUEST, "invalid_booking_request");
    }
    let request_digest = digest(&serde_json::to_string(&input).unwrap());
    match s.store.get(&input.id).await {
        Ok(Some(r)) => {
            return if r.request_digest == request_digest {
                view(&r)
            } else {
                error(StatusCode::CONFLICT, "operation_id_reused")
            };
        }
        Err(_) => return error(StatusCode::SERVICE_UNAVAILABLE, "storage_unavailable"),
        _ => (),
    }
    let start: chrono::DateTime<chrono::Utc> = std::time::SystemTime::now().into();
    if input.slot.start <= start
        || input.slot.end <= input.slot.start
        || input.slot.end > start + chrono::Duration::days(30)
    {
        return error(StatusCode::BAD_REQUEST, "invalid_booking_slot");
    }
    let pages = async {
        let (host, peer) = tokio::try_join!(s.agents.get(&input.host), s.agents.get(&input.peer))
            .map_err(|_| "storage_unavailable")?;
        let page = |r: Option<crate::storage::Record>| {
            r.filter(|r| r.published && r.agent.live)
                .and_then(|r| r.booking_page_url)
                .map(|url| BookingPage { url })
                .ok_or("unknown_live_agent")
        };
        let (host, peer) = (page(host)?, page(peer)?);
        tokio::try_join!(
            s.reader
                .read(&host, start, start + chrono::Duration::days(30)),
            s.reader
                .read(&peer, start, start + chrono::Duration::days(30))
        )
        .map_err(|_| "availability_unavailable")
    };
    let (host, visitor) = match tokio::time::timeout(std::time::Duration::from_secs(9), pages).await
    {
        Ok(Ok(v)) => v,
        Ok(Err(code)) => return error(StatusCode::SERVICE_UNAVAILABLE, code),
        Err(_) => return error(StatusCode::GATEWAY_TIMEOUT, "timeout"),
    };
    if !host.slots.contains(&input.slot)
        || first_host_slot(
            &crate::availability::Schedule {
                slots: vec![input.slot.clone()],
                ..host.clone()
            },
            &visitor,
        )
        .is_none()
    {
        return error(StatusCode::CONFLICT, "slot_no_longer_available");
    }
    let attendee = match Attendee::from_page(&visitor.identity, &input.attendee) {
        Ok(v) => v,
        Err(missing) => {
            return response(
                StatusCode::UNPROCESSABLE_ENTITY,
                json!({"error":"attendee_details_required","missing_fields":missing.fields}),
            );
        }
    };
    let mut record = BookingOperation {
        id: input.id,
        request_digest,
        host: input.host,
        peer: input.peer,
        slot: input.slot.clone(),
        schedule_id: host.schedule_id.clone(),
        email_digest: digest(&attendee.email.to_lowercase()),
        stage: Stage::Submitting,
        job_id: None,
        created_at: now(),
        next_poll: 0,
        revision: 0,
    };
    let request = match BookingRequest::from_schedule(&host, input.slot, attendee) {
        Ok(v) => v,
        Err(_) => return error(StatusCode::BAD_REQUEST, "invalid_booking_request"),
    };
    if s.provider.ready().await.is_err() {
        return error(StatusCode::SERVICE_UNAVAILABLE, "booking_unavailable");
    }
    match s.store.begin(&record).await {
        Ok(true) => (),
        Ok(false) => {
            return match s.store.get(&record.id).await {
                Ok(Some(r)) if r.request_digest == record.request_digest => view(&r),
                _ => error(StatusCode::CONFLICT, "booking_already_in_progress"),
            };
        }
        Err(_) => return error(StatusCode::SERVICE_UNAVAILABLE, "storage_unavailable"),
    }
    // At this point a timeout/crash is ambiguous. No request may reclaim this write.
    let release = match s.provider.submit(&request).await {
        Ok(job) => {
            record.job_id = Some(job);
            record.stage = Stage::Pending;
            false
        }
        Err(
            BookingError::Rejected | BookingError::InvalidRequest | BookingError::NotConfigured,
        ) => {
            record.stage = Stage::Failed;
            true
        }
        Err(_) => {
            record.stage = Stage::Unknown;
            false
        }
    };
    record.revision += 1;
    match s.store.save(&record, 0, release).await {
        Ok(true) => {
            tracing::info!(target:"calendar::booking",event="booking_submitted",operation_id=%record.id,status=?record.stage);
            view(&record)
        }
        _ => error(
            StatusCode::SERVICE_UNAVAILABLE,
            "booking_status_unavailable",
        ),
    }
}
async fn status(State(s): State<Arc<Bookings>>, Path(id): Path<String>) -> Response {
    if !valid_id(&id) {
        return error(StatusCode::NOT_FOUND, "unknown_booking");
    }
    let mut r = match s.store.get(&id).await {
        Ok(Some(r)) => r,
        Ok(None) => return error(StatusCode::NOT_FOUND, "unknown_booking"),
        Err(_) => return error(StatusCode::SERVICE_UNAVAILABLE, "storage_unavailable"),
    };
    if r.stage != Stage::Pending || r.next_poll > now() {
        return view(&r);
    }
    // A short persisted poll lease prevents tabs from hammering Anakin.
    let previous = r.revision;
    r.revision += 1;
    r.next_poll = now() + 12;
    match s.store.save(&r, previous, false).await {
        Ok(true) => (),
        Ok(false) => return view(&r),
        Err(_) => return error(StatusCode::SERVICE_UNAVAILABLE, "storage_unavailable"),
    }
    let outcome = s.provider.status(r.job_id.as_deref().unwrap_or("")).await;
    let previous = r.revision;
    r.revision += 1;
    match outcome {
        Ok(JobStatus::Processing { retry_after_ms }) => {
            r.next_poll = now() + ((retry_after_ms + 999) / 1000).max(3) as i64
        }
        Ok(JobStatus::CompletedUnverified(data)) => {
            r.stage = if crate::booking::confirmed(&data, &r) {
                Stage::Booked
            } else {
                Stage::Unknown
            }
        }
        // A failed job may have failed after Google wrote the appointment.
        Ok(JobStatus::Failed) => r.stage = Stage::Unknown,
        Err(_) => return view(&r),
    }
    let release = r.stage == Stage::Booked;
    match s.store.save(&r, previous, release).await {
        Ok(true) => view(&r),
        _ => error(
            StatusCode::SERVICE_UNAVAILABLE,
            "booking_status_unavailable",
        ),
    }
}
