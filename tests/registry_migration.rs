use calendar::{
    booking_page::BookingPage,
    registry::Registry,
    storage::{AgentStore, MemoryStore},
};
use registry_core::write::{AgentState, DetachedJws, Outcome, Status, evaluate_write};
fn verify(
    record: &calendar::storage::Record,
    current: Option<&AgentState>,
) -> registry_core::write::AcceptedWrite {
    let card = a2a_card::validate_value(record.publication["agentCard"].clone()).unwrap();
    let proofs: Vec<_> = record.publication["proofs"]
        .as_array()
        .unwrap()
        .iter()
        .map(|p| DetachedJws {
            protected: p["protected"].as_str().unwrap().into(),
            payload: p["payload"].as_str().unwrap().into(),
            signature: p["signature"].as_str().unwrap().into(),
        })
        .collect();
    evaluate_write(
        &record.registry_id,
        &card,
        record.publication["keys"].as_array().unwrap(),
        &proofs,
        "https://registry.example.com",
        current,
    )
    .unwrap()
}
#[tokio::test]
async fn migration_preserves_identity_and_passes_registry_version_and_lineage_checks() {
    let store = MemoryStore::fixtures("https://calendar.example.com");
    let mut agent = store.get("alice").await.unwrap().unwrap().agent;
    let page = BookingPage {
        url: "https://calendar.google.com/calendar/appointments/schedules/TestPage".into(),
    };
    agent.id = page.agent_id();
    let registry = Registry::new("https://registry.example.com").unwrap();
    let (old, key) = registry
        .prepare(agent, Some(page.url), "https://calendar.example.com")
        .unwrap();
    let created = verify(&old, None);
    let state = AgentState {
        agent_id: old.registry_id.clone(),
        status: Status::Active,
        card_digest: created.card_digest,
        card_version: created.card_version,
        authorized_kids: created.authorized_kids,
    };
    let live = registry
        .upgrade_to_live(&old, &key, "https://calendar.example.com")
        .unwrap();
    assert_eq!(verify(&live, Some(&state)).outcome, Outcome::Updated);
    assert_eq!(old.registry_id, live.registry_id);
    assert_eq!(old.card_url, live.card_url);
    assert_eq!(old.agent.id, live.agent.id);
    assert_eq!(old.booking_page_url, live.booking_page_url);
    assert!(live.agent.live && live.agent.slots.is_empty());
    assert_eq!(
        live.card_bytes,
        registry
            .upgrade_to_live(&old, &key, "https://calendar.example.com")
            .unwrap()
            .card_bytes
    );
    assert!(
        registry
            .upgrade_to_live(&old, "wrong-key", "https://calendar.example.com")
            .is_err()
    );
    assert!(
        registry
            .upgrade_to_live(&live, &key, "https://calendar.example.com")
            .is_err()
    );
}
