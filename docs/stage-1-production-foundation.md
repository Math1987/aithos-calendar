# Stage 1 — Production foundation

Status: ready for launch review; implementation and deployment have not started.

## Outcome and strict scope

A push to `main` deploys:

1. `GET https://api.calendar.aithos.world/health`, returning HTTP 200 with
   `{"status":"ok","service":"calendar"}` as JSON.
2. `https://calendar.aithos.world/`, serving a blank valid HTML page over HTTPS.

There is no user-facing feature beyond those two endpoints. Do not implement
calendar reads, Anakin, identity, matching, A2A, forms, tutorial, database, or jobs.
No Anakin account or API key is needed to finish this stage.

## Launch prerequisites

| Parameter | Planning observation | Stage 1 action |
| --- | --- | --- |
| Local repository | `/Volumes/Math17/aithos/R&D/calendar`, initialized on `main` | Preserve planning commits |
| GitHub visibility | Public | Create only when stage 1 begins |
| GitHub owner | Authenticated CLI account is `Math1987`; repository owner not yet confirmed | Resolve owner before creation; candidate is `Math1987/calendar` |
| AWS profile | `aithos-prod` | Refresh SSO and verify active account |
| AWS region | Profile reports `eu-west-3` | Use Paris for API, Lambda, buckets and logs |
| AWS account ID | Not verified; SSO session expired during planning | Obtain through STS, never infer from profile name |
| DNS | User expects Route 53; account/zone access not yet verified | Find authoritative public `aithos.world` zone and inspect requested names |
| Website certificate region | `us-east-1` required by CloudFront | Configure a second AWS provider alias |
| Tools | `aws`, `gh`, Terraform available; local Terraform reports `1.14.6` | Pin a supported Terraform version consistently at implementation |

SSO renewal is not needed for reviewing these documents. At launch, use:

```sh
aws sso login --profile aithos-prod
aws sts get-caller-identity --profile aithos-prod
```

Any required interactive AWS login belongs to the operator. Once authenticated,
automate the remaining infrastructure and repository setup through CLI/Terraform.
Do not inspect or copy credential stores or fall back to long-lived AWS keys.

## Proposed files to implement

```text
.github/workflows/deploy-production.yml
src/health/handler.py
web/index.html
infra/bootstrap/
  main.tf
  variables.tf
  outputs.tf
  versions.tf
  terraform.tfvars.example
infra/production/
  backend.tf
  versions.tf
  variables.tf
  main.tf
  api.tf
  website.tf
  dns.tf
  outputs.tf
  terraform.tfvars.example
scripts/bootstrap.sh
docs/operations.md
```

These are future files, not files present in this planning commit. Keep Terraform
files grouped by responsibility without adding modules, workspaces, or environment
directories. Commit provider lockfiles after dependency initialization. No `.env`
is needed for this stage; profile authentication and non-secret configuration suffice.

## Execution sequence

### 1A. Preflight and public repository

- Verify Git status, exact project root, Git identity, AWS account/region, hosted
  zone authority and IAM permissions needed for bootstrap.
- Inspect existing API/website DNS records, certificate validation records,
  account-wide GitHub OIDC provider, and target resource names. Reuse/import only
  deliberately; never overwrite another application's records.
- Confirm the repository owner and create an empty public `calendar` repository
  without a generated README or other unrelated history.
- Set `origin` and record the actual repository and owner IDs. Do not push yet:
  configure OIDC, remote state and repository variables before the first workflow.
- If the repository name already exists, inspect it and resolve ownership/history
  before proceeding. Never force-push or silently replace it.

Manual check: the correct public repository exists, local history is preserved,
and the account/zone/resource identities have been recorded without secrets.

### 1B. Bootstrap Terraform state and IAM

Implement a small rerunnable bootstrap script using the local `aithos-prod` profile.
Its job is to orchestrate Terraform initialization/state migration, not to hide a
second imperative infrastructure definition.

1. Run the bootstrap root initially with a local backend. Its files and backup
   state must already be ignored by Git.
2. Create a dedicated S3 state bucket, with account/region-based unique naming,
   versioning, SSE-S3 encryption, block-public-access settings, and TLS-only
   access. Protect it from accidental destroy; do not enable force deletion.
3. Reuse a compatible existing GitHub OIDC provider through a data source. If none
   exists, create one in bootstrap. Do not change unrelated roles/provider trust.
4. Create a deployment role restricted to this repository's `main` branch, OIDC
   audience `sts.amazonaws.com`, and exact current subject claim format. Resolve
   owner/repository immutable IDs as required for new repositories. No wildcard
   repository or branch trust; no credentialed PR workflow.
5. Create a Lambda execution role that trusts only Lambda and can create streams
   and write events in the designated health log group. Production Terraform owns
   the log group itself; the function has no general AWS permissions.
6. Give the deployment role only the required production-resource permissions and
   access to the production state/lock objects. `iam:PassRole` is limited to the
   execution role and `lambda.amazonaws.com`. Handle any required service-linked
   role creation explicitly in bootstrap, rather than granting broad IAM rights
   to production CI.
7. Generate an ignored `backend.generated.tf` for the bootstrap root, with an S3
   backend pointing to `bootstrap/terraform.tfstate`. Migrate using
   `terraform init -migrate-state`, and verify the remote state contains the
   resources just created before removing any local recovery copy.
8. Use a distinct S3 key, `production/terraform.tfstate`, for production.
   Enable `use_lockfile = true` for both roots. The deployment role can get/put
   production state and get/put/delete its `.tflock` object, but cannot write the
   bootstrap state or change bucket security and OIDC permissions.

The script must detect whether state already exists. If a bootstrap was interrupted,
reconcile the local/remote state before continuing; never blindly overwrite a remote
state object, create a second backend, or restart from empty local state. A fresh
checkout with an existing backend should initialize that backend directly.

Manual check: S3 remote state and versioning exist; rerunning initialization uses
the same bucket/state; bootstrap trust and execution-role policies are reviewable.

### 1C. Minimal runtime and production Terraform

- Write one standard-library Python handler. Return the exact documented JSON
  using API Gateway payload v2. No framework or third-party requirements.
- Write one HTML file with metadata/title and an empty body.
- Configure providers for `eu-west-3` and ACM in `us-east-1`.
- Package only the handler source with `archive_file`. Exclude bytecode/cache
  files, write the ZIP under an ignored build directory, and use its base64 SHA-256
  in Lambda `source_code_hash`. Directory creation must be automatic.
- Create the Lambda, its log group, API Gateway HTTP API, Lambda integration,
  `GET /health` route, `$default` stage, API access logs, and invoke permission.
- Add the regional API custom domain and its root mapping. No native API URL
  fallback counts as finishing this stage: the requested domain must work.
- Create the private website bucket, Terraform-managed HTML object, CloudFront
  OAC, distribution and restricted bucket policy. Use the REST origin, not public
  S3 website hosting. Disable document caching initially.
- Request and DNS-validate the two domain certificates. Create scoped Route 53
  aliases and wait for certificate validation/distribution deployment to finish.
- Output API base URL, health URL, website URL, function name, log-group names,
  and distribution ID. Do not output state or credentials.

Before first deployment, run local Terraform format/validation and inspect the
plan for scope, replacements, and any unexpected deletions. These are implementation
checks, not a new automated CI testing pipeline. Resolve validation-record collisions
through deliberate reuse/import; do not enable blanket DNS overwrite behavior.

### 1D. Configure and run GitHub Actions

Set repository variables, not secrets, for the non-secret deployment inputs:

| Variable | Purpose |
| --- | --- |
| `AWS_REGION` | `eu-west-3` |
| `AWS_ROLE_ARN` | OIDC deployment role |
| `TF_STATE_BUCKET` | Existing bootstrapped state bucket |
| `ROUTE53_ZONE_ID` | Verified public zone |
| `LAMBDA_EXECUTION_ROLE_ARN` | Narrow bootstrap-owned runtime role |

Use these inputs to configure the backend and Terraform variables. Keep the
local AWS profile out of the workflow and production provider configuration;
GitHub receives short-lived AWS credentials through OIDC.

Workflow behavior:

1. Trigger on push to `main`, with `contents: read` and `id-token: write`.
2. Use concurrency group `calendar-production`, `cancel-in-progress: false`, and
   a job timeout that allows initial certificate/distribution creation.
3. Checkout the committed source; install the pinned Terraform CLI.
4. Assume the OIDC role through the AWS credentials action.
5. Run `terraform init -input=false` on the production root with its S3 backend.
6. Run `terraform apply -input=false -auto-approve -lock-timeout=5m` there.
7. Print only the safe target URLs and deployed commit in the job summary.

Pin the Actions to reviewed commit SHAs and commit provider lockfiles before
pushing. No static AWS secrets, PR runs, unit tests, lint jobs, matrices, release
workflow, hosted staging, application secrets, or Anakin calls.

Commit the implementation and push `main` only after repository variables and
bootstrap are ready. That push must trigger the first production deployment.
If OIDC fails, fix its precise subject/audience/trust instead of broadening the
trust to all branches. Do not print the OIDC JWT while diagnosing it.

### 1E. Manual production acceptance

1. Open the Actions run and confirm its successful commit matches local `main`.
2. Check the API on its final hostname:

   ```sh
   curl --fail --show-error -i https://api.calendar.aithos.world/health
   ```

   Expect HTTP 200, JSON content type, and exactly these body fields:

   ```json
   {"status":"ok","service":"calendar"}
   ```

3. Check the static document:

   ```sh
   curl --fail --show-error -i https://calendar.aithos.world/
   ```

   Expect HTTP 200 and HTML content type. Open the URL in a browser: valid TLS,
   blank body, no forms, no scripts, no unexpected certificate warning.
4. Open `http://calendar.aithos.world/` and verify it redirects to HTTPS. Check an
   unconfigured API path returns 404; health is the only functional route.
5. Inspect the two CloudWatch log groups and verify the manual request appears
   without request bodies or credentials. Verify the S3 origin is not publicly
   readable and the state bucket has public access blocked/versioning enabled.
6. Make a harmless tracked documentation change and push `main` again. Confirm
   another successful deployment reuses the same production state and resources.
   Inspect for unnecessary resource changes. No parallel applies should run.
7. Confirm the repository is public, generated/credential files are ignored,
   provider lockfiles are tracked, and the working tree is clean.

The purpose of the second deployment is to prove the CI path is repeatable, not
to generate an empty infrastructure change for its own sake.

## Failure handling and recovery

- Preserve state after a failed or partially completed apply; fix the cause and
  rerun against the same backend. Never delete state to force a successful apply.
- Terraform locks and workflow concurrency protect against simultaneous writers.
  Never cancel a running apply or force-unlock without investigating the owner.
- If DNS or certificate validation fails, identify the specific record/account/
  permission issue. Do not claim the final domains are ready based on native URLs.
- For a later code regression, revert the relevant commit and push `main`; inspect
  any infrastructure reversal before applying. This is not a guarantee that
  reverting a commit safely reverses every infrastructure change.
- Destructive changes and resource replacement outside the planned scope require
  explicit review; no automated destroy workflow is included.

## Stage 1 completion report and stop point

Report the following together:

- Both final URLs and the health curl command/expected JSON.
- Last successful Actions run, deployed commit and local/remote Git state.
- Actual created/reused resources and any remaining deviations.
- Manual acceptance results and tests still awaiting the owner's validation.
- Cost categories: Lambda/API requests and execution, S3 storage/requests,
  CloudFront requests/transfer, CloudWatch ingestion/storage and any relevant DNS
  charges. Reuse the existing hosted zone. Do not assume free-tier coverage or
  quote a fixed monthly cost before checking account/pricing conditions.

Stop at that checkpoint. A healthy API and blank website do not authorize starting
the calendar integration or product UI.

## Technical references

See the verified primary-source links in [Architecture](architecture.md#sources-checked-during-planning),
particularly Terraform's S3 backend permissions and GitHub's current OIDC subject
format. Recheck these details during implementation rather than relying on a
historic copy of configuration.
