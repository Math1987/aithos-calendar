//! The AI Catalog document (`/.well-known/ai-catalog.json`) and the key
//! sets that verify it: a Level 3 "Trusted" catalog assembled from the
//! published records and signed on the way out.
//!
//! Every entry carries the guarantor-signed `trustManifest` stored with its
//! record; a stale manifest (moved URL, changed digest, near expiry) is
//! re-signed and persisted best-effort so the served document is always
//! verifiable. The top-level `signature` is recomputed whenever the unsigned
//! content changes and cached per process otherwise, so the `ETag` (a
//! digest of the served bytes) stays stable while nothing changed.
use crate::{
    identities::Identities,
    storage::Record,
    trust::{EntryDraft, jose, manifest},
};
use axum::{
    extract::{Path, State},
    http::{HeaderValue, StatusCode, header},
    response::{IntoResponse, Response},
};
use serde_json::{Value, json};
use std::sync::{Arc, Mutex};

pub const MEDIA_TYPE: &str = "application/ai-catalog+json";
/// Both the discovery client and this server refuse larger documents.
pub const MAX_BYTES: usize = 1024 * 1024;
/// A manifest is refreshed when less than this much validity remains.
const REFRESH_MARGIN: chrono::Duration = chrono::Duration::days(7);

/// Per-process cache of the last signed document, keyed by the digest of
/// its unsigned canonical form.
#[derive(Default)]
pub struct Signed(Mutex<Option<(String, Arc<Vec<u8>>)>>);

pub(crate) fn entry(state: &Identities, record: &Record) -> Value {
    let agent = &record.agent;
    json!({
        "identifier": state.publisher.urn(&agent.id),
        "displayName": agent.name,
        "type": manifest::CARD_TYPE,
        "url": record.card_url,
        "version": record.card_version,
        "updatedAt": record.updated_at,
        "publisher": {"identifier": state.operator.identity(), "displayName": "Calendar agents"},
        "description": if agent.google_account {
            "Account-linked Google Calendar agent; operation authorization required."
        } else if agent.live {
            "Real public availability; no booking."
        } else {
            "Mock scheduling agent; no calendar access or booking."
        },
        "tags": if agent.google_account { ["calendar", "account"] } else if agent.live { ["calendar", "availability"] } else { ["calendar", "mock"] },
        "trustManifest": record.manifest,
    })
}

/// Make sure the record's manifest binds the card as currently served.
async fn current_manifest(state: &Identities, record: &mut Record) {
    let draft = EntryDraft {
        identifier: state.publisher.urn(&record.agent.id),
        entry_type: manifest::CARD_TYPE.into(),
        url: record.card_url.clone(),
    };
    let current = record.manifest.as_ref().is_some_and(|m| {
        manifest::is_current(
            m,
            &draft,
            &record.card_digest,
            state.trust.identity(),
            chrono::Utc::now(),
            REFRESH_MARGIN,
        )
    });
    if current {
        return;
    }
    let claims = crate::trust::Claims {
        account_verified: record.agent.google_account,
    };
    match state
        .trust
        .manifest_for(&draft, record.card_bytes.as_bytes(), &claims)
        .await
    {
        Ok(fresh) => {
            tracing::info!(target: "calendar::trust", event = "manifest_refreshed", tenant = %record.agent.id, card_digest = %record.card_digest);
            record.manifest = Some(fresh);
            if state.store.update(record).await.is_err() {
                tracing::warn!(target: "calendar::trust", event = "manifest_refresh_not_persisted", tenant = %record.agent.id);
            }
        }
        Err(error) => {
            tracing::warn!(target: "calendar::trust", event = "manifest_refresh_failed", tenant = %record.agent.id, code = %error);
            record.manifest = None;
        }
    }
}

/// The unsigned document, entries sorted for a deterministic signature.
pub async fn document(state: &Identities) -> Result<Value, StatusCode> {
    let mut records = state
        .store
        .published()
        .await
        .map_err(|_| StatusCode::SERVICE_UNAVAILABLE)?;
    records.sort_by(|a, b| a.agent.id.cmp(&b.agent.id));
    let mut entries = Vec::with_capacity(records.len());
    for record in &mut records {
        current_manifest(state, record).await;
        entries.push(entry(state, record));
    }
    Ok(json!({
        "specVersion": "1.0",
        "host": {
            "displayName": "Calendar agents",
            "identifier": state.operator.identity(),
            "documentationUrl": format!("{}/", state.website.trim_end_matches('/')),
        },
        "entries": entries,
    }))
}

/// Add the host manifest and the top-level signature to `document`, reusing
/// the cached result when the unsigned content is unchanged (the host
/// manifest carries `issuedAt`, so it is produced together with the
/// signature and shares its cache lifetime).
pub async fn signed_bytes(
    state: &Identities,
    mut document: Value,
) -> Result<Arc<Vec<u8>>, StatusCode> {
    let key = jose::digest(
        &jose::canonicalize(&document).map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?,
    );
    if let Some((cached_key, bytes)) = state.signed.0.lock().ok().and_then(|c| c.clone())
        && cached_key == key
    {
        return Ok(bytes);
    }
    document["host"]["trustManifest"] = state.operator.host_manifest().await.map_err(|error| {
        tracing::warn!(target: "calendar::trust", event = "host_manifest_failed", code = %error);
        StatusCode::SERVICE_UNAVAILABLE
    })?;
    state
        .operator
        .sign_catalog(&mut document)
        .await
        .map_err(|error| {
            tracing::warn!(target: "calendar::trust", event = "catalog_signature_failed", code = %error);
            StatusCode::SERVICE_UNAVAILABLE
        })?;
    let bytes = jose::canonicalize(&document).map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
    // Match the discovery client's bounded document size, without partial results.
    if bytes.len() > MAX_BYTES {
        tracing::error!(target: "calendar::catalog", event = "catalog_too_large", bytes = bytes.len());
        return Err(StatusCode::SERVICE_UNAVAILABLE);
    }
    let bytes = Arc::new(bytes);
    if let Ok(mut cache) = state.signed.0.lock() {
        *cache = Some((key, bytes.clone()));
    }
    Ok(bytes)
}

fn json_document(
    bytes: Vec<u8>,
    media_type: &'static str,
    cache_control: &'static str,
) -> Response {
    let etag = format!("\"{}\"", &jose::digest(&bytes)[7..]);
    (
        [
            (header::CONTENT_TYPE, HeaderValue::from_static(media_type)),
            (
                header::CACHE_CONTROL,
                HeaderValue::from_static(cache_control),
            ),
            (
                header::ETAG,
                HeaderValue::from_str(&etag).expect("hex etag"),
            ),
        ],
        bytes,
    )
        .into_response()
}

/// `GET /.well-known/ai-catalog.json`
pub async fn serve(State(state): State<Arc<Identities>>) -> Response {
    let document = match document(&state).await {
        Ok(document) => document,
        Err(status) => return status.into_response(),
    };
    let entries = document["entries"].as_array().map_or(0, Vec::len);
    match signed_bytes(&state, document).await {
        Ok(bytes) => {
            tracing::info!(target: "calendar::catalog", event = "catalog_served", entries, bytes = bytes.len());
            let mut response =
                json_document(bytes.as_ref().clone(), MEDIA_TYPE, "public, max-age=60");
            response.headers_mut().insert(
                header::LINK,
                HeaderValue::from_str(&format!(
                    "<{}/.well-known/ai-catalog.json>; rel=\"ai-catalog\"",
                    state.base
                ))
                .expect("base url is a header value"),
            );
            response
        }
        Err(status) => status.into_response(),
    }
}

/// `GET /.well-known/jwks.json`: the operator key set, byte-exact with the
/// host manifest's `subject.digest`.
pub async fn operator_jwks(State(state): State<Arc<Identities>>) -> Response {
    match jose::canonicalize(&state.operator.jwks()) {
        Ok(bytes) => json_document(bytes, "application/jwk-set+json", "public, max-age=300"),
        Err(_) => StatusCode::INTERNAL_SERVER_ERROR.into_response(),
    }
}

/// `GET /trust-provider/.well-known/jwks.json`: the guarantor key set that
/// verifies every entry manifest.
pub async fn guarantor_jwks(State(state): State<Arc<Identities>>) -> Response {
    match jose::canonicalize(&state.trust.jwks()) {
        Ok(bytes) => json_document(bytes, "application/jwk-set+json", "public, max-age=300"),
        Err(_) => StatusCode::INTERNAL_SERVER_ERROR.into_response(),
    }
}

/// `GET /agents/{tenant}/jwks.json`: the key that signed that agent's card.
pub async fn agent_jwks(
    State(state): State<Arc<Identities>>,
    Path(tenant): Path<String>,
) -> Response {
    if !crate::valid_tenant(&tenant) {
        return StatusCode::NOT_FOUND.into_response();
    }
    match state.store.get(&tenant).await {
        Ok(Some(record)) if record.published => match jose::canonicalize(&record.card_jwks) {
            Ok(bytes) => json_document(bytes, "application/jwk-set+json", "public, max-age=300"),
            Err(_) => StatusCode::INTERNAL_SERVER_ERROR.into_response(),
        },
        Ok(_) => StatusCode::NOT_FOUND.into_response(),
        Err(_) => StatusCode::SERVICE_UNAVAILABLE.into_response(),
    }
}

/// `GET /agents/{tenant}/agent-card.json`: the signed card, verbatim.
pub async fn card(State(state): State<Arc<Identities>>, Path(tenant): Path<String>) -> Response {
    if !crate::valid_tenant(&tenant) {
        return StatusCode::NOT_FOUND.into_response();
    }
    match state.store.get(&tenant).await {
        Ok(Some(record)) if record.published => {
            tracing::info!(target: "calendar::catalog", event = "card_served", tenant = %tenant, card_digest = %record.card_digest);
            json_document(
                record.card_bytes.into_bytes(),
                "application/json",
                "public, max-age=60",
            )
        }
        Ok(_) => StatusCode::NOT_FOUND.into_response(),
        Err(_) => StatusCode::SERVICE_UNAVAILABLE.into_response(),
    }
}
