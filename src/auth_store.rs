//! Private OAuth attempts, sessions and account mappings. Never part of discovery.
use async_trait::async_trait;
use aws_sdk_dynamodb::{
    Client,
    error::ProvideErrorMetadata,
    types::{AttributeValue, ReturnValue},
};
use serde::{Deserialize, Serialize};
use std::{collections::HashMap, sync::Mutex};

#[derive(Clone, Serialize, Deserialize)]
pub struct Entry {
    pub value: serde_json::Value,
    /// Zero means a persistent account; other entries expire independently of TTL cleanup.
    pub expires: i64,
    pub binding: String,
}
#[derive(Debug)]
pub struct AuthStoreError;
#[async_trait]
pub trait AuthStore: Send + Sync {
    async fn create(&self, id: &str, entry: Entry) -> Result<bool, AuthStoreError>;
    async fn put(&self, id: &str, entry: Entry) -> Result<(), AuthStoreError>;
    async fn get(&self, id: &str, now: i64) -> Result<Option<Entry>, AuthStoreError>;
    /// Consume a browser-bound login attempt once, atomically, only before its expiry.
    async fn consume(
        &self,
        id: &str,
        binding: &str,
        now: i64,
    ) -> Result<Option<Entry>, AuthStoreError>;
    async fn delete(&self, id: &str) -> Result<(), AuthStoreError>;
    /// Add one to a counter row that expires at `expires`, and return the
    /// new count. Used for rate limits; the row is not an `Entry`.
    async fn increment(&self, id: &str, expires: i64) -> Result<u64, AuthStoreError>;
}
#[derive(Default)]
pub struct MemoryAuthStore(Mutex<HashMap<String, Entry>>);
#[async_trait]
impl AuthStore for MemoryAuthStore {
    async fn create(&self, id: &str, entry: Entry) -> Result<bool, AuthStoreError> {
        let mut rows = self.0.lock().map_err(|_| AuthStoreError)?;
        if rows.contains_key(id) {
            return Ok(false);
        }
        rows.insert(id.into(), entry);
        Ok(true)
    }
    async fn put(&self, id: &str, entry: Entry) -> Result<(), AuthStoreError> {
        self.0
            .lock()
            .map_err(|_| AuthStoreError)?
            .insert(id.into(), entry);
        Ok(())
    }
    async fn get(&self, id: &str, now: i64) -> Result<Option<Entry>, AuthStoreError> {
        Ok(self
            .0
            .lock()
            .map_err(|_| AuthStoreError)?
            .get(id)
            .filter(|r| r.expires == 0 || r.expires > now)
            .cloned())
    }
    async fn consume(
        &self,
        id: &str,
        binding: &str,
        now: i64,
    ) -> Result<Option<Entry>, AuthStoreError> {
        let mut rows = self.0.lock().map_err(|_| AuthStoreError)?;
        if rows
            .get(id)
            .is_some_and(|r| r.expires > now && r.binding == binding)
        {
            Ok(rows.remove(id))
        } else {
            Ok(None)
        }
    }
    async fn delete(&self, id: &str) -> Result<(), AuthStoreError> {
        self.0.lock().map_err(|_| AuthStoreError)?.remove(id);
        Ok(())
    }
    async fn increment(&self, id: &str, expires: i64) -> Result<u64, AuthStoreError> {
        let mut rows = self.0.lock().map_err(|_| AuthStoreError)?;
        let now = crate::auth::now();
        let count = match rows.get(id) {
            Some(row) if row.expires > now => row.value.as_u64().unwrap_or(0) + 1,
            _ => 1,
        };
        let expires = match rows.get(id) {
            Some(row) if row.expires > now => row.expires,
            _ => expires,
        };
        rows.insert(
            id.into(),
            Entry {
                value: serde_json::Value::from(count),
                expires,
                binding: String::new(),
            },
        );
        Ok(count)
    }
}
pub struct DynamoAuthStore {
    client: Client,
    table: String,
}
impl DynamoAuthStore {
    pub fn new(client: Client, table: String) -> Self {
        Self { client, table }
    }
    fn decode(item: HashMap<String, AttributeValue>) -> Result<Entry, AuthStoreError> {
        serde_json::from_str(
            item.get("record")
                .and_then(|v| v.as_s().ok())
                .ok_or(AuthStoreError)?,
        )
        .map_err(|_| AuthStoreError)
    }
}
#[async_trait]
impl AuthStore for DynamoAuthStore {
    async fn create(&self, id: &str, entry: Entry) -> Result<bool, AuthStoreError> {
        let mut request = self
            .client
            .put_item()
            .table_name(&self.table)
            .item("id", AttributeValue::S(id.into()))
            .item(
                "record",
                AttributeValue::S(serde_json::to_string(&entry).map_err(|_| AuthStoreError)?),
            )
            .item("binding", AttributeValue::S(entry.binding))
            .condition_expression("attribute_not_exists(id)");
        if entry.expires > 0 {
            request = request.item("expires_at", AttributeValue::N(entry.expires.to_string()));
        }
        match request.send().await {
            Ok(_) => Ok(true),
            Err(e)
                if e.as_service_error()
                    .is_some_and(|e| e.is_conditional_check_failed_exception()) =>
            {
                Ok(false)
            }
            Err(_) => Err(AuthStoreError),
        }
    }
    async fn put(&self, id: &str, entry: Entry) -> Result<(), AuthStoreError> {
        let mut request = self
            .client
            .put_item()
            .table_name(&self.table)
            .item("id", AttributeValue::S(id.into()))
            .item(
                "record",
                AttributeValue::S(serde_json::to_string(&entry).map_err(|_| AuthStoreError)?),
            )
            .item("binding", AttributeValue::S(entry.binding));
        if entry.expires > 0 {
            request = request.item("expires_at", AttributeValue::N(entry.expires.to_string()));
        }
        request.send().await.map_err(|_| AuthStoreError)?;
        Ok(())
    }
    async fn get(&self, id: &str, now: i64) -> Result<Option<Entry>, AuthStoreError> {
        let result = self
            .client
            .get_item()
            .table_name(&self.table)
            .key("id", AttributeValue::S(id.into()))
            .consistent_read(true)
            .send()
            .await
            .map_err(|_| AuthStoreError)?;
        Ok(result
            .item
            .map(Self::decode)
            .transpose()?
            .filter(|r| r.expires == 0 || r.expires > now))
    }
    async fn consume(
        &self,
        id: &str,
        binding: &str,
        now: i64,
    ) -> Result<Option<Entry>, AuthStoreError> {
        let result = self
            .client
            .delete_item()
            .table_name(&self.table)
            .key("id", AttributeValue::S(id.into()))
            .condition_expression("binding = :binding AND expires_at > :now")
            .expression_attribute_values(":binding", AttributeValue::S(binding.into()))
            .expression_attribute_values(":now", AttributeValue::N(now.to_string()))
            .return_values(ReturnValue::AllOld)
            .send()
            .await;
        match result {
            Ok(r) => r.attributes.map(Self::decode).transpose(),
            Err(e)
                if e.as_service_error()
                    .is_some_and(|e| e.is_conditional_check_failed_exception()) =>
            {
                Ok(None)
            }
            Err(_) => Err(AuthStoreError),
        }
    }
    async fn delete(&self, id: &str) -> Result<(), AuthStoreError> {
        self.client
            .delete_item()
            .table_name(&self.table)
            .key("id", AttributeValue::S(id.into()))
            .send()
            .await
            .map_err(|_| AuthStoreError)?;
        Ok(())
    }
    async fn increment(&self, id: &str, expires: i64) -> Result<u64, AuthStoreError> {
        // The first hit sets the window's expiry; later hits keep it, so the
        // counter is a fixed window that TTL cleanup removes afterwards. A
        // window that already expired but was not cleaned up yet restarts.
        let now = crate::auth::now();
        let result = self
            .client
            .update_item()
            .table_name(&self.table)
            .key("id", AttributeValue::S(id.into()))
            .update_expression("SET expires_at = :exp, #c = :one")
            .condition_expression("attribute_not_exists(id) OR expires_at <= :now")
            .expression_attribute_names("#c", "count")
            .expression_attribute_values(":exp", AttributeValue::N(expires.to_string()))
            .expression_attribute_values(":one", AttributeValue::N("1".into()))
            .expression_attribute_values(":now", AttributeValue::N(now.to_string()))
            .send()
            .await;
        match result {
            Ok(_) => return Ok(1),
            Err(e)
                if e.as_service_error()
                    .is_some_and(|e| e.is_conditional_check_failed_exception()) => {}
            Err(e) => {
                // A denied or failing counter must be visible: it fails the
                // request closed as "storage_unavailable".
                tracing::warn!(
                    event = "storage_error",
                    operation = "increment",
                    code = e
                        .as_service_error()
                        .and_then(|v| v.code())
                        .unwrap_or("unknown")
                );
                return Err(AuthStoreError);
            }
        }
        let updated = self
            .client
            .update_item()
            .table_name(&self.table)
            .key("id", AttributeValue::S(id.into()))
            .update_expression("ADD #c :one")
            .condition_expression("expires_at > :now")
            .expression_attribute_names("#c", "count")
            .expression_attribute_values(":one", AttributeValue::N("1".into()))
            .expression_attribute_values(":now", AttributeValue::N(now.to_string()))
            .return_values(ReturnValue::AllNew)
            .send()
            .await
            .map_err(|e| {
                tracing::warn!(
                    event = "storage_error",
                    operation = "increment",
                    code = e
                        .as_service_error()
                        .and_then(|v| v.code())
                        .unwrap_or("unknown")
                );
                AuthStoreError
            })?;
        updated
            .attributes
            .and_then(|a| a.get("count")?.as_n().ok()?.parse().ok())
            .ok_or(AuthStoreError)
    }
}
