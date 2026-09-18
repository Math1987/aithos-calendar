//! Scenario lab: deliberately broken (and deliberately fine) catalogs that
//! any AI Catalog / A2A client can be pointed at, plus a report that runs
//! this service's own discovery client against each of them.
//!
//! Every scenario is derived from the real published records at request
//! time, so the lab always exercises current keys and current cards.
//! `GET /lab` lists the scenarios; `GET /lab/{scenario}/.well-known/ai-catalog.json`
//! serves one; `GET /lab/report?policy=guaranteed` verifies them all and
//! logs each step under `trace_id` so `/logs` shows the evidence.
use crate::{
    catalog, identities,
    identities::Identities,
    storage::Record,
    trust::{
        Claims, EntryDraft, LocalTrust, MemorySigner, OperatorSigner, Policy, TrustProvider,
        manifest,
    },
};
use axum::{
    Json,
    extract::{Path, Query, State},
    http::{HeaderValue, StatusCode, header},
    response::{IntoResponse, Response},
};
use serde_json::{Value, json};
use std::sync::Arc;

/// Keys the lab needs besides the real ones: a rotation successor for the
/// guarantor (published in the real JWK Set) and an impostor nobody pins.
pub struct Lab {
    pub next: Arc<MemorySigner>,
    pub rogue: Arc<MemorySigner>,
}

impl Lab {
    /// `LAB_KEY_SEED` makes every process of a deployment derive the same
    /// lab keys; without it (local runs) they are random per process.
    pub fn from_env() -> Self {
        match std::env::var("LAB_KEY_SEED") {
            Ok(seed) if !seed.trim().is_empty() => Self::from_seed(&seed),
            _ => Self {
                next: Arc::new(MemorySigner::random()),
                rogue: Arc::new(MemorySigner::random()),
            },
        }
    }
    pub fn from_seed(seed: &str) -> Self {
        Self {
            next: Arc::new(MemorySigner::from_seed(seed, "guarantor-next")),
            rogue: Arc::new(MemorySigner::from_seed(seed, "guarantor-rogue")),
        }
    }
    /// The successor key as it must appear in the real guarantor JWK Set.
    pub fn next_jwk(&self) -> Value {
        self.next.public_jwk()
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Scenario {
    pub name: &'static str,
    pub description: &'static str,
    /// Expected outcome under the `guaranteed` policy: `None` for
    /// acceptance, `Some(code)` for the refusal code.
    pub guaranteed: Option<&'static str>,
    /// Expected outcome under the `integrity` policy.
    pub integrity: Option<&'static str>,
    /// What the specifications say about it, for the report.
    pub note: &'static str,
}

pub const SCENARIOS: &[Scenario] = &[
    Scenario {
        name: "baseline",
        description: "The real catalog, unmodified, served from the lab path.",
        guaranteed: None,
        integrity: None,
        note: "Control case.",
    },
    Scenario {
        name: "tampered-catalog",
        description: "An entry description is changed after the catalog was signed.",
        guaranteed: Some("catalog_signature_invalid"),
        integrity: Some("catalog_signature_invalid"),
        note: "A present catalog signature must verify under every policy.",
    },
    Scenario {
        name: "unsigned-catalog",
        description: "The catalog-level signature is removed.",
        guaranteed: Some("catalog_signature_missing"),
        integrity: None,
        note: "Catalog-level integrity is a SHOULD in AI Catalog Level 3; the integrity policy tolerates its absence.",
    },
    Scenario {
        name: "substituted-card",
        description: "A different card, signed by an unknown key, is served at the entry URL.",
        guaranteed: Some("card_digest_mismatch"),
        integrity: Some("card_digest_mismatch"),
        note: "subject.digest binds the exact served bytes; the digest check fails before any signature is examined.",
    },
    Scenario {
        name: "replayed-entry",
        description: "A genuine manifest is copied into an entry that points at another URL.",
        guaranteed: Some("manifest_subject_mismatch"),
        integrity: Some("manifest_subject_mismatch"),
        note: "subject.url must equal the entry url (AI Catalog §Subject).",
    },
    Scenario {
        name: "expired-manifest",
        description: "The manifest is genuinely signed by the guarantor but its expiresAt is in the past.",
        guaranteed: Some("manifest_expired"),
        integrity: None,
        note: "Consumers SHOULD reject a manifest whose expiresAt has passed; the integrity policy does not look at validity.",
    },
    Scenario {
        name: "downgraded",
        description: "The entry's trustManifest is removed (Level 3 to Level 2).",
        guaranteed: Some("trust_downgrade"),
        integrity: Some("manifest_missing"),
        note: "Nothing in the catalog format prevents a downgrade; only a client policy that requires Level 3 detects it.",
    },
    Scenario {
        name: "unknown-guarantor",
        description: "The manifest is signed by a guarantor whose identity this client does not pin.",
        guaranteed: Some("untrusted_guarantor"),
        integrity: None,
        note: "The impostor's JWK Set is served and valid; refusal comes from client-side pinning, not from the document.",
    },
    Scenario {
        name: "impersonated-guarantor",
        description: "The manifest claims the real guarantor identity but is signed by another key.",
        guaranteed: Some("manifest_signature_invalid"),
        integrity: None,
        note: "The kid is not in the pinned JWK Set.",
    },
    Scenario {
        name: "rotated-key",
        description: "The manifest is signed by the guarantor's successor key, already published in its JWK Set.",
        guaranteed: None,
        integrity: None,
        note: "Rotation works when the new key is published before it signs; the specification says nothing about overlap periods.",
    },
    Scenario {
        name: "revoked-agent",
        description: "The agent was revoked by its operator, but its signed card and manifest are still valid.",
        guaranteed: None,
        integrity: None,
        note: "Specification limit: neither A2A nor AI Catalog defines revocation; only expiresAt bounds the damage.",
    },
    Scenario {
        name: "mirror",
        description: "The same card bytes are served from another URL, with a manifest issued for that URL.",
        guaranteed: None,
        integrity: None,
        note: "A legitimate mirror needs its own manifest (subject.url differs) but the same digest.",
    },
];

pub fn scenario(name: &str) -> Option<&'static Scenario> {
    SCENARIOS.iter().find(|s| s.name == name)
}

fn lab_base(state: &Identities, scenario: &str) -> String {
    format!("{}/lab/{scenario}", state.base)
}

fn lab_card_url(state: &Identities, scenario: &str, id: &str) -> String {
    format!("{}/agents/{id}/agent-card.json", lab_base(state, scenario))
}

/// The scenario's guarantor JWK Set URL (only `unknown-guarantor` has one).
pub fn rogue_identity(state: &Identities) -> String {
    format!(
        "{}/lab/rogue/trust-provider/.well-known/jwks.json",
        state.base
    )
}

/// The catalog of one scenario, signed (or not) as the scenario requires.
pub async fn document(state: &Identities, lab: &Lab, name: &str) -> Result<Value, StatusCode> {
    let scenario = scenario(name).ok_or(StatusCode::NOT_FOUND)?;
    let mut records = state
        .store
        .published()
        .await
        .map_err(|_| StatusCode::SERVICE_UNAVAILABLE)?;
    records.sort_by(|a, b| a.agent.id.cmp(&b.agent.id));
    let now = chrono::Utc::now();
    let mut entries = Vec::with_capacity(records.len());
    for record in &records {
        let id = &record.agent.id;
        let real_url = identities::card_url(&state.base, id);
        let url = lab_card_url(state, name, id);
        let draft = EntryDraft {
            identifier: state.publisher.urn(id),
            entry_type: manifest::CARD_TYPE.into(),
            url: url.clone(),
        };
        let claims = Claims {
            account_verified: record.agent.google_account,
        };
        let bytes = record.card_bytes.as_bytes();
        let trust = state.trust.as_ref();
        let manifest = match scenario.name {
            "downgraded" => None,
            "replayed-entry" => Some(genuine_manifest(state, record, &real_url).await?),
            "expired-manifest" => Some(
                trust
                    .manifest_dated(
                        &draft,
                        bytes,
                        &claims,
                        now - chrono::Duration::days(2),
                        now - chrono::Duration::days(1),
                    )
                    .await
                    .map_err(|_| StatusCode::SERVICE_UNAVAILABLE)?,
            ),
            "unknown-guarantor" => Some(
                LocalTrust::new(&state.base, lab.rogue.clone())
                    .with_identity(rogue_identity(state))
                    .manifest_for(&draft, bytes, &claims)
                    .await
                    .map_err(|_| StatusCode::SERVICE_UNAVAILABLE)?,
            ),
            "impersonated-guarantor" => Some(
                LocalTrust::new(&state.base, lab.rogue.clone())
                    .with_identity(trust.identity().to_owned())
                    .manifest_for(&draft, bytes, &claims)
                    .await
                    .map_err(|_| StatusCode::SERVICE_UNAVAILABLE)?,
            ),
            "rotated-key" => Some(
                LocalTrust::new(&state.base, lab.next.clone())
                    .with_identity(trust.identity().to_owned())
                    .manifest_for(&draft, bytes, &claims)
                    .await
                    .map_err(|_| StatusCode::SERVICE_UNAVAILABLE)?,
            ),
            _ => Some(
                trust
                    .manifest_for(&draft, bytes, &claims)
                    .await
                    .map_err(|_| StatusCode::SERVICE_UNAVAILABLE)?,
            ),
        };
        let mut entry = catalog::entry(state, record);
        entry["url"] = json!(url);
        entry["trustManifest"] = manifest.unwrap_or(Value::Null);
        if entry["trustManifest"].is_null() {
            entry.as_object_mut().unwrap().remove("trustManifest");
        }
        entries.push(entry);
    }
    let mut document = json!({
        "specVersion": "1.0",
        "host": {
            "displayName": format!("Calendar agents (lab: {name})"),
            "identifier": state.operator.identity(),
            "documentationUrl": format!("{}/", state.website.trim_end_matches('/')),
            "trustManifest": state.operator.host_manifest().await.map_err(|_| StatusCode::SERVICE_UNAVAILABLE)?,
        },
        "entries": entries,
    });
    if scenario.name != "unsigned-catalog" {
        state
            .operator
            .sign_catalog(&mut document)
            .await
            .map_err(|_| StatusCode::SERVICE_UNAVAILABLE)?;
    }
    if scenario.name == "tampered-catalog"
        && let Some(entry) = document["entries"].get_mut(0)
    {
        entry["description"] = json!("Tampered after signing.");
    }
    Ok(document)
}

/// The record's real manifest, refreshed if needed, still bound to the
/// real URL: exactly what a replay attacker would copy.
async fn genuine_manifest(
    state: &Identities,
    record: &Record,
    real_url: &str,
) -> Result<Value, StatusCode> {
    if let Some(manifest) = &record.manifest
        && manifest["subject"]["url"] == real_url
    {
        return Ok(manifest.clone());
    }
    state
        .trust
        .manifest_for(
            &EntryDraft {
                identifier: state.publisher.urn(&record.agent.id),
                entry_type: manifest::CARD_TYPE.into(),
                url: real_url.to_owned(),
            },
            record.card_bytes.as_bytes(),
            &Claims {
                account_verified: record.agent.google_account,
            },
        )
        .await
        .map_err(|_| StatusCode::SERVICE_UNAVAILABLE)
}

/// The card served by a scenario at the lab URL.
pub async fn card_bytes(
    state: &Identities,
    lab: &Lab,
    name: &str,
    tenant: &str,
) -> Result<Vec<u8>, StatusCode> {
    scenario(name).ok_or(StatusCode::NOT_FOUND)?;
    let record = match state.store.get(tenant).await {
        Ok(Some(record)) if record.published => record,
        Ok(_) => return Err(StatusCode::NOT_FOUND),
        Err(_) => return Err(StatusCode::SERVICE_UNAVAILABLE),
    };
    if name == "substituted-card" {
        // Same agent, one byte of difference, signed by a key nobody serves.
        let mut card: Value = serde_json::from_str(&record.card_bytes)
            .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
        card["description"] = json!("Substituted card: not the bytes the manifest binds.");
        let key = crate::trust::AgentKey::from_key(lab.rogue.signing_key().clone());
        return crate::trust::card::sign(card, &key, &identities::jwks_url(&state.base, tenant))
            .map(|signed| signed.bytes)
            .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR);
    }
    Ok(record.card_bytes.into_bytes())
}

#[derive(Clone)]
pub struct LabState {
    pub identities: Arc<Identities>,
    pub lab: Arc<Lab>,
    /// Guarantors the report's client pins: the real one only.
    pub trusted_guarantors: Vec<String>,
}

fn json_bytes(bytes: Vec<u8>, media_type: &'static str) -> Response {
    (
        [
            (header::CONTENT_TYPE, HeaderValue::from_static(media_type)),
            (header::CACHE_CONTROL, HeaderValue::from_static("no-store")),
        ],
        bytes,
    )
        .into_response()
}

/// `GET /lab`
pub async fn index(State(state): State<LabState>) -> Response {
    let base = &state.identities.base;
    let scenarios: Vec<Value> = SCENARIOS
        .iter()
        .map(|s| {
            json!({
                "name": s.name,
                "description": s.description,
                "catalog": format!("{base}/lab/{}/.well-known/ai-catalog.json", s.name),
                "expected": {"guaranteed": s.guaranteed.unwrap_or("accepted"), "integrity": s.integrity.unwrap_or("accepted")},
                "note": s.note,
            })
        })
        .collect();
    (
        [(
            header::CACHE_CONTROL,
            HeaderValue::from_static("public, max-age=60"),
        )],
        Json(json!({
            "scenarios": scenarios,
            "report": format!("{base}/lab/report?policy=guaranteed"),
            "guarantor": state.identities.trust.identity(),
            "operator": state.identities.operator.identity(),
            "policies": ["integrity", "guaranteed", "verified-account"],
        })),
    )
        .into_response()
}

/// `GET /lab/{scenario}/.well-known/ai-catalog.json`
pub async fn serve_catalog(State(state): State<LabState>, Path(name): Path<String>) -> Response {
    match document(&state.identities, &state.lab, &name).await {
        Ok(document) => match crate::trust::jose::canonicalize(&document) {
            Ok(bytes) => {
                tracing::info!(target: "calendar::catalog", event = "lab_catalog_served", scenario = %name, bytes = bytes.len());
                json_bytes(bytes, catalog::MEDIA_TYPE)
            }
            Err(_) => StatusCode::INTERNAL_SERVER_ERROR.into_response(),
        },
        Err(status) => status.into_response(),
    }
}

/// `GET /lab/{scenario}/agents/{tenant}/agent-card.json`
pub async fn serve_card(
    State(state): State<LabState>,
    Path((name, tenant)): Path<(String, String)>,
) -> Response {
    if !crate::valid_tenant(&tenant) {
        return StatusCode::NOT_FOUND.into_response();
    }
    match card_bytes(&state.identities, &state.lab, &name, &tenant).await {
        Ok(bytes) => json_bytes(bytes, "application/json"),
        Err(status) => status.into_response(),
    }
}

/// `GET /lab/rogue/trust-provider/.well-known/jwks.json`: the impostor's
/// perfectly valid key set.
pub async fn rogue_jwks(State(state): State<LabState>) -> Response {
    match crate::trust::jose::canonicalize(&json!({"keys": [state.lab.rogue.public_jwk()]})) {
        Ok(bytes) => json_bytes(bytes, "application/jwk-set+json"),
        Err(_) => StatusCode::INTERNAL_SERVER_ERROR.into_response(),
    }
}

#[derive(serde::Deserialize)]
pub struct ReportQuery {
    policy: Option<String>,
}

/// `GET /lab/report?policy=...`: run this service's own discovery client
/// against every scenario and compare with the expectation.
pub async fn report(State(state): State<LabState>, Query(query): Query<ReportQuery>) -> Response {
    let policy = match query.policy.as_deref().map(Policy::parse) {
        None => Policy::Guaranteed,
        Some(Some(policy)) => policy,
        Some(None) => {
            return (
                StatusCode::BAD_REQUEST,
                Json(json!({"error":"unknown_policy"})),
            )
                .into_response();
        }
    };
    let trace_id = a2a::new_message_id();
    let outcome = tokio::time::timeout(
        std::time::Duration::from_secs(25),
        run(&state, policy, &trace_id),
    )
    .await;
    match outcome {
        Ok(Ok(report)) => (
            [(header::CACHE_CONTROL, HeaderValue::from_static("no-store"))],
            Json(report),
        )
            .into_response(),
        Ok(Err(status)) => status.into_response(),
        Err(_) => StatusCode::GATEWAY_TIMEOUT.into_response(),
    }
}

/// The report as a JSON document; `passed` is true when every scenario
/// behaved as expected under `policy`.
pub async fn run(state: &LabState, policy: Policy, trace_id: &str) -> Result<Value, StatusCode> {
    let identities = &state.identities;
    let mut records = identities
        .store
        .published()
        .await
        .map_err(|_| StatusCode::SERVICE_UNAVAILABLE)?;
    records.sort_by(|a, b| a.agent.id.cmp(&b.agent.id));
    let Some(first) = records.first() else {
        return Err(StatusCode::CONFLICT);
    };
    let peer = &identities.publisher.urn(&first.agent.id);
    // Under `verified-account`, an accepted scenario still needs the peer
    // to carry the attestation, which only account-linked agents have.
    let attested = first.agent.google_account;
    let mut results = Vec::with_capacity(SCENARIOS.len());
    let mut passed = true;
    for scenario in SCENARIOS {
        let catalog_url = format!(
            "{}/.well-known/ai-catalog.json",
            lab_base(identities, scenario.name)
        );
        let directory = crate::discovery::PeerDirectory::new(
            &identities.base,
            &catalog_url,
            state.trusted_guarantors.clone(),
        )
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
        let span = tracing::info_span!("lab", trace_id, scenario = scenario.name);
        span.in_scope(|| {
            tracing::info!(target: "calendar::trust", event = "lab_scenario_started", scenario = scenario.name, policy = %policy)
        });
        let outcome =
            tracing::Instrument::instrument(directory.resolve(peer, trace_id, policy), span).await;
        let actual = match &outcome {
            Ok(_) => None,
            Err(error) => Some(error.code()),
        };
        let expected = match policy {
            Policy::Integrity => scenario.integrity,
            Policy::Guaranteed => scenario.guaranteed,
            // The attestation is checked last, once the chain is intact.
            Policy::VerifiedAccount => scenario.guaranteed.or(if attested {
                None
            } else {
                Some("attestation_missing")
            }),
        };
        let ok = actual == expected;
        passed &= ok;
        results.push(json!({
            "scenario": scenario.name,
            "catalog": catalog_url,
            "expected": expected.unwrap_or("accepted"),
            "actual": actual.unwrap_or("accepted"),
            "passed": ok,
            "note": scenario.note,
        }));
    }
    Ok(json!({
        "policy": policy.name(),
        "peer": peer,
        "trace_id": trace_id,
        "passed": passed,
        "results": results,
        "logs": format!("{}/logs?trace={trace_id}", identities.website.trim_end_matches('/')),
    }))
}
