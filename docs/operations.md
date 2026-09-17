# Production operations

## Scope

The service exposes health, persistent public page agents, mock A2A collaboration,
and a minimal browser test. DynamoDB stores signed identities; Aithos publishes
the cards. Production remains mocked until the live availability gate is deployed and cards
are upgraded. The development branch implements real reads and an operator-only
Anakin transport. See [gate 5](live-browser-test.md). See the
[browser guide](browser-test.md) and [public onboarding](public-onboarding.md).

- Repository: https://github.com/Math1987/aithos-calendar (public).
- AWS: account `128066560720`, region `eu-west-3` (Paris).
- API: https://api.calendar.aithos.world/health
- Website: https://calendar.aithos.world/
- Terraform: `1.14.6`; AWS provider `6.64.0`, archive provider `2.8.1`.

## Credentials

GitHub Actions uses OIDC, not the local credentials. The existing AWS OIDC
provider is reused, with the exact immutable subject for this repository's main:

```text
repo:Math1987@55652304/aithos-calendar@1371058059:ref:refs/heads/main
```

For operator commands, use SSO (`AWS_PROFILE=aithos-prod`) or place temporary AWS
credentials and `GH_TOKEN` in the ignored `.env` using `.env.example` names.
The helper runs commands without sourcing arbitrary shell code or printing values:

```sh
python3 scripts/with-env.py aws sts get-caller-identity
python3 scripts/with-env.py gh run list --repo Math1987/aithos-calendar --limit 3
```

An expired local AWS session needs renewing; it does not affect GitHub's OIDC
deployments. Never commit `.env`, copy its values into GitHub variables, or expose
state/plan files. GitHub variables contain only region, role ARNs, bucket and zone ID.

## Bootstrap

The operator manages `infra/bootstrap`: state bucket and both IAM roles/policies.
GitHub cannot change bootstrap permissions or read/write bootstrap state content.
It can list the state bucket to initialize Terraform, and access only production
state and lock objects.

```sh
python3 scripts/with-env.py python3 scripts/bootstrap.py plan
# Review the plan before applying it.
python3 scripts/with-env.py python3 scripts/bootstrap.py apply
```

On first creation the script uses local state, then migrates to S3 after checking
the destination is empty. `-force-copy` is used only for that checked initial
migration. Existing remote state is reused. Incomplete or conflicting state is a
recovery issue: inspect it, never erase it or overwrite it blindly. Ignored local
recovery copies are retained. The generated backend file is recreated from the
script on a fresh checkout.

Backend bucket: `aithos-calendar-tfstate-128066560720-eu-west-3`.
Keys: `bootstrap/terraform.tfstate`, `production/terraform.tfstate`.
Both use native S3 locking, versioning, encryption, blocked public access and TLS.

## Production delivery

Push `main` to run `.github/workflows/deploy-production.yml`. Deployments are
serialized without interrupting an active apply. No PR workflow gets AWS access.
The workflow uses pinned Action commits, Rust toolchain and dependency lockfiles.
Ubuntu 24.04 installs musl-tools and compiles `x86_64-unknown-linux-musl` with
`cargo build --release --locked`. It copies the executable to `.build/bootstrap`.
Terraform archives it with mode 0755 into ignored `infra/production/health.zip`,
tracks its hash and manages the HTML object. No container, Cargo Lambda install,
artifact bucket or separate upload service is required. See [Rust migration](rust-migration.md).

Inspect Terraform changes locally without deploying, **after producing the same
Linux executable** at `.build/bootstrap`. A macOS native build cannot run on Lambda:

```sh
python3 scripts/with-env.py terraform -chdir=infra/production init \
  -backend-config=bucket=aithos-calendar-tfstate-128066560720-eu-west-3
python3 scripts/with-env.py terraform -chdir=infra/production plan \
  -var=route53_zone_id=Z09988302Y6VWTN77SVQ8 \
  -var=lambda_execution_role_arn=arn:aws:iam::128066560720:role/calendar-production-health
```

Routine production applies belong to GitHub. Operator commands must use the same
state. Never use `-lock=false`, delete state, or force-unlock an active deployment.
There is no automated destroy workflow. Review resource replacements/deletions
before pushing infrastructure changes.

## Manual acceptance

```sh
curl --fail --show-error -i https://api.calendar.aithos.world/health
curl --fail --show-error -i https://calendar.aithos.world/
curl --show-error -I http://calendar.aithos.world/
curl --show-error -i https://api.calendar.aithos.world/not-found
```

Expected: JSON HTTP 200 with `{"status":"ok","service":"calendar"}`;
HTML HTTP 200 with the browser test; HTTP-to-HTTPS redirect for the
website; 404 on the unknown API path. The native execute-api endpoint is disabled.

Inspect the workflow run, deployed commit and logs:

```sh
python3 scripts/with-env.py gh run list --repo Math1987/aithos-calendar --limit 3
python3 scripts/with-env.py aws logs tail /aws/lambda/calendar-production-health --since 10m
```

Application and A2A SDK logs share a JSON formatter, with their source identified
by `target`; see [logging](logging.md) for filters and correlation fields.

Lambda logs retain 14 days. They contain runtime invocation reports; the handler
does not log request bodies, credentials, or IP addresses. Health never calls an
external API. API Gateway access logs are deferred: enabling delivery requires
account-level CloudWatch permissions that the deployment role does not have.
See the implementation note in the stage-1 plan.

## DNS, hosting, and recovery

Reuse the authoritative Route 53 zone `Z09988302Y6VWTN77SVQ8`. Production Terraform
owns only the two project names and ACM validation records. The API certificate is
in Paris; CloudFront's certificate is in `us-east-1`. Keep validation records so ACM
can renew certificates. The S3 website bucket stays private; CloudFront OAC grants
only its distribution access. The initial HTML is uncached, so no invalidation step
is required. Do not enable public S3 website hosting.

When an apply fails, preserve its partial state, inspect the actual error, fix it
and rerun the workflow. Do not broaden OIDC trust or attach AdministratorAccess to
the deployment role to repair a failed permission check.

Creation of resources with generated IDs and some API Gateway/tag/OAC discovery
operations require broader resource patterns. Other permissions use project tags,
account/region, exact function/bucket names and DNS record restrictions. IAM role
management is kept outside the deployment role; `PassRole` targets only the health
runtime role. Reevaluate permissions when adding resources in later stages.

A code rollback is a reviewed `git revert` followed by a push to `main`. Review any
infrastructure reversal separately; an old commit does not guarantee safe resource
rollback. S3 state versioning is a recovery aid, not a substitute for reconciliation.

## Cost footprint

Persistent resources are two S3 buckets, a DynamoDB agent table, IAM roles, certificates, log groups, a
Lambda function, HTTP API and CloudFront distribution, plus records in an existing
zone. Usage can incur storage, requests, execution, logs and transfer charges.
There is no NAT, provisioned concurrency, container service, or new DNS
zone. Usage depends on onboarding and A2A traffic; actual billing depends on
traffic and account allowances. No fixed monthly price is promised.

### API Gateway authorization boundary

The deployment role manages only API `if6esu4a73` and its child resources. Its ID
was recorded after first creation so read permissions do not depend on an API-name
condition that AWS exposes only for UpdateApi/DeleteApi. A deliberate API
replacement requires reviewing and updating this exact ARN in bootstrap. Routine
route, integration, stage and function updates do not require that change.

## Mock collaboration gate

The shared Lambda now makes outbound HTTPS calls for catalog, card and A2A
availability discovery. Lambda timeout is 15 seconds; HTTP API integration timeout
is 20 seconds. No IAM expansion or new resource is required. `CATALOG_URL` is
configured in `infra/production/api.tf`; update that environment value through
Terraform to use the future registry. Keep concurrent invocations available so a
coordinator can await the recipient invocation. See [gate 3](agent-collaboration.md)
for limits, the JSON response contract and trace-based verification.

## Dynamic identity operations

Gate 4 adds `calendar-production-agents` (DynamoDB on-demand, SSE, PITR and deletion
protection). Bootstrap grants only the table-specific deployment and runtime
permissions. Public onboarding replaces the initial IAM administration API. Lambda reads project only
record/publication attributes; recovery signing keys are never returned by the API.
See [public onboarding](public-onboarding.md) for CLI commands and publication retries.
An uncertain creation is retried by submitting the same booking-page URL.

### Public onboarding supersedes IAM administration

The current route is anonymous `POST /agents` with `booking_page_url`. The three
`/admin/agents/...` routes are removed. `CALENDAR_WEBSITE_URL` replaces the former
`ADMIN_AWS_ACCOUNT_ID` environment setting. DynamoDB IAM policies are unchanged.
Repost the same URL to resume a pending publication; never regenerate a tenant.
See [public onboarding](public-onboarding.md) for current commands.
