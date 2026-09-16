# Implementation plan

## Working agreement

The production foundation is deployed. The primary learning objective is A2A:
prove discovery, recipient routing and collaboration before adding calendar logic.
One runtime hosts several logical agents. The public repository is
`Math1987/aithos-calendar`.

Each gate ends with a manual acceptance check, deployed commit and known limits.
Wait for owner acceptance before the next gate; do not provision future resources
in advance. The current authorized scope includes the Gate 4 browser preview: reproduce public
onboarding and the mocked A2A exchange with a minimal web interface.

## Gates

| Gate | Deliverable | Manual acceptance |
| --- | --- | --- |
| 1. AWS foundation — complete | OIDC CI/CD, health, blank HTTPS website, Terraform | Both domains work; repeat apply changes nothing. See [verification](stage-1-verification.md). |
| 1 bis. Rust — complete | Replace Python handler with Rust; build Linux ZIP in CI | Same health JSON, `provided.al2023`, successful deployment and repeat apply without infrastructure changes |
| 2. A2A and catalog fixtures — deployed and verified | Official Rust A2A SDK; fixed Alice/Bob cards; URL catalog; deterministic responses | Start from catalog URL in CLI, fetch each card, call its tenant, obtain distinct Hello responses; reject unknown tenant |
| 3. Agent-to-agent exchange — deployed and verified | Alice calls Bob through real A2A HTTP; behavior still mocked | One traced Alice → Bob exchange; correct recipient configuration, bounded call flow, no recursive loop |
| 4. Dynamic identities and registry — deployed and verified | Create agent/card, publish through Aithos and include card URL in catalog | Create two identities, discover and call both; verify URL deduplication, immutable reuse and retry-safe publication; retire hardcoded production fixtures |
| 4 bis. Browser preview — deployed and verified | Home, shared test page, tutorial; spinner and mock success/error | Create/reuse two page agents in a browser, obtain the mock slot, reject identical/invalid links; see [browser test](browser-test.md) |
| 5. Google calendar logic | Replaceable public-page HTTP reader and deterministic interval selection | Controlled calendars: differing appointment durations, busy event, no common interval; use host duration and verify the full interval |
| 6. Booking | Anakin adapter, durable operation, connect the existing UI to real booking | One real booking; verify attendee identity, invitation, both calendars, retries and ambiguous provider outcomes |
| 7. LLM | AWS Bedrock behind an explicit application boundary | Natural-language interaction drives the same tested operations; code enforces availability, identity and duplicate prevention |

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

For gate 6, resolve booking identity and email verification. A shared service
mailbox is an option to evaluate, not an accepted substitute for the visitor.
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
