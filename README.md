# Calendar

A minimal scheduling service: share a link, let the other person paste their
public Google booking-page link, and find a suitable appointment automatically.

## Current status

Stage 1 is implemented; first production deployment is being prepared. It contains
only `GET /health`, a blank static website, Terraform and GitHub Actions.
Repository: https://github.com/Math1987/aithos-calendar (public).

## Documentation

- [Operations and deployment](docs/operations.md)
- [Product design](docs/product-design.md)
- [Technical architecture](docs/architecture.md)
- [Implementation plan](docs/implementation-plan.md)
- [Stage 1: production foundation](docs/stage-1-production-foundation.md)

All project documentation is written in English. The eventual GitHub repository
will be public. Keep account credentials, calendar samples, attendee details,
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

These are target addresses, not a claim that the service is deployed.
