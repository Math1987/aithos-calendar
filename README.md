# Calendar

A minimal scheduling service: share a link, let the other person paste their
public Google booking-page link, and find a suitable appointment automatically.

## Current status

The Rust service is deployed with a URL catalog,
Alice/Bob Agent Cards and a shared A2A 1.0 JSON-RPC endpoint with mock greetings and agent-to-agent slot matching.
Alice → Bob and Bob → Alice were verified in production on September 16, 2026.
See the [collaboration guide](docs/agent-collaboration.md) for commands and evidence.
The static website remains blank.
Repository: https://github.com/Math1987/aithos-calendar (public).

## Documentation

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

Calendar reading, booking and LLM integration have not started. See gate 4 for registry publication status.
