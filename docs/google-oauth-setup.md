# Google OAuth setup for Aithos Calendar

## Configuration status

The following Google Cloud settings were saved and verified in the console on
2026-09-17. The first server-side sign-in gate is implemented locally. Deployment and a real
Google login round trip remain to be verified.

| Item | Value |
| --- | --- |
| Google Cloud project name | Aithos Calendar |
| Project ID | `aithos-calendar` |
| Project number | `235708078636` |
| Organization | `aithos.fr` (`1034671875644`) |
| Google Calendar API | Enabled (`calendar-json.googleapis.com`) |
| OAuth application name | Aithos Calendar |
| Google Auth Platform audience | External |
| Publishing status | Testing |
| Support and developer contact | `mathieu@aithos.fr` |
| Test users | `mathieu@aithos.fr`, `mathieucolla@gmail.com` |
| Application home page | `https://calendar.aithos.world/` |
| Authorized domain | `aithos.world` |
| Consent-screen sign-in scopes | `openid`, `email`, `profile` |
| OAuth client name and type | Aithos Calendar Web; Web application |
| OAuth client ID | `235708078636-686f8i71em5mmsn1b29prrfv4tl8gpt3.apps.googleusercontent.com` |
| Authorized redirect URI | `https://api.calendar.aithos.world/auth/google/callback` |
| Authorized JavaScript origins | None configured |

The callback URI matches the backend route implemented in the first sign-in gate.
A Google client alone does not make login work; the backend must also be deployed. No localhost or wildcard redirect is configured.

## Permissions

The consent screen declares `openid email profile` for the initial sign-in
gate. Request these scopes during login. Validate the Google
ID token and use its stable OIDC `sub` claim as the external identity key. Map
that key to an internal user and one persistent A2A agent. Do not infer ownership
of existing anonymous booking-page agents from a matching email or calendar URL.

The following Calendar scopes have **not** been added to the consent screen or
requested from users yet. Add and request them when the corresponding Calendar
flow is implemented:

| Purpose | Scope | Reason |
| --- | --- | --- |
| Read availability | `https://www.googleapis.com/auth/calendar.freebusy` | Accepted by `freeBusy.query`; returns busy intervals without event details. |
| Create an organizer event on the user's primary calendar | `https://www.googleapis.com/auth/calendar.events.owned` | Accepted by `events.insert` and applies to calendars the user owns. |
| Read the primary calendar's time zone | `https://www.googleapis.com/auth/calendar.calendarlist.readonly` | `calendarList.get("primary")` returns a calendar-list entry with `timeZone`. |

The last scope is necessary only if the product must read the calendar's configured
time zone. `freeBusy.query` can instead return times in UTC or in a time zone
supplied by the application, but it does not return the calendar's configured
time zone. These scopes do not need to be requested for the initial login and
agent-mapping gate. Do not use the broader `calendar` or `calendar.readonly`
scopes for this flow.

## Backend configuration and secret handling

The existing repository-root `.env` contains `GOOGLE_OAUTH_CLIENT_ID`,
`GOOGLE_OAUTH_CLIENT_SECRET`, and `GOOGLE_OAUTH_REDIRECT_URI`. Existing values
were preserved. `.env` is ignored by Git, owned by the current user, and has
mode `0600`. The downloaded client JSON was removed after the values were
stored. Never commit the secret or a client JSON file. For production, follow
the existing AWS Secrets Manager pattern: store the client secret outside
Terraform state under `calendar/production/google-oauth-client` and pass only
`GOOGLE_OAUTH_CLIENT_SECRET_ID` to the Lambda. Production secret metadata and IAM permissions are defined in `infra/bootstrap/auth.tf`;
the private DynamoDB table and runtime wiring are defined in `infra/production`.
The actual secret value must be uploaded separately before deployment.

Implement a server-side authorization-code flow with state validation, PKCE,
OIDC token validation, a secure application session, offline access when needed,
and protected storage and rotation of per-user refresh tokens. Keep tokens out
of AgentCards and the catalog.

## Testing and release

The app explicitly limits this pilot to the two configured test accounts using
`GOOGLE_OAUTH_TEST_USERS`. Google can exempt basic sign-in-only requests from its
Testing restrictions; the backend therefore enforces the pilot allowlist itself. For
Calendar scopes, refresh tokens issued in Testing expire after seven days;
Google exempts grants limited to basic sign-in scopes. Handle reconnection when
refresh tokens expire or are revoked. Before public release, complete the
required OAuth app information, domain verification where requested, scope
review or verification, privacy policy, and publishing transition. Do not use
placeholder legal URLs.

The Google-side configuration was verified by the setup agent. Local credential
presence, the client ID, callback URI, file permissions and Git exclusion were
verified independently. A real login round trip remains a manual acceptance
check after backend deployment. See [the sign-in gate](google-sign-in.md).

## Google references

- [Configure the OAuth consent screen](https://developers.google.com/workspace/guides/configure-oauth-consent)
- [OAuth 2.0 for web-server applications](https://developers.google.com/identity/protocols/oauth2/web-server)
- [Calendar API scopes](https://developers.google.com/workspace/calendar/api/auth)
- [CalendarList.get](https://developers.google.com/workspace/calendar/api/v3/reference/calendarList/get)
- [Freebusy.query](https://developers.google.com/workspace/calendar/api/v3/reference/freebusy/query)
- [Events.insert](https://developers.google.com/workspace/calendar/api/v3/reference/events/insert)
- [Refresh-token expiration in Testing](https://developers.google.com/identity/protocols/oauth2#expiration)
