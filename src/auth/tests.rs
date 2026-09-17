use super::*;
use crate::{
    auth_store::MemoryAuthStore,
    google_identity::{IdentityProvider, VerifiedIdentity},
    storage::MemoryStore,
};
use axum::{
    body::{Body, to_bytes},
    extract::Path,
    http::Request,
};
use serde_json::Value;
use std::{
    collections::HashMap,
    sync::{
        Mutex,
        atomic::{AtomicUsize, Ordering},
    },
};
use tower::ServiceExt;

#[derive(Default)]
struct TestGoogle {
    calls: AtomicUsize,
}
#[async_trait::async_trait]
impl IdentityProvider for TestGoogle {
    fn authorization_url(
        &self,
        state: &str,
        nonce: &str,
        verifier: &str,
        _calendar: bool,
    ) -> String {
        assert!(token_valid(nonce));
        assert!(token_valid(verifier));
        format!("https://accounts.google.com/auth?state={state}")
    }
    async fn verify(
        &self,
        code: &str,
        nonce: &str,
        verifier: &str,
    ) -> Result<VerifiedIdentity, &'static str> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        assert!(token_valid(nonce));
        assert!(token_valid(verifier));
        if code == "invalid" {
            return Err("invalid");
        }
        Ok(VerifiedIdentity {
            sub: code.into(),
            email: if code == "outside" {
                "outside@example.com"
            } else {
                "test@example.com"
            }
            .into(),
            name: "Private Person".into(),
            refresh_token: None,
            scopes: vec![],
        })
    }
}
async fn fixture() -> (Auth, Arc<TestGoogle>, tokio::task::JoinHandle<()>) {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let origin = format!("http://{}", listener.local_addr().unwrap());
    let cards = Arc::new(Mutex::new(HashMap::<String, String>::new()));
    let write = cards.clone();
    let read = cards;
    let routes = Router::new()
        .route(
            "/v1/agents/{id}",
            axum::routing::put(move |Path(id): Path<String>, Json(body): Json<Value>| {
                let write = write.clone();
                async move {
                    let canonical = a2a_card::validate_value(body["agentCard"].clone()).unwrap();
                    write
                        .lock()
                        .unwrap()
                        .insert(id, String::from_utf8(canonical.bytes).unwrap());
                    StatusCode::OK
                }
            }),
        )
        .route(
            "/v1/agents/{id}/agent-card.json",
            get(move |Path(id): Path<String>| {
                let read = read.clone();
                async move { read.lock().unwrap().get(&id).cloned().unwrap() }
            }),
        );
    let task = tokio::spawn(async move { axum::serve(listener, routes).await.unwrap() });
    let provider = Arc::new(TestGoogle::default());
    (
        Auth {
            store: Arc::new(MemoryAuthStore::default()),
            agents: Arc::new(MemoryStore::default()),
            provider: provider.clone(),
            registry: crate::registry::Registry::new(&origin).unwrap(),
            base: "https://api.calendar.test".into(),
            website: "https://calendar.test".into(),
            allowed_emails: vec!["test@example.com".into()],
            connected: None,
        },
        provider,
        task,
    )
}
async fn call(
    s: &Auth,
    path: &str,
    method: &str,
    cookies: Option<&str>,
    origin: Option<&str>,
) -> Response {
    let mut req = Request::builder().uri(path).method(method);
    if let Some(c) = cookies {
        req = req.header(header::COOKIE, c);
    }
    if let Some(o) = origin {
        req = req.header(header::ORIGIN, o);
    }
    router(s.clone())
        .oneshot(req.body(Body::empty()).unwrap())
        .await
        .unwrap()
}
fn response_cookie(response: &Response, name: &str) -> String {
    response
        .headers()
        .get_all(header::SET_COOKIE)
        .iter()
        .filter_map(|h| h.to_str().ok())
        .find(|v| v.starts_with(&format!("{name}=")))
        .unwrap()
        .split(';')
        .next()
        .unwrap()
        .into()
}
async fn begin(s: &Auth) -> (String, String) {
    let response = call(s, "/auth/google/start?host=alice", "GET", None, None).await;
    assert_eq!(response.status(), StatusCode::SEE_OTHER);
    assert_eq!(response.headers()[header::CACHE_CONTROL], "no-store");
    let location =
        reqwest::Url::parse(response.headers()[header::LOCATION].to_str().unwrap()).unwrap();
    let state = location
        .query_pairs()
        .find(|(k, _)| k == "state")
        .unwrap()
        .1
        .into_owned();
    (state, response_cookie(&response, LOGIN_COOKIE))
}
async fn login(s: &Auth, subject: &str) -> String {
    let (state, cookies) = begin(s).await;
    let response = call(
        s,
        &format!("/auth/google/callback?state={state}&code={subject}"),
        "GET",
        Some(&cookies),
        None,
    )
    .await;
    assert_eq!(
        response.headers()[header::LOCATION],
        "https://calendar.test/account?host=alice"
    );
    response_cookie(&response, SESSION_COOKIE)
}
async fn value(response: Response) -> Value {
    serde_json::from_slice(&to_bytes(response.into_body(), 65536).await.unwrap()).unwrap()
}

#[tokio::test]
async fn login_reuses_account_and_agent_without_exposing_identity() {
    let (s, _, server) = fixture().await;
    let cookie = login(&s, "subject-one").await;
    let profile = value(call(&s, "/auth/me", "GET", Some(&cookie), None).await).await;
    assert_eq!(profile["email"], "test@example.com");
    assert_eq!(profile["calendar_connected"], false);
    let agent = value(call(&s, "/auth/agent", "POST", Some(&cookie), Some(&s.website)).await).await;
    assert_eq!(agent["publication_status"], "published");
    assert_eq!(agent["id"], profile["id"]);
    let again = login(&s, "subject-one").await;
    let second = value(call(&s, "/auth/agent", "POST", Some(&again), Some(&s.website)).await).await;
    assert_eq!(agent, second);
    let record = s
        .agents
        .get(profile["id"].as_str().unwrap())
        .await
        .unwrap()
        .unwrap();
    assert!(!record.card_bytes.contains("test@example.com"));
    assert!(!record.card_bytes.contains("subject-one"));
    assert!(!record.card_bytes.contains("Private Person"));
    let card: Value = serde_json::from_str(&record.card_bytes).unwrap();
    assert_eq!(card["skills"].as_array().unwrap().len(), 3);
    assert_eq!(card["skills"][0]["id"], "greeting");
    let protocol = crate::app_with_store(
        &s.base,
        &format!("{}/.well-known/ai-catalog.json", s.base),
        s.agents.clone(),
        None,
        s.website.clone(),
    )
    .unwrap();
    let reply = protocol.oneshot(Request::builder().uri("/a2a").method("POST").header("content-type","application/json")
        .body(Body::from(json!({"jsonrpc":"2.0","id":"test","method":"SendMessage","params":{"tenant":profile["id"],"message":{"messageId":"test-message","role":"ROLE_USER","parts":[{"data":{"operation":"get_availability"}}]}}}).to_string())).unwrap()).await.unwrap();
    let reply = value(reply).await;
    assert!(
        reply.get("error").is_some() && reply.get("result").is_none(),
        "account agents must not fall through to mock availability"
    );

    let other = login(&s, "subject-two").await;
    let other_profile = value(call(&s, "/auth/me", "GET", Some(&other), None).await).await;
    assert_ne!(
        profile["id"], other_profile["id"],
        "same email with another sub must not claim an account"
    );
    assert_eq!(s.agents.published().await.unwrap().len(), 1);
    server.abort();
}
#[tokio::test]
async fn callback_requires_browser_binding_and_cannot_be_replayed() {
    let (s, provider, server) = fixture().await;
    let (state, cookies) = begin(&s).await;
    let path = format!("/auth/google/callback?state={state}&code=subject-one");
    let wrong = format!("{LOGIN_COOKIE}={}", random());
    for cookie in [None, Some(wrong.as_str())] {
        let response = call(&s, &path, "GET", cookie, None).await;
        assert!(
            response.headers()[header::LOCATION]
                .to_str()
                .unwrap()
                .ends_with("error=invalid_login")
        );
    }
    assert_eq!(provider.calls.load(Ordering::SeqCst), 0);
    let (first, second) = tokio::join!(
        call(&s, &path, "GET", Some(&cookies), None),
        call(&s, &path, "GET", Some(&cookies), None)
    );
    let locations =
        [first, second].map(|r| r.headers()[header::LOCATION].to_str().unwrap().to_owned());
    assert_eq!(
        locations
            .iter()
            .filter(|s| s.ends_with("?host=alice"))
            .count(),
        1
    );
    assert_eq!(provider.calls.load(Ordering::SeqCst), 1);
    server.abort();
}
#[tokio::test]
async fn expiry_logout_and_cross_origin_requests_fail_closed() {
    let (s, _, server) = fixture().await;
    assert_eq!(
        call(&s, "/auth/me", "GET", None, None).await.status(),
        StatusCode::UNAUTHORIZED
    );
    let cookie = login(&s, "subject-one").await;
    for origin in [None, Some("https://evil.example")] {
        assert_eq!(
            call(&s, "/auth/agent", "POST", Some(&cookie), origin)
                .await
                .status(),
            StatusCode::FORBIDDEN
        );
        assert_eq!(
            call(&s, "/auth/logout", "POST", Some(&cookie), origin)
                .await
                .status(),
            StatusCode::FORBIDDEN
        );
    }
    assert_eq!(
        call(&s, "/auth/logout", "POST", Some(&cookie), Some(&s.website))
            .await
            .status(),
        StatusCode::NO_CONTENT
    );
    assert_eq!(
        call(&s, "/auth/me", "GET", Some(&cookie), None)
            .await
            .status(),
        StatusCode::UNAUTHORIZED
    );
    let expired = random();
    s.store
        .create(
            &key("session", &expired),
            Entry {
                value: json!({"account_key":"unused"}),
                expires: now() - 1,
                binding: String::new(),
            },
        )
        .await
        .unwrap();
    assert_eq!(
        call(
            &s,
            "/auth/me",
            "GET",
            Some(&format!("{SESSION_COOKIE}={expired}")),
            None
        )
        .await
        .status(),
        StatusCode::UNAUTHORIZED
    );
    s.store
        .create(
            "expired-attempt",
            Entry {
                value: json!({}),
                expires: now() - 1,
                binding: "bound".into(),
            },
        )
        .await
        .unwrap();
    assert!(
        s.store
            .consume("expired-attempt", "bound", now())
            .await
            .unwrap()
            .is_none()
    );
    assert_eq!(
        call(
            &s,
            "/auth/google/start?host=https://evil.example",
            "GET",
            None,
            None
        )
        .await
        .status(),
        StatusCode::BAD_REQUEST
    );
    server.abort();
}
#[tokio::test]
async fn denial_and_failed_verification_never_create_a_session() {
    let (s, _, server) = fixture().await;
    for tail in ["error=access_denied", "code=invalid", "code=outside"] {
        let (state, cookies) = begin(&s).await;
        let response = call(
            &s,
            &format!("/auth/google/callback?state={state}&{tail}"),
            "GET",
            Some(&cookies),
            None,
        )
        .await;
        assert!(
            response.headers()[header::LOCATION]
                .to_str()
                .unwrap()
                .contains("?error=")
        );
        assert!(
            !response
                .headers()
                .get_all(header::SET_COOKIE)
                .iter()
                .any(|c| c.to_str().unwrap().starts_with(SESSION_COOKIE))
        );
    }
    assert!(s.agents.published().await.unwrap().is_empty());
    server.abort();
}
#[tokio::test]
async fn simultaneous_logins_share_one_identity_and_retry_publication() {
    let (s, _, server) = fixture().await;
    let (a, b) = tokio::join!(login(&s, "same-sub"), login(&s, "same-sub"));
    let a_profile = value(call(&s, "/auth/me", "GET", Some(&a), None).await).await;
    let b_profile = value(call(&s, "/auth/me", "GET", Some(&b), None).await).await;
    assert_eq!(a_profile["id"], b_profile["id"]);
    let mut down = s.clone();
    down.registry = crate::registry::Registry::new("http://127.0.0.1:1").unwrap();
    let pending = call(&down, "/auth/agent", "POST", Some(&a), Some(&s.website)).await;
    assert_eq!(pending.status(), StatusCode::ACCEPTED);
    let pending = value(pending).await;
    // Signed bytes and registry URL are preserved, including when registry recovery is needed.
    let again = value(call(&down, "/auth/agent", "POST", Some(&a), Some(&s.website)).await).await;
    assert_eq!(pending["id"], again["id"]);
    assert_eq!(pending["agent_card_url"], again["agent_card_url"]);
    server.abort();
}

#[tokio::test]
async fn calendar_consent_requires_session_and_rejects_account_switching() {
    let (s, _, server) = fixture().await;
    let missing = call(&s, "/auth/google/start?calendar=true", "GET", None, None).await;
    assert_eq!(
        missing.headers()[header::LOCATION],
        "https://calendar.test/account?error=sign_in_required"
    );
    let existing = login(&s, "subject-one").await;
    let before = value(call(&s, "/auth/me", "GET", Some(&existing), None).await).await;
    let start = call(
        &s,
        "/auth/google/start?calendar=true",
        "GET",
        Some(&existing),
        None,
    )
    .await;
    let url = reqwest::Url::parse(start.headers()[header::LOCATION].to_str().unwrap()).unwrap();
    let state = url
        .query_pairs()
        .find(|(k, _)| k == "state")
        .unwrap()
        .1
        .into_owned();
    let binding = response_cookie(&start, LOGIN_COOKIE);
    let response = call(
        &s,
        &format!("/auth/google/callback?state={state}&code=another-subject"),
        "GET",
        Some(&format!("{binding}; {existing}")),
        None,
    )
    .await;
    assert_eq!(
        response.headers()[header::LOCATION],
        "https://calendar.test/account?error=wrong_google_account"
    );
    assert_eq!(
        value(call(&s, "/auth/me", "GET", Some(&existing), None).await).await,
        before
    );
    server.abort();
}
