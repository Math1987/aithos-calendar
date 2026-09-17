use async_trait::async_trait;
use axum::{
    Router,
    body::{Body, to_bytes},
    http::Request,
};
use calendar::{
    availability::{AvailabilityReader, PageIdentity, ReadError, Schedule, Slot},
    booking::{BookingError, BookingProvider, BookingRequest, JobStatus},
    booking_api::{Bookings, router},
    booking_page::BookingPage,
    booking_store::MemoryBookingStore,
    storage::{AgentStore, MemoryStore, Record},
};
use chrono::{Duration, Utc};
use serde_json::{Value, json};
use std::sync::{
    Arc,
    atomic::{AtomicUsize, Ordering},
};
use tower::ServiceExt;
struct Reader {
    slot: Slot,
    missing: bool,
    busy: bool,
}
#[async_trait]
impl AvailabilityReader for Reader {
    async fn read(
        &self,
        p: &BookingPage,
        start: chrono::DateTime<Utc>,
        end: chrono::DateTime<Utc>,
    ) -> Result<Schedule, ReadError> {
        Ok(Schedule {
            schedule_id: p.url.rsplit('/').next().unwrap().into(),
            title: "Test".into(),
            timezone: "UTC".into(),
            duration_minutes: 30,
            window_start: start,
            window_end: end,
            slots: if self.busy {
                vec![]
            } else {
                vec![self.slot.clone()]
            },
            identity: PageIdentity {
                display_name: Some("Test Guest".into()),
                email: if self.missing {
                    None
                } else {
                    Some("guest@example.com".into())
                },
            },
        })
    }
}
struct Provider {
    writes: AtomicUsize,
    polls: AtomicUsize,
    unknown: bool,
    completed: std::sync::atomic::AtomicBool,
}
#[async_trait]
impl BookingProvider for Provider {
    async fn submit(&self, _: &BookingRequest) -> Result<String, BookingError> {
        self.writes.fetch_add(1, Ordering::SeqCst);
        if self.unknown {
            Err(BookingError::SubmissionUnknown)
        } else {
            Ok("job-test".into())
        }
    }
    async fn status(&self, _: &str) -> Result<JobStatus, BookingError> {
        self.polls.fetch_add(1, Ordering::SeqCst);
        if self.completed.load(Ordering::SeqCst) {
            return Ok(JobStatus::CompletedUnverified(
                json!({"provider_result":"unverified"}),
            ));
        }
        Ok(JobStatus::Processing {
            retry_after_ms: 3000,
        })
    }
}
async fn setup(missing: bool, busy: bool, unknown: bool) -> (Router, Arc<Provider>, Value) {
    let agents = Arc::new(MemoryStore::default());
    for id in ["host", "guest"] {
        agents
            .create(
                &Record {
                    agent: serde_json::from_value(
                        json!({"id":id,"name":id,"live":true,"slots":[]}),
                    )
                    .unwrap(),
                    booking_page_url: Some(format!(
                        "https://calendar.google.com/calendar/appointments/schedules/{id}"
                    )),
                    registry_id: id.into(),
                    card_url: String::new(),
                    card_bytes: String::new(),
                    card_digest: String::new(),
                    publication: Value::Null,
                    published: true,
                },
                "",
            )
            .await
            .unwrap();
    }
    let start =
        chrono::DateTime::from_timestamp((Utc::now() + Duration::days(1)).timestamp(), 0).unwrap();
    let slot = Slot {
        start,
        end: start + Duration::minutes(30),
    };
    let provider = Arc::new(Provider {
        writes: AtomicUsize::new(0),
        polls: AtomicUsize::new(0),
        unknown,
        completed: std::sync::atomic::AtomicBool::new(false),
    });
    let app = router(Bookings {
        agents,
        store: Arc::new(MemoryBookingStore::default()),
        reader: Arc::new(Reader {
            slot: slot.clone(),
            missing,
            busy,
        }),
        provider: provider.clone(),
    });
    (
        app,
        provider,
        json!({"id":"01234567-89ab-4def-8123-456789abcdef","host":"host","peer":"guest","slot":slot}),
    )
}
async fn call(app: &Router, method: &str, path: &str, data: Value) -> (u16, Value) {
    let res = app
        .clone()
        .oneshot(
            Request::builder()
                .method(method)
                .uri(path)
                .header("content-type", "application/json")
                .body(Body::from(data.to_string()))
                .unwrap(),
        )
        .await
        .unwrap();
    let status = res.status().as_u16();
    let body = to_bytes(res.into_body(), 10000).await.unwrap();
    (status, serde_json::from_slice(&body).unwrap())
}
#[tokio::test]
async fn concurrent_submission_and_refresh_write_once() {
    let (app, p, input) = setup(false, false, false).await;
    let (a, b) = tokio::join!(
        call(&app, "POST", "/bookings", input.clone()),
        call(&app, "POST", "/bookings", input.clone())
    );
    assert_eq!(a.0, 202);
    assert_eq!(b.0, 202);
    assert_eq!(p.writes.load(Ordering::SeqCst), 1);
    let path = format!("/bookings/{}", input["id"].as_str().unwrap());
    for _ in 0..3 {
        assert_eq!(
            call(&app, "GET", &path, Value::Null).await.1["status"],
            "pending"
        );
    }
    assert_eq!(p.polls.load(Ordering::SeqCst), 1);
    let mut other = input.clone();
    other["id"] = json!("11234567-89ab-4def-8123-456789abcdef");
    assert_eq!(call(&app, "POST", "/bookings", other).await.0, 409);
    let mut changed = input;
    changed["attendee"] = json!({"email":"other@example.com"});
    assert_eq!(
        call(&app, "POST", "/bookings", changed).await.1["error"],
        "operation_id_reused"
    );
    assert_eq!(p.writes.load(Ordering::SeqCst), 1);
}
#[tokio::test]
async fn unknown_submission_is_never_retried() {
    let (app, p, mut input) = setup(false, false, true).await;
    for _ in 0..2 {
        let (status, data) = call(&app, "POST", "/bookings", input.clone()).await;
        assert_eq!(status, 200);
        assert_eq!(data["status"], "unknown");
        assert_eq!(data["reserved"], false);
    }
    input["id"] = json!("11234567-89ab-4def-8123-456789abcdef");
    assert_eq!(call(&app, "POST", "/bookings", input).await.0, 409);
    assert_eq!(p.writes.load(Ordering::SeqCst), 1);
}
#[tokio::test]
async fn missing_contact_can_be_supplied_without_earlier_write() {
    let (app, p, mut input) = setup(true, false, false).await;
    let (status, data) = call(&app, "POST", "/bookings", input.clone()).await;
    assert_eq!(status, 422);
    assert_eq!(data["missing_fields"], json!(["email"]));
    assert_eq!(p.writes.load(Ordering::SeqCst), 0);
    input["attendee"] = json!({"email":"guest@example.com"});
    assert_eq!(call(&app, "POST", "/bookings", input).await.0, 202);
    assert_eq!(p.writes.load(Ordering::SeqCst), 1);
}
#[tokio::test]
async fn stale_slot_and_unknown_agent_never_reach_provider() {
    let (app, p, mut input) = setup(false, true, false).await;
    assert_eq!(
        call(&app, "POST", "/bookings", input.clone()).await.1["error"],
        "slot_no_longer_available"
    );
    input["peer"] = json!("unknown");
    assert_eq!(
        call(&app, "POST", "/bookings", input).await.1["error"],
        "unknown_live_agent"
    );
    assert_eq!(p.writes.load(Ordering::SeqCst), 0);
}

#[tokio::test]
async fn first_completed_provider_job_requires_confirmation_without_resubmitting() {
    let (app, p, input) = setup(false, false, false).await;
    p.completed.store(true, Ordering::SeqCst);
    assert_eq!(call(&app, "POST", "/bookings", input.clone()).await.0, 202);
    let path = format!("/bookings/{}", input["id"].as_str().unwrap());
    for _ in 0..2 {
        let (_, result) = call(&app, "GET", &path, Value::Null).await;
        assert_eq!(result["status"], "confirmation_required");
        assert_eq!(result["reserved"], false);
        assert!(result.get("job_id").is_none());
    }
    assert_eq!(p.writes.load(Ordering::SeqCst), 1);
    assert_eq!(p.polls.load(Ordering::SeqCst), 1);
}
