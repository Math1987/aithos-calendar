//! Fixed-window rate limits on the private store, so they hold across
//! Lambda instances. Each limit is a counter row `limit:<scope>:<subject>`
//! that expires with its window; the store's TTL cleans it up.
//!
//! The limits protect the expensive or abusable paths once any Google
//! account can sign in: anonymous agent creation and schedule reads (per
//! client IP), autonomous tasks and proposals (per account), inbound
//! bookings (per host) and the scenario lab (globally). They are
//! deliberately coarse; API Gateway throttling remains the first line.
use crate::auth_store::AuthStore;
use axum::{
    extract::FromRequestParts,
    http::{HeaderMap, StatusCode, header, request::Parts},
    response::{IntoResponse, Response},
};
use lambda_http::RequestExt;
use serde_json::json;
use sha2::{Digest, Sha256};
use std::sync::Arc;

/// One limit: at most `max` hits per `window` seconds for one subject.
#[derive(Clone, Copy, Debug)]
pub struct Limit {
    pub scope: &'static str,
    pub max: u64,
    pub window: i64,
}

pub const AGENT_CREATION_PER_IP: Limit = Limit {
    scope: "agents",
    max: 10,
    window: 3600,
};
pub const SCHEDULE_READS_PER_IP: Limit = Limit {
    scope: "schedule",
    max: 60,
    window: 3600,
};
pub const LOGIN_STARTS_PER_IP: Limit = Limit {
    scope: "login",
    max: 30,
    window: 600,
};
pub const TASKS_PER_ACCOUNT: Limit = Limit {
    scope: "tasks",
    max: 20,
    window: 86_400,
};
pub const PROPOSALS_PER_ACCOUNT: Limit = Limit {
    scope: "proposals",
    max: 60,
    window: 86_400,
};
pub const INBOUND_PROPOSALS_PER_HOST: Limit = Limit {
    scope: "inbound",
    max: 20,
    window: 86_400,
};
pub const LAB_RUNS_GLOBAL: Limit = Limit {
    scope: "lab",
    max: 3,
    window: 60,
};

#[derive(Debug, PartialEq, Eq)]
pub enum LimitError {
    /// Over the limit; seconds until the window ends.
    Exceeded {
        retry_after: i64,
    },
    Unavailable,
}

impl IntoResponse for LimitError {
    fn into_response(self) -> Response {
        match self {
            Self::Exceeded { retry_after } => (
                StatusCode::TOO_MANY_REQUESTS,
                [(header::RETRY_AFTER, retry_after.to_string())],
                axum::Json(json!({"error":"rate_limited","retry_after_seconds":retry_after})),
            )
                .into_response(),
            Self::Unavailable => (
                StatusCode::SERVICE_UNAVAILABLE,
                axum::Json(json!({"error":"storage_unavailable"})),
            )
                .into_response(),
        }
    }
}

#[derive(Clone)]
pub struct Limits {
    store: Arc<dyn AuthStore>,
}

impl Limits {
    pub fn new(store: Arc<dyn AuthStore>) -> Self {
        Self { store }
    }
    /// Count one hit for `subject` and refuse when the window is full. The
    /// subject is hashed so no IP address or account id sits in a key.
    pub async fn hit(&self, limit: Limit, subject: &str) -> Result<(), LimitError> {
        let now = crate::auth::now();
        let window_end = now - now.rem_euclid(limit.window) + limit.window;
        let key = format!(
            "limit:{}:{:x}",
            limit.scope,
            Sha256::digest(format!("{}:{subject}", limit.scope))
        );
        let count = self
            .store
            .increment(&key, window_end)
            .await
            .map_err(|_| LimitError::Unavailable)?;
        if count > limit.max {
            tracing::info!(event = "rate_limited", scope = limit.scope);
            return Err(LimitError::Exceeded {
                retry_after: (window_end - now).max(1),
            });
        }
        Ok(())
    }
}

/// The caller's network address as API Gateway saw it. Behind API Gateway
/// the request context is authoritative; the last `x-forwarded-for` entry
/// is what a proxy appended, the first is whatever the client sent.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ClientIp(pub String);

impl ClientIp {
    pub fn from_parts(headers: &HeaderMap, extensions: &axum::http::Extensions) -> Self {
        if let Some(lambda_http::request::RequestContext::ApiGatewayV2(context)) =
            extensions.request_context_ref()
            && let Some(ip) = context.http.source_ip.as_deref()
            && !ip.is_empty()
        {
            return Self(ip.to_owned());
        }
        let forwarded = headers
            .get("x-forwarded-for")
            .and_then(|v| v.to_str().ok())
            .and_then(|v| v.rsplit(',').next())
            .map(str::trim)
            .filter(|v| !v.is_empty() && v.len() <= 64);
        Self(forwarded.unwrap_or("unknown").to_owned())
    }
}

impl<S: Send + Sync> FromRequestParts<S> for ClientIp {
    type Rejection = std::convert::Infallible;
    async fn from_request_parts(parts: &mut Parts, _: &S) -> Result<Self, Self::Rejection> {
        Ok(Self::from_parts(&parts.headers, &parts.extensions))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::auth_store::MemoryAuthStore;

    #[tokio::test]
    async fn a_window_admits_max_hits_then_refuses_until_it_ends() {
        let limits = Limits::new(Arc::new(MemoryAuthStore::default()));
        let limit = Limit {
            scope: "test",
            max: 2,
            window: 3600,
        };
        assert_eq!(limits.hit(limit, "a").await, Ok(()));
        assert_eq!(limits.hit(limit, "a").await, Ok(()));
        let refused = limits.hit(limit, "a").await.unwrap_err();
        assert!(
            matches!(refused, LimitError::Exceeded { retry_after } if (1..=3600).contains(&retry_after))
        );
        // Another subject and another scope have their own windows.
        assert_eq!(limits.hit(limit, "b").await, Ok(()));
        let other = Limit {
            scope: "other",
            ..limit
        };
        assert_eq!(limits.hit(other, "a").await, Ok(()));
        assert_eq!(
            refused.into_response().status(),
            StatusCode::TOO_MANY_REQUESTS
        );
    }

    #[test]
    fn client_ip_prefers_the_gateway_context_then_the_appended_forwarded_entry() {
        let mut headers = HeaderMap::new();
        headers.insert("x-forwarded-for", "1.2.3.4, 10.0.0.9".parse().unwrap());
        let extensions = axum::http::Extensions::new();
        assert_eq!(
            ClientIp::from_parts(&headers, &extensions),
            ClientIp("10.0.0.9".into())
        );
        assert_eq!(
            ClientIp::from_parts(&HeaderMap::new(), &extensions),
            ClientIp("unknown".into())
        );
    }
}
