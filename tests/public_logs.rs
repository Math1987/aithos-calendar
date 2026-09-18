//! The public log feed, exercised on the real binary: the trust chain and
//! SDK events of an exchange appear under their trace, and sentinel
//! secrets or personal data sent with the requests never do.
use serde_json::{Value, json};
use std::{
    process::{Child, Command, Stdio},
    time::Duration,
};

struct Server {
    child: Child,
    base: String,
}
impl Drop for Server {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

async fn start() -> Server {
    let socket = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let address = socket.local_addr().unwrap();
    drop(socket);
    let child = Command::new(env!("CARGO_BIN_EXE_calendar"))
        .env("CALENDAR_LISTEN", address.to_string())
        .env("CALENDAR_PUBLIC_URL", format!("http://{address}"))
        .env("LAB_KEY_SEED", "public-logs-test")
        .env_remove("RUST_LOG")
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .unwrap();
    let mut server = Server {
        child,
        base: format!("http://{address}"),
    };
    let client = reqwest::Client::new();
    tokio::time::timeout(Duration::from_secs(10), async {
        loop {
            assert!(server.child.try_wait().unwrap().is_none());
            if client
                .get(format!("{}/health", server.base))
                .send()
                .await
                .is_ok()
            {
                break;
            }
            tokio::time::sleep(Duration::from_millis(25)).await;
        }
    })
    .await
    .expect("local binary starts");
    server
}

async fn feed(server: &Server, query: &str) -> Value {
    reqwest::get(format!("{}/logs/events?{query}", server.base))
        .await
        .unwrap()
        .json()
        .await
        .unwrap()
}

#[tokio::test]
async fn feed_shows_the_trust_chain_per_trace_and_redacts_everything_else() {
    let server = start().await;
    let trace = a2a::new_message_id();
    let sentinel_header = "Bearer header-must-not-be-logged";
    let reply: Value = reqwest::Client::new()
        .post(format!("{}/a2a", server.base))
        .header("authorization", sentinel_header)
        .json(&json!({"jsonrpc":"2.0", "id":"feed", "method":"SendMessage", "params":{
            "tenant":"alice", "metadata":{"calendarTraceId":trace, "email":"private@example.com", "note":"body-must-not-be-logged"},
            "message":{"role":"ROLE_USER", "messageId":a2a::new_message_id(), "parts":[{"data":{
                "operation":"find_common_slot", "peer":"urn:air:127.0.0.1:agent:bob", "duration_minutes":30
            }}]}
        }}))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert!(reply.get("error").is_none(), "{reply}");
    // The lab report also logs, under its own trace.
    let report: Value = reqwest::get(format!("{}/lab/report?policy=guaranteed", server.base))
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    let lab_trace = report["trace_id"].as_str().unwrap();

    let all = feed(&server, "limit=200").await;
    let text = all.to_string();
    for forbidden in [
        "header-must-not-be-logged",
        "body-must-not-be-logged",
        "private@example.com",
        "@",
        "Bearer",
        "Alice",
        "2030-01-15",
    ] {
        assert!(!text.contains(forbidden), "{forbidden} leaked: {text}");
    }
    assert_eq!(
        all["sources"],
        json!(["a2a-sdk", "ai-catalog", "trust", "app"])
    );

    let exchange = feed(&server, &format!("trace={trace}")).await;
    let events = exchange["events"].as_array().unwrap();
    assert!(!events.is_empty(), "{exchange}");
    assert!(events.iter().all(|e| e["trace_id"] == trace));
    let has = |event: &str| events.iter().any(|e| e["event"] == event);
    for step in [
        "operation_received",
        "catalog_fetched",
        "catalog_signature_verified",
        "card_digest_verified",
        "card_signature_verified",
        "peer_verified",
        "peer_call",
        "negotiation_completed",
    ] {
        assert!(has(step), "{step} missing from {exchange}");
    }
    let sources: std::collections::BTreeSet<&str> =
        events.iter().filter_map(|e| e["source"].as_str()).collect();
    assert!(
        sources.contains("trust") && sources.contains("app") && sources.contains("a2a-sdk"),
        "{sources:?}"
    );
    assert!(events.iter().any(|e| e["source"] == "a2a-sdk"
        && e["message"] == "A2A server request"
        && e["method"] == "SendMessage"));
    let verified = events
        .iter()
        .find(|e| e["event"] == "peer_verified")
        .unwrap();
    assert_eq!(verified["policy"], "integrity");
    assert_eq!(verified["tenant"], "alice");
    assert!(
        verified["card_digest"]
            .as_str()
            .unwrap()
            .starts_with("sha256:")
    );
    // Timestamps are ordered and the window is bounded.
    let stamps: Vec<&str> = events
        .iter()
        .map(|e| e["timestamp"].as_str().unwrap())
        .collect();
    assert!(stamps.windows(2).all(|w| w[0] <= w[1]));
    let since = feed(
        &server,
        &format!("trace={trace}&since={}", stamps[stamps.len() - 1]),
    )
    .await;
    assert!(since["events"].as_array().unwrap().is_empty());

    let lab = feed(&server, &format!("trace={lab_trace}&source=trust")).await;
    let lab_events = lab["events"].as_array().unwrap();
    assert!(lab_events.iter().all(|e| e["source"] == "trust"));
    for scenario in ["tampered-catalog", "expired-manifest", "unknown-guarantor"] {
        assert!(
            lab_events
                .iter()
                .any(|e| e["scenario"] == scenario && e["event"] == "peer_rejected"),
            "{scenario}: {lab}"
        );
    }
    assert!(
        lab_events
            .iter()
            .any(|e| e["event"] == "caller_rejected" && e["code"] == "caller_key_mismatch")
    );

    let trust_only = feed(&server, "source=trust&limit=5").await;
    assert_eq!(trust_only["events"].as_array().unwrap().len(), 5);
    assert_eq!(
        reqwest::get(format!("{}/logs/events?source=cloudwatch", server.base))
            .await
            .unwrap()
            .status(),
        400
    );
}
