//! Conservative, integer-nanodollar admission control; there is no refund API.
//! Successful calls consume their full pre-authorized maximum. Unknown calls stay
//! held across ALL future months. A missing/corrupt ledger disables inference.
use super::state::{Result, StateStore};
use chrono::{DateTime, Datelike, Utc};
use serde::{Deserialize, Serialize};
use std::{collections::BTreeMap, sync::Arc};
pub const HARD_LIMIT: u64 = 30_000_000_000;
pub const OPERATING_LIMIT: u64 = 25_000_000_000;
const KEY: &str = "budget";
#[derive(Clone, Serialize, Deserialize, Debug)]
#[serde(deny_unknown_fields)]
pub struct Ledger {
    pub month: i32,
    pub spent: u64,
    pub held: BTreeMap<String, u64>,
}
impl Ledger {
    pub fn month(at: DateTime<Utc>) -> i32 {
        at.year() * 12 + at.month0() as i32
    }
    fn advance(&mut self, at: DateTime<Utc>) -> Result<()> {
        self.total()?;
        let month = Self::month(at);
        if month < self.month {
            return Err("budget_clock_regressed");
        }
        if month > self.month {
            self.month = month;
            self.spent = 0;
        }
        self.total()?;
        Ok(())
    }
    pub fn total(&self) -> Result<u64> {
        self.held
            .values()
            .try_fold(self.spent, |s, n| {
                s.checked_add(*n).ok_or("budget_overflow")
            })
            .and_then(|n| {
                if n <= OPERATING_LIMIT {
                    Ok(n)
                } else {
                    Err("invalid_budget")
                }
            })
    }
    fn reserve(&mut self, id: &str, amount: u64, at: DateTime<Utc>) -> Result<()> {
        self.advance(at)?;
        if amount == 0 || self.held.contains_key(id) {
            return Err("invalid_reservation");
        }
        if self
            .total()?
            .checked_add(amount)
            .is_none_or(|v| v > OPERATING_LIMIT)
        {
            return Err("budget_exhausted");
        }
        // A growing unresolved ledger fails closed before DynamoDB's 400 KiB limit.
        if self.held.len() >= 2000 {
            return Err("budget_unresolved");
        }
        self.held.insert(id.into(), amount);
        Ok(())
    }
    fn finish(&mut self, id: &str, at: DateTime<Utc>) -> Result<()> {
        self.advance(at)?;
        // Removing a hold always charges the SAME maximum in the completion month.
        // It never replenishes allowance, including across month boundaries.
        if let Some(amount) = self.held.remove(id) {
            self.spent = self.spent.checked_add(amount).ok_or("budget_overflow")?;
        }
        self.total()?;
        Ok(())
    }
}
pub struct Budget {
    pub store: Arc<dyn StateStore>,
}
impl Budget {
    pub async fn reserve(&self, amount: u64, at: DateTime<Utc>) -> Result<String> {
        let id = crate::auth::random();
        for _ in 0..64 {
            let row = self
                .store
                .read(KEY)
                .await?
                .ok_or("budget_not_initialized")?;
            let mut ledger: Ledger =
                serde_json::from_value(row.value).map_err(|_| "invalid_budget")?;
            ledger.reserve(&id, amount, at)?;
            if self
                .store
                .cas(
                    KEY,
                    Some(row.revision),
                    serde_json::to_value(ledger).unwrap(),
                )
                .await?
            {
                return Ok(id);
            }
        }
        Err("budget_contention")
    }
    pub async fn finish(&self, id: &str, at: DateTime<Utc>) -> Result<()> {
        for _ in 0..64 {
            let row = self
                .store
                .read(KEY)
                .await?
                .ok_or("budget_not_initialized")?;
            let mut ledger: Ledger =
                serde_json::from_value(row.value).map_err(|_| "invalid_budget")?;
            ledger.finish(id, at)?;
            if self
                .store
                .cas(
                    KEY,
                    Some(row.revision),
                    serde_json::to_value(ledger).unwrap(),
                )
                .await?
            {
                return Ok(());
            }
        }
        Err("budget_contention")
    }
}
#[cfg(test)]
mod tests {
    use super::super::state::MemoryState;
    use super::*;
    fn at(s: &str) -> DateTime<Utc> {
        s.parse().unwrap()
    }
    async fn setup() -> (Arc<MemoryState>, Arc<Budget>) {
        let store = Arc::new(MemoryState::default());
        store
            .cas(
                KEY,
                None,
                serde_json::to_value(Ledger {
                    month: Ledger::month(at("2026-09-01T00:00:00Z")),
                    spent: 0,
                    held: BTreeMap::new(),
                })
                .unwrap(),
            )
            .await
            .unwrap();
        (store.clone(), Arc::new(Budget { store }))
    }
    #[tokio::test]
    async fn simultaneous_agents_never_overdraw() {
        let (store, budget) = setup().await;
        let mut tasks = vec![];
        for _ in 0..120 {
            let b = budget.clone();
            tasks.push(tokio::spawn(async move {
                b.reserve(1_000_000_000, at("2026-09-02T12:00:00Z")).await
            }));
        }
        let mut accepted = 0;
        for t in tasks {
            if t.await.unwrap().is_ok() {
                accepted += 1;
            }
        }
        assert_eq!(accepted, 25);
        let l: Ledger =
            serde_json::from_value(store.read(KEY).await.unwrap().unwrap().value).unwrap();
        assert_eq!(l.total().unwrap(), OPERATING_LIMIT);
        assert!(l.total().unwrap() < HARD_LIMIT);
    }
    #[tokio::test]
    async fn lost_response_remains_held_across_months_and_duplicate_finish_never_refunds() {
        let (store, b) = setup().await;
        let id = b
            .reserve(OPERATING_LIMIT, at("2026-09-30T23:59:59Z"))
            .await
            .unwrap();
        assert!(b.reserve(1, at("2026-10-01T00:00:00Z")).await.is_err());
        b.finish(&id, at("2026-10-01T00:00:01Z")).await.unwrap();
        b.finish(&id, at("2026-10-01T00:00:02Z")).await.unwrap();
        assert!(b.reserve(1, at("2026-10-01T00:00:03Z")).await.is_err());
        assert!(b.reserve(1, at("2026-09-01T00:00:00Z")).await.is_err());
        assert!(
            b.reserve(OPERATING_LIMIT, at("2026-11-01T00:00:00Z"))
                .await
                .is_ok()
        );
        let l: Ledger =
            serde_json::from_value(store.read(KEY).await.unwrap().unwrap().value).unwrap();
        assert_eq!(l.total().unwrap(), OPERATING_LIMIT);
    }
    #[tokio::test]
    async fn missing_corrupt_overflow_and_expensive_requests_fail_closed() {
        let empty = Arc::new(MemoryState::default());
        let b = Budget {
            store: empty.clone(),
        };
        assert!(b.reserve(1, at("2026-09-01T00:00:00Z")).await.is_err());
        empty
            .cas(KEY, None, serde_json::json!({"spent":0}))
            .await
            .unwrap();
        assert!(b.reserve(1, at("2026-09-01T00:00:00Z")).await.is_err());
        let (_, b) = setup().await;
        for cost in [0, OPERATING_LIMIT + 1, u64::MAX] {
            assert!(b.reserve(cost, at("2026-09-01T00:00:00Z")).await.is_err());
        }
    }
    #[test]
    fn exhaustive_boundary_arithmetic_preserves_invariant() {
        for spent in [0, 1, OPERATING_LIMIT - 1, OPERATING_LIMIT, u64::MAX] {
            for amount in [0, 1, 2, OPERATING_LIMIT - 1, OPERATING_LIMIT, u64::MAX] {
                let mut l = Ledger {
                    month: Ledger::month(at("2026-09-01T00:00:00Z")),
                    spent,
                    held: BTreeMap::new(),
                };
                if l.reserve("x", amount, at("2026-09-01T00:00:00Z")).is_ok() {
                    assert!(l.total().unwrap() <= OPERATING_LIMIT);
                    let before = l.total().unwrap();
                    l.finish("x", at("2026-09-01T00:00:00Z")).unwrap();
                    assert_eq!(l.total().unwrap(), before);
                }
            }
        }
    }
}
