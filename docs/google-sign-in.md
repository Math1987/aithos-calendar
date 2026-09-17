# Gate: Google sign-in and a persistent A2A identity

## Scope

Google sign-in creates or reuses one private profile and one public A2A identity.
There is no Calendar authorization, calendar reading, meeting negotiation or
booking for these new identities yet. No LLM is involved. Existing public
booking-page identities and their scheduling flow remain available independently.

The Google project/client setup is recorded in [google-oauth-setup.md](google-oauth-setup.md).

## Browser flow

1. Open `/account`, or follow Continue with Google on the home page.
2. The backend saves a ten-minute OAuth attempt and redirects to Google.
3. Google returns an authorization code to `/auth/google/callback`.
4. The backend consumes the browser-bound state once, exchanges the code with
   PKCE, and uses the `openidconnect` crate to validate the signed ID token,
   issuer, audience, expiry, nonce, and access-token hash when present. A verified
   email is required. This pilot enforces an explicit two-account allowlist.
5. The verified Google `sub` selects a private account mapping. Its public agent
   ID is a separate random 256-bit value. Reconnecting never claims an old public
   booking-page identity, even if the email or booking-page URL matches.
6. A new opaque, HttpOnly session cookie returns the browser to `/account`.
7. The page calls authenticated `POST /auth/agent` to create or resume publication
   of the same signed AgentCard in Aithos. A published card appears in the catalog.
   Retrying a pending publication reuses both the agent ID and signed card.

Opening an account agent's `/book/{id}` link lets a visitor sign in and get their
own identity. The page explicitly explains that Calendar scheduling is the next
gate. The host ID survives sign-in as context, not as authority over that account.

## API and data boundaries

| Route | Purpose |
| --- | --- |
| `GET /auth/google/start?host=<optional agent ID>` | Start Google login; fixed return destination |
| `GET /auth/google/callback` | One-time state + PKCE + OIDC verification |
| `GET /auth/me` | Current private profile, or 401 |
| `POST /auth/agent` | Create/reuse/publish only the authenticated user's agent |
| `POST /auth/logout` | Delete the current server session and clear its cookie |

All auth responses use `Cache-Control: no-store` and `Referrer-Policy: no-referrer`.
Session and login cookies use the `__Host-` prefix, Secure, HttpOnly, Path=/,
SameSite=Lax, with no Domain attribute. Sessions expire after 12 hours, including
when DynamoDB has not yet removed an expired TTL record. Session tokens are
hashed in storage. Login state and browser bindings are also stored as digests;
PKCE verifier and nonce live only in the private expiring attempt record.

Mutating authenticated routes require the exact website Origin. API Gateway
allows credentialed CORS only for `https://calendar.aithos.world`. Legacy anonymous
requests still omit cookies. No Google tokens, browser sessions, email addresses
or profile names go into the AgentCard, catalog, browser localStorage, or logs.
Google access/ID tokens are used transiently for login and discarded; refresh
tokens are neither requested nor stored at this gate.

Public account cards use version `0.5.0` and advertise only greeting. Their A2A
endpoint rejects availability and scheduling operations until the connector is
implemented. `tenant` remains routing information and proves no caller identity.

Private account mappings, attempts and sessions use the separate
`calendar-production-auth` table (encryption at rest, PITR, expiry for transient
rows). The client secret is held in Secrets Manager at
`calendar/production/google-oauth-client`; Terraform manages metadata only.
Runtime IAM permits only the required table operations and that secret.

## Deployment order

1. With valid operator AWS credentials, run `scripts/bootstrap.py plan` through
   `scripts/with-env.py`, review the plan, then apply it. This creates only the
   OAuth secret metadata and the scoped runtime/deployment policies.
2. Run `python3 scripts/with-env.py python3 scripts/store-google-secret.py`.
   It checks the AWS account and Google configuration, uploads the value to the
   existing secret and verifies it without printing it.
3. Merge/push the tested change to `main` so GitHub Actions builds the Linux
   Lambda and applies production Terraform. Do not apply a local stale Lambda
   artifact. New auth table, routes, environment and credentialed CORS deploy
   together with the web page.
4. CI checks health, existing A2A, browser routes and `scripts/smoke-auth.py`.
   The auth smoke test creates and consumes one expiring login attempt; it does
   not sign in, publish an agent, request Calendar access or book anything.

## Manual acceptance after deployment

1. Open `https://calendar.aithos.world/account` and click Continue with Google.
2. Sign in with one configured test account. Only identity/profile permissions
   should be requested. Verify the account email and “Your agent is ready”.
3. Note the agent identifier and link under Agent details. Open the AgentCard and
   confirm it advertises greeting only and contains no email/name from Google.
4. Reload: the session and identity should remain the same.
5. Sign out: `/auth/me` should now return 401. Sign in again with the same Google
   account: the agent ID and registry URL must remain unchanged.
6. Open the share link in a second browser profile and sign in with the other
   test account. Its agent must have a different ID. Each account keeps its own
   identity and the UI explains that scheduling is not enabled yet.
7. Cancel Google login: expect a clear retry message and no new session.
8. An expired or replayed callback must fail and offer a fresh login.

Backend automated tests cover state binding/replay, signed ID-token validation,
expiry, cancellation, the test-account boundary, cross-origin mutations,
logout, concurrent mapping creation, publication retries, identity reuse and
public-card privacy. Production smoke checks never consent to Google access or
create a real user session.

## Next gate

Add incremental Calendar consent, encrypted refresh-token storage, timezone and
FreeBusy reading, followed by authenticated A2A availability exchange. Keep
calendar credentials with their owner's agent. Booking through the official
Calendar API is a subsequent gate with explicit user confirmation and retry
protection.

## Local validation — 2026-09-17

- 48 offline Rust tests passed; the two pre-existing live Google-page tests remain
  opt-in. The account agent's refusal of Calendar operations was also checked.
- Terraform validation passed for both bootstrap and production.
- Rust formatting, browser JavaScript syntax and Git whitespace checks passed.
- Isolated browser checks verified the signed-in agent page, sign-out, cancelled
  sign-in message and home-page coexistence with public booking links.
- The OAuth client secret is absent from tracked/unignored files.
- Deployment is pending renewal of operator AWS credentials: STS returned
  `ExpiredToken`. No AWS changes or production deployment were made for this gate.
