use crate::agents::Agent;
use async_trait::async_trait;
use aws_sdk_dynamodb::{Client, error::ProvideErrorMetadata, types::AttributeValue};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::{collections::HashMap, sync::Mutex};

#[derive(Clone, Serialize, Deserialize)]
pub struct Record {
    pub agent: Agent,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub booking_page_url: Option<String>,
    pub registry_id: String,
    pub card_url: String,
    pub card_bytes: String,
    pub card_digest: String,
    pub publication: Value,
    #[serde(skip)]
    pub published: bool,
}

#[derive(Debug)]
pub struct StoreError;

fn failed(operation: &str, code: Option<&str>) -> StoreError {
    tracing::error!(event = "storage_error", operation, code);
    StoreError
}

#[async_trait]
pub trait AgentStore: Send + Sync {
    async fn get(&self, id: &str) -> Result<Option<Record>, StoreError>;
    /// Atomic insert. Keep the recovery signing key separate from readable records.
    async fn create(&self, record: &Record, signing_key: &str) -> Result<bool, StoreError>;
    async fn publish(&self, id: &str) -> Result<(), StoreError>;
    async fn published(&self) -> Result<Vec<Record>, StoreError>;
}

#[derive(Default)]
pub struct MemoryStore(Mutex<HashMap<String, (Record, String)>>);
impl MemoryStore {
    pub fn fixtures(base: &str) -> Self {
        Self(Mutex::new(
            crate::agents::fixtures()
                .into_iter()
                .map(|agent| {
                    let record = Record {
                        card_bytes: serde_json::to_string(&agent.card(base)).unwrap(),
                        card_url: format!("{base}/agents/{}/agent-card.json", agent.id),
                        booking_page_url: None,
                        registry_id: String::new(),
                        card_digest: String::new(),
                        publication: Value::Null,
                        published: true,
                        agent,
                    };
                    (record.agent.id.clone(), (record, String::new()))
                })
                .collect(),
        ))
    }
}
#[async_trait]
impl AgentStore for MemoryStore {
    async fn get(&self, id: &str) -> Result<Option<Record>, StoreError> {
        Ok(self
            .0
            .lock()
            .map_err(|_| StoreError)?
            .get(id)
            .map(|(r, _)| r.clone()))
    }
    async fn create(&self, record: &Record, signing_key: &str) -> Result<bool, StoreError> {
        use std::collections::hash_map::Entry;
        match self
            .0
            .lock()
            .map_err(|_| StoreError)?
            .entry(record.agent.id.clone())
        {
            Entry::Occupied(_) => Ok(false),
            Entry::Vacant(entry) => {
                entry.insert((record.clone(), signing_key.into()));
                Ok(true)
            }
        }
    }
    async fn publish(&self, id: &str) -> Result<(), StoreError> {
        self.0
            .lock()
            .map_err(|_| StoreError)?
            .get_mut(id)
            .ok_or(StoreError)?
            .0
            .published = true;
        Ok(())
    }
    async fn published(&self) -> Result<Vec<Record>, StoreError> {
        Ok(self
            .0
            .lock()
            .map_err(|_| StoreError)?
            .values()
            .filter(|(r, _)| r.published)
            .map(|(r, _)| r.clone())
            .collect())
    }
}

pub struct DynamoStore {
    client: Client,
    table: String,
}
impl DynamoStore {
    pub fn new(client: Client, table: String) -> Self {
        Self { client, table }
    }
    fn decode(item: &HashMap<String, AttributeValue>) -> Result<Record, StoreError> {
        let raw = item
            .get("record")
            .and_then(|v| v.as_s().ok())
            .ok_or(StoreError)?;
        let mut record: Record = serde_json::from_str(raw).map_err(|_| StoreError)?;
        record.published = item
            .get("published")
            .and_then(|v| v.as_bool().ok())
            .copied()
            .unwrap_or(false);
        Ok(record)
    }
}
#[async_trait]
impl AgentStore for DynamoStore {
    async fn get(&self, id: &str) -> Result<Option<Record>, StoreError> {
        let result = self
            .client
            .get_item()
            .table_name(&self.table)
            .key("id", AttributeValue::S(id.into()))
            .consistent_read(true)
            .projection_expression("#r, published")
            .expression_attribute_names("#r", "record")
            .send()
            .await
            .map_err(|e| failed("dynamodb", e.as_service_error().and_then(|v| v.code())))?;
        result.item.as_ref().map(Self::decode).transpose()
    }
    async fn create(&self, record: &Record, signing_key: &str) -> Result<bool, StoreError> {
        let result = self
            .client
            .put_item()
            .table_name(&self.table)
            .item("id", AttributeValue::S(record.agent.id.clone()))
            .item(
                "record",
                AttributeValue::S(serde_json::to_string(record).map_err(|_| StoreError)?),
            )
            .item("signing_key", AttributeValue::S(signing_key.into()))
            .item("published", AttributeValue::Bool(false))
            .condition_expression("attribute_not_exists(id)")
            .send()
            .await;
        match result {
            Ok(_) => Ok(true),
            Err(error)
                if error
                    .as_service_error()
                    .is_some_and(|e| e.is_conditional_check_failed_exception()) =>
            {
                Ok(false)
            }
            Err(e) => Err(failed(
                "put_item",
                e.as_service_error().and_then(|v| v.code()),
            )),
        }
    }
    async fn publish(&self, id: &str) -> Result<(), StoreError> {
        self.client
            .update_item()
            .table_name(&self.table)
            .key("id", AttributeValue::S(id.into()))
            .update_expression("SET published = :yes")
            .expression_attribute_values(":yes", AttributeValue::Bool(true))
            .condition_expression("attribute_exists(id)")
            .send()
            .await
            .map_err(|e| failed("dynamodb", e.as_service_error().and_then(|v| v.code())))?;
        Ok(())
    }
    async fn published(&self) -> Result<Vec<Record>, StoreError> {
        let mut result = Vec::new();
        let mut cursor = None;
        let mut scanned = 0;
        loop {
            let page = self
                .client
                .scan()
                .table_name(&self.table)
                .consistent_read(true)
                .projection_expression("#r, published")
                .expression_attribute_names("#r", "record")
                .filter_expression("published = :yes")
                .expression_attribute_values(":yes", AttributeValue::Bool(true))
                .set_exclusive_start_key(cursor)
                .send()
                .await
                .map_err(|e| failed("scan", e.as_service_error().and_then(|v| v.code())))?;
            scanned += page.scanned_count();
            if scanned > 1000 {
                return Err(StoreError);
            }
            for item in page.items() {
                result.push(Self::decode(item)?);
            }
            // Bounded pilot catalog; never silently truncate discovery.
            if result.len() > 1000 {
                return Err(StoreError);
            }
            cursor = page.last_evaluated_key;
            if cursor.as_ref().is_none_or(HashMap::is_empty) {
                break;
            }
        }
        Ok(result)
    }
}
