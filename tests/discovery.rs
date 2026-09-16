use axum::{
    body::{Body, to_bytes},
    http::{Request, StatusCode},
};
use serde_json::{Value, json};
use tower::ServiceExt;

async fn request(path: &str, payload: Option<Value>) -> (StatusCode, Value) {
    let builder = Request::builder().uri(path);
    let req = match payload {
        Some(value) => builder
            .method("POST")
            .header("content-type", "application/json")
            .body(Body::from(value.to_string()))
            .unwrap(),
        None => builder.body(Body::empty()).unwrap(),
    };
    let response = calendar::app("https://calendar.test")
        .oneshot(req)
        .await
        .unwrap();
    let status = response.status();
    let body = to_bytes(response.into_body(), 1024 * 1024).await.unwrap();
    (status, serde_json::from_slice(&body).unwrap_or(Value::Null))
}

fn message(tenant: Value) -> Value {
    let mut request = json!({"jsonrpc":"2.0", "id":"test-1", "method":"SendMessage", "params": {
        "message": {"messageId":"test-message", "role":"ROLE_USER", "parts":[{"text":"Hello"}]}
    }});
    if !tenant.is_null() {
        request["params"]["tenant"] = tenant;
    }
    request
}

#[tokio::test]
async fn catalog_to_card_to_message_uses_recipient_tenant() {
    let (status, catalog) = request("/.well-known/ai-catalog.json", None).await;
    assert_eq!(status, StatusCode::OK);
    let catalog: ai_catalog::AiCatalog = serde_json::from_value(catalog).unwrap();
    assert_eq!(catalog.entries.len(), 2);
    for entry in catalog.entries {
        let path = entry
            .url
            .unwrap()
            .strip_prefix("https://calendar.test")
            .unwrap()
            .to_owned();
        let (status, card) = request(&path, None).await;
        assert_eq!(status, StatusCode::OK);
        let card: a2a::AgentCard = serde_json::from_value(card).unwrap();
        let interface = &card.supported_interfaces[0];
        assert_eq!(interface.url, "https://calendar.test/a2a");
        assert_eq!(interface.protocol_binding, "JSONRPC");
        assert_eq!(interface.protocol_version, "1.0");
        assert_eq!(card.capabilities.streaming, Some(false));
        let (status, reply) = request("/a2a", Some(message(json!(interface.tenant)))).await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(reply["id"], "test-1");
        assert_eq!(
            reply["result"]["message"]["parts"][0]["text"],
            format!("Hello from {}", card.name)
        );
        assert_eq!(reply["result"]["message"]["role"], "ROLE_AGENT");
        assert!(reply["result"].get("task").is_none());
    }
}

#[tokio::test]
async fn missing_unknown_and_case_mismatched_tenants_never_default() {
    for tenant in [Value::Null, json!(""), json!("unknown"), json!("Alice")] {
        let (_, reply) = request("/a2a", Some(message(tenant))).await;
        assert_eq!(reply["error"]["code"], -32602, "{reply}");
        assert!(reply.get("result").is_none());
    }
    assert_eq!(
        request("/agents/unknown/agent-card.json", None).await.0,
        StatusCode::NOT_FOUND
    );
}

#[tokio::test]
async fn unsupported_stateful_and_streaming_operations_are_rejected() {
    for method in ["SendStreamingMessage", "ListTasks", "GetExtendedAgentCard"] {
        let mut payload = message(json!("alice"));
        payload["method"] = json!(method);
        let (_, reply) = request("/a2a", Some(payload)).await;
        assert!(reply.get("error").is_some(), "{reply}");
        assert!(reply.get("result").is_none());
    }
    let mut payload = message(json!("bob"));
    payload["params"]["message"]["taskId"] = json!("another-agent-task");
    let (_, reply) = request("/a2a", Some(payload)).await;
    assert!(reply.get("error").is_some());
}

#[tokio::test]
async fn malformed_or_nontext_message_is_not_executed() {
    let mut payload = message(json!("alice"));
    payload["params"]["message"]["parts"] = json!([]);
    let (_, reply) = request("/a2a", Some(payload)).await;
    assert_eq!(reply["error"]["code"], -32602);
    let mut payload = message(json!("bob"));
    payload["params"]["message"]["parts"] = json!([{"url":"https://example.com/private-file"}]);
    let (_, reply) = request("/a2a", Some(payload)).await;
    assert!(reply.get("error").is_some());
}
