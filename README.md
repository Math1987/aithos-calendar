# Calendar

A minimal scheduling service: share a link, let the other person paste their
public Google booking-page link, and find a suitable appointment automatically.

## Current status

Local planning only. No application, Terraform resources, GitHub repository,
workflow, or deployment has been created yet. The next implementation increment
is limited to production delivery of `GET /health` and an empty static website.
Implementation starts after the planning checkpoint with the project owner.

## Documentation

- [Product design](docs/product-design.md)
- [Technical architecture](docs/architecture.md)

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
