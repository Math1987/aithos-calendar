use axum::{
    Json, Router,
    body::{Body, to_bytes},
    extract::{Path, Request},
    http::StatusCode,
    middleware::{self, Next},
    routing::{get, post},
};
use serde_json::{Value, json};
use std::sync::{Arc, Mutex};
use tokio::task::JoinHandle;

mod common;

struct Server {
    base: String,
    requests: Arc<Mutex<Vec<Value>>>,
    job: JoinHandle<()>,
}
impl Drop for Server {
    fn drop(&mut self) {
        self.job.abort();
    }
}

async fn start(mode: &'static str) -> Server {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base = format!("http://{}", listener.local_addr().unwrap());
    let fixture = common::Fixture::new(
        &base,
        vec![
            common::mock_agent("alice", &[("09:00", "10:00"), ("14:00", "15:00")]),
            common::mock_agent("bob", &[("09:30", "10:30"), ("15:00", "16:00")]),
        ],
    )
    .await;
    let endpoint = match mode {
        "unavailable" | "slow" | "invalid" | "bad_slots" => format!("{base}/peer-fixture"),
        "external" => "http://169.254.169.254/latest/meta-data".to_owned(),
        _ => format!("{base}/a2a"),
    };
    // Custom card URLs, signed by each agent's own key, listed in a signed
    // catalog served from a path of the fixture router.
    let mut cards = std::collections::HashMap::new();
    let mut entries = Vec::new();
    for id in ["alice", "bob"] {
        let bytes = fixture
            .sign_card(
                id,
                json!({
                    "name":id, "description":"Test card", "version":"1.0",
                    "supportedInterfaces":[{"url":endpoint, "protocolBinding":"JSONRPC", "protocolVersion":"1.0", "tenant":id}],
                    "capabilities":{}, "defaultInputModes":["application/json"], "defaultOutputModes":["application/json"], "skills":[]
                }),
            )
            .await;
        let url = format!("{base}/custom-card/{id}?revision=1");
        entries.push((fixture.urn(id), url, bytes.clone()));
        cards.insert(id.to_owned(), bytes);
    }
    let catalog = if mode == "absent" {
        fixture.catalog(&[]).await
    } else {
        fixture.catalog(&entries).await
    };
    let requests = Arc::new(Mutex::new(Vec::new()));
    let captured = requests.clone();
    let cards = Arc::new(cards);
    let fixture_routes = Router::new()
        .route("/registry/catalog", get(move || async move { Json(catalog.clone()) }))
        .route("/custom-card/{id}", get(move |Path(id): Path<String>| {
            let cards = cards.clone();
            async move { ([("content-type", "application/json")], cards[&id].clone()) }
        }))
        .route("/peer-fixture", post(move |Json(request): Json<Value>| async move {
            if mode == "slow" { tokio::time::sleep(std::time::Duration::from_secs(5)).await; }
            let reply = if mode == "bad_slots" {
                json!({"jsonrpc":"2.0", "id":request["id"], "result":{"message":{
                    "messageId":"fixture", "role":"ROLE_AGENT", "parts":[{"data":{
                        "status":"availability", "agent":format!("urn:air:127.0.0.1:agent:bob"), "mock":true,
                        "trace_id":request["params"]["metadata"]["calendarTraceId"],
                        "slots":[{"start":"2030-01-15T11:00:00Z", "end":"2030-01-15T10:00:00Z"}]
                    }}]
                }}})
            } else { json!({"unexpected":true}) };
            (if mode == "unavailable" { StatusCode::SERVICE_UNAVAILABLE } else { StatusCode::OK }, Json(reply))
        }));
    let config = fixture
        .config()
        .with_catalog(&format!("{base}/registry/catalog"));
    let app = calendar::build(config)
        .unwrap()
        .merge(fixture_routes)
        .layer(middleware::from_fn(move |request: Request, next: Next| {
            let captured = captured.clone();
            async move {
                let declared: u64 = request
                    .headers()
                    .get("content-length")
                    .and_then(|v| v.to_str().ok()?.parse().ok())
                    .unwrap_or(0);
                // Oversized bodies are the application's business (its cap).
                if request.uri().path() != "/a2a" || declared > calendar::MAX_A2A_BODY {
                    return next.run(request).await;
                }
                let (parts, body) = request.into_parts();
                let bytes = to_bytes(body, calendar::MAX_A2A_BODY as usize)
                    .await
                    .unwrap();
                if let Ok(value) = serde_json::from_slice::<Value>(&bytes) {
                    captured.lock().unwrap().push(value);
                }
                next.run(Request::from_parts(parts, Body::from(bytes)))
                    .await
            }
        }));
    let job = tokio::spawn(async move {
        axum::serve(listener, app).await.unwrap();
    });
    Server {
        base,
        requests,
        job,
    }
}

async fn send(server: &Server, tenant: &str, operation: Value) -> Value {
    reqwest::Client::new()
        .post(format!("{}/a2a", server.base))
        .json(&json!({
            "jsonrpc":"2.0", "id":"integration", "method":"SendMessage",
            "params":{"tenant":tenant, "message":{"role":"ROLE_USER", "messageId":"incoming",
                "parts":[{"data":operation}]}}
        }))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap()
}
fn operation(peer: &str, duration: u16) -> Value {
    json!({"operation":"find_common_slot", "peer":format!("urn:air:127.0.0.1:agent:{peer}"), "duration_minutes":duration})
}
fn data(reply: &Value) -> &Value {
    reply["result"]["message"]["parts"]
        .as_array()
        .unwrap_or_else(|| panic!("{reply}"))
        .iter()
        .find_map(|part| part.get("data"))
        .unwrap()
}

#[tokio::test]
async fn discovers_custom_card_urls_and_calls_both_directions_through_sdk() {
    let server = start("normal").await;
    for (caller, peer) in [("alice", "bob"), ("bob", "alice")] {
        let result = send(&server, caller, operation(peer, 30)).await;
        let result = data(&result);
        assert_eq!(result["status"], "slot_found");
        assert_eq!(
            result["slot"],
            json!({"start":"2030-01-15T09:30:00Z", "end":"2030-01-15T10:00:00Z"})
        );
        assert_eq!(result["reserved"], false);
        let requests = server.requests.lock().unwrap();
        let leaf = requests.last().unwrap();
        assert_eq!(leaf["params"]["tenant"], peer);
        assert_eq!(
            leaf["params"]["message"]["parts"][0]["data"]["operation"],
            "get_availability"
        );
        assert_eq!(
            leaf["params"]["metadata"]["calendarTraceId"],
            result["trace_id"]
        );
    }
    // Exactly one incoming coordination and one outgoing availability request per exchange.
    assert_eq!(server.requests.lock().unwrap().len(), 4);
}

#[tokio::test]
async fn no_overlap_is_a_valid_result_not_a_booking() {
    let server = start("normal").await;
    let result = send(&server, "alice", operation("bob", 60)).await;
    assert_eq!(data(&result)["status"], "no_common_slot");
    assert_eq!(data(&result)["slot"], Value::Null);
    assert_eq!(data(&result)["reserved"], false);
}

#[tokio::test]
async fn missing_peer_does_not_fall_back_or_call_an_agent() {
    let server = start("absent").await;
    let result = send(&server, "alice", operation("bob", 30)).await;
    assert_eq!(data(&result)["code"], "peer_not_found");
    assert_eq!(server.requests.lock().unwrap().len(), 1);
}

#[tokio::test]
async fn unavailable_or_invalid_peer_is_reported_without_success() {
    for mode in ["unavailable", "invalid"] {
        let server = start(mode).await;
        let result = send(&server, "alice", operation("bob", 30)).await;
        assert_eq!(data(&result)["status"], "error");
        assert_eq!(data(&result)["reserved"], false);
    }
}

#[tokio::test]
async fn external_endpoint_is_rejected_before_any_call() {
    let server = start("external").await;
    let result = send(&server, "alice", operation("bob", 30)).await;
    assert_eq!(data(&result)["code"], "invalid_peer_card");
    assert_eq!(server.requests.lock().unwrap().len(), 1);
}

#[tokio::test]
async fn peer_timeout_is_bounded() {
    let server = start("slow").await;
    let start = std::time::Instant::now();
    let result = send(&server, "alice", operation("bob", 30)).await;
    assert_eq!(data(&result)["code"], "timeout");
    assert!(start.elapsed() < std::time::Duration::from_secs(4));
}

#[tokio::test]
async fn invalid_duration_self_request_and_unknown_operation_are_rejected() {
    let server = start("normal").await;
    for input in [
        operation("alice", 30),
        operation("bob", 0),
        operation("bob", 481),
        json!({"operation":"book"}),
        json!({"operation":"find_common_slot", "peer":"urn:air:127.0.0.1:agent:bob", "duration_minutes":30.5}),
    ] {
        let reply = send(&server, "alice", input).await;
        assert_eq!(reply["error"]["code"], -32602, "{reply}");
    }
    assert_eq!(server.requests.lock().unwrap().len(), 5);
}

#[tokio::test]
async fn invalid_peer_intervals_cannot_produce_a_common_slot() {
    let server = start("bad_slots").await;
    let result = send(&server, "alice", operation("bob", 30)).await;
    assert_eq!(data(&result)["status"], "error");
    assert_eq!(data(&result)["code"], "invalid_peer_response");
}

/// The SDK router would accept 10 MiB; a message here is a few KiB, so an
/// oversized or length-less POST is refused before it is read.
#[tokio::test]
async fn oversized_a2a_bodies_are_refused_before_parsing() {
    let server = start("normal").await;
    let client = reqwest::Client::new();
    let big = format!(
        r#"{{"jsonrpc":"2.0","id":"big","method":"SendMessage","params":{{"tenant":"alice","message":{{"messageId":"m","role":"ROLE_USER","parts":[{{"text":"{}"}}]}}}}}}"#,
        "a".repeat(calendar::MAX_A2A_BODY as usize)
    );
    let response = client
        .post(format!("{}/a2a", server.base))
        .header("content-type", "application/json")
        .body(big)
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), 413);
    let small = send(&server, "alice", operation("bob", 30)).await;
    assert_eq!(data(&small)["status"], "slot_found", "{small}");
}
