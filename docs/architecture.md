# Technical architecture

Status: the Rust foundation and Alice/Bob mock A2A gate are deployed and verified.
The sections below describe the foundation; the A2A section records its extension.

## Confirmed choices

| Choice | Reason / boundary |
| --- | --- |
| One AWS production environment | No hosted development or staging infrastructure |
| Rust Lambda, ZIP package | Share the language with the A2A/catalog ecosystem; minimal `lambda_http` handler |
| API Gateway HTTP API | Small public HTTP surface; no REST API features needed |
| Terraform | Own AWS resources, certificates, DNS records, and static object |
| GitHub Actions on `main` | One delivery path with serialized deployment |
| GitHub OIDC to AWS | Temporary credentials; no stored AWS access keys |
| S3 remote Terraform state with native locking | Durable state without a DynamoDB locking table |
| Private S3 + CloudFront for website | Static hosting with HTTPS on the requested domain |
| Direct HTTP availability adapter, later | Proven read path; isolate the undocumented Google protocol |
| Anakin booking adapter, later | Chosen booking provider; live behavior remains to be validated |

The earlier handoff excluded CloudFront when describing an API-only foundation.
The current requirement adds an AWS-hosted HTTPS static site. Use CloudFront for
that site only; do not put it in front of the API.

## Stage 1 topology

```mermaid
flowchart TD
    GitHub[GitHub Actions: main] -->|OIDC| Deploy[Scoped AWS deployment role]
    Deploy --> TF[Terraform: production root]
    TF --> State[Private versioned S3 state + lockfile]
    Browser[Browser] --> Web[calendar.aithos.world / CloudFront]
    Web -->|Origin Access Control| Assets[Private S3 / index.html]
    Client[HTTP client] --> API[api.calendar.aithos.world / HTTP API]
    API -->|GET /health| Lambda[Rust Lambda]
    Lambda --> Logs[CloudWatch Logs]
```

### API

- Primary region: `eu-west-3`, as configured for `aithos-prod`.
- Rust `1.95.0`, AWS OS-only runtime `provided.al2023`.
- Static Linux musl binary, `x86_64`, 128 MB memory, 5-second timeout.
- `lambda_http` adapts API Gateway HTTP API v2 requests/responses.
- One public route: `GET /health`.
- HTTP 200, `Content-Type: application/json`, body:

  ```json
  {"status":"ok","service":"calendar"}
  ```

- Health is a liveness check; it performs no network or provider calls.
- `$default` stage with auto deployment, root custom-domain mapping, no catch-all
  application route. Unsupported routes return 404 through API Gateway.
- Disable the default execute-api endpoint after configuring the custom domain.
- Lambda invoke permission limited to this API and health route.
- Terraform-managed Lambda log group with 14-day retention. API access logs are
  deferred for this health-only stage because their delivery requires additional
  account-level CloudWatch permissions; see the stage-1 implementation adjustment.
- No CORS yet: the blank website makes no API calls.

### Website

- One S3 bucket distinct from the Terraform state bucket.
- Block public access; use the S3 REST origin and CloudFront Origin Access Control.
- Bucket policy grants reads only to the project's CloudFront distribution.
- Serve a minimal valid `index.html` with an empty body, page title, and no scripts,
  forms, framework, analytics, or application assets.
- Terraform uploads the object with `text/html; charset=utf-8` and content hash.
- CloudFront default root object is `index.html`; redirect HTTP to HTTPS.
- Disable caching for this initial document to avoid adding an invalidation step.
  Revisit cache policy when real assets exist.
- No SPA routing fallback yet; no application routes exist.

### DNS and certificates

- Reuse the existing authoritative public zone for `aithos.world` after verifying
  its account, zone ID, and delegation. Never create a duplicate zone as a shortcut.
- API: regional ACM certificate in `eu-west-3`, API Gateway custom domain, Route 53
  alias at `api.calendar.aithos.world`.
- Website: ACM certificate in `us-east-1` for CloudFront, alternate domain
  `calendar.aithos.world`, Route 53 A/AAAA aliases.
- Automate certificate DNS validation with Terraform. Touch only the two requested
  names and required validation records; inspect collisions before applying.

## Terraform organization and ownership

Use two small root configurations, with no reusable Terraform modules yet:

- `infra/bootstrap`: state bucket, GitHub OIDC integration, deployment role/policy,
  and the narrowly scoped Lambda execution role. Run only with the operator's AWS
  profile. Reuse an existing account-wide GitHub OIDC provider without taking
  ownership of or modifying other projects' provider configuration.
- `infra/production`: Lambda, logs, HTTP API, both domain certificates/records,
  CloudFront, private website bucket, and its single static object.

Bootstrap and production have separate state keys in the same encrypted,
versioned, private S3 bucket. Enable `use_lockfile = true`. Production CI cannot
write bootstrap state, modify its own trust/permissions, or change execution-role
permissions. Pass the execution-role ARN as non-secret configuration; allow
`iam:PassRole` only for that role and only to Lambda.

The deployment policy grants the actions necessary for the listed production
resources, scoped by ARN, name, tag, region, DNS record, and service conditions
where supported. Document API operations which require wildcard resources.
Do not substitute AdministratorAccess to make a failed deployment succeed.

Bootstrap initially uses ignored local state to create its backend, then migrates
that state into `bootstrap/terraform.tfstate`. Production always initializes
against `production/terraform.tfstate`; Actions must never use ephemeral local
state. The detailed bootstrap sequence is part of the stage 1 plan.

Protect the state bucket from accidental destruction and do not force-delete
nonempty buckets. Secrets are never Terraform variable values or managed secret
contents. There are no application secrets at stage 1.

## Delivery

One workflow, triggered by pushes to `main`; no PR deployment or
`pull_request_target` execution. Permissions: `contents: read`, `id-token: write`.
Use a single concurrency group with `cancel-in-progress: false`.

Sequence: checkout -> build the locked Rust application for Linux musl -> install pinned Terraform -> assume deployment role using
OIDC -> initialize remote backend -> noninteractive apply -> publish target URLs
in the job summary, after checking runtime configuration and the real health response.
Terraform packages the executable `.build/bootstrap` through `archive_file` and
wires its hash to `source_code_hash`; no Docker or separate build service.

Resolve supported provider versions during implementation, commit dependency
lockfiles, and use the same pinned Terraform CLI version locally and in CI.
Pin third-party Actions to reviewed full commit SHAs.

OIDC trust must match the exact repository, immutable IDs when applicable,
`refs/heads/main`, and audience `sts.amazonaws.com`. New GitHub repositories can
use a subject format containing owner/repository IDs: inspect the actual policy
format at creation rather than copying an older trust-policy example.
Do not add a GitHub Environment without adjusting the subject policy.

CI checks Rust formatting, compiles the locked release, and verifies deployed health.
No separate PR-validation, matrix, release, or staging pipeline at this stage.
Use manual end-to-end acceptance after deployment. A successful workflow is not
by itself proof that the service works on its final domains.

## Future application boundaries — design only

Keep a small Rust application with explicit injected dependencies, not a
plugin framework or a collection of microservices:

| Boundary | Responsibility |
| --- | --- |
| HTTP handlers | Input/output and operation status |
| Scheduling application | Candidate selection and booking orchestration |
| `AvailabilityReader` | Normalized page metadata and available intervals |
| `BookingProvider` | Submit a booking and retrieve its authoritative outcome |
| Repository | Persist share links and durable operations when needed |
| A2A transport | Expose discovery/tasks using the same application logic |

Provider adapters own Google URLs, JSON/protobuf field positions, public client
configuration, Anakin credentials, job IDs, and provider error translation.
Core scheduling uses timezone-aware instants, intervals, a host-defined duration,
and normalized outcomes. Replacing a reader or booking provider should not change
the product handlers or scheduling rules.

Do not use equal visitor and host durations as a matching requirement. Validate
full-interval availability and the limitations recorded in the product design.
Constrain accepted URL hosts and redirects before any server-side fetch.

Anakin uses asynchronous jobs. API Gateway HTTP API has a maximum 30-second
integration timeout, so the eventual booking request should return an operation
ID promptly and expose status polling. Add persistence and a durable execution
mechanism when implementing booking; do not leave a Lambda running after its
HTTP response or keep an HTTP request waiting for the entire booking.
Before enabling writes, resolve vendor idempotency, ambiguous submission outcomes,
identity, email verification, and the visitor calendar's actual busy state.

At the A2A stage, multiple logical agents share this infrastructure. Generate
Agent Cards from validated server configuration and map each agent to its own
resources. A2A discovery does not prove page ownership or grant permission to
book for someone. One designated organizer creates one booking. No A2A SDK,
registry integration, database, worker, or LLM is installed for health.

## A2A discovery and mock-agent gate

One service implementation will host multiple logical agents. Each has an ID,
Agent Card and configuration; later each may have separate memory. Start with
fixed Alice and Bob fixtures, without an LLM or calendar calls.

- Serve URL entries in `/.well-known/ai-catalog.json` pointing to
  `/agents/alice/agent-card.json` and `/agents/bob/agent-card.json`.
- Cards advertise the shared A2A service and their own tenant identifier using
  the supported A2A version's fields and SDK transport mapping.
- Build a client from the **recipient's** card. Calling Bob uses tenant `bob`.
  The server validates this recipient and loads Bob's configuration; unknown
  tenants fail explicitly and must never fall back to another user's config.
- A tenant selects the recipient. It does not authenticate the caller or authorize
  calendar access. The external CLI is initially a test caller, not Alice.
- First test CLI → Alice and CLI → Bob. A later gate tests Alice → Bob over real
  A2A HTTP, even though both are hosted by the same Lambda service.
- The catalog lists card URLs; each card still needs its own serving route.
  Integrate the existing Aithos registry when agent creation becomes dynamic.

The mock gate uses official `a2a-server-lf` 0.4.4, `a2a-lf` 0.3.1 and
`ai-catalog` 0.2.1. See [CLI tests and implementation](a2a-cli-test.md). Keep application behavior separate from transport so fixture
responses can be replaced without changing discovery or tenant routing.

## Explicitly absent from stage 1

Application DynamoDB, Anakin account/secret, OAuth, Google calendar access,
booking operations, SQS, Step Functions, EventBridge jobs, VPC/NAT, containers,
ECR, layers, framework, SDK A2A, LLM, application authentication, and product UI.
Keep ZIP packaging while the binary fits Lambda constraints.

## Sources checked during planning

- [Lambda runtimes](https://docs.aws.amazon.com/lambda/latest/dg/lambda-runtimes.html)
- [HTTP API quotas](https://docs.aws.amazon.com/apigateway/latest/developerguide/http-api-quotas.html)
- [API regional domains](https://docs.aws.amazon.com/apigateway/latest/developerguide/apigateway-regional-api-custom-domain-create.html)
- [CloudFront HTTPS certificates](https://docs.aws.amazon.com/AmazonCloudFront/latest/DeveloperGuide/cnames-and-https-procedures.html)
- [S3 origin access control](https://docs.aws.amazon.com/AmazonCloudFront/latest/DeveloperGuide/private-content-restricting-access-to-s3.html)
- [Terraform S3 state and locking](https://developer.hashicorp.com/terraform/language/backend/s3)
- [GitHub OIDC with AWS](https://docs.github.com/en/actions/how-tos/secure-your-work/security-harden-deployments/oidc-in-aws)
- [Anakin appointments](https://anakin.io/catalog/google_appointments)

## Gate 3: outbound A2A collaboration

See [agent collaboration](agent-collaboration.md) for the current configuration,
wire contract, time budgets and acceptance. The same function now supports an
immediate `find_common_slot` operation that discovers and calls a peer through
the official Rust A2A client. Matching remains deterministic on fixed UTC fixtures.
The nested invocation performs only `get_availability`. No task persistence,
background worker or external calendar integration is introduced.

## Gate 4: persistent identities

The existing Lambda supports DynamoDB-backed agents with anonymous booking-page
onboarding. Aithos hosts signed card bytes; Calendar projects confirmed
publications into its AI Catalog and keeps serving identical card bytes locally.
See [public onboarding](public-onboarding.md) for uniqueness, retries and current
limits, and the [initial identity deployment](dynamic-agents.md) for key custody.

## Public onboarding correction

The first version has no user authentication. `POST /agents` accepts only a public
Google booking-page URL. A replaceable adapter resolves its canonical identity;
a deterministic SHA-256 tenant and a conditional DynamoDB insert guarantee one
agent per page, including concurrent submissions. Existing records and signed
bytes are reused. The former IAM admin routes are removed; Lambda IAM remains
internal. No new table, index or worker is introduced. See [public onboarding](public-onboarding.md).
