# Google OAuth verification dossier — A2A Calendar POC

Prepared 2026-09-18 for the request filed from the Google Cloud console
(Google Auth Platform → Verification Center) for project `aithos-calendar`
(display name **A2A Calendar POC**). Everything below is taken from the
code in this repository; file and function names are given so a reviewer
can check them.

## 1. What the app is

The A2A Calendar POC gives a Google account a software agent that finds a
meeting time with another person's agent and books it in both Google
Calendars. Agents discover and verify each other through a signed AI
Catalog and talk over the A2A protocol; the Google Calendar scopes are
what let an agent know when its owner is free, learn the owner's meeting
habits, and create the meeting.

- Home page: https://calendar.aithos.world/ (describes the app and its use of Calendar data, links to the policies)
- Privacy policy: https://calendar.aithos.world/privacy (retention table, deletion, Google API Services User Data Policy / Limited Use statement)
- Terms of use: https://calendar.aithos.world/terms
- Source code: https://github.com/Math1987/aithos-calendar

## 2. Scopes requested and why each is needed

Sign-in requests only `openid email profile`. The three Calendar scopes
are requested in a separate, explicit consent step
(`/auth/google/start?calendar=true`, `src/google_identity.rs::authorization_url`,
`access_type=offline`, `include_granted_scopes=true`). The scope list is
the constant `SCOPES` in `src/google_calendar.rs`; the server verifies the
granted set before storing a token (`GoogleCalendar::connect`).

### `https://www.googleapis.com/auth/calendar.freebusy`

| | |
| --- | --- |
| Feature | Finding a time that is free for both people. |
| Code | `src/google_calendar.rs`, `GoogleCalendar::availability` → `POST /freeBusy` for `primary`, window ≤ 30 days (`Window::valid`, `src/scheduling.rs`). |
| Why narrower is not enough | Free/busy is the narrowest Calendar scope that reveals when the user is busy; without it the agent cannot avoid conflicts. |
| What is stored | Nothing from the response. Busy intervals are reduced in memory to free weekday 09:00–18:00 slots (`working_slots`) and used for the one negotiation. |

### `https://www.googleapis.com/auth/calendar.calendarlist.readonly`

| | |
| --- | --- |
| Feature | Interpreting working hours and the agreed time in the user's time zone. |
| Code | `GoogleCalendar::availability` → `GET /users/me/calendarList/primary`, keeps `timeZone` only. |
| Why narrower is not enough | The time zone of the primary calendar is not part of the free/busy response or of the OpenID profile; `calendar.calendarlist.readonly` is the read-only, list-only scope that exposes it. |
| What is stored | The time zone string, inside the transient availability result; not persisted. |

### `https://www.googleapis.com/auth/calendar.events.owned`

| | |
| --- | --- |
| Feature | (a) Learning the user's meeting habits with the other person; (b) creating the agreed meeting on the host's primary calendar; (c) accepting the invitation on the guest's primary calendar; (d) recognising a meeting the app created. |
| Code | (a) `GoogleCalendar::history` → `GET /calendars/primary/events` (nine months back to three months ahead, `singleEvents=true`, at most 8 pages); (b) `GoogleCalendar::insert` → `POST /calendars/primary/events?sendUpdates=all`; (c) `GoogleCalendar::accept` → `GET` then `PATCH /calendars/primary/events/{id}` with the user's own `responseStatus: accepted`; (d) `GoogleCalendar::event` → `GET /calendars/primary/events/{id}`. |
| Why narrower is not enough | The app must write events (`events.insert`, `events.patch`), which no read-only scope allows; `calendar.events.owned` is limited to calendars the user owns, which is all the app touches (the primary calendar). The broader `calendar` and `calendar.events` scopes are not requested. |
| What is stored | From history: per event, title (≤ 120 chars), description (≤ 180 chars), start/end, whether the user organised it, whether the peer attended, series id (`src/agent/preferences.rs::Observation`); up to 60 sent once per person per day to the model, up to 20 cached for 24 hours. From the created event: its deterministic id in the booking record (30 days after the meeting). |

## 3. Data handling summary for the reviewer

- Refresh tokens are encrypted with a customer-managed AWS KMS key bound to the account (`GoogleCalendar::connect`), access tokens are minted per operation and not stored (`GoogleCalendar::token`).
- The meeting-habit analysis runs on Amazon Bedrock in `eu-west-3` (`src/agent/model.rs`); the model receives event titles, descriptions and times and returns only a preferred weekday/hour, a duration and a lunch flag; its instructions forbid returning descriptions or identities.
- Nothing from Google is shown publicly: the public catalog, cards and log feed carry agent identifiers, signatures and codes only (`src/public_logs.rs` allow-lists, proven by `tests/public_logs.rs`).
- Deletion: `DELETE /account` (`src/auth.rs::delete_account`) revokes the grant at `https://oauth2.googleapis.com/revoke` (`GoogleCalendar::revoke`) and removes the token, the agent, the account and the session.
- Limited Use: stated in section 9 of the privacy policy; Calendar data is used only for the features above, never for advertising, never sold, transferred only to Amazon Bedrock for the analysis.

## 4. Demo video script

Record at 1080p with the browser's address bar visible, in English, without
cuts inside a step. Use two Google accounts (host and guest). Unlisted
YouTube link goes into the form.

1. **Home page** — open https://calendar.aithos.world/. Show the app name "A2A Calendar POC", the paragraph "What it does with your Google Calendar", and click the Privacy policy and Terms links; scroll the privacy policy to the scope table and to section 9 (Limited Use).
2. **Sign-in** — "Continue with Google" → Google's account chooser shows the app name **A2A Calendar POC**; sign in as the host. Show the account page: name, e-mail, "Your agent's link".
3. **Calendar consent** — click "Connect Google Calendar". On Google's consent screen, show the three Calendar permissions requested (free/busy, calendar list read-only, events you own) and approve.
4. **Scope use, visibly** — on the account page, paste the guest's agent link and click "Organize and book". While the agents work, open https://calendar.aithos.world/logs in a second tab and show the trust verification steps and the A2A exchange. Back on the account page, show the outcome: "Your meeting is booked … Confirmed in both primary calendars" and, when present, "Previous meetings with this person" (this is the `calendar.events.owned` history read; the free/busy read and the time-zone read happened during the negotiation).
5. **The created event** — open Google Calendar for the host: show the event "Host name / Guest name" with the guest as attendee. Open Google Calendar for the guest: show the same event, accepted.
6. **Deletion** — on the host's account page, open "Delete my account and agent", click "Delete permanently"; show the "Your account was deleted" message. Then open https://myaccount.google.com/connections (Third-party apps & services) for the host and show that A2A Calendar POC no longer has access. Reload https://api.calendar.aithos.world/.well-known/ai-catalog.json to show the host's agent is gone from the catalog.

## 5. Prerequisites checklist (console)

| Item | Where | Value |
| --- | --- | --- |
| App name | Google Auth Platform → Branding | `A2A Calendar POC` |
| User support e-mail, developer contact | Branding, Contact information | `mathieu@aithos.fr` |
| App logo | Branding | none (a logo triggers a separate brand review) |
| Home page | Branding | `https://calendar.aithos.world/` |
| Privacy policy link | Branding | `https://calendar.aithos.world/privacy` |
| Terms of service link | Branding | `https://calendar.aithos.world/terms` |
| Authorized domain | Branding | `aithos.world` |
| Domain ownership | Google Search Console, with an owner account of the Cloud project | see "Domain verification" below |
| Scopes | Data access | `openid`, `email`, `profile`, `…/auth/calendar.freebusy`, `…/auth/calendar.calendarlist.readonly`, `…/auth/calendar.events.owned` |
| Per-scope justification | Verification form | section 2 of this document |
| Demo video | Verification form | unlisted YouTube link recorded with the script in section 4 |
| Publishing status | Audience | "In production" (publish before or while filing; the app keeps working with the unverified-app screen and the 100-user cap during review) |
| Project display name | Cloud console → project settings | `A2A Calendar POC` (ID `aithos-calendar` unchanged) |

### Domain verification

Two ways, both need a value that only Search Console gives:

- **URL-prefix property `https://calendar.aithos.world/`** (recommended): Search Console's "HTML file" method hands out a file `google<token>.html`; commit it as `web/google<token>.html` and add it to `infra/production/website.tf` as an `aws_s3_object` next to `privacy`/`terms` (the deploy role may write only listed keys, so add the key to `infra/bootstrap/trust.tf` and apply bootstrap first). CI then serves it at `https://calendar.aithos.world/google<token>.html`. This is fully in Terraform and does not touch the apex zone.
- **Domain property `aithos.world`** (DNS TXT `google-site-verification=<token>` at the apex): the Route 53 zone `Z09988302Y6VWTN77SVQ8` hosts the apex, but production Terraform manages only the `calendar.` and `api.calendar.` records and the deploy role is restricted to those names and to A/AAAA/CNAME types (`infra/bootstrap/deployment-policy.tf`). An apex TXT therefore has to be added by hand (Route 53 console or admin CLI), merged into the existing apex TXT record set if there is one; it is not added to `dns.tf`.

## 6. After filing

Google's review of sensitive scopes typically takes several business days
to a few weeks and may come back with questions about the video or the
policy. Keep the app published meanwhile. When the status changes, record
it in `docs/google-oauth-setup.md`.
