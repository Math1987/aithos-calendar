use super::*;
use crate::{
    agents::Agent,
    auth_store::MemoryAuthStore,
    booking_store::MemoryBookingStore,
    google_calendar::{Event, Result as CalendarResult},
    storage::MemoryStore,
};
use std::sync::{
    Mutex,
    atomic::{AtomicBool, AtomicUsize, Ordering},
};

#[derive(Default)]
struct FakeCalendars {
    events: Mutex<std::collections::HashMap<String, Event>>,
    inserts: AtomicUsize,
    lose_insert_response: AtomicBool,
    delay_guest: AtomicBool,
    busy: AtomicBool,
}
#[async_trait::async_trait]
impl Calendars for FakeCalendars {
    async fn connected(&self, _: &str) -> CalendarResult<bool> {
        Ok(true)
    }
    async fn connect(&self, _: &str, _: &str, _: Option<&str>, _: &[String]) -> CalendarResult<()> {
        Ok(())
    }
    async fn disconnect(&self, _: &str) -> CalendarResult<()> {
        Ok(())
    }
    async fn email(&self, id: &str) -> CalendarResult<String> {
        Ok(format!("{id}@example.com"))
    }
    async fn availability(&self, _: &str, w: &Window) -> CalendarResult<Availability> {
        Ok(Availability {
            timezone: "Europe/Paris".into(),
            slots: if self.busy.load(Ordering::SeqCst) {
                vec![]
            } else {
                crate::google_calendar::working_slots(w, "Europe/Paris", &[])?
            },
        })
    }
    async fn event(&self, _: &str, id: &str, _: &Slot) -> CalendarResult<Option<Event>> {
        Ok(self.events.lock().unwrap().get(id).cloned())
    }
    async fn insert(
        &self,
        host: &str,
        id: &str,
        slot: &Slot,
        email: &str,
    ) -> CalendarResult<Event> {
        assert_eq!(host, "host");
        assert_eq!(email, "guest@example.com");
        self.inserts.fetch_add(1, Ordering::SeqCst);
        let event = Event {
            id: id.into(),
            slot: slot.clone(),
            accepted: false,
        };
        self.events.lock().unwrap().insert(id.into(), event.clone());
        if self.lose_insert_response.load(Ordering::SeqCst) {
            Err("booking_outcome_unknown")
        } else {
            Ok(event)
        }
    }
    async fn accept(&self, who: &str, id: &str, slot: &Slot) -> CalendarResult<Option<Event>> {
        assert_eq!(who, "guest");
        if self.delay_guest.load(Ordering::SeqCst) {
            return Ok(None);
        }
        let mut e = self.event(who, id, slot).await?.unwrap();
        e.accepted = true;
        Ok(Some(e))
    }
}
async fn fixture() -> (
    Arc<Connected>,
    Arc<FakeCalendars>,
    tokio::task::JoinHandle<()>,
) {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base = format!("http://{}", listener.local_addr().unwrap());
    let agents = Arc::new(MemoryStore::default());
    for id in ["host", "guest"] {
        let agent = Agent {
            id: id.into(),
            name: id.into(),
            live: false,
            google_account: true,
            slots: vec![],
        };
        let registry = crate::registry::Registry::new(&base).unwrap();
        let (mut r, _) = registry.prepare(agent, None, &base).unwrap();
        r.card_url = format!("{base}/agents/{id}/agent-card.json");
        r.published = true;
        agents.create(&r, "").await.unwrap();
    }
    let calendars = Arc::new(FakeCalendars::default());
    let service = Arc::new(Connected {
        jobs: None,
        calendars: calendars.clone(),
        store: Arc::new(MemoryAuthStore::default()),
        bookings: Arc::new(MemoryBookingStore::default()),
        agents: agents.clone(),
        directory: Arc::new(
            crate::discovery::PeerDirectory::new_with_registry(
                &base,
                &format!("{base}/.well-known/ai-catalog.json"),
                None,
            )
            .unwrap(),
        ),
        website: base.clone(),
    });
    let app = crate::app_with_connector(
        &base,
        &format!("{base}/.well-known/ai-catalog.json"),
        agents,
        None,
        base.clone(),
        Arc::new(crate::booking_page::GoogleBookingPages::new().unwrap()),
        None,
        Some(service.clone()),
    )
    .unwrap();
    let task = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    (service, calendars, task)
}
async fn unlock_poll(s: &Connected, id: &str) {
    let mut r = s.bookings.get(id).await.unwrap().unwrap();
    let prev = r.revision;
    r.next_poll = 0;
    r.revision += 1;
    assert!(s.bookings.save(&r, prev, false).await.unwrap());
}

#[tokio::test]
async fn sdk_discovery_booking_and_duplicate_confirmation_share_one_event() {
    let (s, c, t) = fixture().await;
    let p = s.propose("guest", "host").await.unwrap().unwrap();
    assert_eq!(p.slot.end - p.slot.start, chrono::Duration::minutes(30));
    let (a, b) = tokio::join!(s.confirm("guest", &p.id), s.confirm("guest", &p.id));
    assert!(
        [
            a.unwrap()["status"].as_str().unwrap(),
            b.unwrap()["status"].as_str().unwrap()
        ]
        .contains(&"booked")
    );
    assert_eq!(s.confirm("guest", &p.id).await.unwrap()["reserved"], true);
    assert_eq!(c.inserts.load(Ordering::SeqCst), 1);
    assert!(s.confirm("host", &p.id).await.is_err());
    t.abort();
}
#[tokio::test]
async fn uncertain_write_is_reconciled_without_another_insert() {
    let (s, c, t) = fixture().await;
    c.lose_insert_response.store(true, Ordering::SeqCst);
    let p = s.propose("guest", "host").await.unwrap().unwrap();
    assert_eq!(
        s.confirm("guest", &p.id).await.unwrap()["status"],
        "outcome_unknown"
    );
    assert_eq!(s.bookings.active("guest").await.unwrap().unwrap().id, p.id);
    unlock_poll(&s, &p.id).await;
    assert_eq!(s.confirm("guest", &p.id).await.unwrap()["status"], "booked");
    assert_eq!(c.inserts.load(Ordering::SeqCst), 1);
    t.abort();
}
#[tokio::test]
async fn guest_propagation_is_pending_until_accepted() {
    let (s, c, t) = fixture().await;
    c.delay_guest.store(true, Ordering::SeqCst);
    let p = s.propose("guest", "host").await.unwrap().unwrap();
    assert_eq!(
        s.confirm("guest", &p.id).await.unwrap()["status"],
        "confirming_guest"
    );
    c.delay_guest.store(false, Ordering::SeqCst);
    unlock_poll(&s, &p.id).await;
    assert_eq!(s.confirm("guest", &p.id).await.unwrap()["status"], "booked");
    assert_eq!(c.inserts.load(Ordering::SeqCst), 1);
    t.abort();
}
#[tokio::test]
async fn stale_proposal_and_another_user_cannot_book() {
    let (s, c, t) = fixture().await;
    let p = s.propose("guest", "host").await.unwrap().unwrap();
    assert_eq!(
        s.confirm("host", &p.id).await.unwrap_err(),
        "invalid_booking"
    );
    c.busy.store(true, Ordering::SeqCst);
    assert_eq!(
        s.confirm("guest", &p.id).await.unwrap_err(),
        "slot_no_longer_available"
    );
    assert_eq!(c.inserts.load(Ordering::SeqCst), 0);
    t.abort();
}
#[tokio::test]
async fn capability_is_required_and_bound_to_recipient_payload_and_expiry() {
    let (s, _, t) = fixture().await;
    let data = json!({"operation":"get_availability","window":Window::next_month()});
    let mut headers = a2a_server::ServiceParams::new();
    assert!(s.handle("host", &headers, &data).await.is_err());
    let token = random();
    let key = format!("a2a-grant:{}", digest(&token));
    headers.insert("authorization".into(), vec![format!("Bearer {token}")]);
    let row = Entry {
        value: json!({"recipient":"host","caller":"guest","request":data}),
        expires: now() + 60,
        binding: String::new(),
    };
    s.store.put(&key, row.clone()).await.unwrap();
    assert!(s.handle("guest", &headers, &data).await.is_err());
    assert!(
        s.handle("host", &headers, &json!({"operation":"commit_booking"}))
            .await
            .is_err()
    );
    assert_eq!(
        s.handle("host", &headers, &data).await.unwrap()["status"],
        "availability"
    );
    s.store
        .put(
            &key,
            Entry {
                expires: now() - 1,
                ..row
            },
        )
        .await
        .unwrap();
    assert!(s.handle("host", &headers, &data).await.is_err());
    t.abort();
}
#[test]
fn links_cannot_redirect_discovery_to_another_origin() {
    for url in [
        "https://evil.test/book/host",
        "https://calendar.test/book/host?url=x",
        "https://calendar.test/book/a/b",
        "https://user@calendar.test/book/host",
    ] {
        assert!(parse_host(url, "https://calendar.test").is_err());
    }
    assert_eq!(
        parse_host("https://calendar.test/book/host", "https://calendar.test").unwrap(),
        "host"
    );
}

#[derive(Default)]
struct TestQueue(Mutex<Vec<String>>);
#[async_trait::async_trait]
impl crate::agent::jobs::Queue for TestQueue {
    async fn enqueue(&self, id: &str, _: i32) -> crate::agent::state::Result<()> {
        self.0.lock().unwrap().push(id.into());
        Ok(())
    }
}
#[tokio::test]
async fn autonomous_job_recovers_ambiguous_google_write_and_never_books_twice() {
    use crate::agent::{
        jobs::{Job, Jobs},
        state::{MemoryState, StateStore},
    };
    let (s, c, t) = fixture().await;
    let state = Arc::new(MemoryState::default());
    let queue = Arc::new(TestQueue::default());
    let jobs = Jobs {
        store: state.clone(),
        queue: queue.clone(),
    };
    let id = format!("gc{}", "a".repeat(40));
    let key = format!("job:{id}");
    state
        .cas(
            &key,
            None,
            serde_json::to_value(Job {
                id: id.clone(),
                host: "host".into(),
                peer: "guest".into(),
                status: "queued".into(),
                created: now(),
                lease_until: 0,
                attempts: 0,
                result: json!({}),
            })
            .unwrap(),
        )
        .await
        .unwrap();
    c.lose_insert_response.store(true, Ordering::SeqCst);
    jobs.process(&id, &s, None).await.unwrap();
    assert_eq!(
        state.read(&key).await.unwrap().unwrap().value["status"],
        "working"
    );
    assert_eq!(queue.0.lock().unwrap().len(), 1);
    unlock_poll(&s, &id).await;
    jobs.process(&id, &s, None).await.unwrap();
    jobs.process(&id, &s, None).await.unwrap();
    let row = state.read(&key).await.unwrap().unwrap();
    assert_eq!(row.value["status"], "booked");
    assert_eq!(row.value["result"]["reserved"], true);
    assert_eq!(c.inserts.load(Ordering::SeqCst), 1);
    t.abort();
}
#[tokio::test]
async fn learned_duration_survives_a2a_protobuf_round_trip() {
    let (s, _, t) = fixture().await;
    for (account, peer) in [("guest", "host"), ("host", "guest")] {
        let profile = crate::agent::preferences::Profile {
            preferences: crate::agent::preferences::Preferences {
                duration_minutes: 45,
                ..Default::default()
            },
            source: "learned".into(),
            previous_meetings: vec![],
        };
        s.store
            .put(
                &crate::agent::preferences::key(account, peer),
                Entry {
                    value: serde_json::to_value(profile).unwrap(),
                    expires: now() + 600,
                    binding: String::new(),
                },
            )
            .await
            .unwrap();
    }
    let p = s.propose("guest", "host").await.unwrap().unwrap();
    assert_eq!((p.slot.end - p.slot.start).num_minutes(), 45);
    t.abort();
}

pub(crate) async fn service_fixture() -> (Arc<Connected>, tokio::task::JoinHandle<()>) {
    let (s, _, t) = fixture().await;
    (s, t)
}
