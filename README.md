# Calendar

A minimal scheduling service: share a link, let the other person paste their
public Google booking-page link, and find a suitable appointment automatically.

## Current status

The Rust service hosts persistent booking-page agents through one shared A2A 1.0
JSON-RPC endpoint. Signed AgentCards are published on Aithos, and the dynamic AI
Catalog references their registry URLs. Public onboarding creates or reuses the
agent for a Google booking-page URL.

Gate 5 is deployed: the [website](https://calendar.aithos.world) displays real
meeting metadata and finds a complete host appointment covered by both pages'
advertised availability through A2A. The two existing test-page agents were
upgraded to card version 0.4.0 without changing their identities or shared links.
See the [manual gate 5 test](docs/live-browser-test.md).

Gate 6 adds a booking pilot: find the first shared slot from tomorrow in the
host time zone, review it, then click **Book**. This sends a real Anakin request
with durable status and duplicate protection. The first live confirmation remains
to be validated; completed requests ask you to check Google’s email and calendar.
See [booking acceptance](docs/booking-browser.md). The first usable version requires no LLM;
Bedrock remains optional for a future conversational interface.
Repository: https://github.com/Math1987/aithos-calendar (public).

## Documentation

- [Real provider checks (in progress)](docs/live-providers.md)
- [Public onboarding manual test](docs/public-onboarding.md)
- [Dynamic identities and Aithos](docs/dynamic-agents.md)
- [Application and SDK logs](docs/logging.md)
- [Agent-to-agent collaboration](docs/agent-collaboration.md)
- [Alice/Bob CLI test](docs/a2a-cli-test.md)
- [Rust migration](docs/rust-migration.md)
- [Stage 1 verification](docs/stage-1-verification.md)
- [Operations and deployment](docs/operations.md)
- [Product design](docs/product-design.md)
- [Technical architecture](docs/architecture.md)
- [Implementation plan](docs/implementation-plan.md)
- [Stage 1: production foundation](docs/stage-1-production-foundation.md)

All project documentation is written in English. The GitHub repository is public. Keep account credentials, calendar samples, attendee details,
Terraform state, and generated artifacts out of Git.

## Deployment targets

| Component | Target |
| --- | --- |
| Website | `https://calendar.aithos.world` |
| API | `https://api.calendar.aithos.world` |
| First endpoint | `GET /health` |
| AWS local profile | `aithos-prod` |
| Primary region | `eu-west-3` (profile configuration) |
| Environment | Production only |

Real Google availability and the Anakin booking transport use separate
adapters. Public page onboarding requires no user authentication; a pasted page
does not prove ownership. The website only submits a booking after the visitor clicks Book.
