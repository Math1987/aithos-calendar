# Implementation plan

## Working agreement

The production foundation is deployed. The primary learning objective is A2A:
prove discovery, recipient routing and collaboration before adding calendar logic.
One runtime hosts several logical agents. The public repository is
`Math1987/aithos-calendar`.

Each gate ends with a manual acceptance check, deployed commit and known limits.
Wait for owner acceptance before the next gate; do not provision future resources
in advance. The current authorized scope is the real Google HTTP reader and Anakin booking
integration. Begin with controlled provider checks, then connect them to A2A
and the browser. Derive attendee contact from the visitor booking page, requesting
missing details only when needed; this supersedes the fixed test attendee.

## Gates

| Gate | Deliverable | Manual acceptance |
| --- | --- | --- |
| 1. AWS foundation — complete | OIDC CI/CD, health, blank HTTPS website, Terraform | Both domains work; repeat apply changes nothing. See [verification](stage-1-verification.md). |
| 1 bis. Rust — complete | Replace Python handler with Rust; build Linux ZIP in CI | Same health JSON, `provided.al2023`, successful deployment and repeat apply without infrastructure changes |
| 2. A2A and catalog fixtures — deployed and verified | Official Rust A2A SDK; fixed Alice/Bob cards; URL catalog; deterministic responses | Start from catalog URL in CLI, fetch each card, call its tenant, obtain distinct Hello responses; reject unknown tenant |
| 3. Agent-to-agent exchange — deployed and verified | Alice calls Bob through real A2A HTTP; behavior still mocked | One traced Alice → Bob exchange; correct recipient configuration, bounded call flow, no recursive loop |
| 4. Dynamic identities and registry — deployed and verified | Create agent/card, publish through Aithos and include card URL in catalog | Create two identities, discover and call both; verify URL deduplication, immutable reuse and retry-safe publication; retire hardcoded production fixtures |
| 4 bis. Browser preview — deployed and verified | Home, shared test page, tutorial; spinner and mock success/error | Create/reuse two page agents in a browser, obtain the mock slot, reject identical/invalid links; see [browser test](browser-test.md) |
| 5. Real availability in the browser — accepted | Replaceable public-page HTTP reader, real A2A exchange, actual meeting metadata and deterministic host-slot selection | Controlled calendars: differing appointment durations, busy event, no common interval; use host duration and verify the full interval |
| 6. Booking — implementation underway | Anakin adapter, durable operation, connect the existing UI to real booking | One real booking; verify attendee identity, invitation, both calendars, retries and ambiguous provider outcomes |
| Optional later: natural language | Bedrock only if conversational input becomes useful | Reuse deterministic scheduling and booking; no LLM required for the first usable version |

## Gate details

For gate 2, constructing a client means loading the recipient's card into a
protocol client. It does not create an agent. CLI → Alice and CLI → Bob are two
independent tests. Alice → Bob is gate 3, using Bob's card and recipient tenant.
Tenant routing is not caller authentication.

For gate 4, public onboarding requires no user authentication. One canonical Google
booking page maps to one server-managed agent and card. Repeated and concurrent
submissions reuse the same identity; pending publication can be resumed. Resolving
page identity brings a small part of gate 5 forward, without reading availability.
See [public onboarding](public-onboarding.md). A public URL does not prove ownership.

For gate 5, public booking pages expose only their advertised intervals. They do
not prove availability outside that coverage. Isolate the undocumented HTTP
protocol, validate URL hosts and redirects, and resolve coverage gaps before
booking. Match the target appointment duration, not equal page durations.

For gate 6, use the visitor page’s public contact with an explicit fallback for
missing or ambiguous fields. This is not authenticated ownership. Validate email
confirmation, any Google verification requirements, and the effect on both calendars.
Use durable operation status and an appropriate execution mechanism for Anakin's
asynchronous work. Do not retry uncertain submissions blindly. Do not keep HTTP
requests waiting for a long booking operation or run background work after a
Lambda HTTP response. Follow the [product design](product-design.md), without
adding a permanent confirmation page to the agreed flow.

For gate 7, an LLM is an optional decision-making component, not a prerequisite
for A2A communication. Keep deterministic scheduling and authorization checks in
application code.

Manual commands and acceptance evidence: [Alice/Bob CLI test](a2a-cli-test.md).

Gate 3 implementation and CLI commands: [agent collaboration](agent-collaboration.md).

Gate 4 implementation, ownership boundaries and acceptance commands: [dynamic agents](dynamic-agents.md).

## Current acceptance: real availability accepted; booking pilot

See [live browser test](live-browser-test.md). Gate 5 reads real schedules and uses
A2A in both directions. Gate 6 now adds an explicit Book button after proposing the first common slot
from tomorrow. The first owner-confirmed browser booking is the live validation
step; inspect its Anakin result before automatic confirmed-success classification. The usable version has no LLM or Bedrock dependency.

Gate 6 implementation and remaining live confirmation prerequisite: [browser booking](booking-browser.md).


## Google account connector — September 2026

The next gates replace the public-page provider path for authenticated accounts:

1. **Sign-in and persistent identity**: Google login, secure session, one profile and
   Aithos AgentCard per verified Google account. Reconnect/reuse and two-account
   browser acceptance: [manual procedure](google-sign-in.md).
2. **Calendar access and A2A availability**: incremental consent, encrypted offline
   credentials, primary calendars, 09:00–18:00 weekdays in each owner's timezone,
   30-minute slots from tomorrow. Each agent accesses only its owner's calendar.
3. **Official Google booking**: recheck availability, explicit Book action, one
   organizer event and guest invitation, idempotency and clear outcomes.
4. **Preferences**: multiple calendars and optional LLM ranking of valid slots.

No LLM is required for the first three gates. Anonymous page identities are not
automatically migrated or assigned to a Google account.

The connected-account Calendar and booking gates are implemented together. See
[Google Calendar manual acceptance and boundaries](google-calendar-booking.md).
The live two-account consent and real meeting test remain manual acceptance.

## Autonomous agent V0

Implementation and manual acceptance: [Autonomous agent](autonomous-agent.md).
Budget admission control is a prerequisite for every paid model invocation.
The authenticated browser path now delegates a complete booking to a durable
worker; deterministic defaults remain available when inference is disabled.
