//! Shared fixtures for integration tests: agents signed by an ephemeral
//! trust provider, plus helpers to build (and deliberately corrupt)
//! catalogs served by a test router.
#![allow(dead_code)]
use calendar::{
    agents::{Agent, Publisher},
    identities,
    storage::{AgentStore, MemoryStore},
    trust::{
        AgentKey, Claims, EntryDraft, LocalTrust, Operator, TrustProvider, card, jose, manifest,
    },
};
use serde_json::{Value, json};
use std::{collections::HashMap, sync::Arc};

pub struct Fixture {
    pub base: String,
    pub trust: Arc<dyn TrustProvider>,
    pub operator: Arc<Operator>,
    pub lab: Arc<calendar::lab::Lab>,
    pub store: Arc<MemoryStore>,
    pub keys: HashMap<String, AgentKey>,
}

pub fn mock_agent(id: &str, slots: &[(&str, &str)]) -> Agent {
    Agent {
        id: id.into(),
        name: id.to_uppercase(),
        live: false,
        google_account: false,
        slots: slots
            .iter()
            .map(|(start, end)| calendar::agents::slot(start, end))
            .collect(),
    }
}

impl Fixture {
    /// Records for `agents`, each signed with its own fresh key.
    pub async fn new(base: &str, agents: Vec<Agent>) -> Self {
        let lab = Arc::new(calendar::lab::Lab::from_seed("integration-tests"));
        let trust: Arc<dyn TrustProvider> =
            Arc::new(LocalTrust::ephemeral(base).with_published_key(lab.next_jwk()));
        let store = Arc::new(MemoryStore::default());
        let mut keys = HashMap::new();
        for agent in agents {
            let id = agent.id.clone();
            let (record, key) = identities::issue(trust.as_ref(), agent, None, base)
                .await
                .unwrap();
            store.create(&record, &key.encode()).await.unwrap();
            keys.insert(id, key);
        }
        Self {
            base: base.trim_end_matches('/').into(),
            trust,
            operator: Arc::new(Operator::ephemeral(base)),
            lab,
            store,
            keys,
        }
    }
    /// The application configuration for this fixture.
    pub fn config(&self) -> calendar::Config {
        calendar::Config::new(&self.base, self.trust.clone(), self.store.clone())
            .with_operator(self.operator.clone())
            .with_lab(self.lab.clone())
    }
    /// Start the application on a loopback listener.
    pub async fn serve(&self, config: calendar::Config) -> (String, tokio::task::JoinHandle<()>) {
        let listener = tokio::net::TcpListener::bind(self.base.trim_start_matches("http://"))
            .await
            .unwrap();
        let app = calendar::build(config).unwrap();
        let job = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
        (self.base.clone(), job)
    }
    pub fn publisher(&self) -> Publisher {
        Publisher::from_base(&self.base)
    }
    pub fn urn(&self, id: &str) -> String {
        self.publisher().urn(id)
    }
    /// Sign an arbitrary card with `id`'s key, as served bytes.
    pub async fn sign_card(&self, id: &str, card: Value) -> Vec<u8> {
        card::sign(card, &self.keys[id], &identities::jwks_url(&self.base, id))
            .unwrap()
            .bytes
    }
    /// A signed manifest for an entry serving `card_bytes` at `url`.
    pub async fn manifest(&self, identifier: &str, url: &str, card_bytes: &[u8]) -> Value {
        self.trust
            .manifest_for(
                &EntryDraft {
                    identifier: identifier.into(),
                    entry_type: manifest::CARD_TYPE.into(),
                    url: url.into(),
                },
                card_bytes,
                &Claims::default(),
            )
            .await
            .unwrap()
    }
    /// A signed Level 3 catalog from `(identifier, url, card_bytes)` entries.
    pub async fn catalog(&self, entries: &[(String, String, Vec<u8>)]) -> Value {
        let mut list = Vec::new();
        for (identifier, url, bytes) in entries {
            list.push(json!({
                "identifier": identifier, "type": manifest::CARD_TYPE, "url": url,
                "displayName": identifier, "version": "1.0", "updatedAt": "2030-01-01T00:00:00Z",
                "publisher": {"identifier": self.operator.identity(), "displayName": "Test"},
                "trustManifest": self.manifest(identifier, url, bytes).await,
            }));
        }
        let mut catalog = json!({
            "specVersion": "1.0",
            "host": {"displayName": "Test", "identifier": self.operator.identity(),
                     "trustManifest": self.operator.host_manifest().await.unwrap()},
            "entries": list,
        });
        self.operator.sign_catalog(&mut catalog).await.unwrap();
        catalog
    }
    pub fn digest(bytes: &[u8]) -> String {
        jose::digest(bytes)
    }
}
