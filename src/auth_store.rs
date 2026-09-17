//! Private OAuth attempts, sessions and account mappings. Never part of discovery.
use async_trait::async_trait;
use aws_sdk_dynamodb::{
    Client,
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
    async fn get(&self, id: &str, now: i64) -> Result<Option<Entry>, AuthStoreError>;
    /// Consume a browser-bound login attempt once, atomically, only before its expiry.
    async fn consume(
        &self,
        id: &str,
        binding: &str,
        now: i64,
    ) -> Result<Option<Entry>, AuthStoreError>;
    async fn delete(&self, id: &str) -> Result<(), AuthStoreError>;
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
}
