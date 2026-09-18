# Google Calendar: consent, A2A scheduling and confirmed booking

## Current browser flow

1. Sign in at `/account`. The existing Google identity and agent are reused.
2. Click **Connect Google Calendar**, choose the same Google account, and grant
   all requested permissions. Repeat in another browser profile for the second account.
3. Copy the host's **agent link** (`https://calendar.aithos.world/book/<id>`).
4. As the visitor, open that link or paste it in **The other person’s agent link**.
5. Click **Find a time**. The first common 30-minute interval is proposed, from
   tomorrow in the host's timezone, within the next 30 days. Both calendars must
   be free during their own Monday–Friday 09:00–18:00 working windows.
6. Click **Confirm booking**. The server rechecks both primary calendars, creates
   one host event with the visitor as attendee, and accepts it using the visitor's
   own authorization. Success requires verification of the guest calendar copy.

There is no LLM in this path. Google Calendar API handles booking directly;
Anakin and public Google booking pages remain a separate, legacy flow. A public
Google booking-page link cannot stand in for an authenticated Aithos agent link.

Both participants can see each other's Google email address on the shared event.
The UI explains this before confirmation. No event titles or descriptions are
read to compute availability. This version uses primary calendars only; busy
items on other calendars do not block a time. It does not create a Google Meet
conference link, manage cancellations, or implement working-hour preferences.

## Identity and permission boundaries

- Sign-in requests `openid email profile` only. Calendar consent is incremental,
  with PKCE, one-time browser-bound state, offline access and explicit consent.
- Calendar consent must return the same verified Google subject as the signed-in
  account bound to the attempt. Choosing another account cannot attach its tokens.
- Calendar scopes: `calendar.freebusy`, `calendar.events.owned`, and
  `calendar.calendarlist.readonly`. CalendarList supplies the primary timezone.
- Refresh tokens are encrypted by `alias/calendar-production-google-tokens` with
  encryption context `{service: calendar, account: <opaque ID>}`. Only ciphertext
  and the private invitation email are persisted in the auth table. Google tokens
  never appear in cards, catalog, logs, browser storage, A2A messages or TF state.
- Disconnect deletes the service's saved connection. Sign-out only ends the app
  session. Neither action deletes events; disconnect does not revoke the entire
  Google app grant. Users can revoke the app in Google account settings.
- This remains an External/Testing pilot restricted to the two configured emails.
  Calendar refresh tokens in Testing can expire after seven days. Reconnect then.
  Public launch still requires the appropriate Google publishing/verification work.

## A2A and discovery

The visitor's coordinator loads the catalog, fetches the host's registered card,
selects its exact `/a2a` interface and tenant, and builds the official Rust SDK
client. It sends `get_availability`, then `commit_booking` only after confirmation.
The host handler accesses its own credential and calendar. OAuth credentials are
not transferred between agents. A common runtime can execute either identity.

Each internal call gets a random 90-second bearer capability, stored by digest,
bound to caller, recipient and the complete operation payload. It is deleted after
the call. A tenant or public AgentCard alone grants no Calendar access. The SDK
passes the actual HTTP authorization header separately from message metadata.
Browser writes require a session and the exact website Origin.

Account cards are version `0.6.0`, signed by the agent's own key and served
from this deployment with a guarantor-signed manifest carrying the
`account-verified` attestation (`docs/trust-layer.md`). The runtime reads an
agent's signing key only to sign its outgoing A2A requests. Public greetings
remain available.

### Pinned SDK compatibility

The pinned Rust SDK serializes security requirements as OpenAPI maps; A2A 1.0
canonical wire JSON uses `schemes` and `StringList`. It also expects an explicit
empty `list` while canonical JSON omits default fields. Signing normalizes to
canonical A2A JSON; discovery adapts the in-memory client view before SDK parsing.
Signed bytes are never rewritten after publication. A regression test uses signed
canonical cards through the real SDK. Other clients with the same SDK limitation
may need this adapter until an upstream fix; this is not an OAuth workaround.

## Booking state and retries

- A server-stored proposal is valid for 15 minutes, belongs to one visitor, and
  cannot be changed by the browser. Confirmation is an authenticated POST.
- `gc` + 40 hex characters identifies the operation. Legacy anonymous booking
  endpoints accept UUIDs only and cannot retrieve connected-account operations.
- The existing bookings table atomically stores the operation, locks both agents,
  and guards the pair/time against duplicates. A revision check claims each
  reconciliation attempt. `/auth/me` returns the visitor's pending operation ID
  so recovery is possible from another browser after sign-in.
- The Google event ID is deterministic: `ac` + SHA-256(operation ID). A duplicate
  request or lost response reconciles this event, without another event insertion.
- Host insertion uses `sendUpdates=all`. Guest RSVP updates only its response
  (`attendeesOmitted=true`), with an ETag when available. Hidden invitations can
  still receive an RSVP update; a missing copy remains pending. Readback checks
  the event ID, times, non-cancelled status and accepted self attendee.
- `booked` means the host event and accepted guest copy were verified.
  `confirming_guest` means propagation/acceptance remains pending.
  `outcome_unknown` means verification did not establish a final outcome.
- Uncertain operations retain locks. **Check booking** reconciles the same ID.
  An operator must investigate a persistently absent/deleted/moved event; do not
  simply clear locks and retry. Host calendar changes can be made outside this
  service, so Google FreeBusy + insertion cannot provide an atomic external lock.
  A change made elsewhere between final checks and insertion remains a race.

## Manual acceptance

Use the two test accounts in separate browser profiles. Connect both calendars,
then compare a proposed slot against both primary calendars. Confirm one real
meeting only when ready to send the Google invitation. Verify one event in each
calendar with identical times and the guest marked accepted. Refresh the page and
repeat the status check: there must be no duplicate event.

Also verify: own link rejected; host without Calendar consent yields a clear
message; declined consent leaves sign-in intact; a newly blocked proposed time
is rejected; reconnect preserves the account and card; unknown/pending results
never claim success or encourage a fresh booking.

Publication, sign-in and Calendar errors are displayed separately. The earlier
blanket catch could label rendering or session errors as publication failures.
The two existing production account agents were found published during diagnosis;
the exact cause of the reported transient message was not captured.

Automated acceptance uses fake Google responses and a real local A2A HTTP/SDK
exchange. Production smoke checks verify routing, identity isolation, anonymous
access rejection, cards and health without accessing or booking real calendars.

## Deployment

Bootstrap creates the dedicated KMS keys (Google token encryption, operator
and guarantor signing), aliases and scoped runtime policies. CI then builds
the Lambda and deploys the `/calendar/*` POST routes, environment, and
website together. Accounts created before the trust layer are not migrated:
the agents table is purged and each person signs in again to obtain a
locally signed card (decision recorded in the archive handoff).

The meeting created on the host's calendar is titled `"<Host> / <Guest>"`
from the Google sign-in names (stored by account id at first login), falling
back to the local part of the connected e-mail and then to `Host`/`Guest`;
names are trimmed, stripped of control characters and bounded to 64
characters, and never appear in logs. The event carries the private property
`a2aBookingId`; reconciliation uses the deterministic event id, not the
property.

## References

- [Google: display a shared event in attendees' calendars](https://developers.google.com/workspace/calendar/api/concepts/inviting-attendees-to-events)
- [Google: event fields and RSVP-only updates](https://developers.google.com/workspace/calendar/api/v3/reference/events)
- [Google: event insertion](https://developers.google.com/workspace/calendar/api/v3/reference/events/insert)
- [Google: OAuth web-server flow](https://developers.google.com/identity/protocols/oauth2/web-server)

## Deployment evidence — 2026-09-17

- Code commit: `b06d7b825e2da148d8b1feca9d33940f95e51a08`.
- [Production deployment](https://github.com/Math1987/aithos-calendar/actions/runs/35203717100)
  succeeded in 8m20s, including 59 offline Rust tests, health, discovery/A2A,
  website/CORS and authenticated-route smoke checks. Two legacy live-page tests
  remain opt-in.
- Bootstrap added exactly the KMS key, alias and runtime policy. IAM simulation
  allows encryption/decryption with the Calendar/account context and denies
  both operations without it.
- The two existing account identities were upgraded to card version `0.6.0`
  (a step that predates the trust layer; those records are purged before the
  trust layer is deployed). Both reject anonymous availability requests.
- Browser preview verified proposal, explicit confirmation, success and a host
  without Calendar authorization. The published account page was reloaded and
  checked separately.
- No real meeting was created during automated or browser verification. The two
  users must still grant Calendar consent and perform the live acceptance test.
