use axum::{
    Json, Router,
    body::{Body, to_bytes},
    extract::Path,
    http::{Request, StatusCode},
    response::IntoResponse,
    routing::get,
};
use calendar::storage::{AgentStore, MemoryStore};
use serde_json::{Value, json};
use std::{
    collections::HashMap,
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, Ordering},
    },
};
use tokio::task::JoinHandle;
use tower::ServiceExt;

use calendar::booking_page::{BookingPage, BookingPages, GoogleBookingPages, PageError};
const HOST: &str = "https://calendar.google.com/calendar/appointments/schedules/HostPage";
const GUEST: &str = "https://calendar.google.com/calendar/appointments/schedules/GuestPage";

#[derive(Default)]
struct TestPages {
    validations: std::sync::atomic::AtomicUsize,
    reject: AtomicBool,
    barrier: Mutex<Option<Arc<tokio::sync::Barrier>>>,
}
#[async_trait::async_trait]
impl BookingPages for TestPages {
    async fn resolve(&self, input: &str) -> Result<BookingPage, PageError> {
        let url = match input {
            "https://calendar.app.google/host" => HOST,
            "https://calendar.app.google/guest" => GUEST,
            _ => input,
        };
        GoogleBookingPages::new().unwrap().resolve(url).await
    }
    async fn validate(&self, _: &BookingPage) -> Result<(), PageError> {
        self.validations.fetch_add(1, Ordering::SeqCst);
        let barrier = self.barrier.lock().unwrap().clone();
        if let Some(barrier) = barrier {
            barrier.wait().await;
        }
        if self.reject.load(Ordering::SeqCst) {
            Err(PageError::Unrecognized)
        } else {
            Ok(())
        }
    }
}

struct Setup {
    app: Router,
    base: String,
    store: Arc<MemoryStore>,
    writes: Arc<Mutex<HashMap<String, String>>>,
    calls: Arc<Mutex<Vec<Value>>>,
    pages: Arc<TestPages>,
    jobs: Vec<JoinHandle<()>>,
}
impl Drop for Setup {
    fn drop(&mut self) {
        for job in &self.jobs {
            job.abort();
        }
    }
}
async fn setup(fail: bool) -> Setup {
    setup_with_reader(fail, None).await
}
async fn setup_with_reader(
    fail: bool,
    reader: Option<Arc<dyn calendar::availability::AvailabilityReader>>,
) -> Setup {
    let registry_socket = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let registry_origin = format!("http://{}", registry_socket.local_addr().unwrap());
    let writes = Arc::new(Mutex::new(HashMap::<String, String>::new()));
    let calls = Arc::new(Mutex::new(Vec::new()));
    let fail = Arc::new(AtomicBool::new(fail));
    let store_write = writes.clone();
    let store_read = writes.clone();
    let seen = calls.clone();
    let fail_once = fail.clone();
    let origin = registry_origin.clone();
    let registry = Router::new()
        .route(
            "/v1/agents/{id}",
            axum::routing::put(move |Path(id): Path<String>, Json(body): Json<Value>| {
                let data = store_write.clone();
                let seen = seen.clone();
                let fail_once = fail_once.clone();
                let origin = origin.clone();
                async move {
                    let card = a2a_card::validate_value(body["agentCard"].clone()).unwrap();
                    let keys = body["keys"].as_array().unwrap();
                    let proofs: Vec<_> = body["proofs"]
                        .as_array()
                        .unwrap()
                        .iter()
                        .map(|p| registry_core::write::DetachedJws {
                            protected: p["protected"].as_str().unwrap().into(),
                            payload: p["payload"].as_str().unwrap().into(),
                            signature: p["signature"].as_str().unwrap().into(),
                        })
                        .collect();
                    // Independent Aithos verifier checks signatures, key ownership and publication intent.
                    registry_core::write::evaluate_write(&id, &card, keys, &proofs, &origin, None)
                        .unwrap();
                    seen.lock().unwrap().push(body);
                    let bytes = String::from_utf8(card.bytes).unwrap();
                    let mut stored = data.lock().unwrap();
                    if let Some(previous) = stored.get(&id) {
                        assert_eq!(previous, &bytes, "retry must preserve signed bytes");
                    }
                    stored.insert(id, bytes);
                    if fail_once.swap(false, Ordering::SeqCst) {
                        StatusCode::SERVICE_UNAVAILABLE
                    } else {
                        StatusCode::OK
                    }
                }
            }),
        )
        .route(
            "/v1/agents/{id}/agent-card.json",
            get(move |Path(id): Path<String>| {
                let data = store_read.clone();
                async move {
                    match data.lock().unwrap().get(&id) {
                        Some(bytes) => {
                            ([("content-type", "application/json")], bytes.clone()).into_response()
                        }
                        None => StatusCode::NOT_FOUND.into_response(),
                    }
                }
            }),
        );
    let registry_job =
        tokio::spawn(async move { axum::serve(registry_socket, registry).await.unwrap() });
    let socket = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base = format!("http://{}", socket.local_addr().unwrap());
    let store = Arc::new(MemoryStore::default());
    let pages = Arc::new(TestPages::default());
    let app = calendar::app_with_reader(
        &base,
        &format!("{base}/.well-known/ai-catalog.json"),
        store.clone(),
        Some(calendar::registry::Registry::new(&registry_origin).unwrap()),
        base.clone(),
        pages.clone(),
        reader,
    )
    .unwrap();
    let network = app.clone();
    let job = tokio::spawn(async move { axum::serve(socket, network).await.unwrap() });
    Setup {
        app,
        base,
        store,
        writes,
        calls,
        pages,
        jobs: vec![job, registry_job],
    }
}
async fn submit(setup: &Setup, body: Value) -> (StatusCode, Value) {
    // No authentication or trusted Lambda context at all.
    let req = Request::builder()
        .method("POST")
        .uri("/agents")
        .header("content-type", "application/json")
        .body(Body::from(body.to_string()))
        .unwrap();
    let response = setup.app.clone().oneshot(req).await.unwrap();
    let status = response.status();
    assert_eq!(response.headers()["cache-control"], "no-store");
    let bytes = to_bytes(response.into_body(), 65536).await.unwrap();
    (
        status,
        serde_json::from_slice(&bytes).unwrap_or(Value::Null),
    )
}
async fn create(setup: &Setup, url: &str) -> (StatusCode, Value) {
    submit(setup, json!({"booking_page_url":url})).await
}
async fn catalog(setup: &Setup) -> Value {
    reqwest::get(format!("{}/.well-known/ai-catalog.json", setup.base))
        .await
        .unwrap()
        .json()
        .await
        .unwrap()
}

#[tokio::test]
async fn public_creation_reuses_aliases_and_resumes_lost_registry_response() {
    let s = setup(true).await;
    let (status, pending) = create(&s, "https://calendar.app.google/host").await;
    assert_eq!(status, StatusCode::ACCEPTED, "{pending}");
    assert_eq!(pending["publication_status"], "pending");
    assert_eq!(pending["booking_page_url"], HOST);
    assert_eq!(catalog(&s).await["entries"], json!([]));
    let id = pending["id"].as_str().unwrap();
    assert_eq!(
        reqwest::get(format!("{}/agents/{id}/agent-card.json", s.base))
            .await
            .unwrap()
            .status(),
        StatusCode::NOT_FOUND
    );
    // Existing records must not be revalidated or regenerated during a Google outage.
    s.pages.reject.store(true, Ordering::SeqCst);
    let (status, published) = create(&s, &format!("{HOST}?gv=true#fragment")).await;
    assert_eq!(status, StatusCode::OK, "{published}");
    assert_eq!(published["publication_status"], "published");
    for key in ["id", "registry_id", "agent_card_url", "share_url"] {
        assert_eq!(pending[key], published[key]);
    }
    assert_eq!(s.pages.validations.load(Ordering::SeqCst), 1);
    let calls = s.calls.lock().unwrap();
    assert_eq!(calls.len(), 2);
    assert_eq!(calls[0], calls[1]);
    drop(calls);
    assert_eq!(create(&s, HOST).await.1, published);
    assert_eq!(
        s.calls.lock().unwrap().len(),
        2,
        "confirmed reuse must not republish"
    );
    assert_eq!(s.writes.lock().unwrap().len(), 1);
    assert_eq!(catalog(&s).await["entries"].as_array().unwrap().len(), 1);
    assert!(published.get("signing_key").is_none());
    assert!(published.get("owner").is_none());
}

#[tokio::test]
async fn dynamic_page_agents_discover_registry_cards_and_collaborate() {
    let s = setup(false).await;
    let mut ids = Vec::new();
    for url in [HOST, GUEST] {
        let (status, body) = create(&s, url).await;
        assert_eq!(status, StatusCode::OK, "{body}");
        let id = body["id"].as_str().unwrap().to_owned();
        let record = s.store.get(&id).await.unwrap().unwrap();
        let local = reqwest::get(format!("{}/agents/{id}/agent-card.json", s.base))
            .await
            .unwrap()
            .text()
            .await
            .unwrap();
        assert_eq!(local, record.card_bytes);
        assert_eq!(record.booking_page_url.as_deref(), Some(url));
        ids.push(id);
    }
    let typed: ai_catalog::AiCatalog = serde_json::from_value(catalog(&s).await).unwrap();
    assert_eq!(typed.entries.len(), 2);
    for entry in typed.entries {
        assert!(!entry.url.unwrap().starts_with(&s.base));
    }
    for (caller, peer) in [(&ids[0], &ids[1]), (&ids[1], &ids[0])] {
        let reply:Value=reqwest::Client::new().post(format!("{}/a2a",s.base)).json(&json!({"jsonrpc":"2.0","id":"dynamic","method":"SendMessage","params":{
            "tenant":caller,"message":{"role":"ROLE_USER","messageId":"test","parts":[{"data":{"operation":"find_common_slot","peer":format!("urn:aithos:calendar:agent:{peer}"),"duration_minutes":30}}]}
        }})).send().await.unwrap().json().await.unwrap();
        let result = reply["result"]["message"]["parts"]
            .as_array()
            .unwrap()
            .iter()
            .find_map(|p| p.get("data"))
            .unwrap();
        assert_eq!(result["status"], "slot_found", "{result}");
        assert_eq!(result["reserved"], false);
        assert_eq!(result["slot"]["start"], "2030-01-15T09:30:00Z");
    }
}

#[tokio::test]
async fn simultaneous_first_submissions_have_one_winner_and_one_card() {
    let s = setup(false).await;
    // Force both requests past the missing-record check before either inserts.
    *s.pages.barrier.lock().unwrap() = Some(Arc::new(tokio::sync::Barrier::new(2)));
    let (first, second) = futures::join!(
        create(&s, HOST),
        create(&s, "https://calendar.app.google/host")
    );
    assert_eq!(first.0, StatusCode::OK, "{}", first.1);
    assert_eq!(second.0, StatusCode::OK, "{}", second.1);
    assert_eq!(first.1, second.1);
    assert_eq!(s.pages.validations.load(Ordering::SeqCst), 2);
    assert_eq!(s.writes.lock().unwrap().len(), 1);
    assert_eq!(s.store.published().await.unwrap().len(), 1);
    let calls = s.calls.lock().unwrap();
    assert!(calls.len() >= 1);
    assert!(calls.iter().all(|c| c == &calls[0]));
}

#[tokio::test]
async fn rejected_input_never_creates_records_and_cannot_override_existing_agent() {
    let s = setup(false).await;
    for input in [
        json!({}),
        json!({"booking_page_url":"http://127.0.0.1/private"}),
        json!({"booking_page_url":HOST,"name":"Spoofed"}),
        json!({"booking_page_url":HOST,"id":"arbitrary"}),
        json!({"booking_page_url":HOST,"mock_availability":[]}),
        json!({"booking_page_url":"x".repeat(5000)}),
    ] {
        assert!(!submit(&s, input).await.0.is_success());
    }
    assert!(s.store.published().await.unwrap().is_empty());
    assert!(s.calls.lock().unwrap().is_empty());
    s.pages.reject.store(true, Ordering::SeqCst);
    assert_eq!(create(&s, HOST).await.0, StatusCode::UNPROCESSABLE_ENTITY);
    let id = BookingPage { url: HOST.into() }.agent_id();
    assert!(s.store.get(&id).await.unwrap().is_none());
    s.pages.reject.store(false, Ordering::SeqCst);
    let original = create(&s, HOST).await.1;
    assert_eq!(
        submit(&s, json!({"booking_page_url":HOST,"name":"Changed"}))
            .await
            .0,
        StatusCode::UNPROCESSABLE_ENTITY
    );
    assert_eq!(create(&s, HOST).await.1, original);
    for method in ["GET", "PUT", "POST"] {
        let response = s
            .app
            .clone()
            .oneshot(
                Request::builder()
                    .method(method)
                    .uri("/admin/agents/old")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::NOT_FOUND);
    }
}

#[derive(Default)]
struct LiveReader(std::sync::atomic::AtomicU8);
#[async_trait::async_trait]
impl calendar::availability::AvailabilityReader for LiveReader {
    async fn read(
        &self,
        page: &BookingPage,
        start: chrono::DateTime<chrono::Utc>,
        end: chrono::DateTime<chrono::Utc>,
    ) -> Result<calendar::availability::Schedule, calendar::availability::ReadError> {
        use calendar::availability::{PageIdentity, ReadError, Schedule, Slot};
        use chrono::Duration;
        let mode = self.0.load(Ordering::SeqCst);
        if mode == 2 {
            return Err(ReadError::Unavailable);
        }
        let host = page.url == HOST;
        let offered = start + Duration::days(1);
        let slots = if host {
            vec![Slot {
                start: offered,
                end: offered + Duration::minutes(60),
            }]
        } else {
            vec![
                Slot {
                    start: offered,
                    end: offered + Duration::minutes(30),
                },
                Slot {
                    start: offered + Duration::minutes(if mode == 1 { 31 } else { 30 }),
                    end: offered + Duration::minutes(if mode == 1 { 61 } else { 60 }),
                },
            ]
        };
        Ok(Schedule {
            schedule_id: if host { "HostPage" } else { "GuestPage" }.into(),
            identity: PageIdentity {
                display_name: Some("Private Contact".into()),
                email: Some("private@example.com".into()),
            },
            title: "Actual meeting <script>".into(),
            timezone: "Europe/Paris".into(),
            duration_minutes: if host { 60 } else { 30 },
            window_start: start,
            window_end: end,
            slots,
        })
    }
}
async fn live_send(s: &Setup, host: &str, operation: Value) -> Value {
    reqwest::Client::new().post(format!("{}/a2a",s.base)).json(&json!({"jsonrpc":"2.0","id":"live","method":"SendMessage","params":{
        "tenant":host,"message":{"role":"ROLE_USER","messageId":"test","parts":[{"data":operation}]}
    }})).send().await.unwrap().json().await.unwrap()
}
fn reply_data(reply: &Value) -> &Value {
    reply["result"]["message"]["parts"]
        .as_array()
        .unwrap_or_else(|| panic!("{reply}"))
        .iter()
        .find_map(|p| p.get("data"))
        .unwrap()
}
#[tokio::test]
async fn live_agents_use_host_duration_and_full_peer_coverage_without_contact_leaks() {
    let reader = Arc::new(LiveReader::default());
    let s = setup_with_reader(false, Some(reader.clone())).await;
    let host = create(&s, HOST).await.1;
    let guest = create(&s, GUEST).await.1;
    assert_eq!(host["mock"], false);
    let id = host["id"].as_str().unwrap();
    let card: Value =
        serde_json::from_str(&s.store.get(id).await.unwrap().unwrap().card_bytes).unwrap();
    assert_eq!(card["version"], "0.4.0");
    let metadata = reqwest::get(format!("{}/agents/{id}/schedule", s.base))
        .await
        .unwrap();
    assert_eq!(metadata.headers()["cache-control"], "no-store");
    let metadata: Value = metadata.json().await.unwrap();
    assert_eq!(metadata["schedule"]["duration_minutes"], 60);
    assert!(!metadata.to_string().contains("private@example.com"));
    let operation = json!({"operation":"find_common_slot","peer":guest["identifier"]});
    let reply = live_send(&s, id, operation.clone()).await;
    let data = reply_data(&reply);
    assert_eq!(data["status"], "slot_found", "{reply}");
    assert_eq!(data["duration_minutes"], 60.0);
    assert_eq!(data["mock"], false);
    assert_eq!(data["reserved"], false);
    assert!(!reply.to_string().contains("Private Contact"));
    assert!(!reply.to_string().contains("private@example.com"));
    // A one-minute gap in the visitor coverage must reject the whole host slot.
    reader.0.store(1, Ordering::SeqCst);
    let no_slot = live_send(&s, id, operation.clone()).await;
    assert_eq!(reply_data(&no_slot)["status"], "no_common_slot");
    assert!(reply_data(&no_slot)["slot"].is_null());
    // A Google failure cannot fall back to the old 2030 interval.
    reader.0.store(2, Ordering::SeqCst);
    let failure = live_send(&s, id, operation.clone()).await;
    assert_eq!(reply_data(&failure)["status"], "error");
    assert_eq!(reply_data(&failure)["reserved"], false);
    reader.0.store(0, Ordering::SeqCst);
    let stale = live_send(
        &s,
        id,
        json!({"operation":"find_common_slot","peer":guest["identifier"],"duration_minutes":30}),
    )
    .await;
    assert_eq!(stale["error"]["code"], -32602);
    let unknown = live_send(
        &s,
        id,
        json!({"operation":"find_common_slot","peer":"urn:aithos:calendar:agent:missing"}),
    )
    .await;
    assert_eq!(reply_data(&unknown)["code"], "peer_not_found");
    let availability = live_send(&s, id, json!({"operation":"get_availability"})).await;
    assert_eq!(reply_data(&availability)["mock"], false);
    assert!(!availability.to_string().contains("private@example.com"));
}

#[tokio::test]
async fn legacy_mock_links_require_upgrade_and_cannot_supply_live_availability() {
    let s = setup(false).await;
    let agent = create(&s, HOST).await.1;
    let id = agent["id"].as_str().unwrap();
    let response = reqwest::get(format!("{}/agents/{id}/schedule", s.base))
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::CONFLICT);
    let reply = live_send(&s,id,json!({"operation":"get_availability","window":{"start":"2030-01-01T00:00:00Z","end":"2030-01-02T00:00:00Z"}})).await;
    assert_eq!(reply["error"]["code"], -32602);
}

#[tokio::test]
#[ignore = "requires two GOOGLE_BOOKING_TEST_* URLs; Google reads only, registry is local"]
async fn live_google_pages_collaborate_over_a2a_without_booking() {
    let host_url = std::env::var("GOOGLE_BOOKING_TEST_HOST").unwrap();
    let guest_url = std::env::var("GOOGLE_BOOKING_TEST_GUEST").unwrap();
    let s = setup_with_reader(
        false,
        Some(Arc::new(
            calendar::availability::GoogleHttpReader::new().unwrap(),
        )),
    )
    .await;
    let host = create(&s, &host_url).await.1;
    let guest = create(&s, &guest_url).await.1;
    for (caller, peer) in [(&host, &guest), (&guest, &host)] {
        let reply = live_send(
            &s,
            caller["id"].as_str().unwrap(),
            json!({"operation":"find_common_slot","peer":peer["identifier"]}),
        )
        .await;
        let data = reply_data(&reply);
        assert!(
            ["slot_found", "no_common_slot"].contains(&data["status"].as_str().unwrap()),
            "{reply}"
        );
        assert_eq!(data["mock"], false);
        assert_eq!(data["reserved"], false);
        println!(
            "Live Google → A2A → Google: {}; duration={} minutes",
            data["status"], data["duration_minutes"]
        );
    }
}
