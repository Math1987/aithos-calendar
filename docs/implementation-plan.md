# Implementation plan

## Working agreement

The production foundation is deployed. The primary learning objective is A2A:
prove discovery, recipient routing and collaboration before adding calendar logic.
One runtime hosts several logical agents. The public repository is
`Math1987/aithos-calendar`.

Each gate ends with a manual acceptance check, deployed commit and known limits.
Wait for owner acceptance before the next gate; do not provision future resources
in advance. The current authorized scope is the Rust health migration only.

## Gates

| Gate | Deliverable | Manual acceptance |
| --- | --- | --- |
| 1. AWS foundation — complete | OIDC CI/CD, health, blank HTTPS website, Terraform | Both domains work; repeat apply changes nothing. See [verification](stage-1-verification.md). |
| 1 bis. Rust — complete | Replace Python handler with Rust; build Linux ZIP in CI | Same health JSON, `provided.al2023`, successful deployment and repeat apply without infrastructure changes |
| 2. A2A and catalog fixtures | Official Rust A2A SDK; fixed Alice/Bob cards; URL catalog; deterministic responses | Start from catalog URL in CLI, fetch each card, call its tenant, obtain distinct Hello responses; reject unknown tenant |
| 3. Agent-to-agent exchange | Alice calls Bob through real A2A HTTP; behavior still mocked | One traced Alice → Bob exchange; correct recipient configuration, bounded call flow, no recursive loop |
| 4. Dynamic identities and registry | Create agent/card, publish through Aithos and include card URL in catalog | Create two identities, discover and call both; verify ownership/isolation and retry-safe publication; retire hardcoded production fixtures |
| 5. Google calendar logic | Replaceable public-page HTTP reader and deterministic interval selection | Controlled calendars: differing appointment durations, busy event, no common interval; use host duration and verify the full interval |
| 6. Booking and minimal UI | Anakin adapter, durable operation, agreed pages and spinner/outcome | One real booking; verify attendee identity, invitation, both calendars, retries and ambiguous provider outcomes |
| 7. LLM | AWS Bedrock behind an explicit application boundary | Natural-language interaction drives the same tested operations; code enforces availability, identity and duplicate prevention |

## Gate details

For gate 2, constructing a client means loading the recipient's card into a
protocol client. It does not create an agent. CLI → Alice and CLI → Bob are two
independent tests. Alice → Bob is gate 3, using Bob's card and recipient tenant.
Tenant routing is not caller authentication.

For gate 4, decide authentication, ownership, persistence and authorization before
exposing identity creation. A public booking URL alone is not proof of ownership.
A registry publication failure must be recoverable without creating duplicate
agents. The catalog lists URLs; cards remain served by Calendar.

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
