use axum::{
    Router,
    body::{Body, to_bytes},
    http::{Request, StatusCode},
};
use calendar::{
    storage::{AgentStore, MemoryStore},
    trust::TrustProvider,
};
use serde_json::{Value, json};
use std::sync::{
    Arc, Mutex,
    atomic::{AtomicBool, Ordering},
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
    trust: Arc<dyn TrustProvider>,
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
async fn setup() -> Setup {
    setup_with_reader(None).await
}
async fn setup_with_reader(
    reader: Option<Arc<dyn calendar::availability::AvailabilityReader>>,
) -> Setup {
    let socket = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base = format!("http://{}", socket.local_addr().unwrap());
    let store = Arc::new(MemoryStore::default());
    let trust: Arc<dyn TrustProvider> = Arc::new(calendar::trust::LocalTrust::ephemeral(&base));
    let pages = Arc::new(TestPages::default());
    let app = calendar::build(
        calendar::Config::new(&base, trust.clone(), store.clone())
            .with_pages(pages.clone())
            .with_reader(reader),
    )
    .unwrap();
    let network = app.clone();
    let job = tokio::spawn(async move { axum::serve(socket, network).await.unwrap() });
    Setup {
        app,
        base,
        store,
        trust,
        pages,
        jobs: vec![job],
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
async fn public_creation_publishes_a_signed_card_and_a_trusted_catalog_entry() {
    let s = setup().await;
    let (status, published) = create(&s, "https://calendar.app.google/host").await;
    assert_eq!(status, StatusCode::OK, "{published}");
    assert_eq!(published["publication_status"], "published");
    assert_eq!(published["booking_page_url"], HOST);
    let id = published["id"].as_str().unwrap();
    assert_eq!(
        published["identifier"],
        format!("urn:air:127.0.0.1:agent:{id}")
    );
    assert_eq!(
        published["agent_card_url"],
        format!("{}/agents/{id}/agent-card.json", s.base)
    );
    // The served card is byte-exact, signed by the key served at its jku,
    // and bound by the catalog entry's manifest.
    let served = reqwest::get(published["agent_card_url"].as_str().unwrap())
        .await
        .unwrap();
    assert_eq!(
        served.headers()["etag"].to_str().unwrap().trim_matches('"'),
        &published["card_digest"].as_str().unwrap()[7..]
    );
    let bytes = served.bytes().await.unwrap();
    let record = s.store.get(id).await.unwrap().unwrap();
    assert_eq!(bytes.as_ref(), record.card_bytes.as_bytes());
    assert_eq!(calendar::trust::jose::digest(&bytes), record.card_digest);
    let jwks: Value = reqwest::get(format!("{}/agents/{id}/jwks.json", s.base))
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    let keys = calendar::trust::jose::Jwks::parse(&jwks).unwrap();
    let card: Value = serde_json::from_slice(&bytes).unwrap();
    calendar::trust::card::verify(&card, &keys).unwrap();
    let catalog_value = catalog(&s).await;
    let entry = &catalog_value["entries"][0];
    assert_eq!(entry["identifier"], published["identifier"]);
    assert_eq!(entry["url"], published["agent_card_url"]);
    assert_eq!(
        entry["trustManifest"]["subject"]["digest"],
        record.card_digest
    );
    assert_eq!(entry["trustManifest"]["identity"], s.trust.identity());
    let typed: ai_catalog::AiCatalog = serde_json::from_value(catalog_value.clone()).unwrap();
    let validation = ai_catalog_validate::validate(&typed);
    assert!(validation.is_valid, "{:?}", validation.errors);
    assert_eq!(
        validation.conformance_level,
        ai_catalog_validate::ConformanceLevel::Trusted
    );
    let report = ai_catalog_trust::analyze_catalog(&typed);
    assert!(
        report
            .findings
            .iter()
            .all(|f| f.severity != ai_catalog_trust::Severity::Error),
        "{:?}",
        report.findings
    );
    assert!(ai_catalog_trust::verify_digest(&record.card_digest, &bytes).unwrap());
    // Operator key set (catalog signature) and guarantor key set (manifests)
    // are two different documents on two different paths.
    let operator_identity = catalog_value["host"]["identifier"].as_str().unwrap();
    assert_eq!(
        operator_identity,
        format!("{}/.well-known/jwks.json", s.base)
    );
    assert_eq!(
        s.trust.identity(),
        format!("{}/trust-provider/.well-known/jwks.json", s.base)
    );
    let operator: Value = reqwest::get(operator_identity)
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    let operator_keys = calendar::trust::jose::Jwks::parse(&operator).unwrap();
    calendar::trust::verify::verify_catalog(&catalog_value, &operator_keys).unwrap();
    let guarantor: Value = reqwest::get(s.trust.identity())
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    let guarantor_keys = calendar::trust::jose::Jwks::parse(&guarantor).unwrap();
    assert_ne!(operator, guarantor);
    calendar::trust::verify::verify_manifest(
        &entry["trustManifest"],
        &guarantor_keys,
        s.trust.identity(),
        "application/a2a-agent-card+json",
        entry["url"].as_str().unwrap(),
        chrono::Utc::now(),
    )
    .unwrap();
    assert!(
        calendar::trust::verify::verify_manifest(
            &entry["trustManifest"],
            &operator_keys,
            s.trust.identity(),
            "application/a2a-agent-card+json",
            entry["url"].as_str().unwrap(),
            chrono::Utc::now(),
        )
        .is_err(),
        "the operator key must not verify a guarantor manifest"
    );
    // Existing records must not be revalidated or regenerated during a Google outage.
    s.pages.reject.store(true, Ordering::SeqCst);
    let (status, again) = create(&s, &format!("{HOST}?gv=true#fragment")).await;
    assert_eq!(status, StatusCode::OK, "{again}");
    assert_eq!(again, published);
    assert_eq!(s.pages.validations.load(Ordering::SeqCst), 1);
    assert_eq!(create(&s, HOST).await.1, published);
    assert_eq!(catalog(&s).await["entries"].as_array().unwrap().len(), 1);
    assert!(published.get("signing_key").is_none());
    assert!(published.get("owner").is_none());
    assert!(!published.to_string().contains("aithos"));
}

#[tokio::test]
async fn dynamic_page_agents_discover_local_cards_and_collaborate() {
    let s = setup().await;
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
        assert!(entry.url.unwrap().starts_with(&s.base));
        assert!(entry.trust_manifest.unwrap().signature.is_some());
    }
    for (caller, peer) in [(&ids[0], &ids[1]), (&ids[1], &ids[0])] {
        let reply:Value=reqwest::Client::new().post(format!("{}/a2a",s.base)).json(&json!({"jsonrpc":"2.0","id":"dynamic","method":"SendMessage","params":{
            "tenant":caller,"message":{"role":"ROLE_USER","messageId":"test","parts":[{"data":{"operation":"find_common_slot","peer":format!("urn:air:127.0.0.1:agent:{peer}"),"duration_minutes":30}}]}
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
    let s = setup().await;
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
    let published = s.store.published().await.unwrap();
    assert_eq!(published.len(), 1);
    assert_eq!(published[0].card_digest, first.1["card_digest"]);
}

#[tokio::test]
async fn rejected_input_never_creates_records_and_cannot_override_existing_agent() {
    let s = setup().await;
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
    let s = setup_with_reader(Some(reader.clone())).await;
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
        json!({"operation":"find_common_slot","peer":"urn:air:127.0.0.1:agent:missing"}),
    )
    .await;
    assert_eq!(reply_data(&unknown)["code"], "peer_not_found");
    let availability = live_send(&s, id, json!({"operation":"get_availability"})).await;
    assert_eq!(reply_data(&availability)["mock"], false);
    assert!(!availability.to_string().contains("private@example.com"));
}

#[tokio::test]
async fn legacy_mock_links_require_upgrade_and_cannot_supply_live_availability() {
    let s = setup().await;
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
#[ignore = "requires two GOOGLE_BOOKING_TEST_* URLs; Google reads only"]
async fn live_google_pages_collaborate_over_a2a_without_booking() {
    let host_url = std::env::var("GOOGLE_BOOKING_TEST_HOST").unwrap();
    let guest_url = std::env::var("GOOGLE_BOOKING_TEST_GUEST").unwrap();
    let s = setup_with_reader(Some(Arc::new(
        calendar::availability::GoogleHttpReader::new().unwrap(),
    )))
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
