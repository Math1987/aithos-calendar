//! Durable booking operations and atomic exclusion of concurrent agent bookings.
use crate::availability::Slot;
use async_trait::async_trait;
use aws_sdk_dynamodb::{
    Client,
    types::{AttributeValue as A, Delete, Put, TransactWriteItem, Update},
};
use serde::{Deserialize, Serialize};
use std::{collections::HashMap, sync::Mutex};

#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum Stage {
    Submitting,
    Pending,
    ConfirmationRequired,
    Booked,
    Failed,
    SlotUnavailable,
    Unknown,
}
#[derive(Clone, Serialize, Deserialize)]
pub struct BookingOperation {
    pub id: String,
    pub request_digest: String,
    pub host: String,
    pub peer: String,
    pub slot: Slot,
    pub schedule_id: String,
    pub email_digest: String,
    pub stage: Stage,
    pub job_id: Option<String>,
    pub created_at: i64,
    pub next_poll: i64,
    pub revision: u64,
}
impl BookingOperation {
    fn released_guards(&self) -> usize {
        if matches!(self.stage, Stage::Failed | Stage::SlotUnavailable) {
            3
        } else {
            2
        }
    }
    fn guards(&self) -> [String; 3] {
        let mut pair = [&self.host, &self.peer];
        pair.sort();
        [
            format!("agent:{}", self.host),
            format!("agent:{}", self.peer),
            format!(
                "slot:{}:{}:{}:{}",
                pair[0],
                pair[1],
                self.slot.start.timestamp(),
                self.slot.end.timestamp()
            ),
        ]
    }
}
#[derive(Debug)]
pub struct StoreError;
#[async_trait]
pub trait BookingStore: Send + Sync {
    async fn get(&self, id: &str) -> Result<Option<BookingOperation>, StoreError>;
    async fn active(&self, agent: &str) -> Result<Option<BookingOperation>, StoreError>;
    /// Atomic operation + two agent locks + persistent pair/slot duplicate guard.
    async fn begin(&self, record: &BookingOperation) -> Result<bool, StoreError>;
    /// CAS on revision; release agent locks only after a known terminal result.
    async fn save(
        &self,
        record: &BookingOperation,
        previous: u64,
        release: bool,
    ) -> Result<bool, StoreError>;
}
#[derive(Default)]
struct Memory {
    records: HashMap<String, BookingOperation>,
    guards: HashMap<String, String>,
}
#[derive(Default)]
pub struct MemoryBookingStore(Mutex<Memory>);
#[async_trait]
impl BookingStore for MemoryBookingStore {
    async fn active(&self, agent: &str) -> Result<Option<BookingOperation>, StoreError> {
        let s = self.0.lock().map_err(|_| StoreError)?;
        Ok(s.guards
            .get(&format!("agent:{agent}"))
            .and_then(|id| s.records.get(id))
            .cloned())
    }
    async fn get(&self, id: &str) -> Result<Option<BookingOperation>, StoreError> {
        Ok(self
            .0
            .lock()
            .map_err(|_| StoreError)?
            .records
            .get(id)
            .cloned())
    }
    async fn begin(&self, r: &BookingOperation) -> Result<bool, StoreError> {
        let mut s = self.0.lock().map_err(|_| StoreError)?;
        if s.records.contains_key(&r.id) || r.guards().iter().any(|g| s.guards.contains_key(g)) {
            return Ok(false);
        }
        for g in r.guards() {
            s.guards.insert(g, r.id.clone());
        }
        s.records.insert(r.id.clone(), r.clone());
        Ok(true)
    }
    async fn save(
        &self,
        r: &BookingOperation,
        previous: u64,
        release: bool,
    ) -> Result<bool, StoreError> {
        let mut s = self.0.lock().map_err(|_| StoreError)?;
        if s.records
            .get(&r.id)
            .is_none_or(|old| old.revision != previous)
        {
            return Ok(false);
        }
        if release {
            for g in r.guards().iter().take(r.released_guards()) {
                if s.guards.get(g) != Some(&r.id) {
                    return Err(StoreError);
                }
            }
        }
        s.records.insert(r.id.clone(), r.clone());
        if release {
            for g in r.guards().iter().take(r.released_guards()) {
                s.guards.remove(g);
            }
        }
        Ok(true)
    }
}
pub struct DynamoBookingStore {
    client: Client,
    table: String,
}
impl DynamoBookingStore {
    pub fn new(client: Client, table: String) -> Self {
        Self { client, table }
    }
}
#[async_trait]
impl BookingStore for DynamoBookingStore {
    async fn active(&self, agent: &str) -> Result<Option<BookingOperation>, StoreError> {
        let out = self
            .client
            .get_item()
            .table_name(&self.table)
            .key("id", A::S(format!("agent:{agent}")))
            .consistent_read(true)
            .projection_expression("#o")
            .expression_attribute_names("#o", "owner")
            .send()
            .await
            .map_err(|_| StoreError)?;
        let id = out
            .item
            .as_ref()
            .and_then(|item| item.get("owner"))
            .and_then(|v| v.as_s().ok());
        match id {
            Some(id) => self.get(id).await,
            None => Ok(None),
        }
    }
    async fn get(&self, id: &str) -> Result<Option<BookingOperation>, StoreError> {
        let out = self
            .client
            .get_item()
            .table_name(&self.table)
            .key("id", A::S(format!("op:{id}")))
            .consistent_read(true)
            .projection_expression("#r")
            .expression_attribute_names("#r", "record")
            .send()
            .await
            .map_err(|_| StoreError)?;
        out.item
            .map(|v| {
                serde_json::from_str(
                    v.get("record")
                        .and_then(|v| v.as_s().ok())
                        .ok_or(StoreError)?,
                )
                .map_err(|_| StoreError)
            })
            .transpose()
    }
    async fn begin(&self, r: &BookingOperation) -> Result<bool, StoreError> {
        let mut writes = vec![
            TransactWriteItem::builder()
                .put(
                    Put::builder()
                        .table_name(&self.table)
                        .item("id", A::S(format!("op:{}", r.id)))
                        .item(
                            "record",
                            A::S(serde_json::to_string(r).map_err(|_| StoreError)?),
                        )
                        .item("revision", A::N(r.revision.to_string()))
                        .condition_expression("attribute_not_exists(id)")
                        .build()
                        .map_err(|_| StoreError)?,
                )
                .build(),
        ];
        for (index, g) in r.guards().into_iter().enumerate() {
            let mut put = Put::builder()
                .table_name(&self.table)
                .item("id", A::S(g))
                .item("owner", A::S(r.id.clone()))
                .condition_expression("attribute_not_exists(id)");
            if index == 2 {
                put = put.item(
                    "expires_at",
                    A::N((r.slot.end.timestamp() + 86400).to_string()),
                );
            }
            writes.push(
                TransactWriteItem::builder()
                    .put(put.build().map_err(|_| StoreError)?)
                    .build(),
            );
        }
        match self
            .client
            .transact_write_items()
            .set_transact_items(Some(writes))
            .send()
            .await
        {
            Ok(_) => Ok(true),
            Err(e)
                if e.as_service_error().is_some_and(|e| {
                    if let aws_sdk_dynamodb::operation::transact_write_items::TransactWriteItemsError::TransactionCanceledException(cancelled) = e {
                        cancelled.cancellation_reasons().iter().any(|r| r.code() == Some("ConditionalCheckFailed"))
                            && cancelled.cancellation_reasons().iter().all(|r| matches!(r.code(), None | Some("None" | "ConditionalCheckFailed")))
                    } else { false }
                }) =>
            {
                Ok(false)
            }
            Err(_) => Err(StoreError),
        }
    }
    async fn save(
        &self,
        r: &BookingOperation,
        previous: u64,
        release: bool,
    ) -> Result<bool, StoreError> {
        let mut update = Update::builder()
            .table_name(&self.table)
            .key("id", A::S(format!("op:{}", r.id)))
            .update_expression("SET #r = :r, revision = :next")
            .condition_expression("revision = :previous")
            .expression_attribute_names("#r", "record")
            .expression_attribute_values(
                ":r",
                A::S(serde_json::to_string(r).map_err(|_| StoreError)?),
            )
            .expression_attribute_values(":next", A::N(r.revision.to_string()))
            .expression_attribute_values(":previous", A::N(previous.to_string()));
        // Keep ambiguous operations for reconciliation; only resolved records expire.
        if matches!(
            r.stage,
            Stage::Booked | Stage::Failed | Stage::SlotUnavailable
        ) {
            update = update
                .update_expression("SET #r = :r, revision = :next, expires_at = :expiry")
                .expression_attribute_values(
                    ":expiry",
                    A::N((r.slot.end.timestamp() + 30 * 86400).to_string()),
                );
        }
        let update = update.build().map_err(|_| StoreError)?;
        let mut writes = vec![TransactWriteItem::builder().update(update).build()];
        if release {
            for g in r.guards().iter().take(r.released_guards()) {
                writes.push(
                    TransactWriteItem::builder()
                        .delete(
                            Delete::builder()
                                .table_name(&self.table)
                                .key("id", A::S(g.clone()))
                                .condition_expression("#owner = :owner")
                                .expression_attribute_names("#owner", "owner")
                                .expression_attribute_values(":owner", A::S(r.id.clone()))
                                .build()
                                .map_err(|_| StoreError)?,
                        )
                        .build(),
                );
            }
        }
        match self
            .client
            .transact_write_items()
            .set_transact_items(Some(writes))
            .send()
            .await
        {
            Ok(_) => Ok(true),
            Err(e)
                if e.as_service_error().is_some_and(|e| {
                    if let aws_sdk_dynamodb::operation::transact_write_items::TransactWriteItemsError::TransactionCanceledException(cancelled) = e {
                        cancelled.cancellation_reasons().iter().any(|r| r.code() == Some("ConditionalCheckFailed"))
                            && cancelled.cancellation_reasons().iter().all(|r| matches!(r.code(), None | Some("None" | "ConditionalCheckFailed")))
                    } else { false }
                }) =>
            {
                Ok(false)
            }
            Err(_) => Err(StoreError),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn operation(id: &str) -> BookingOperation {
        let start = chrono::Utc::now() + chrono::Duration::days(1);
        BookingOperation {
            id: id.into(),
            request_digest: String::new(),
            host: "a".into(),
            peer: "b".into(),
            slot: Slot {
                start,
                end: start + chrono::Duration::minutes(30),
            },
            schedule_id: String::new(),
            email_digest: String::new(),
            stage: Stage::Submitting,
            job_id: None,
            created_at: 0,
            next_poll: 0,
            revision: 0,
        }
    }
    #[tokio::test]
    async fn confirmed_pair_cannot_be_rebooked_in_reverse_but_other_slots_can() {
        let store = MemoryBookingStore::default();
        let mut first = operation("first");
        assert!(store.begin(&first).await.unwrap());
        first.stage = Stage::Booked;
        first.revision = 1;
        assert!(store.save(&first, 0, true).await.unwrap());
        let mut reverse = first.clone();
        reverse.id = "reverse".into();
        std::mem::swap(&mut reverse.host, &mut reverse.peer);
        assert!(!store.begin(&reverse).await.unwrap());
        reverse.slot.start += chrono::Duration::hours(1);
        reverse.slot.end += chrono::Duration::hours(1);
        assert!(store.begin(&reverse).await.unwrap());
        assert!(!store.save(&first, 0, true).await.unwrap());
    }
    #[tokio::test]
    async fn known_rejection_releases_guards_but_unknown_keeps_them() {
        let store = MemoryBookingStore::default();
        let mut first = operation("first");
        assert!(store.begin(&first).await.unwrap());
        first.stage = Stage::Unknown;
        first.revision = 1;
        assert!(store.save(&first, 0, false).await.unwrap());
        let mut retry = first.clone();
        retry.id = "retry".into();
        assert!(!store.begin(&retry).await.unwrap());
        first.stage = Stage::Failed;
        first.revision = 2;
        assert!(store.save(&first, 1, true).await.unwrap());
        assert!(store.begin(&retry).await.unwrap());
    }
}
