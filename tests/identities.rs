use axum::{
    Json, Router,
    body::{Body, to_bytes},
    extract::Path,
    http::{Request, StatusCode},
    response::IntoResponse,
    routing::get,
};
use calendar::storage::{AgentStore, MemoryStore};
use lambda_http::{RequestExt, request::RequestContext};
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

const ACCOUNT: &str = "123456789012";
const OWNER: &str = "arn:aws:sts::123456789012:assumed-role/calendar-owner/session-one";
const ID1: &str = "22222222-2222-4222-8222-222222222222";
const ID2: &str = "33333333-3333-4333-8333-333333333333";

struct Setup {
    app: Router,
    base: String,
    store: Arc<MemoryStore>,
    writes: Arc<Mutex<HashMap<String, String>>>,
    calls: Arc<Mutex<Vec<Value>>>,
    fail: Arc<AtomicBool>,
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
    let app = calendar::app_with_store(
        &base,
        &format!("{base}/.well-known/ai-catalog.json"),
        store.clone(),
        Some(calendar::registry::Registry::new(&registry_origin).unwrap()),
        ACCOUNT.into(),
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
        fail,
        jobs: vec![job, registry_job],
    }
}
fn definition(name: &str, start: &str, end: &str) -> Value {
    json!({"name":name,"mock_availability":[{"start":format!("2030-01-15T{start}:00Z"),"end":format!("2030-01-15T{end}:00Z")}]})
}
async fn admin(
    setup: &Setup,
    method: &str,
    path: &str,
    body: Value,
    owner: Option<&str>,
) -> (StatusCode, Value) {
    let mut req = Request::builder()
        .method(method)
        .uri(path)
        .header("content-type", "application/json")
        .header("x-owner", OWNER)
        .header("authorization", "fake-iam-header")
        .body(Body::from(body.to_string()))
        .unwrap();
    if let Some(owner) = owner {
        let context=serde_json::from_value(json!({"accountId":ACCOUNT,"http":{"method":method,"path":path,"protocol":"HTTP/1.1","sourceIp":"127.0.0.1","userAgent":"test"},
            "authorizer":{"iam":{"accountId":ACCOUNT,"userArn":owner}},"routeKey":format!("{method} /admin/agents/{{id}}") })).unwrap();
        req = req.with_request_context(RequestContext::ApiGatewayV2(context));
    }
    let response = setup.app.clone().oneshot(req).await.unwrap();
    let status = response.status();
    let bytes = to_bytes(response.into_body(), 65536).await.unwrap();
    (
        status,
        serde_json::from_slice(&bytes).unwrap_or(Value::Null),
    )
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
async fn owner_authentication_conflicts_and_retry_after_lost_registry_response() {
    let s = setup(true).await;
    let path = format!("/admin/agents/{ID1}");
    let input = definition("Owner one", "09:00", "10:00");
    assert_eq!(
        admin(&s, "PUT", &path, input.clone(), None).await.0,
        StatusCode::FORBIDDEN
    );
    let (status, pending) = admin(&s, "PUT", &path, input.clone(), Some(OWNER)).await;
    assert_eq!(status, StatusCode::ACCEPTED, "{pending}");
    assert_eq!(pending["publication_status"], "pending");
    assert_eq!(catalog(&s).await["entries"], json!([]));
    assert_eq!(
        reqwest::get(format!("{}/agents/{ID1}/agent-card.json", s.base))
            .await
            .unwrap()
            .status(),
        StatusCode::NOT_FOUND
    );
    let thief = "arn:aws:sts::123456789012:assumed-role/another-owner/session";
    assert_eq!(
        admin(&s, "PUT", &path, input.clone(), Some(thief)).await.0,
        StatusCode::FORBIDDEN
    );
    assert_eq!(
        admin(&s, "GET", &path, Value::Null, Some(thief)).await.0,
        StatusCode::FORBIDDEN
    );
    assert_eq!(
        admin(
            &s,
            "POST",
            &format!("{path}/publish"),
            Value::Null,
            Some(thief)
        )
        .await
        .0,
        StatusCode::FORBIDDEN
    );
    let (status, published) = admin(
        &s,
        "POST",
        &format!("{path}/publish"),
        Value::Null,
        Some("arn:aws:sts::123456789012:assumed-role/calendar-owner/session-two"),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{published}");
    assert_eq!(published["registry_id"], pending["registry_id"]);
    assert_eq!(s.writes.lock().unwrap().len(), 1);
    let calls = s.calls.lock().unwrap();
    assert_eq!(calls.len(), 2);
    assert_eq!(calls[0], calls[1]);
    drop(calls);
    assert_eq!(
        admin(&s, "PUT", &path, input, Some(OWNER)).await.0,
        StatusCode::OK
    );
    assert_eq!(
        s.calls.lock().unwrap().len(),
        2,
        "already published retry avoids another write"
    );
    assert_eq!(
        admin(
            &s,
            "PUT",
            &path,
            definition("Different", "09:00", "10:00"),
            Some(OWNER)
        )
        .await
        .0,
        StatusCode::CONFLICT
    );
    assert!(published.get("signing_key").is_none() && published.get("publication").is_none());
    assert_eq!(catalog(&s).await["entries"].as_array().unwrap().len(), 1);
}

#[tokio::test]
async fn dynamic_agents_discover_registry_cards_and_collaborate_without_fixtures() {
    let s = setup(false).await;
    for (id, name, start, end) in [
        (ID1, "Dynamic host", "09:00", "10:00"),
        (ID2, "Dynamic guest", "09:30", "10:30"),
    ] {
        let (status, body) = admin(
            &s,
            "PUT",
            &format!("/admin/agents/{id}"),
            definition(name, start, end),
            Some(OWNER),
        )
        .await;
        assert_eq!(status, StatusCode::OK, "{body}");
        let record = s.store.get(id).await.unwrap().unwrap();
        let local = reqwest::get(format!("{}/agents/{id}/agent-card.json", s.base))
            .await
            .unwrap()
            .text()
            .await
            .unwrap();
        assert_eq!(local, record.card_bytes);
    }
    let catalog = catalog(&s).await;
    let typed: ai_catalog::AiCatalog = serde_json::from_value(catalog).unwrap();
    assert_eq!(typed.entries.len(), 2);
    for entry in typed.entries {
        assert!(!entry.url.unwrap().starts_with(&s.base));
    }
    for (caller, peer) in [(ID1, ID2), (ID2, ID1)] {
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
    assert_eq!(
        reqwest::get(format!("{}/agents/alice/agent-card.json", s.base))
            .await
            .unwrap()
            .status(),
        StatusCode::NOT_FOUND
    );
}

#[tokio::test]
async fn concurrent_creation_has_one_identity_and_invalid_definitions_do_not_write() {
    let s = setup(false).await;
    let path = format!("/admin/agents/{ID1}");
    let input = definition("Concurrent", "09:00", "10:00");
    let (first, second) = futures::join!(
        admin(&s, "PUT", &path, input.clone(), Some(OWNER)),
        admin(&s, "PUT", &path, input, Some(OWNER))
    );
    assert_eq!(first.0, StatusCode::OK, "{}", first.1);
    assert_eq!(second.0, StatusCode::OK, "{}", second.1);
    assert_eq!(first.1["registry_id"], second.1["registry_id"]);
    assert_eq!(s.writes.lock().unwrap().len(), 1);
    for input in [
        definition("Bad", "10:00", "09:00"),
        json!({"name":"","mock_availability":[]}),
        json!({"name":"Bad","mock_availability":[],"owner":"someone"}),
    ] {
        assert!(
            !admin(
                &s,
                "PUT",
                &format!("/admin/agents/{ID2}"),
                input,
                Some(OWNER)
            )
            .await
            .0
            .is_success()
        );
    }
    assert!(s.store.get(ID2).await.unwrap().is_none());
    assert!(!s.fail.load(Ordering::SeqCst));
}
