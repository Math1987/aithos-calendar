//! Public log feed: an allow-listed copy of application, trust-layer and A2A
//! SDK events, served at `GET /logs/events` and shown live on `/logs`.
//!
//! CloudWatch is never exposed. A `tracing` layer copies only events whose
//! target and name are on the allow-list, and of those only the fields on
//! the field allow-list (identifiers, codes, counters, digests). E-mails,
//! names, tokens, request bodies, calendar intervals and headers are not on
//! any list, so they cannot reach the feed; `tests/public_logs.rs` proves
//! it with sentinel values.
//!
//! Events are buffered per process and flushed to the store at the end of
//! each request (Lambda freezes the process after the response, so nothing
//! may be left to a background task). Retention is short: the feed is a
//! live tail, not an archive.
use aws_sdk_dynamodb::types::{AttributeValue, PutRequest, WriteRequest};
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};
use std::{
    collections::HashMap,
    sync::{Arc, Mutex},
};
use tracing::{Event, Subscriber, field::Visit, span};
use tracing_subscriber::{Layer, layer::Context, registry::LookupSpan};

/// Targets whose events may be copied, with the public source label.
pub fn source_of(target: &str) -> Option<&'static str> {
    if target.starts_with("a2a_client") || target.starts_with("a2a_server") {
        Some("a2a-sdk")
    } else if target == "calendar::catalog" {
        Some("ai-catalog")
    } else if target == "calendar::trust" {
        Some("trust")
    } else if target.starts_with("calendar::") {
        Some("app")
    } else {
        None
    }
}

/// Application event names that may be copied (`event = "..."`).
pub const EVENTS: &[&str] = &[
    // trust layer
    "catalog_fetched",
    "catalog_unsigned",
    "catalog_signature_verified",
    "catalog_signature_rejected",
    "manifest_verified",
    "manifest_unverified",
    "manifest_rejected",
    "card_digest_verified",
    "card_digest_rejected",
    "card_signature_verified",
    "card_signature_rejected",
    "peer_verified",
    "peer_rejected",
    "caller_verified",
    "caller_rejected",
    "lab_scenario_started",
    "manifest_refreshed",
    "manifest_refresh_failed",
    "host_manifest_failed",
    "catalog_signature_failed",
    "card_signing_failed",
    "operator_key_loaded",
    "ephemeral_key",
    // catalog
    "catalog_served",
    "card_served",
    "lab_catalog_served",
    "catalog_too_large",
    // application
    "operation_received",
    "negotiation_completed",
    "peer_call",
    "connected_peer_call",
    "identity_created",
    "identity_reused",
    "storage_error",
    "agent_job_retry",
];

/// SDK messages that may be copied (`message`, from the A2A interceptors).
pub const MESSAGES: &[&str] = &[
    "A2A client request",
    "A2A client response",
    "A2A client error",
    "A2A server request",
    "A2A server response",
    "A2A server error",
];

/// Fields that may be copied. Anything else recorded on an event is dropped.
pub const FIELDS: &[&str] = &[
    "event",
    "message",
    "method",
    "status",
    "code",
    "operation",
    "peer",
    "recipient_tenant",
    "caller",
    "policy",
    "scenario",
    "kid",
    "card_digest",
    "guarantor",
    "issuer",
    "entries",
    "bytes",
    "mock",
    "duration_ms",
    "role",
];

/// Span fields that give an event its correlation context.
const SPAN_FIELDS: &[&str] = &["tenant", "trace_id", "scenario"];

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct PublicEvent {
    /// RFC 3339 with milliseconds.
    pub timestamp: String,
    pub level: String,
    pub source: String,
    pub target: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub trace_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tenant: Option<String>,
    #[serde(flatten)]
    pub fields: Map<String, Value>,
}

#[derive(Default)]
struct SpanFields(HashMap<&'static str, String>);

impl Visit for SpanFields {
    fn record_debug(&mut self, field: &tracing::field::Field, value: &dyn std::fmt::Debug) {
        if let Some(name) = SPAN_FIELDS.iter().find(|n| **n == field.name()) {
            self.0
                .insert(name, format!("{value:?}").trim_matches('"').to_owned());
        }
    }
    fn record_str(&mut self, field: &tracing::field::Field, value: &str) {
        if let Some(name) = SPAN_FIELDS.iter().find(|n| **n == field.name()) {
            self.0.insert(name, value.to_owned());
        }
    }
}

#[derive(Default)]
struct EventFields(Map<String, Value>);

impl EventFields {
    fn allowed(name: &str) -> Option<&'static str> {
        FIELDS.iter().copied().find(|f| *f == name)
    }
}

impl Visit for EventFields {
    fn record_debug(&mut self, field: &tracing::field::Field, value: &dyn std::fmt::Debug) {
        if let Some(name) = Self::allowed(field.name()) {
            let text = format!("{value:?}");
            self.0.insert(
                name.into(),
                Value::String(text.trim_matches('"').to_owned()),
            );
        }
    }
    fn record_str(&mut self, field: &tracing::field::Field, value: &str) {
        if let Some(name) = Self::allowed(field.name()) {
            self.0.insert(name.into(), Value::String(value.to_owned()));
        }
    }
    fn record_i64(&mut self, field: &tracing::field::Field, value: i64) {
        if let Some(name) = Self::allowed(field.name()) {
            self.0.insert(name.into(), Value::from(value));
        }
    }
    fn record_u64(&mut self, field: &tracing::field::Field, value: u64) {
        if let Some(name) = Self::allowed(field.name()) {
            self.0.insert(name.into(), Value::from(value));
        }
    }
    fn record_bool(&mut self, field: &tracing::field::Field, value: bool) {
        if let Some(name) = Self::allowed(field.name()) {
            self.0.insert(name.into(), Value::from(value));
        }
    }
}

/// Per-process buffer of events awaiting a flush.
#[derive(Default)]
pub struct Sink {
    pending: Mutex<Vec<PublicEvent>>,
}

const MAX_PENDING: usize = 1000;

impl Sink {
    pub fn push(&self, event: PublicEvent) {
        if let Ok(mut pending) = self.pending.lock()
            && pending.len() < MAX_PENDING
        {
            pending.push(event);
        }
    }
    pub fn drain(&self) -> Vec<PublicEvent> {
        self.pending
            .lock()
            .map(|mut pending| std::mem::take(&mut *pending))
            .unwrap_or_default()
    }
}

/// The `tracing` layer that copies allow-listed events into the sink.
pub struct PublicLayer {
    pub sink: Arc<Sink>,
}

impl<S> Layer<S> for PublicLayer
where
    S: Subscriber + for<'a> LookupSpan<'a>,
{
    fn on_new_span(&self, attrs: &span::Attributes<'_>, id: &span::Id, ctx: Context<'_, S>) {
        let mut fields = SpanFields::default();
        attrs.record(&mut fields);
        if let Some(span) = ctx.span(id) {
            span.extensions_mut().insert(fields);
        }
    }

    fn on_record(&self, id: &span::Id, values: &span::Record<'_>, ctx: Context<'_, S>) {
        if let Some(span) = ctx.span(id)
            && let Some(fields) = span.extensions_mut().get_mut::<SpanFields>()
        {
            values.record(fields);
        }
    }

    fn on_event(&self, event: &Event<'_>, ctx: Context<'_, S>) {
        let metadata = event.metadata();
        let Some(source) = source_of(metadata.target()) else {
            return;
        };
        let mut fields = EventFields::default();
        event.record(&mut fields);
        let name = fields.0.get("event").and_then(Value::as_str);
        let message = fields.0.get("message").and_then(Value::as_str);
        let listed = name.is_some_and(|n| EVENTS.contains(&n))
            || message.is_some_and(|m| MESSAGES.contains(&m));
        if !listed {
            return;
        }
        let mut context: HashMap<&'static str, String> = HashMap::new();
        if let Some(scope) = ctx.event_scope(event) {
            for span in scope.from_root() {
                if let Some(span_fields) = span.extensions().get::<SpanFields>() {
                    for (key, value) in &span_fields.0 {
                        context.insert(key, value.clone());
                    }
                }
            }
        }
        if let Some(scenario) = context.get("scenario") {
            fields
                .0
                .entry("scenario")
                .or_insert_with(|| Value::String(scenario.clone()));
        }
        self.sink.push(PublicEvent {
            timestamp: chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Millis, true),
            level: metadata.level().to_string(),
            source: source.into(),
            target: metadata.target().into(),
            trace_id: context.get("trace_id").cloned(),
            tenant: context.get("tenant").cloned(),
            fields: fields.0,
        });
    }
}

/// What the feed is served from.
#[async_trait::async_trait]
pub trait PublicStore: Send + Sync {
    async fn write(&self, events: Vec<PublicEvent>);
    /// Most recent events (ascending by timestamp), optionally after
    /// `since` and restricted to one trace or source.
    async fn recent(&self, query: &LogQuery) -> Vec<PublicEvent>;
}

#[derive(Clone, Debug, Default, Deserialize)]
pub struct LogQuery {
    /// RFC 3339 lower bound (exclusive).
    pub since: Option<String>,
    pub trace: Option<String>,
    pub source: Option<String>,
    pub limit: Option<usize>,
}

impl LogQuery {
    pub fn limit(&self) -> usize {
        self.limit.unwrap_or(100).clamp(1, 200)
    }
    fn matches(&self, event: &PublicEvent) -> bool {
        self.since
            .as_ref()
            .is_none_or(|since| event.timestamp.as_str() > since.as_str())
            && self
                .trace
                .as_ref()
                .is_none_or(|trace| event.trace_id.as_deref() == Some(trace.as_str()))
            && self
                .source
                .as_ref()
                .is_none_or(|source| &event.source == source)
    }
}

/// In-memory ring buffer for local runs and tests.
#[derive(Default)]
pub struct MemoryPublicStore {
    events: Mutex<Vec<PublicEvent>>,
}

const MEMORY_CAPACITY: usize = 5000;

#[async_trait::async_trait]
impl PublicStore for MemoryPublicStore {
    async fn write(&self, events: Vec<PublicEvent>) {
        if let Ok(mut all) = self.events.lock() {
            all.extend(events);
            if all.len() > MEMORY_CAPACITY {
                let excess = all.len() - MEMORY_CAPACITY;
                all.drain(..excess);
            }
        }
    }
    async fn recent(&self, query: &LogQuery) -> Vec<PublicEvent> {
        let Ok(all) = self.events.lock() else {
            return Vec::new();
        };
        let mut selected: Vec<PublicEvent> = all
            .iter()
            .rev()
            .filter(|e| query.matches(e))
            .take(query.limit())
            .cloned()
            .collect();
        selected.reverse();
        selected
    }
}

/// DynamoDB table with a TTL: partition key `bucket` (UTC hour), sort key
/// `sk` (timestamp plus a per-write counter), GSI `trace-index` on
/// `trace_id`/`sk`.
pub struct DynamoPublicStore {
    client: aws_sdk_dynamodb::Client,
    table: String,
    ttl_seconds: i64,
}

fn bucket_of(timestamp: &str) -> String {
    timestamp.get(..13).unwrap_or(timestamp).to_owned()
}

impl DynamoPublicStore {
    pub fn new(client: aws_sdk_dynamodb::Client, table: String, ttl_seconds: i64) -> Self {
        Self {
            client,
            table,
            ttl_seconds,
        }
    }
    fn item(
        &self,
        event: &PublicEvent,
        sequence: usize,
    ) -> Option<HashMap<String, AttributeValue>> {
        let json = serde_json::to_string(event).ok()?;
        let mut item = HashMap::from([
            (
                "bucket".to_owned(),
                AttributeValue::S(bucket_of(&event.timestamp)),
            ),
            (
                "sk".to_owned(),
                AttributeValue::S(format!(
                    "{}#{sequence:04}#{}",
                    event.timestamp,
                    std::process::id()
                )),
            ),
            ("event".to_owned(), AttributeValue::S(json)),
            ("source".to_owned(), AttributeValue::S(event.source.clone())),
            (
                "expires".to_owned(),
                AttributeValue::N((chrono::Utc::now().timestamp() + self.ttl_seconds).to_string()),
            ),
        ]);
        if let Some(trace) = &event.trace_id {
            item.insert("trace_id".into(), AttributeValue::S(trace.clone()));
        }
        Some(item)
    }
    fn decode(item: &HashMap<String, AttributeValue>) -> Option<PublicEvent> {
        serde_json::from_str(item.get("event")?.as_s().ok()?).ok()
    }
    /// The hour buckets a query without a trace may touch: this hour and
    /// the previous one (the feed is a live tail).
    fn buckets() -> [String; 2] {
        let now = chrono::Utc::now();
        [
            bucket_of(
                &(now - chrono::Duration::hours(1))
                    .to_rfc3339_opts(chrono::SecondsFormat::Millis, true),
            ),
            bucket_of(&now.to_rfc3339_opts(chrono::SecondsFormat::Millis, true)),
        ]
    }
}

#[async_trait::async_trait]
impl PublicStore for DynamoPublicStore {
    async fn write(&self, events: Vec<PublicEvent>) {
        for (chunk_index, chunk) in events.chunks(25).enumerate() {
            let requests: Vec<WriteRequest> = chunk
                .iter()
                .enumerate()
                .filter_map(|(i, event)| {
                    let item = self.item(event, chunk_index * 25 + i)?;
                    PutRequest::builder()
                        .set_item(Some(item))
                        .build()
                        .ok()
                        .map(|put| WriteRequest::builder().put_request(put).build())
                })
                .collect();
            if requests.is_empty() {
                continue;
            }
            if let Err(error) = self
                .client
                .batch_write_item()
                .request_items(&self.table, requests)
                .send()
                .await
            {
                // Never let the public feed break a request; CloudWatch keeps
                // the authoritative record.
                tracing::debug!(event = "public_log_write_failed", error = %error);
            }
        }
    }
    async fn recent(&self, query: &LogQuery) -> Vec<PublicEvent> {
        let mut events = Vec::new();
        let limit = query.limit();
        if let Some(trace) = &query.trace {
            let result = self
                .client
                .query()
                .table_name(&self.table)
                .index_name("trace-index")
                .key_condition_expression("trace_id = :t")
                .expression_attribute_values(":t", AttributeValue::S(trace.clone()))
                .scan_index_forward(false)
                .limit(limit as i32)
                .send()
                .await;
            if let Ok(page) = result {
                events.extend(page.items().iter().filter_map(Self::decode));
            }
        } else {
            for bucket in Self::buckets().iter().rev() {
                if events.len() >= limit {
                    break;
                }
                let mut request = self
                    .client
                    .query()
                    .table_name(&self.table)
                    .key_condition_expression("bucket = :b")
                    .expression_attribute_values(":b", AttributeValue::S(bucket.clone()))
                    .scan_index_forward(false)
                    .limit((limit - events.len()) as i32);
                if let Some(source) = &query.source {
                    request = request
                        .filter_expression("#s = :src")
                        .expression_attribute_names("#s", "source")
                        .expression_attribute_values(":src", AttributeValue::S(source.clone()));
                }
                if let Ok(page) = request.send().await {
                    events.extend(page.items().iter().filter_map(Self::decode));
                }
            }
        }
        events.retain(|e| query.matches(e));
        events.sort_by(|a, b| a.timestamp.cmp(&b.timestamp));
        events
    }
}

/// Shared handle: the process sink plus the store it flushes into.
#[derive(Clone)]
pub struct PublicLogs {
    pub sink: Arc<Sink>,
    pub store: Arc<dyn PublicStore>,
}

impl PublicLogs {
    pub fn memory(sink: Arc<Sink>) -> Self {
        Self {
            sink,
            store: Arc::new(MemoryPublicStore::default()),
        }
    }
    /// Write everything buffered so far; bounded so it never stalls a response.
    pub async fn flush(&self) {
        let events = self.sink.drain();
        if events.is_empty() {
            return;
        }
        let _ = tokio::time::timeout(
            std::time::Duration::from_millis(1500),
            self.store.write(events),
        )
        .await;
    }
}

/// Axum middleware: flush the public feed after every response.
pub async fn flush_after(
    axum::extract::State(logs): axum::extract::State<PublicLogs>,
    request: axum::extract::Request,
    next: axum::middleware::Next,
) -> axum::response::Response {
    let response = next.run(request).await;
    logs.flush().await;
    response
}

/// `GET /logs/events`
pub async fn events(
    axum::extract::State(logs): axum::extract::State<PublicLogs>,
    axum::extract::Query(query): axum::extract::Query<LogQuery>,
) -> axum::response::Response {
    use axum::response::IntoResponse;
    if query
        .trace
        .as_ref()
        .is_some_and(|t| t.len() > 64 || !t.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'-'))
        || query
            .source
            .as_ref()
            .is_some_and(|s| !["a2a-sdk", "ai-catalog", "trust", "app"].contains(&s.as_str()))
        || query.since.as_ref().is_some_and(|s| s.len() > 40)
    {
        return (
            axum::http::StatusCode::BAD_REQUEST,
            axum::Json(serde_json::json!({"error":"invalid_query"})),
        )
            .into_response();
    }
    let events = logs.store.recent(&query).await;
    (
        [("cache-control", "public, max-age=2")],
        axum::Json(serde_json::json!({
            "events": events,
            "now": chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Millis, true),
            "sources": ["a2a-sdk", "ai-catalog", "trust", "app"],
        })),
    )
        .into_response()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sources_and_allow_lists() {
        assert_eq!(source_of("a2a_client::middleware"), Some("a2a-sdk"));
        assert_eq!(source_of("a2a_server::middleware"), Some("a2a-sdk"));
        assert_eq!(source_of("calendar::catalog"), Some("ai-catalog"));
        assert_eq!(source_of("calendar::trust"), Some("trust"));
        assert_eq!(source_of("calendar::a2a"), Some("app"));
        assert_eq!(source_of("hyper::proto"), None);
        for forbidden in [
            "email",
            "name",
            "title",
            "token",
            "slots",
            "window",
            "authorization",
            "url",
            "body",
        ] {
            assert!(!FIELDS.contains(&forbidden), "{forbidden}");
        }
        let query = LogQuery {
            limit: Some(1000),
            ..Default::default()
        };
        assert_eq!(query.limit(), 200);
    }

    #[tokio::test]
    async fn memory_store_is_a_bounded_live_tail() {
        let store = MemoryPublicStore::default();
        let event = |i: usize, trace: &str| PublicEvent {
            timestamp: format!("2030-01-01T00:00:{:02}.000Z", i % 60),
            level: "INFO".into(),
            source: "trust".into(),
            target: "calendar::trust".into(),
            trace_id: Some(trace.into()),
            tenant: None,
            fields: Map::new(),
        };
        store.write((0..3).map(|i| event(i, "a")).collect()).await;
        store.write(vec![event(3, "b")]).await;
        let all = store.recent(&LogQuery::default()).await;
        assert_eq!(all.len(), 4);
        assert!(all[0].timestamp < all[3].timestamp);
        let b = store
            .recent(&LogQuery {
                trace: Some("b".into()),
                ..Default::default()
            })
            .await;
        assert_eq!(b.len(), 1);
        let since = store
            .recent(&LogQuery {
                since: Some("2030-01-01T00:00:01.000Z".into()),
                ..Default::default()
            })
            .await;
        assert_eq!(since.len(), 2);
        let app = store
            .recent(&LogQuery {
                source: Some("app".into()),
                ..Default::default()
            })
            .await;
        assert!(app.is_empty());
    }
}
