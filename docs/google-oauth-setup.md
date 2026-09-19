# Google OAuth setup for the A2A Calendar POC

## Status

| Item | Value | State |
| --- | --- | --- |
| Google Cloud project display name | `A2A Calendar POC` | done 2026-09-19 |
| Project ID / number | `aithos-calendar` / `235708078636` (the ID cannot change) | fixed |
| Organization | `aithos.fr` (`1034671875644`) | fixed |
| Google Calendar API | Enabled (`calendar-json.googleapis.com`) | done |
| OAuth application name (consent screen) | `A2A Calendar POC` | done; branding validated and published 2026-09-19 |
| Google Auth Platform audience | External | done |
| Publishing status | **In production** since 2026-09-19 (scope verification pending, see `google-verification.md`) | done |
| Support and developer contact | `mathieu@aithos.fr` | done |
| Application home page | `https://calendar.aithos.world/` | done |
| Privacy policy / terms of service | `https://calendar.aithos.world/privacy`, `https://calendar.aithos.world/terms` | done |
| Authorized domain | `aithos.world`; `https://calendar.aithos.world/` verified in Search Console (HTML tag) | done |
| Consent-screen scopes (Data access) | `openid`, `email`, `profile`, `…/auth/calendar.freebusy`, `…/auth/calendar.events.owned`, `…/auth/calendar.calendarlist.readonly` | done; only `calendar.events.owned` is classed sensitive |
| OAuth client | `A2A Calendar POC Web`, Web application, ID `235708078636-686f8i71em5mmsn1b29prrfv4tl8gpt3.apps.googleusercontent.com` | renamed; ID unchanged |
| Authorized redirect URI | `https://api.calendar.aithos.world/auth/google/callback` | done |
| Authorized JavaScript origins | none | done |

Access in the backend is configured, not hard-coded (`src/auth.rs::Access`):

| `GOOGLE_OAUTH_ACCESS` | Effect |
| --- | --- |
| `public` | any Google account may sign in (production, `infra/production/api.tf`) |
| `allowlist` or unset | only the addresses in `GOOGLE_OAUTH_TEST_USERS` (the pilot mode; the safe default when the variable is missing) |

The allow-list is enforced server-side because Google exempts basic sign-in from its Testing restrictions.

## Scopes and what each one does

The consent screen must declare every scope the code requests
(`src/google_calendar.rs::SCOPES`). Sign-in requests only
`openid email profile`; the Calendar scopes are requested in a second,
explicit consent (`/auth/google/start?calendar=true`, offline access).
`docs/google-verification.md` has the per-scope justification written for
Google's reviewers; in short:

| Scope | Feature | Code |
| --- | --- | --- |
| `calendar.freebusy` | free/busy of the primary calendar in a window ≤ 30 days, reduced to weekday 09:00–18:00 slots | `GoogleCalendar::availability` |
| `calendar.calendarlist.readonly` | the primary calendar's time zone | `GoogleCalendar::availability` (`calendarList/primary`) |
| `calendar.events.owned` | past meetings with the peer (habits), the "Host / Guest" event, the guest's acceptance | `GoogleCalendar::history`, `insert`, `accept` |

Refresh tokens are encrypted with the KMS key `alias/calendar-production-google-tokens`
(encryption context bound to the account) and stored in the private auth table;
access tokens are minted per operation and never stored. In production
(published app) refresh tokens no longer expire after seven days as they do in Testing.

## Verification

The three Calendar scopes are sensitive, so a published app needs Google's
verification. Until it is granted the app works with the "unverified app"
screen and a cap of 100 new users. The dossier, the demo-video script and
the prerequisites checklist are in `docs/google-verification.md`; the
requirements met by the code are: a home page that describes the app and
its use of Calendar data with visible links to `/privacy` and `/terms`
(`web/index.html`); a privacy policy stating the Google API Services User
Data Policy / Limited Use compliance, retention and deletion
(`web/privacy.html`); account deletion that revokes the grant
(`DELETE /account`, `src/auth.rs`); no "Aithos" branding in the app.

## Backend configuration and secret handling

The repository-root `.env` holds `GOOGLE_OAUTH_CLIENT_ID`,
`GOOGLE_OAUTH_CLIENT_SECRET` and `GOOGLE_OAUTH_REDIRECT_URI` for local use;
it is ignored by Git and never printed. Production reads the client secret
from AWS Secrets Manager (`calendar/production/google-oauth-client`,
metadata in `infra/bootstrap/auth.tf`, value uploaded out of band) through
`GOOGLE_OAUTH_CLIENT_SECRET_ID`; the private DynamoDB table and the
runtime variables are in `infra/production`. Never commit the secret or a
client JSON file.

The flow is a server-side authorization-code flow with state, PKCE, nonce
and ID-token validation (`src/google_identity.rs`), a one-shot
browser-bound login row and an opaque session cookie (`src/auth.rs`).
Tokens never appear in Agent Cards, the catalog or the logs.

## Google references

- [Configure the OAuth consent screen](https://developers.google.com/workspace/guides/configure-oauth-consent)
- [OAuth app verification](https://support.google.com/cloud/answer/13463073)
- [Unverified apps and the 100-user cap](https://support.google.com/cloud/answer/7454865)
- [Google API Services User Data Policy](https://developers.google.com/terms/api-services-user-data-policy)
- [Calendar API scopes](https://developers.google.com/workspace/calendar/api/auth)
- [Refresh-token expiration in Testing](https://developers.google.com/identity/protocols/oauth2#expiration)

## Connected Calendar implementation

The backend requests the three Calendar permissions incrementally through
`/auth/google/start?calendar=true` after sign-in. The
[connected booking guide](google-calendar-booking.md) is the manual test
and describes encrypted token storage, expiration and reconnection.
