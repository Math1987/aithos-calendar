//! Strongly consistent compare-and-swap storage. Missing state never means zero spend.
use async_trait::async_trait;
use aws_sdk_dynamodb::{Client, types::AttributeValue as A};
use serde_json::Value;
#[derive(Clone)]
pub struct Row {
    pub revision: u64,
    pub value: Value,
}
pub type Result<T> = std::result::Result<T, &'static str>;
#[async_trait]
pub trait StateStore: Send + Sync {
    async fn read(&self, key: &str) -> Result<Option<Row>>;
    async fn cas(&self, key: &str, previous: Option<u64>, value: Value) -> Result<bool>;
}
pub struct DynamoState {
    pub client: Client,
    pub table: String,
}
#[async_trait]
impl StateStore for DynamoState {
    async fn read(&self, key: &str) -> Result<Option<Row>> {
        let r = self
            .client
            .get_item()
            .table_name(&self.table)
            .key("id", A::S(key.into()))
            .consistent_read(true)
            .send()
            .await
            .map_err(|_| "state_unavailable")?;
        r.item
            .map(|v| {
                Ok(Row {
                    revision: v
                        .get("revision")
                        .and_then(|v| v.as_n().ok())
                        .and_then(|v| v.parse().ok())
                        .ok_or("invalid_state")?,
                    value: serde_json::from_str(
                        v.get("record")
                            .and_then(|v| v.as_s().ok())
                            .ok_or("invalid_state")?,
                    )
                    .map_err(|_| "invalid_state")?,
                })
            })
            .transpose()
    }
    async fn cas(&self, key: &str, previous: Option<u64>, value: Value) -> Result<bool> {
        let revision = previous
            .map_or(Some(0), |n| n.checked_add(1))
            .ok_or("state_overflow")?;
        let mut r = self
            .client
            .put_item()
            .table_name(&self.table)
            .item("id", A::S(key.into()))
            .item("revision", A::N(revision.to_string()))
            .item("record", A::S(value.to_string()));
        if key.starts_with("job:")
            && value["status"]
                .as_str()
                .is_some_and(|s| ["booked", "failed", "no_common_slot"].contains(&s))
        {
            r = r.item(
                "expires_at",
                A::N((crate::auth::now() + 30 * 86400).to_string()),
            );
        }
        r = match previous {
            Some(n) => r
                .condition_expression("revision = :previous")
                .expression_attribute_values(":previous", A::N(n.to_string())),
            None => r.condition_expression("attribute_not_exists(id)"),
        };
        match r.send().await {
            Ok(_) => Ok(true),
            Err(e)
                if e.as_service_error()
                    .is_some_and(|e| e.is_conditional_check_failed_exception()) =>
            {
                Ok(false)
            }
            Err(_) => Err("state_unavailable"),
        }
    }
}
#[cfg(test)]
#[derive(Default)]
pub struct MemoryState(pub std::sync::Mutex<std::collections::HashMap<String, Row>>);
#[cfg(test)]
#[async_trait]
impl StateStore for MemoryState {
    async fn read(&self, key: &str) -> Result<Option<Row>> {
        Ok(self.0.lock().unwrap().get(key).cloned())
    }
    async fn cas(&self, key: &str, previous: Option<u64>, value: Value) -> Result<bool> {
        tokio::task::yield_now().await;
        let mut rows = self.0.lock().unwrap();
        if rows.get(key).map(|r| r.revision) != previous {
            return Ok(false);
        }
        rows.insert(
            key.into(),
            Row {
                revision: previous.map_or(0, |n| n + 1),
                value,
            },
        );
        Ok(true)
    }
}
