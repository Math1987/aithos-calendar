use crate::agents::Agent;
use async_trait::async_trait;
use aws_sdk_dynamodb::{Client, error::ProvideErrorMetadata, types::AttributeValue};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::{collections::HashMap, sync::Mutex};

/// One published agent: its signed card, byte-exact as served, the public
/// key that verifies it, and the operator-signed AI Catalog manifest that
/// binds the card digest. The agent's private signing key is stored
/// separately by [`AgentStore::create`] and never read back into a record.
#[derive(Clone, Serialize, Deserialize)]
pub struct Record {
    pub agent: Agent,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub booking_page_url: Option<String>,
    /// Where the card is served: `{base}/agents/{id}/agent-card.json`.
    pub card_url: String,
    /// RFC 8785 canonical card, `signatures` included; served verbatim.
    pub card_bytes: String,
    /// `sha256:<hex>` of `card_bytes`; the manifest's `subject.digest`.
    pub card_digest: String,
    /// The card's own `version` member, repeated on the catalog entry.
    #[serde(default)]
    pub card_version: String,
    /// `{"keys":[...]}` served at `{base}/agents/{id}/jwks.json` (`jku`).
    #[serde(default)]
    pub card_jwks: Value,
    /// RFC 3339 time of the last card change (catalog `updatedAt`).
    #[serde(default)]
    pub updated_at: String,
    /// Operator-signed `trustManifest` for the catalog entry.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub manifest: Option<Value>,
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
    /// The agent's own signing key, needed to sign outgoing requests.
    async fn signing_key(&self, id: &str) -> Result<Option<String>, StoreError>;
    async fn publish(&self, id: &str) -> Result<(), StoreError>;
    /// Replace the readable record of an existing agent (re-signed card or
    /// refreshed manifest). The signing key is untouched.
    async fn update(&self, record: &Record) -> Result<(), StoreError>;
    async fn published(&self) -> Result<Vec<Record>, StoreError>;
    /// Remove an agent, its card and its signing key. Absent is success.
    async fn delete(&self, id: &str) -> Result<(), StoreError>;
}

#[derive(Default)]
pub struct MemoryStore(Mutex<HashMap<String, (Record, String)>>);
impl MemoryStore {
    /// Alice and Bob, signed with fresh keys and manifests by `trust`.
    pub async fn fixtures(base: &str, trust: &dyn crate::trust::TrustProvider) -> Self {
        let store = Self::default();
        for agent in crate::agents::fixtures() {
            let (record, key) = crate::identities::issue(trust, agent, None, base)
                .await
                .expect("fixture agents sign");
            store.create(&record, &key.encode()).await.unwrap();
        }
        store
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
    async fn signing_key(&self, id: &str) -> Result<Option<String>, StoreError> {
        Ok(self
            .0
            .lock()
            .map_err(|_| StoreError)?
            .get(id)
            .map(|(_, key)| key.clone())
            .filter(|key| !key.is_empty()))
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
    async fn update(&self, record: &Record) -> Result<(), StoreError> {
        let mut store = self.0.lock().map_err(|_| StoreError)?;
        let (current, _) = store.get_mut(&record.agent.id).ok_or(StoreError)?;
        let published = current.published;
        *current = record.clone();
        current.published = published;
        Ok(())
    }
    async fn delete(&self, id: &str) -> Result<(), StoreError> {
        self.0.lock().map_err(|_| StoreError)?.remove(id);
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
            .item("published", AttributeValue::Bool(record.published))
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
    async fn signing_key(&self, id: &str) -> Result<Option<String>, StoreError> {
        let result = self
            .client
            .get_item()
            .table_name(&self.table)
            .key("id", AttributeValue::S(id.into()))
            .consistent_read(true)
            .projection_expression("signing_key")
            .send()
            .await
            .map_err(|e| failed("dynamodb", e.as_service_error().and_then(|v| v.code())))?;
        Ok(result
            .item
            .as_ref()
            .and_then(|item| item.get("signing_key"))
            .and_then(|v| v.as_s().ok())
            .cloned()
            .filter(|key| !key.is_empty()))
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
    async fn update(&self, record: &Record) -> Result<(), StoreError> {
        self.client
            .update_item()
            .table_name(&self.table)
            .key("id", AttributeValue::S(record.agent.id.clone()))
            .update_expression("SET #r = :record")
            .expression_attribute_names("#r", "record")
            .expression_attribute_values(
                ":record",
                AttributeValue::S(serde_json::to_string(record).map_err(|_| StoreError)?),
            )
            .condition_expression("attribute_exists(id)")
            .send()
            .await
            .map_err(|e| failed("update_item", e.as_service_error().and_then(|v| v.code())))?;
        Ok(())
    }
    async fn delete(&self, id: &str) -> Result<(), StoreError> {
        self.client
            .delete_item()
            .table_name(&self.table)
            .key("id", AttributeValue::S(id.into()))
            .send()
            .await
            .map_err(|e| failed("delete_item", e.as_service_error().and_then(|v| v.code())))?;
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
