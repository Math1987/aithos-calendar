//! Destructive only to an explicitly named temporary test table; never production.
use calendar::agent::{
    budget::{Budget, Ledger, OPERATING_LIMIT},
    state::{DynamoState, StateStore},
};
use std::sync::Arc;
#[tokio::main(flavor = "current_thread")]
async fn main() -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    let table = std::env::var("BUDGET_TEST_TABLE")?;
    assert!(table.starts_with("calendar-budget-test-"));
    let config = aws_config::load_defaults(aws_config::BehaviorVersion::latest()).await;
    let store = Arc::new(DynamoState {
        client: aws_sdk_dynamodb::Client::new(&config),
        table,
    });
    let now: chrono::DateTime<chrono::Utc> = std::time::SystemTime::now().into();
    assert!(
        store
            .cas(
                "budget",
                None,
                serde_json::to_value(Ledger {
                    month: Ledger::month(now),
                    spent: 0,
                    held: Default::default()
                })?
            )
            .await?
    );
    let budget = Arc::new(Budget {
        store: store.clone(),
    });
    let mut tasks = Vec::new();
    for _ in 0..80 {
        let b = budget.clone();
        tasks.push(tokio::spawn(
            async move { b.reserve(1_000_000_000, now).await },
        ));
    }
    let mut admitted = Vec::new();
    for t in tasks {
        if let Ok(id) = t.await? {
            admitted.push(id);
        }
    }
    assert_eq!(admitted.len(), 25);
    for id in admitted {
        budget.finish(&id, now).await?;
        budget.finish(&id, now).await?;
    }
    assert!(budget.reserve(1, now).await.is_err());
    let ledger: Ledger = serde_json::from_value(store.read("budget").await?.unwrap().value)?;
    assert_eq!(ledger.total()?, OPERATING_LIMIT);
    println!(
        "PASS real DynamoDB: 80 concurrent reservations, exactly 25 admitted; duplicate settlements do not refund; no Bedrock invocation."
    );
    Ok(())
}
