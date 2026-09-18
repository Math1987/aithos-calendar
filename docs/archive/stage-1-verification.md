# Stage 1 verification

Verified on 2026-09-15. Only the health API and blank website are deployed.

## Delivery

- Public repository: https://github.com/Math1987/aithos-calendar
- First successful workflow: [34944531672](https://github.com/Math1987/aithos-calendar/actions/runs/34944531672)
- Infrastructure commit: `b5612da64179ebbd95aa9ca42f04fd3948d2a807`.
- AWS account `128066560720`, primary region `eu-west-3`.
- This documentation-only commit triggers the repeatability deployment. Its final
  result is recorded in GitHub Actions and the handoff, avoiding another reporting
  commit just to update a run ID.

## Observed acceptance checks

| Check | Observed result |
| --- | --- |
| `GET https://api.calendar.aithos.world/health` | HTTP 200, JSON content type, exact body `{"status":"ok","service":"calendar"}` |
| Unknown API path `/not-found` | HTTP 404 |
| Native execute-api endpoint | HTTP 404; AWS confirms `DisableExecuteApiEndpoint=true` |
| `https://calendar.aithos.world/` | HTTP 200, valid TLS, HTML matches the tracked 192-byte file |
| Website browser check | Title `Calendar`, visually blank page, no certificate warning |
| Website over HTTP | HTTP 301 to the HTTPS hostname |
| Direct anonymous S3 object request | HTTP 403 AccessDenied |
| Website bucket policy | AWS reports `IsPublic=false`; all four public-access blocks enabled |
| Terraform state bucket | Versioning enabled, AES256 default encryption, all public-access blocks enabled |
| Lambda logs | START/END/REPORT for the manual health invocation, 14-day retention configured |
| API inventory | Exactly one Calendar HTTP API, `if6esu4a73` |

The browser resolved the website normally. Local command-line `curl` still had a
hostname-resolution failure during initial DNS propagation; the HTTP website checks
used `--resolve` with an IP returned by public DNS, keeping normal TLS hostname
validation. Authoritative Route 53 and public resolver queries both returned the
website records. No system DNS settings were changed.

## Managed foundation

Bootstrap owns the two IAM roles/policies and protected state bucket (9 Terraform
resources). Production owns 25 resources: Lambda and invoke permission, runtime
log group, HTTP API/route/integration/stage/domain mapping, private website bucket
and HTML, CloudFront with OAC, two ACM certificates with validation, and scoped DNS
records. Terraform certificate-validation resources represent validation checks.

- State bucket: `aithos-calendar-tfstate-128066560720-eu-west-3`.
- State keys: `bootstrap/terraform.tfstate`, `production/terraform.tfstate`.
- Website bucket: `aithos-calendar-web-128066560720-eu-west-3`.
- Lambda: `calendar-production-health`.
- CloudFront: `EZVXF25TY4NM6`.
- Reused: authoritative hosted zone `Z09988302Y6VWTN77SVQ8`, existing GitHub OIDC
  provider, existing API Gateway service-linked role. No IONOS action was needed.

GitHub authenticates through OIDC for this repository's exact immutable main-branch
subject. Local temporary AWS credentials are not CI secrets. State, plans, ZIPs,
`.env`, and generated backend files remain ignored.

## Adjustments and operational limits

Initial runs exposed missing scoped IAM actions and an invalid manually entered
managed-cache-policy ID. The final configuration looks up `Managed-CachingDisabled`
by name. Partial Terraform state was preserved and reconciled; an API taint caused
one replacement during recovery, and the current API was verified before removing
its taint. No duplicate Calendar API remains.

API management is scoped to the current exact API ARN. A deliberate future API
replacement requires an operator bootstrap change. Routine deployment uses the same
resources and remote state.

API Gateway access logs were deferred after automatic approval review rejected
account-level CloudWatch delivery/policy permissions for CI. Those permissions were
never granted. Lambda runtime logs remain enabled. The empty, unused API log group
was removed. See the [stage-1 adjustment](stage-1-production-foundation.md#implementation-adjustment-api-access-logs).

Usage may incur Lambda/API requests, S3 storage/requests, CloudFront transfer and
requests, CloudWatch storage/ingestion and DNS charges. There is no new hosted zone,
NAT, database, provisioned concurrency, or container service. Actual billing depends
on traffic and account allowances.

## Owner acceptance and stop point

Open the two production URLs and inspect the successful Actions run. A blank page
is the expected website. No calendar reading, booking, Anakin, database, product UI,
or A2A implementation has started. Review stage 1 before authorizing stage 2.
