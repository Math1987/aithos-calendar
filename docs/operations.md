# Production operations

## Scope

Stage 1 deploys only a liveness endpoint and blank static website. No calendars,
booking provider, application database, or A2A implementation is running.

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
The workflow uses pinned Action commits and provider lockfiles. Terraform archives
the single Python source into an ignored `infra/production/health.zip`, tracks its
hash, and manages the HTML object. There is no separate build or upload command.

Inspect changes locally without deploying:

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
HTML HTTP 200 with a blank body in the browser; HTTP-to-HTTPS redirect for the
website; 404 on the unknown API path. The native execute-api endpoint is disabled.

Inspect the workflow run, deployed commit and logs:

```sh
python3 scripts/with-env.py gh run list --repo Math1987/aithos-calendar --limit 3
python3 scripts/with-env.py aws logs tail /aws/lambda/calendar-production-health --since 10m
python3 scripts/with-env.py aws logs tail /aws/apigateway/calendar-production --since 10m
```

Logs retain 14 days. API logs include request IDs, route, status and timing metadata;
no request bodies, credentials, or IP addresses. Health never calls an external API.

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

Persistent resources are two S3 buckets, IAM roles, certificates, log groups, a
Lambda function, HTTP API and CloudFront distribution, plus records in an existing
zone. Usage can incur storage, requests, execution, logs and transfer charges.
There is no NAT, provisioned concurrency, database, container service, or new DNS
zone. At this blank/health stage usage should be low; actual billing depends on
traffic and account allowances. No fixed monthly price is promised.
