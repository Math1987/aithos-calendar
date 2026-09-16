//! Exercise the real binary's subscriber and async context, not a test-only logger.
use serde_json::{Value, json};
use std::{
    fs::{self, OpenOptions},
    path::PathBuf,
    process::{Child, Command, Stdio},
    time::Duration,
};

struct Server {
    child: Child,
    log: PathBuf,
    base: String,
}
impl Drop for Server {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
        let _ = fs::remove_file(&self.log);
    }
}

async fn start() -> Server {
    let socket = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let address = socket.local_addr().unwrap();
    drop(socket);
    let log = std::env::temp_dir().join(format!("calendar-logs-{}.jsonl", a2a::new_message_id()));
    let output = OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&log)
        .unwrap();
    let child = Command::new(env!("CARGO_BIN_EXE_calendar"))
        .env("CALENDAR_LISTEN", address.to_string())
        .env("CALENDAR_PUBLIC_URL", format!("http://{address}"))
        .env(
            "CATALOG_URL",
            format!("http://{address}/.well-known/ai-catalog.json"),
        )
        .env_remove("RUST_LOG")
        .stdout(Stdio::null())
        .stderr(output)
        .spawn()
        .unwrap();
    let mut server = Server {
        child,
        log,
        base: format!("http://{address}"),
    };
    let client = reqwest::Client::new();
    tokio::time::timeout(Duration::from_secs(10), async {
        loop {
            assert!(
                server.child.try_wait().unwrap().is_none(),
                "{}",
                fs::read_to_string(&server.log).unwrap()
            );
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

async fn send(server: &Server, tenant: &str, trace: &str, parts: Value) -> Value {
    reqwest::Client::new()
        .post(format!("{}/a2a", server.base))
        .header("authorization", "Bearer header-must-not-be-logged")
        .timeout(Duration::from_secs(15))
        .json(
            &json!({"jsonrpc":"2.0", "id":"log-test", "method":"SendMessage", "params":{
                "tenant":tenant, "metadata":{"calendarTraceId":trace},
                "message":{"role":"ROLE_USER", "messageId":a2a::new_message_id(), "parts":parts}
            }}),
        )
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap()
}

#[tokio::test]
async fn json_logs_distinguish_sdk_and_app_without_mixing_concurrent_traces() {
    let server = start().await;
    let trace_a = a2a::new_message_id();
    let trace_b = a2a::new_message_id();
    let operation = |peer: &str| {
        json!([{"data":{
            "operation":"find_common_slot", "peer":format!("urn:aithos:calendar:agent:{peer}"), "duration_minutes":30
        }}])
    };
    let (alice, bob) = futures::join!(
        send(&server, "alice", &trace_a, operation("bob")),
        send(&server, "bob", &trace_b, operation("alice")),
    );
    for response in [&alice, &bob] {
        assert!(response.get("error").is_none(), "{response}");
    }
    let unknown_trace = a2a::new_message_id();
    let unknown = send(
        &server,
        "unknown",
        &unknown_trace,
        json!([{"text":"body-must-not-be-logged"}]),
    )
    .await;
    assert!(unknown.get("error").is_some());
    let greeting_trace = a2a::new_message_id();
    let greeting = send(
        &server,
        "alice",
        &greeting_trace,
        json!([{"text":"body-must-not-be-logged"}]),
    )
    .await;
    assert!(greeting.get("error").is_none());

    let raw = fs::read_to_string(&server.log).unwrap();
    assert!(!raw.contains("header-must-not-be-logged"));
    assert!(!raw.contains("body-must-not-be-logged"));
    let logs: Vec<Value> = raw
        .lines()
        .map(|line| serde_json::from_str(line).expect("each log is JSON"))
        .collect();
    for (trace, caller, peer) in [(&trace_a, "alice", "bob"), (&trace_b, "bob", "alice")] {
        let events: Vec<_> = logs
            .iter()
            .filter(|log| log["span"]["trace_id"] == *trace)
            .collect();
        for message in ["A2A client request", "A2A client response"] {
            let event = events
                .iter()
                .find(|e| e["message"] == message)
                .expect("SDK client event");
            assert_eq!(event["target"], "a2a_client::middleware");
            assert_eq!(event["span"]["tenant"], caller);
            assert_eq!(event["method"], "SendMessage");
        }
        for tenant in [caller, peer] {
            for message in ["A2A server request", "A2A server response"] {
                assert!(
                    events
                        .iter()
                        .any(|e| e["target"] == "a2a_server::middleware"
                            && e["message"] == message
                            && e["span"]["tenant"] == tenant)
                );
            }
        }
        let completed = events
            .iter()
            .find(|e| e["event"] == "negotiation_completed")
            .expect("application outcome");
        assert_eq!(completed["target"], "calendar::a2a");
        assert_eq!(completed["status"], "slot_found");
        assert_eq!(completed["span"]["tenant"], caller);
        let peer_call = events.iter().find(|e| e["event"] == "peer_call").unwrap();
        assert_eq!(peer_call["target"], "calendar::discovery");
        assert_eq!(peer_call["recipient_tenant"], peer);
    }
    assert!(logs.iter().any(|e| e["span"]["trace_id"] == unknown_trace
        && e["target"] == "a2a_server::middleware"
        && e["level"] == "WARN"
        && e["message"] == "A2A server error"));
    assert!(
        logs.iter()
            .any(|e| e["span"]["trace_id"] == greeting_trace
                && e["message"] == "A2A server response")
    );
}
