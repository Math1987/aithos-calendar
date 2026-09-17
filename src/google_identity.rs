//! OIDC verifies identity; explicit incremental consent may also return Calendar credentials.
use async_trait::async_trait;
use openidconnect::{
    core::{CoreClient, CoreProviderMetadata},
    *,
};
use std::{sync::Arc, time::Duration};

pub struct VerifiedIdentity {
    pub sub: String,
    pub email: String,
    pub name: String,
    pub refresh_token: Option<String>,
    pub scopes: Vec<String>,
}
#[async_trait]
pub trait IdentityProvider: Send + Sync {
    fn authorization_url(&self, state: &str, nonce: &str, verifier: &str, calendar: bool)
    -> String;
    async fn verify(
        &self,
        code: &str,
        nonce: &str,
        verifier: &str,
    ) -> Result<VerifiedIdentity, &'static str>;
}
pub struct GoogleIdentity {
    client_id: String,
    redirect_uri: String,
    secret_id: String,
    secrets: aws_sdk_secretsmanager::Client,
    secret: tokio::sync::OnceCell<String>,
    http: reqwest::Client,
}
impl GoogleIdentity {
    pub fn new(
        client_id: String,
        redirect_uri: String,
        secret_id: String,
        secrets: aws_sdk_secretsmanager::Client,
    ) -> Result<Self, lambda_http::Error> {
        Ok(Self {
            client_id,
            redirect_uri,
            secret_id,
            secrets,
            secret: tokio::sync::OnceCell::new(),
            http: reqwest::Client::builder()
                .timeout(Duration::from_secs(5))
                .connect_timeout(Duration::from_secs(2))
                .redirect(reqwest::redirect::Policy::none())
                .build()?,
        })
    }
}
#[async_trait]
impl IdentityProvider for GoogleIdentity {
    fn authorization_url(
        &self,
        state: &str,
        nonce: &str,
        verifier: &str,
        calendar: bool,
    ) -> String {
        // Authorization endpoint is fixed; discovery/JWKS is fetched for each callback.
        let challenge =
            PkceCodeChallenge::from_code_verifier_sha256(&PkceCodeVerifier::new(verifier.into()));
        let mut url = reqwest::Url::parse("https://accounts.google.com/o/oauth2/v2/auth").unwrap();
        url.query_pairs_mut().extend_pairs([
            ("client_id", self.client_id.as_str()),
            ("redirect_uri", self.redirect_uri.as_str()),
            ("response_type", "code"),
            ("scope", "openid email profile"),
            ("state", state),
            ("nonce", nonce),
            ("code_challenge", challenge.as_str()),
            ("code_challenge_method", "S256"),
            ("prompt", "select_account"),
        ]);
        if calendar {
            url.query_pairs_mut()
                .append_pair("access_type", "offline")
                .append_pair("include_granted_scopes", "true")
                .append_pair("prompt", "consent select_account");
            // Replace identity-only scope/prompt instead of emitting duplicate parameters.
            let pairs: Vec<(String, String)> = url
                .query_pairs()
                .filter(|(k, _)| k != "scope" && k != "prompt")
                .map(|(k, v)| (k.into_owned(), v.into_owned()))
                .collect();
            url.set_query(None);
            url.query_pairs_mut()
                .extend_pairs(pairs)
                .append_pair("scope", crate::google_calendar::SCOPES)
                .append_pair("prompt", "consent select_account");
        }
        url.into()
    }
    async fn verify(
        &self,
        code: &str,
        nonce: &str,
        verifier: &str,
    ) -> Result<VerifiedIdentity, &'static str> {
        let secret = self
            .secret
            .get_or_try_init(|| async {
                let result = self
                    .secrets
                    .get_secret_value()
                    .secret_id(&self.secret_id)
                    .send()
                    .await
                    .map_err(|_| "google_unavailable")?;
                result
                    .secret_string()
                    .filter(|s| !s.is_empty())
                    .map(str::to_owned)
                    .ok_or("google_unavailable")
            })
            .await?;
        // Adapt the application's existing reqwest version to the SDK's HTTP interface.
        let http_client = self.http.clone();
        let http = move |request: HttpRequest| {
            let client = http_client.clone();
            async move {
                let (parts, body) = request.into_parts();
                let response = client
                    .request(parts.method, parts.uri.to_string())
                    .headers(parts.headers)
                    .body(body)
                    .send()
                    .await
                    .map_err(|_| std::io::Error::other("google_unavailable"))?;
                let status = response.status();
                let headers = response.headers().clone();
                let mut bytes = Vec::new();
                let mut response = response;
                while let Some(chunk) = response
                    .chunk()
                    .await
                    .map_err(|_| std::io::Error::other("google_unavailable"))?
                {
                    if bytes.len() + chunk.len() > 256 * 1024 {
                        return Err(std::io::Error::other("google_unavailable"));
                    }
                    bytes.extend_from_slice(&chunk);
                }
                let mut result = HttpResponse::new(bytes);
                *result.status_mut() = status;
                *result.headers_mut() = headers;
                Ok::<_, std::io::Error>(result)
            }
        };
        let metadata = CoreProviderMetadata::discover_async(
            IssuerUrl::new("https://accounts.google.com".into()).unwrap(),
            &http,
        )
        .await
        .map_err(|_| "google_unavailable")?;
        let client = CoreClient::from_provider_metadata(
            metadata,
            ClientId::new(self.client_id.clone()),
            Some(ClientSecret::new(secret.clone())),
        )
        .set_auth_type(AuthType::RequestBody)
        .set_redirect_uri(
            RedirectUrl::new(self.redirect_uri.clone()).map_err(|_| "google_unavailable")?,
        );
        let tokens = client
            .exchange_code(AuthorizationCode::new(code.into()))
            .map_err(|_| "google_unavailable")?
            .set_pkce_verifier(PkceCodeVerifier::new(verifier.into()))
            .request_async(&http)
            .await
            .map_err(|_| "google_login_failed")?;
        let token = tokens.id_token().ok_or("google_login_failed")?;
        let verifier = client.id_token_verifier();
        let mut identity = verify_token(token, &verifier, nonce, tokens.access_token())?;
        identity.refresh_token = tokens.refresh_token().map(|t| t.secret().clone());
        identity.scopes = tokens
            .scopes()
            .map(|s| s.iter().map(|s| s.as_str().to_owned()).collect())
            .unwrap_or_default();
        Ok(identity)
    }
}
fn verify_token(
    token: &core::CoreIdToken,
    verifier: &core::CoreIdTokenVerifier<'_>,
    nonce: &str,
    access_token: &AccessToken,
) -> Result<VerifiedIdentity, &'static str> {
    let claims = token
        .claims(verifier, &Nonce::new(nonce.into()))
        .map_err(|_| "google_login_failed")?;
    if let Some(expected) = claims.access_token_hash() {
        let actual = AccessTokenHash::from_token(
            access_token,
            token.signing_alg().map_err(|_| "google_login_failed")?,
            token
                .signing_key(verifier)
                .map_err(|_| "google_login_failed")?,
        )
        .map_err(|_| "google_login_failed")?;
        if &actual != expected {
            return Err("google_login_failed");
        }
    }
    if claims.email_verified() != Some(true) {
        return Err("google_login_failed");
    }
    let email = claims
        .email()
        .ok_or("google_login_failed")?
        .as_str()
        .to_owned();
    let name = claims
        .name()
        .and_then(|v| v.get(None))
        .map(|v| v.as_str())
        .unwrap_or("Calendar user")
        .to_owned();
    Ok(VerifiedIdentity {
        sub: claims.subject().as_str().into(),
        email,
        name,
        refresh_token: None,
        scopes: vec![],
    })
}

pub type Provider = Arc<dyn IdentityProvider>;

#[cfg(test)]
mod tests {
    use super::*;
    use openidconnect::core::{
        CoreEdDsaPrivateSigningKey, CoreIdToken, CoreIdTokenClaims, CoreIdTokenVerifier,
        CoreJsonWebKeySet, CoreJwsSigningAlgorithm,
    };
    #[test]
    fn oidc_rejects_invalid_claims_signatures_and_token_substitution() {
        // Public deterministic TEST key, never used outside this test.
        let pem = "-----BEGIN PRIVATE KEY-----\nMC4CAQAwBQYDK2VwBCIEIAcHBwcHBwcHBwcHBwcHBwcHBwcHBwcHBwcHBwcHBwcH\n-----END PRIVATE KEY-----";
        let signing = CoreEdDsaPrivateSigningKey::from_ed25519_pem(pem, None).unwrap();
        let now: chrono::DateTime<chrono::Utc> = std::time::SystemTime::now().into();
        let access = AccessToken::new("test-access".into());
        let verifier = CoreIdTokenVerifier::new_public_client(
            ClientId::new("our-client".into()),
            IssuerUrl::new("https://accounts.google.com".into()).unwrap(),
            CoreJsonWebKeySet::new(vec![signing.as_verification_key()]),
        )
        .set_allowed_algs(vec![CoreJwsSigningAlgorithm::EdDsa]);
        let token =
            |issuer: &str, audience: &str, expiry: i64, nonce: &str, email_verified: bool| {
                CoreIdToken::new(
                    CoreIdTokenClaims::new(
                        IssuerUrl::new(issuer.into()).unwrap(),
                        vec![Audience::new(audience.into())],
                        now + chrono::Duration::seconds(expiry),
                        now,
                        StandardClaims::new(SubjectIdentifier::new("stable-sub".into()))
                            .set_email(Some(EndUserEmail::new("user@example.com".into())))
                            .set_email_verified(Some(email_verified)),
                        Default::default(),
                    )
                    .set_nonce(Some(Nonce::new(nonce.into()))),
                    &signing,
                    CoreJwsSigningAlgorithm::EdDsa,
                    Some(&access),
                    None,
                )
                .unwrap()
            };
        let good = token(
            "https://accounts.google.com",
            "our-client",
            300,
            "bound-nonce",
            true,
        );
        assert_eq!(
            verify_token(&good, &verifier, "bound-nonce", &access)
                .unwrap()
                .sub,
            "stable-sub"
        );
        for bad in [
            token(
                "https://wrong.example",
                "our-client",
                300,
                "bound-nonce",
                true,
            ),
            token(
                "https://accounts.google.com",
                "another-client",
                300,
                "bound-nonce",
                true,
            ),
            token(
                "https://accounts.google.com",
                "our-client",
                -10,
                "bound-nonce",
                true,
            ),
            token(
                "https://accounts.google.com",
                "our-client",
                300,
                "another-nonce",
                true,
            ),
            token(
                "https://accounts.google.com",
                "our-client",
                300,
                "bound-nonce",
                false,
            ),
        ] {
            assert!(verify_token(&bad, &verifier, "bound-nonce", &access).is_err());
        }
        assert!(
            verify_token(
                &good,
                &verifier,
                "bound-nonce",
                &AccessToken::new("substituted".into())
            )
            .is_err()
        );
        let mut raw = serde_json::to_value(&good)
            .unwrap()
            .as_str()
            .unwrap()
            .to_owned();
        let signature_start = raw.rfind('.').unwrap() + 1;
        raw.replace_range(
            signature_start..signature_start + 1,
            if &raw[signature_start..signature_start + 1] == "A" {
                "B"
            } else {
                "A"
            },
        );
        let forged: CoreIdToken = serde_json::from_value(serde_json::json!(raw)).unwrap();
        assert!(verify_token(&forged, &verifier, "bound-nonce", &access).is_err());
    }
}
