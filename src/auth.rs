//! Browser sign-in. Routing tenants never establish a user's identity.
use crate::{
    auth_store::{AuthStore, Entry},
    google_identity::Provider,
    storage::AgentStore,
};
use axum::{
    Json, Router,
    extract::{Query, Request, State},
    http::{HeaderMap, StatusCode, header},
    middleware::{self, Next},
    response::{IntoResponse, Redirect, Response},
    routing::{get, post},
};
use rand_core::{OsRng, RngCore};
use serde::{Deserialize, Serialize};
use serde_json::json;
use sha2::{Digest, Sha256};
use std::{
    sync::Arc,
    time::{Duration, SystemTime, UNIX_EPOCH},
};

const LOGIN_COOKIE: &str = "__Host-calendar-login";
const SESSION_COOKIE: &str = "__Host-calendar-session";
const SESSION_SECONDS: i64 = 12 * 3600;
#[derive(Clone)]
pub struct Auth {
    pub store: Arc<dyn AuthStore>,
    pub agents: Arc<dyn AgentStore>,
    pub provider: Provider,
    pub trust: Arc<dyn crate::trust::TrustProvider>,
    pub base: String,
    pub website: String,
    pub allowed_emails: Vec<String>,
    pub connected: Option<Arc<crate::connected::Connected>>,
}
#[derive(Clone, Serialize, Deserialize)]
pub(crate) struct Account {
    pub id: String,
    pub email: String,
    pub name: String,
}
#[derive(Serialize, Deserialize)]
struct Attempt {
    nonce: String,
    verifier: String,
    host: Option<String>,
    #[serde(default)]
    calendar: bool,
    #[serde(default)]
    account: Option<String>,
}
pub(crate) fn now() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_secs() as i64
}
pub(crate) fn random() -> String {
    let mut bytes = [0; 32];
    OsRng.fill_bytes(&mut bytes);
    crate::trust::jose::b64url(&bytes)
}
fn hash(raw: &str) -> String {
    format!("{:x}", Sha256::digest(raw.as_bytes()))
}
fn key(prefix: &str, raw: &str) -> String {
    format!("{prefix}:{}", hash(raw))
}
fn token_valid(raw: &str) -> bool {
    raw.len() == 43
        && raw
            .bytes()
            .all(|c| c.is_ascii_alphanumeric() || c == b'-' || c == b'_')
}
fn cookie(headers: &HeaderMap, name: &str) -> Option<String> {
    let mut found = headers
        .get_all(header::COOKIE)
        .iter()
        .filter_map(|v| v.to_str().ok())
        .flat_map(|v| v.split(';'))
        .filter_map(|v| v.trim().split_once('='))
        .filter(|(k, v)| *k == name && token_valid(v))
        .map(|(_, v)| v.to_owned());
    let first = found.next();
    if found.next().is_some() { None } else { first }
}
fn set_cookie(response: &mut Response, name: &str, value: &str, seconds: i64) {
    response.headers_mut().append(
        header::SET_COOKIE,
        format!("{name}={value}; Path=/; Secure; HttpOnly; SameSite=Lax; Max-Age={seconds}")
            .parse()
            .unwrap(),
    );
}
pub(crate) fn error(status: StatusCode, code: &'static str) -> Response {
    (status, Json(json!({"error":code}))).into_response()
}
fn unavailable() -> Response {
    error(StatusCode::SERVICE_UNAVAILABLE, "auth_unavailable")
}
pub(crate) async fn private_response(request: Request, next: Next) -> Response {
    let mut response = tokio::time::timeout(Duration::from_secs(22), next.run(request))
        .await
        .unwrap_or_else(|_| unavailable());
    response
        .headers_mut()
        .insert(header::CACHE_CONTROL, "no-store".parse().unwrap());
    response
        .headers_mut()
        .insert("referrer-policy", "no-referrer".parse().unwrap());
    response
}
pub fn router(state: Auth) -> Router {
    Router::new()
        .route("/auth/google/start", get(start))
        .route("/auth/google/callback", get(callback))
        .route("/auth/me", get(me))
        .route("/auth/logout", post(logout))
        .route("/auth/agent", post(agent))
        .layer(axum::extract::DefaultBodyLimit::max(1024))
        .layer(middleware::from_fn(private_response))
        .with_state(Arc::new(state))
}
#[derive(Deserialize)]
struct Start {
    host: Option<String>,
    #[serde(default)]
    calendar: bool,
}
async fn start(
    State(s): State<Arc<Auth>>,
    headers: HeaderMap,
    Query(input): Query<Start>,
) -> Response {
    if input
        .host
        .as_deref()
        .is_some_and(|id| !crate::valid_tenant(id))
    {
        return error(StatusCode::BAD_REQUEST, "invalid_host");
    }
    let account = if input.calendar {
        match current(&s, &headers).await {
            Ok(a) => Some(a.id),
            Err(_) => return failed_login(&s, "sign_in_required"),
        }
    } else {
        None
    };
    let state = random();
    let binding = random();
    let attempt = Attempt {
        nonce: random(),
        verifier: random(),
        host: input.host,
        calendar: input.calendar,
        account,
    };
    let url =
        s.provider
            .authorization_url(&state, &attempt.nonce, &attempt.verifier, attempt.calendar);
    let row = Entry {
        value: serde_json::to_value(&attempt).unwrap(),
        expires: now() + 600,
        binding: hash(&binding),
    };
    if !matches!(s.store.create(&key("login", &state), row).await, Ok(true)) {
        return unavailable();
    }
    let mut response = Redirect::to(&url).into_response();
    set_cookie(&mut response, LOGIN_COOKIE, &binding, 600);
    response
}
#[derive(Deserialize)]
struct Callback {
    state: Option<String>,
    code: Option<String>,
    error: Option<String>,
}
fn failed_login(s: &Auth, code: &str) -> Response {
    let mut response = Redirect::to(&format!("{}/account?error={code}", s.website)).into_response();
    set_cookie(&mut response, LOGIN_COOKIE, "", 0);
    response
}
async fn callback(
    State(s): State<Arc<Auth>>,
    headers: HeaderMap,
    Query(input): Query<Callback>,
) -> Response {
    let (Some(state), Some(binding)) = (
        input.state.filter(|v| token_valid(v)),
        cookie(&headers, LOGIN_COOKIE),
    ) else {
        return failed_login(&s, "invalid_login");
    };
    let attempt = match s
        .store
        .consume(&key("login", &state), &hash(&binding), now())
        .await
    {
        Ok(Some(row)) => match serde_json::from_value::<Attempt>(row.value) {
            Ok(a) => a,
            Err(_) => return unavailable(),
        },
        Ok(None) => return failed_login(&s, "invalid_login"),
        Err(_) => return unavailable(),
    };
    if input.error.is_some() {
        return failed_login(&s, "login_cancelled");
    }
    let Some(code) = input.code.filter(|v| !v.is_empty() && v.len() <= 4096) else {
        return failed_login(&s, "invalid_login");
    };
    let identity = match s
        .provider
        .verify(&code, &attempt.nonce, &attempt.verifier)
        .await
    {
        Ok(id) if !id.sub.is_empty() && id.sub.len() <= 255 => id,
        _ => return failed_login(&s, "google_login_failed"),
    };
    // Basic Google sign-in can bypass Google's test-user restriction. Enforce the pilot here.
    if !s
        .allowed_emails
        .iter()
        .any(|email| email.eq_ignore_ascii_case(&identity.email))
    {
        return failed_login(&s, "test_account_required");
    }
    let account_key = key("google", &identity.sub);
    if attempt.calendar {
        let matching = s
            .store
            .get(&account_key, now())
            .await
            .ok()
            .flatten()
            .is_some_and(|row| row.value["id"].as_str() == attempt.account.as_deref());
        if !matching {
            return failed_login(&s, "wrong_google_account");
        }
    }
    let account = match s.store.get(&account_key, now()).await {
        Ok(Some(row)) => match serde_json::from_value::<Account>(row.value) {
            Ok(a) => a,
            Err(_) => return unavailable(),
        },
        Ok(None) => {
            let candidate = Account {
                id: random(),
                email: identity.email.clone(),
                name: identity.name.clone(),
            };
            let row = Entry {
                value: serde_json::to_value(&candidate).unwrap(),
                expires: 0,
                binding: String::new(),
            };
            match s.store.create(&account_key, row).await {
                Ok(true) => candidate,
                Ok(false) => match s.store.get(&account_key, now()).await {
                    Ok(Some(row)) => match serde_json::from_value(row.value) {
                        Ok(a) => a,
                        Err(_) => return unavailable(),
                    },
                    _ => return unavailable(),
                },
                Err(_) => return unavailable(),
            }
        }
        Err(_) => return unavailable(),
    };
    // Display name by account id, for meeting titles; refreshed at every
    // login so accounts created earlier get one too and renames propagate.
    // The e-mail stays with the Google-keyed account row.
    let profile = Entry {
        value: json!({"name": identity.name}),
        expires: 0,
        binding: String::new(),
    };
    if s.store
        .put(&format!("profile:{}", account.id), profile)
        .await
        .is_err()
    {
        return unavailable();
    }
    if attempt.calendar {
        let Some(service) = &s.connected else {
            return failed_login(&s, "calendar_unavailable");
        };
        if let Err(code) = service
            .calendars
            .connect(
                &account.id,
                &identity.email,
                identity.refresh_token.as_deref(),
                &identity.scopes,
            )
            .await
        {
            return failed_login(&s, code);
        }
    }
    // Only an opaque random cookie goes to the browser. Its digest is the database key.
    let session = random();
    let row = Entry {
        value: json!({"account_key":account_key}),
        expires: now() + SESSION_SECONDS,
        binding: String::new(),
    };
    if !matches!(
        s.store.create(&key("session", &session), row).await,
        Ok(true)
    ) {
        return unavailable();
    }
    if let Some(old) = cookie(&headers, SESSION_COOKIE) {
        if s.store.delete(&key("session", &old)).await.is_err() {
            return unavailable();
        }
    }
    tracing::info!(event="google_login_completed", tenant=%account.id);
    let destination = attempt
        .host
        .map(|id| format!("/account?host={id}"))
        .unwrap_or_else(|| "/account".into());
    let mut response = Redirect::to(&format!("{}{destination}", s.website)).into_response();
    set_cookie(&mut response, LOGIN_COOKIE, "", 0);
    set_cookie(&mut response, SESSION_COOKIE, &session, SESSION_SECONDS);
    response
}
pub(crate) async fn current(s: &Auth, headers: &HeaderMap) -> Result<Account, Response> {
    let token = cookie(headers, SESSION_COOKIE)
        .ok_or_else(|| error(StatusCode::UNAUTHORIZED, "sign_in_required"))?;
    let session = s
        .store
        .get(&key("session", &token), now())
        .await
        .map_err(|_| unavailable())?
        .ok_or_else(|| error(StatusCode::UNAUTHORIZED, "sign_in_required"))?;
    let account_key = session.value["account_key"]
        .as_str()
        .ok_or_else(unavailable)?;
    let account = s
        .store
        .get(account_key, now())
        .await
        .map_err(|_| unavailable())?
        .ok_or_else(unavailable)?;
    serde_json::from_value(account.value).map_err(|_| unavailable())
}
async fn me(State(s): State<Arc<Auth>>, headers: HeaderMap) -> Response {
    match current(&s, &headers).await {
        Ok(a) => {
            let connected = match &s.connected {
                Some(c) => match c.calendars.connected(&a.id).await {
                    Ok(v) => v,
                    Err(_) => return unavailable(),
                },
                None => false,
            };
            let pending = match &s.connected {
                Some(c) => match c.bookings.active(&a.id).await {
                    Ok(r) => r
                        .filter(|r| r.schedule_id == "google" && r.peer == a.id)
                        .map(|r| r.id),
                    Err(_) => return unavailable(),
                },
                None => None,
            };
            Json(json!({"id":a.id,"name":a.name,"email":a.email,"calendar_connected":connected,"pending_booking_id":pending}))
                .into_response()
        }
        Err(e) => e,
    }
}
pub(crate) fn origin_ok(s: &Auth, headers: &HeaderMap) -> bool {
    headers.get(header::ORIGIN).and_then(|v| v.to_str().ok()) == Some(s.website.as_str())
}
async fn logout(State(s): State<Arc<Auth>>, headers: HeaderMap) -> Response {
    if !origin_ok(&s, &headers) {
        return error(StatusCode::FORBIDDEN, "invalid_origin");
    }
    if let Some(token) = cookie(&headers, SESSION_COOKIE) {
        if s.store.delete(&key("session", &token)).await.is_err() {
            return unavailable();
        }
    }
    let mut response = StatusCode::NO_CONTENT.into_response();
    set_cookie(&mut response, SESSION_COOKIE, "", 0);
    response
}
async fn agent(State(s): State<Arc<Auth>>, headers: HeaderMap) -> Response {
    if !origin_ok(&s, &headers) {
        return error(StatusCode::FORBIDDEN, "invalid_origin");
    }
    let account = match current(&s, &headers).await {
        Ok(a) => a,
        Err(e) => return e,
    };
    let mut record = match s.agents.get(&account.id).await {
        Ok(Some(r)) => r,
        Ok(None) => {
            let agent = crate::agents::Agent {
                id: account.id.clone(),
                name: format!("Calendar agent {}", &account.id[..8]),
                google_account: true,
                live: false,
                slots: vec![],
            };
            let (record, key) = match crate::identities::issue(
                s.trust.as_ref(),
                agent,
                None,
                &s.base,
            )
            .await
            {
                Ok(r) => r,
                Err(error) => {
                    tracing::warn!(target: "calendar::trust", event = "card_signing_failed", tenant = %account.id, code = %error);
                    return unavailable();
                }
            };
            match s.agents.create(&record, &key.encode()).await {
                Ok(true) => record,
                Ok(false) => match s.agents.get(&account.id).await {
                    Ok(Some(r)) => r,
                    _ => return unavailable(),
                },
                Err(_) => return unavailable(),
            }
        }
        Err(_) => return unavailable(),
    };
    if !record.agent.google_account || record.booking_page_url.is_some() {
        return error(StatusCode::CONFLICT, "agent_identity_conflict");
    }
    if record.card_version != crate::agents::ACCOUNT_CARD_VERSION {
        // The card template changed since this card was signed: re-issue it
        // with the agent's existing key, so the kid and JWK Set are unchanged.
        let key = match s.agents.signing_key(&record.agent.id).await {
            Ok(Some(encoded)) => crate::trust::AgentKey::decode(&encoded).ok(),
            _ => None,
        };
        let Some(key) = key else {
            return unavailable();
        };
        let (fresh, _) = match crate::identities::reissue(
            s.trust.as_ref(),
            record.agent.clone(),
            None,
            &s.base,
            key,
        )
        .await
        {
            Ok(r) => r,
            Err(error) => {
                tracing::warn!(target: "calendar::trust", event = "card_signing_failed", tenant = %account.id, code = %error);
                return unavailable();
            }
        };
        let mut fresh = fresh;
        fresh.published = record.published;
        if s.agents.update(&fresh).await.is_err() {
            return unavailable();
        }
        tracing::info!(target: "calendar::trust", event = "card_reissued", tenant = %account.id, card_digest = %fresh.card_digest);
        record = fresh;
    }
    if !record.published {
        // Records created before publication became immediate.
        if s.agents.publish(&record.agent.id).await.is_err() {
            return unavailable();
        }
        record.published = true;
    }
    (if record.published { StatusCode::OK } else { StatusCode::ACCEPTED }, Json(json!({
        "id":record.agent.id, "identifier":crate::agents::Publisher::from_base(&s.base).urn(&record.agent.id), "agent_card_url":record.card_url,
        "share_url":format!("{}/book/{}",s.website,record.agent.id),
        "publication_status":if record.published {"published"} else {"pending"}, "calendar_connected":false,
    }))).into_response()
}

#[cfg(test)]
mod tests;
